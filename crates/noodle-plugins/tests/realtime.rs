//! The plugin node follows the real-time rule: once built, processing a block
//! never allocates or frees, even with notes, parameter changes and a wire
//! modulating a parameter.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::*;
use noodle_core::NodeId;
use noodle_engine::{
    Config, Context, Event, EventKind, EventsOut, Instance, Io, NodeType, NoteId, Setup, Shape,
    SignalBuffer, SignalIn, SignalOut, Transport,
};

struct Guarded;

thread_local! {
    static REALTIME: Cell<bool> = const { Cell::new(false) };
    static VIOLATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = REALTIME.try_with(|realtime| {
        if realtime.get() {
            VIOLATIONS.with(|v| v.set(v.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Guarded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Guarded = Guarded;

fn realtime(f: impl FnOnce()) -> usize {
    REALTIME.set(true);
    f();
    REALTIME.set(false);
    VIOLATIONS.replace(0)
}

const FRAMES: usize = 256;

#[test]
fn processing_blocks_with_notes_and_modulation_never_allocates() {
    let catalog = catalog();
    never_allocates(node_type(&catalog), NodeId(0), false);
}

#[test]
fn a_vst3_plugin_processing_blocks_with_notes_and_modulation_never_allocates() {
    let catalog = vst3_catalog();
    never_allocates(vst3_node_type(&catalog), NodeId(0), false);
}

/// The editor's edits are passed to the processor on the audio thread too:
/// the test plugin's editor changes `Gain` when it is attached, and the next
/// blocks send that on.
#[test]
fn a_vst3_editors_edits_reaching_the_processor_never_allocate() {
    struct Fake;
    impl noodle_plugins::EditorWindow for Fake {
        fn parent(&self) -> noodle_plugins::ParentWindow {
            noodle_plugins::ParentWindow::X11(1)
        }
        fn set_size(&mut self, _: u32, _: u32, _: bool) {}
        fn size(&self) -> (u32, u32) {
            (200, 100)
        }
        fn show(&mut self) {}
        fn hide(&mut self) {}
        fn is_closed(&self) -> bool {
            false
        }
    }
    // Another test in this binary may have set the factory; any will do.
    let _ = noodle_plugins::set_window_factory(|_| Some(Box::new(Fake)));
    let catalog = vst3_catalog();
    never_allocates(vst3_node_type(&catalog), NodeId(77), true);
    assert!(!noodle_plugins::is_running(NodeId(77)));
}

fn never_allocates(node_type: &noodle_plugins::PluginNodeType, id: NodeId, editor: bool) {
    let config = Config::new();
    let layout = node_type.layout(&config).unwrap();
    // Wire audio in and the gain parameter.
    let input_shapes: Vec<_> = layout
        .inputs
        .iter()
        .enumerate()
        .map(|(i, _)| if i == 0 { Shape::STEREO } else { Shape::MONO })
        .collect();
    let output_shapes = node_type
        .output_shapes(&config, &layout, &input_shapes)
        .unwrap();
    let Instance::Realtime(mut node) = node_type
        .instantiate(&Setup {
            node: id,
            config: &config,
            sample_rate: 48_000.0,
            max_frames: FRAMES,
            input_shapes: &input_shapes,
            output_shapes: &output_shapes,
            seed: 0,
            position: 0,
        })
        .unwrap()
    else {
        panic!("realtime node");
    };

    let mut input_buffers: Vec<_> = input_shapes
        .iter()
        .map(|&shape| SignalBuffer::new(shape, FRAMES))
        .collect();
    let mut output_buffer = SignalBuffer::new(output_shapes[0], FRAMES);
    let ctx = Context {
        sample_rate: 48_000.0,
        frames: FRAMES,
        transport: Transport {
            playing: true,
            ..Transport::default()
        },
    };

    let mut heard = false;
    for block in 0..40usize {
        // The gain wire moves every block.
        {
            let mut gain = input_buffers[1].as_out(FRAMES);
            for (i, s) in gain.lane_mut(0, 0).iter_mut().enumerate() {
                *s = ((block * FRAMES + i) as f32 * 0.001).sin().abs();
            }
        }
        let events = match block % 4 {
            0 => vec![Event {
                time: 3,
                kind: EventKind::NoteOn {
                    note: NoteId(block as u32),
                    channel: 0,
                    key: 60 + block as u8,
                    velocity: 0.8,
                },
            }],
            2 => vec![Event {
                time: 10,
                kind: EventKind::NoteOff {
                    note: NoteId(block as u32 - 2),
                    velocity: 0.0,
                },
            }],
            _ => Vec::new(),
        };
        // The second note port plays in the blocks between.
        let second = match block % 4 {
            1 => vec![Event {
                time: 20,
                kind: EventKind::NoteOn {
                    note: NoteId(1_000 + block as u32),
                    channel: 0,
                    key: 70,
                    velocity: 0.8,
                },
            }],
            3 => vec![Event {
                time: 5,
                kind: EventKind::NoteOff {
                    note: NoteId(1_000 + block as u32 - 2),
                    velocity: 0.0,
                },
            }],
            _ => Vec::new(),
        };
        if editor && block == 10 {
            // The edit is made now, outside the counted blocks, and passed on
            // inside them.
            noodle_plugins::open_editor(id).expect("the editor opens");
        }
        if block == 20 {
            // A seek resets the node on the audio thread.
            assert_eq!(realtime(|| node.reset()), 0, "reset allocated or freed");
        }
        let inputs: Vec<SignalIn<'_>> = input_buffers.iter().map(|b| b.as_in(FRAMES)).collect();
        let mut outputs: Vec<SignalOut<'_>> = vec![output_buffer.as_out(FRAMES)];
        let event_inputs: [&[Event]; 2] = [&events, &second];
        let mut event_outputs: Vec<EventsOut<'_>> = Vec::new();
        let violations = realtime(|| {
            node.process(
                &ctx,
                Io {
                    inputs: &inputs,
                    outputs: &mut outputs,
                    event_inputs: &event_inputs,
                    event_outputs: &mut event_outputs,
                },
            );
        });
        assert_eq!(violations, 0, "block {block} allocated or freed");
        heard |= output_buffer
            .as_in(FRAMES)
            .lane(0, 0)
            .iter()
            .any(|&s| s != 0.0);
    }
    // The blocks were real: the notes made sound.
    assert!(heard);
    if editor {
        // And the edit was real: Gain, which the wire was driving, is the
        // editor's now (a quarter) in what the plugin reports.
        let captured = noodle_plugins::capture(id).expect("capture");
        assert!(captured.params.iter().any(|&(p, v)| p == 1 && v == 0.25));
    }
}

/// Many modulated parameters and a burst of notes in one block: more events
/// than the buffer holds. The extras are dropped, nothing allocates, and the
/// notes still arrive (parameter changes stop short to leave them room).
#[test]
fn a_flood_of_parameter_changes_and_notes_never_allocates_and_keeps_notes() {
    flood(plugin_path());
}

#[test]
fn a_vst3_flood_of_parameter_changes_and_notes_never_allocates_and_keeps_notes() {
    flood(vst3_path());
}

fn flood(path: std::path::PathBuf) {
    use noodle_plugins::{Catalog, ParamDesc, scan_file};
    let mut file = scan_file(&scanner(), &path);
    // A thousand extra parameters, which the plugin ignores. (Real ids 1 and 2
    // stay, so the gain still works.)
    let plugin = &mut file.plugins[0];
    for id in 100..1100 {
        plugin.params.push(ParamDesc {
            id,
            name: format!("Extra {id}"),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            stepped: false,
        });
    }
    let catalog = Catalog::new(&[file]);
    let node_type = catalog.iter().next().expect("the test plugin");
    let config = Config::new();
    let layout = node_type.layout(&config).unwrap();
    let input_shapes = vec![Shape::MONO; layout.inputs.len()];
    let output_shapes = node_type
        .output_shapes(&config, &layout, &input_shapes)
        .unwrap();
    let Instance::Realtime(mut node) = node_type
        .instantiate(&Setup {
            node: NodeId(0),
            config: &config,
            sample_rate: 48_000.0,
            max_frames: FRAMES,
            input_shapes: &input_shapes,
            output_shapes: &output_shapes,
            seed: 0,
            position: 0,
        })
        .unwrap()
    else {
        panic!("realtime node");
    };
    let mut inputs: Vec<_> = input_shapes
        .iter()
        .map(|&shape| SignalBuffer::new(shape, FRAMES))
        .collect();
    let mut output = SignalBuffer::new(output_shapes[0], FRAMES);
    let ctx = Context {
        sample_rate: 48_000.0,
        frames: FRAMES,
        transport: Transport {
            playing: true,
            ..Transport::default()
        },
    };
    // Every block, every extra parameter moves: 1000 params x 8 steps is
    // well past the 4096 events a block holds. The notes land late in the
    // block, after the modulation has filled the buffer.
    let notes: Vec<Event> = (0..30u32)
        .map(|n| Event {
            time: 200 + n,
            kind: EventKind::NoteOn {
                note: NoteId(n),
                channel: 0,
                key: 60 + n as u8 % 12,
                velocity: 0.5,
            },
        })
        .collect();
    let mut heard = false;
    for block in 0..6usize {
        for (port, buffer) in inputs.iter_mut().enumerate().skip(1) {
            let mut out = buffer.as_out(FRAMES);
            for (i, s) in out.lane_mut(0, 0).iter_mut().enumerate() {
                *s = ((block * 977 + i * 13 + port) % 100) as f32 / 100.0;
            }
        }
        let block_notes: &[Event] = if block == 1 { &notes } else { &[] };
        let signals: Vec<SignalIn<'_>> = inputs.iter().map(|b| b.as_in(FRAMES)).collect();
        let mut outs: Vec<SignalOut<'_>> = vec![output.as_out(FRAMES)];
        let event_inputs: [&[Event]; 2] = [block_notes, &[]];
        let mut event_outputs: Vec<EventsOut<'_>> = Vec::new();
        let violations = realtime(|| {
            node.process(
                &ctx,
                Io {
                    inputs: &signals,
                    outputs: &mut outs,
                    event_inputs: &event_inputs,
                    event_outputs: &mut event_outputs,
                },
            );
        });
        assert_eq!(violations, 0, "block {block} allocated or freed");
        heard |= output.as_in(FRAMES).lane(0, 0).iter().any(|&s| s != 0.0);
    }
    assert!(heard, "the notes were dropped with the flood of changes");
}
