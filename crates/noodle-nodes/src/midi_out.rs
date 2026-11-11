//! The MIDI Out node: an events wire sent to a MIDI output port, with clock
//! and start/stop if you want them.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, ParamInfo, Ports, Setup, Transport, Unit,
};
use noodle_io::{MidiOutHub, MidiOutSender, OutMessage, now_micros};

pub const MIDI_OUT_ID: &str = "noodle.event.midi_out";

/// The config key of the output port: a port's name, or empty for none. A
/// name rather than a choice, like the port of a MIDI In.
pub const MIDI_OUT_PORT_KEY: &str = "port";

/// The port a MIDI Out node sends to.
pub static MIDI_OUT_PORT: ConfigInfo = ConfigInfo::text(MIDI_OUT_PORT_KEY, "Port");

/// MIDI clock ticks per quarter note, and the ticks (960 to a quarter) of
/// the timeline between two of them.
const CLOCK_PER_QUARTER: f64 = 24.0;
const TICKS_PER_CLOCK: f64 = 960.0 / CLOCK_PER_QUARTER;

/// Notes sounding at once that can be tracked to end them later; a note-off
/// carries no channel or key, so the node remembers them.
const HELD: usize = 256;

#[derive(Ports)]
#[ports(nondeterministic)]
struct MidiOutPorts {
    #[event_input("in", "Events")]
    events: (),
    /// Sends MIDI clock (24 to a quarter note) while the transport plays.
    #[param("clock", "Clock", ParamInfo::choice(["Off", "On"]))]
    clock: (),
    /// Sends Start (or Song Position and Continue, from anywhere but the
    /// beginning) when the transport plays and Stop when it stops.
    #[param("start_stop", "Start/Stop", ParamInfo::choice(["Off", "On"]))]
    start_stop: (),
    /// Shifts everything this node sends, for a device with a latency of its
    /// own. Positive is later.
    #[param(
        "offset",
        "Offset",
        ParamInfo::new(-0.2, 0.2, 0.0).unit(Unit::Seconds).smoothing(0.0)
    )]
    offset: (),
}

static INFO: NodeInfo = NodeInfo {
    id: MIDI_OUT_ID,
    version: 1,
    name: "MIDI Out",
    category: "Events",
};

/// Sends notes, controllers, pitch bend and the other raw MIDI that arrive on
/// its `in` wire to the port named by its `port` config, as MIDI 1.0. Per-note
/// expression has no MIDI 1.0 form and is not sent.
///
/// Everything leaves when the audio it goes with is heard, which is the
/// block's start plus the output latency the device reports, so an external
/// synth sounds with the project. `Offset` moves it from there.
///
/// **Clock.** With `Clock` on, 24 clock messages per quarter note go out
/// while the transport plays, on the timeline's grid, so they follow tempo
/// changes. With `Start/Stop` on, starting the transport at the beginning
/// sends Start, starting it anywhere else sends Song Position Pointer and
/// Continue, and stopping sends Stop. Stopping, a seek and removing the node
/// end the notes it has started.
///
/// With no device driving the engine (an offline render) nothing is sent.
pub struct MidiOut {
    hub: MidiOutHub,
}

impl MidiOut {
    pub fn new(hub: &MidiOutHub) -> Self {
        Self { hub: hub.clone() }
    }
}

impl NodeType for MidiOut {
    fn always_runs(&self) -> bool {
        true
    }

    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        std::slice::from_ref(&MIDI_OUT_PORT)
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(MidiOutPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let port = MIDI_OUT_PORT.get_text(setup.config);
        Ok(Instance::realtime(MidiOutNode {
            out: Out::new(self.hub.sender(&port)),
        }))
    }
}

struct MidiOutNode {
    out: Out,
}

impl Node for MidiOutNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let first = |port: usize| io.inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let settings = Settings {
            clock: first(MidiOutPorts::CLOCK) >= 0.5,
            start_stop: first(MidiOutPorts::START_STOP) >= 0.5,
            offset_micros: (f64::from(first(MidiOutPorts::OFFSET)) * 1e6) as i64,
        };
        let Some(heard) = self.out.sender.heard_at() else {
            // Nothing drives the engine in real time, so there is no time to
            // send at. Keep the transport in mind for when something does.
            self.out.playing = Some(ctx.transport.playing);
            return;
        };
        self.out.block(
            &ctx.transport,
            ctx.sample_rate,
            ctx.frames,
            io.event_inputs[MidiOutPorts::EVENTS],
            heard,
            settings,
        );
    }

    fn reset(&mut self) {
        self.out.release_all(now_micros());
    }

    fn stopped(&mut self) {
        self.out.release_all(now_micros());
    }
}

