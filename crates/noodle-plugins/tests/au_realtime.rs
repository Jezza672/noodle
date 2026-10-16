//! The AU node follows the real-time rule on the Rust side: once built,
//! processing a block (audio in, a wired parameter, notes, the transport,
//! a seek) never allocates or frees. What Apple's units do inside their own
//! render calls is their code and isn't seen by Rust's allocator. macOS only.

#![cfg(target_os = "macos")]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use common::scanner;
use noodle_core::NodeId;
use noodle_engine::{
    Config, Context, Event, EventKind, EventsOut, Instance, Io, NodeType, NoteId, Setup, Shape,
    SignalBuffer, SignalIn, SignalOut, Transport,
};
use noodle_plugins::{Catalog, scan_file};

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
const RATE: f32 = 48_000.0;

/// `aufx` `dely` `appl`: Apple's delay.
const DELAY: &str = "61756678-64656c79-6170706c";
/// `aumu` `dls ` `appl`: Apple's General MIDI synthesizer.
const DLS: &str = "61756d75-646c7320-6170706c";

/// Processes 40 blocks of the unit `id`. Its audio inputs get a sine, its
/// first parameter is driven by a wire that moves every block, and its note
/// ports play and release notes. Returns whether any block made sound.
fn never_allocates(id: &str) -> bool {
    let file = scan_file(&scanner(), &PathBuf::from(format!("audiounit:{id}")));
    assert_eq!(file.error, None, "scanning {id}");
    let catalog = Catalog::new(&[file]);
    let node_type = catalog.iter().next().expect("the unit is in the catalog");

    let config = Config::new();
    let layout = node_type.layout(&config).unwrap();
    let audio_inputs = node_type.descriptor().audio_inputs.len();
    let input_shapes: Vec<_> = (0..layout.inputs.len())
        .map(|i| {
            if i < audio_inputs {
                Shape::STEREO
            } else {
                Shape::MONO
            }
        })
        .collect();
    let output_shapes = node_type
        .output_shapes(&config, &layout, &input_shapes)
        .unwrap();
    let Instance::Realtime(mut node) = node_type
        .instantiate(&Setup {
            node: NodeId(0),
            config: &config,
            sample_rate: RATE,
            max_frames: FRAMES,
            input_shapes: &input_shapes,
            output_shapes: &output_shapes,
            seed: 0,
            position: 0,
        })
        .unwrap()
    else {
        panic!("a realtime node");
    };

    let mut input_buffers: Vec<_> = input_shapes
        .iter()
        .map(|&shape| SignalBuffer::new(shape, FRAMES))
        .collect();
    let mut output_buffer = SignalBuffer::new(output_shapes[0], FRAMES);
    let notes = layout.event_inputs.len();
    let ctx = Context {
        sample_rate: RATE,
        frames: FRAMES,
        transport: Transport {
            playing: true,
            ..Transport::default()
        },
    };

    let mut heard = false;
    for block in 0..40usize {
        for (port, buffer) in input_buffers.iter_mut().enumerate() {
            let mut signal = buffer.as_out(FRAMES);
            for channel in 0..signal.shape().channels {
                for (i, s) in signal.lane_mut(0, channel).iter_mut().enumerate() {
                    let t = (block * FRAMES + i) as f32;
                    // Audio is a tone; the parameter wires sweep 0..1.
                    *s = if port < audio_inputs {
                        (t * 0.05).sin() * 0.5
                    } else {
                        (t * 0.001).sin().abs()
                    };
                }
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
        if block == 20 {
            // A seek resets the node on the audio thread.
            assert_eq!(realtime(|| node.reset()), 0, "reset allocated or freed");
        }
        let inputs: Vec<SignalIn<'_>> = input_buffers.iter().map(|b| b.as_in(FRAMES)).collect();
        let mut outputs: Vec<SignalOut<'_>> = vec![output_buffer.as_out(FRAMES)];
        let event_inputs: Vec<&[Event]> = (0..notes).map(|_| &events[..]).collect();
        let mut event_outputs: Vec<EventsOut<'_>> = Vec::new();
        let violations = realtime(|| {
            node.process(
                &ctx,
                Io {
                    inputs: &inputs,
                    outputs: &mut outputs,
                    event_inputs: &event_inputs,
                    event_outputs: &mut event_outputs,
                    spectral_inputs: &[],
                    spectral_outputs: &mut [],
                },
            );
        });
        assert_eq!(violations, 0, "{id}: block {block} allocated or freed");
        let out = output_buffer.as_in(FRAMES);
        heard |= (0..out.shape().channels)
            .any(|channel| out.lane(0, channel).iter().any(|&s| s.abs() > 1e-6));
    }
    heard
}

#[test]
fn an_au_effect_processing_audio_and_a_wired_parameter_never_allocates() {
    assert!(never_allocates(DELAY), "the delay made sound");
}

#[test]
fn an_au_instrument_playing_notes_never_allocates() {
    assert!(never_allocates(DLS), "the synth made sound");
}
