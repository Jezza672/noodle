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
    let node_type = node_type(&catalog);
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
        let inputs: Vec<SignalIn<'_>> = input_buffers.iter().map(|b| b.as_in(FRAMES)).collect();
        let mut outputs: Vec<SignalOut<'_>> = vec![output_buffer.as_out(FRAMES)];
        let event_inputs: [&[Event]; 1] = [&events];
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
}