impl Drop for MidiOutNode {
    fn drop(&mut self) {
        self.out.release_all(now_micros());
    }
}

struct Settings {
    clock: bool,
    start_stop: bool,
    offset_micros: i64,
}

/// What the node does with a block, apart from reading its ports.
struct Out {
    sender: MidiOutSender,
    /// Whether the transport was playing in the previous block; `None` before
    /// the first, so a node built during playback doesn't announce a start.
    playing: Option<bool>,
    /// Where the previous block's end was expected to be, in ticks, to notice
    /// a seek or a loop wrap.
    expected: Option<f64>,
    /// The notes started and not yet ended: id, channel and key.
    held: Vec<(NoteId, u8, u8)>,
}

impl Out {
    fn new(sender: MidiOutSender) -> Self {
        Self {
            sender,
            playing: None,
            expected: None,
            held: Vec::with_capacity(HELD),
        }
    }

    /// Sends the block whose first frame is heard at `heard` microseconds.
    fn block(
        &mut self,
        transport: &Transport,
        sample_rate: f32,
        frames: usize,
        events: &[Event],
        heard: u64,
        settings: Settings,
    ) {
        let base = heard as i64 + settings.offset_micros;
        let at = |frame: u32| -> u64 {
            (base + (f64::from(frame) * 1e6 / f64::from(sample_rate)) as i64).max(0) as u64
        };

        let was_playing = self.playing.unwrap_or(transport.playing);
        let ticks_per_frame = transport.bpm / 60.0 * 960.0 / f64::from(sample_rate);
        let jumped = self
            .expected
            .is_some_and(|e| (e - transport.tick).abs() > TICKS_PER_CLOCK);
        if transport.playing && !was_playing {
            if settings.start_stop {
                if transport.tick < 0.5 {
                    self.send(at(0), [0xFA, 0, 0]);
                } else {
                    self.send_position(at(0), transport.tick);
                    self.send(at(0), [0xFB, 0, 0]);
                }
            }
        } else if transport.playing && jumped && settings.start_stop {
            // A seek or loop wrap: tell the device where the song now is.
            self.send_position(at(0), transport.tick);
            self.send(at(0), [0xFB, 0, 0]);
        } else if !transport.playing && was_playing {
            self.release_all(at(0));
            if settings.start_stop {
                self.send(at(0), [0xFC, 0, 0]);
            }
        }
        self.playing = Some(transport.playing);
        self.expected = transport
            .playing
            .then_some(transport.tick + frames as f64 * ticks_per_frame);

        for event in events {
            if let Some(bytes) = self.translate(&event.kind) {
                self.send(at(event.time), bytes);
            }
        }

        if settings.clock && transport.playing && transport.bpm > 0.0 {
            // The frame a tick falls on is the first at or after it, with a
            // little give so a tick on a block boundary is sent once.
            let frame_of = |tick: f64| ((tick - transport.tick) / ticks_per_frame - 1e-6).ceil();
            let mut n = (transport.tick / TICKS_PER_CLOCK - 1e-6).ceil();
            loop {
                let frame = frame_of(n * TICKS_PER_CLOCK).max(0.0);
                if frame >= frames as f64 {
                    break;
                }
                self.send(at(frame as u32), [0xF8, 0, 0]);
                n += 1.0;
            }
        }
    }

    /// Song Position Pointer, which counts sixteenths in 14 bits.
    fn send_position(&mut self, due: u64, tick: f64) {
        let position = ((tick / 240.0) as u32).min(0x3FFF);
        self.send(due, [0xF2, (position & 0x7F) as u8, (position >> 7) as u8]);
    }

    fn send(&mut self, due: u64, bytes: [u8; 3]) {
        let _ = self.sender.send(OutMessage::new(due, bytes));
    }

    /// The MIDI 1.0 message for an event, remembering notes so their ends can
    /// be addressed.
    fn translate(&mut self, kind: &EventKind) -> Option<[u8; 3]> {
        let data = |v: f32| (v * 127.0).round().clamp(0.0, 127.0) as u8;
        match *kind {
            EventKind::NoteOn {
                note,
                channel,
                key,
                velocity,
            } => {
                if self.held.len() == HELD {
                    self.held.remove(0);
                }
                self.held.push((note, channel & 0x0F, key & 0x7F));
                Some([0x90 | channel & 0x0F, key & 0x7F, data(velocity).max(1)])
            }
            EventKind::NoteOff { note, velocity } => {
                let at = self.held.iter().rposition(|(id, ..)| *id == note)?;
                let (_, channel, key) = self.held.remove(at);
                Some([0x80 | channel, key, data(velocity)])
            }
            EventKind::Expression { .. } => None,
            ref other => other.midi1(),
        }
    }

    /// Ends every note this node has started, at `due`.
    fn release_all(&mut self, due: u64) {
        while let Some((_, channel, key)) = self.held.pop() {
            self.send(due, [0x80 | channel, key, 0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_io::AudioClock;
    use rtrb::Consumer;
    use std::sync::Arc;

    const RATE: f32 = 48_000.0;

    fn out() -> (Out, Consumer<OutMessage>) {
        let (sender, ring) = MidiOutSender::capture(Arc::new(AudioClock::default()));
        (Out::new(sender), ring)
    }

    fn drain(ring: &mut Consumer<OutMessage>) -> Vec<(u64, Vec<u8>)> {
        let mut sent = Vec::new();
        while let Ok(m) = ring.pop() {
            sent.push((m.due, m.bytes().to_vec()));
        }
        sent
    }

    fn playing_at(position: u64) -> Transport {
        Transport {
            playing: true,
            position,
            tick: position as f64 * 2.0 * 960.0 / f64::from(RATE),
            ..Transport::default()
        }
    }

    fn off() -> Settings {
        Settings {
            clock: false,
            start_stop: false,
            offset_micros: 0,
        }
    }

    fn note_on(time: u32, id: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 2,
                key,
                velocity: 1.0,
            },
        }
    }

    #[test]
    fn events_leave_when_their_frame_is_heard_shifted_by_the_offset() {
        let (mut node, mut ring) = out();
        let events = [note_on(480, 7, 60)];
        // 480 frames at 48 kHz are 10 ms after the block is heard.
        node.block(&playing_at(0), RATE, 512, &events, 1_000_000, off());
        assert_eq!(drain(&mut ring), [(1_010_000, vec![0x92, 60, 127])]);
        node.block(
            &playing_at(512),
            RATE,
            512,
            &[Event {
                time: 0,
                kind: EventKind::NoteOff {
                    note: NoteId(7),
                    velocity: 0.0,
                },
            }],
            2_000_000,
            Settings {
                offset_micros: 25_000,
                ..off()
            },
        );
        // The note-off finds its channel and key; the offset adds 25 ms.
        assert_eq!(drain(&mut ring), [(2_025_000, vec![0x82, 60, 0])]);
        // One that was never started is dropped.
        node.block(
            &playing_at(1024),
            RATE,
            512,
            &[Event {
                time: 0,
                kind: EventKind::NoteOff {
                    note: NoteId(99),
                    velocity: 0.0,
                },
            }],
            3_000_000,
            off(),
        );
        assert!(drain(&mut ring).is_empty());
    }

    #[test]
    fn raw_midi_passes_and_expression_does_not() {
        let (mut node, mut ring) = out();
        let events = [
            Event {
                time: 0,
                kind: EventKind::Midi([0xB0, 1, 64]),
            },
            Event {
                time: 0,
                kind: EventKind::Expression {
                    note: NoteId(1),
                    expression: noodle_engine::Expression::Pitch,
                    value: 1.0,
                },
            },
            Event {
                time: 1,
                kind: EventKind::Midi([0xC3, 9, 0]),
            },
        ];
        node.block(&playing_at(0), RATE, 64, &events, 0, off());
        let sent = drain(&mut ring);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].1, [0xB0, 1, 64]);
        assert_eq!(sent[1].1, [0xC3, 9]);
    }

    /// The clock at 120 bpm is 48 messages a second, one every 1000 samples,
    /// whatever the block size is.
    #[test]
    fn clock_ticks_at_24_to_the_quarter_note() {
        for block in [480usize, 512, 333] {
            let (mut node, mut ring) = out();
            let settings = || Settings {
                clock: true,
                ..off()
            };
            let mut ticks = Vec::new();
            let mut position = 0u64;
            // Two seconds.
            while position < 96_000 {
                let frames = block.min(96_000 - position as usize);
                // The time the block is heard is its position in microseconds.
                let heard = position * 1_000_000 / RATE as u64;
                node.block(&playing_at(position), RATE, frames, &[], heard, settings());
                ticks.extend(drain(&mut ring));
                position += frames as u64;
            }
            assert!(ticks.iter().all(|(_, b)| b == &[0xF8]));
            assert_eq!(ticks.len(), 96, "block {block}");
            for pair in ticks.windows(2) {
                let gap = pair[1].0 as i64 - pair[0].0 as i64;
                assert!((gap - 20_833).abs() <= 25, "gap {gap} with block {block}");
            }
        }
    }

    #[test]
    fn the_clock_follows_the_tempo_and_is_silent_when_off_or_stopped() {
        let (mut node, mut ring) = out();
        let fast = Transport {
            bpm: 240.0,
            ..playing_at(0)
        };
        let on = Settings {
            clock: true,
            ..off()
        };
        // Half a second at 240 bpm is two quarters: 48 clocks.
        node.block(&fast, RATE, 24_000, &[], 0, on);
        assert_eq!(drain(&mut ring).len(), 48);
        node.block(&playing_at(0), RATE, 24_000, &[], 0, off());
        let stopped = Transport {
            playing: false,
            ..playing_at(0)
        };
        node.block(
            &stopped,
            RATE,
            24_000,
            &[],
            0,
            Settings {
                clock: true,
                ..off()
            },
        );
        assert!(drain(&mut ring).iter().all(|(_, b)| b != &[0xF8]));
    }

    #[test]
    fn start_continue_and_stop() {
        let both = || Settings {
            start_stop: true,
            ..off()
        };
        // From the beginning: Start.
        let (mut node, mut ring) = out();
        let idle = Transport {
            playing: false,
            ..playing_at(0)
        };
        node.block(&idle, RATE, 64, &[], 400, both());
        node.block(&playing_at(0), RATE, 64, &[], 500, both());
        assert_eq!(drain(&mut ring), [(500, vec![0xFA])]);
        // Playing on sends nothing more.
        node.block(&playing_at(64), RATE, 64, &[], 500, both());
        assert!(drain(&mut ring).is_empty());
        // Stopping sends Stop and ends the notes it started.
        node.block(&playing_at(64), RATE, 64, &[note_on(0, 1, 60)], 500, both());
        drain(&mut ring);
        let stopped = Transport {
            playing: false,
            ..playing_at(128)
        };
        node.block(&stopped, RATE, 64, &[], 900, both());
        assert_eq!(
            drain(&mut ring),
            [(900, vec![0x82, 60, 0]), (900, vec![0xFC])]
        );
        // Starting again 4 s in, which at 120 bpm is 8 quarters, 32 sixteenths: the song
        // position, then Continue.
        let later = playing_at(4 * 48_000);
        node.block(&later, RATE, 64, &[], 1_000, both());
        assert_eq!(
            drain(&mut ring),
            [(1_000, vec![0xF2, 32, 0]), (1_000, vec![0xFB])]
        );
        // A seek while playing sends the new position and Continue.
        node.block(&later, RATE, 64, &[], 1_100, both());
        drain(&mut ring);
        node.block(&playing_at(8 * 48_000), RATE, 64, &[], 1_200, both());
        assert_eq!(
            drain(&mut ring),
            [(1_200, vec![0xF2, 64, 0]), (1_200, vec![0xFB])]
        );
        // With the toggle off, none of it.
        let (mut quiet, mut quiet_ring) = out();
        quiet.block(&playing_at(0), RATE, 64, &[], 0, off());
        quiet.block(&stopped, RATE, 64, &[], 0, off());
        assert!(drain(&mut quiet_ring).is_empty());
    }

    #[test]
    fn a_node_built_during_playback_does_not_announce_a_start() {
        let (mut node, mut ring) = out();
        let both = Settings {
            start_stop: true,
            ..off()
        };
        node.block(&playing_at(96_000), RATE, 64, &[], 0, both);
        assert!(drain(&mut ring).is_empty());
    }

    #[test]
    fn dropping_the_node_ends_its_notes() {
        let (mut node, mut ring) = out();
        node.block(&playing_at(0), RATE, 64, &[note_on(0, 1, 64)], 0, off());
        drain(&mut ring);
        node.release_all(77);
        assert_eq!(drain(&mut ring), [(77, vec![0x82, 64, 0])]);
        node.release_all(78);
        assert!(drain(&mut ring).is_empty());
    }
}
