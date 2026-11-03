//! The Launch Quantise node.

use noodle_engine::{
    Config, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup,
};

/// Holds each note back until the next beat, bar or longer boundary of the
/// tempo's grid, so a pad pressed a little early or late starts exactly on
/// time. It is how a pad launches a loop "on the bar".
///
/// - `grid`: *Off* passes everything straight through; otherwise the unit the
///   notes wait for: a beat, half a bar, a bar, two bars or four. Bars count
///   from the last time-signature change, as everywhere else.
/// - `mode`: *Hold* delays note-ons and note-offs alike, so the note lasts as
///   long as the pad was held, to the grid. A pad released before its note has
///   even started still plays one whole grid unit, so a tap launches a loop
///   for a bar rather than for nothing. *Toggle* turns pads into switches: one
///   press starts the note on the next boundary, the next press of the same
///   key ends it on the one after that, and releases are ignored. A second
///   press before the start happens cancels the launch.
///
/// A press that lands exactly on a boundary plays at once. The wait follows
/// the transport's ticks; while the transport is stopped the grid free-runs
/// from where it left off, so pads work with the transport stopped. If the
/// playhead jumps (a seek or loop wrap), the notes waiting play at once rather
/// than at a boundary that no longer exists.
///
/// Controller and other raw MIDI events pass straight through, not held. A
/// per-note expression passes unless its note is still waiting, when it is
/// dropped. At most 128 notes can be waiting; past that, new ones are dropped.
pub struct LaunchQuantise;

pub const LAUNCH_QUANTISE_ID: &str = "noodle.event.launch_quantise";

#[derive(Ports)]
struct Ps {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "grid",
        "Grid",
        ParamInfo::choice(["Off", "Beat", "Half bar", "Bar", "2 bars", "4 bars"]).default_to(3.0)
    )]
    grid: (),
    #[param("mode", "Mode", ParamInfo::choice(["Hold", "Toggle"]))]
    mode: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: LAUNCH_QUANTISE_ID,
    version: 1,
    name: "Launch Quantise",
    category: "Events",
};

impl NodeType for LaunchQuantise {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Ps::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(Quantiser::new(setup.sample_rate)))
    }
}

const WAITING: usize = 128;

#[derive(Clone, Copy)]
struct Waiting {
    /// The tick the event plays at.
    target: f64,
    event: EventKind,
}

struct Quantiser {
    sample_rate: f32,
    /// Sorted by target; equal targets stay in the order they came.
    waiting: [Option<Waiting>; WAITING],
    /// Toggle mode: the note a key's switch is on with.
    running: [Option<u32>; 128],
    expected: Option<f64>,
    free_tick: f64,
}

impl Quantiser {
    fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            waiting: [None; WAITING],
            running: [None; 128],
            expected: None,
            free_tick: 0.0,
        }
    }

    fn insert(&mut self, target: f64, event: EventKind) {
        let Some(free) = self.waiting.iter().position(Option::is_none) else {
            return;
        };
        self.waiting[free] = Some(Waiting { target, event });
        // Bubble it into place, behind anything with the same target.
        let mut i = free;
        while i > 0 {
            match (self.waiting[i - 1], self.waiting[i]) {
                (Some(a), Some(b)) if a.target > b.target => self.waiting.swap(i - 1, i),
                _ => break,
            }
            i -= 1;
        }
        // Keep the filled slots at the front, in order.
        self.compact();
    }

    fn compact(&mut self) {
        let mut write = 0;
        for read in 0..WAITING {
            if self.waiting[read].is_some() {
                self.waiting.swap(write, read);
                write += 1;
            }
        }
    }

    /// Removes and returns the first waiting event that is due before `tick`.
    fn pop_due(&mut self, tick: f64) -> Option<Waiting> {
        let first = self.waiting[0]?;
        if first.target < tick {
            self.waiting[0] = None;
            self.compact();
            Some(first)
        } else {
            None
        }
    }

    fn start_of(&self, note: u32) -> Option<f64> {
        self.waiting.iter().flatten().find_map(|w| match w.event {
            EventKind::NoteOn { note: n, .. } if n.0 == note => Some(w.target),
            _ => None,
        })
    }

    fn cancel(&mut self, note: u32) -> bool {
        let slot = self.waiting.iter().position(|w| {
            matches!(w, Some(Waiting { event: EventKind::NoteOn { note: n, .. }, .. }) if n.0 == note)
        });
        match slot {
            Some(i) => {
                self.waiting[i] = None;
                self.compact();
                true
            }
            None => false,
        }
    }
}

impl Node for Quantiser {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let grid = first(Ps::GRID).round().clamp(0.0, 5.0) as usize;
        let toggle = first(Ps::MODE) >= 0.5;
        let out = &mut event_outputs[Ps::OUT];

        let frames = ctx.frames;
        let tps = ctx.transport.bpm * 960.0 / 60.0 / f64::from(self.sample_rate);
        let start = if ctx.transport.playing {
            ctx.transport.tick
        } else {
            self.free_tick
        };
        let end = start + frames as f64 * tps;
        if self.expected.is_none_or(|e| (e - start).abs() > 1.0) {
            // The playhead jumped: whatever waits plays now.
            for w in self.waiting.iter_mut().flatten() {
                w.target = start;
            }
        }
        self.expected = Some(end);
        self.free_tick = end;

        let bar = ctx.transport.signature.ticks_per_bar() as f64;
        let beat = ctx.transport.signature.ticks_per_beat() as f64;
        let unit = match grid {
            1 => beat,
            2 => bar / 2.0,
            3 => bar,
            4 => 2.0 * bar,
            5 => 4.0 * bar,
            _ => 0.0,
        };
        let origin = ctx.transport.signature_start;
        if !toggle || unit <= 0.0 {
            // Switches only exist in toggle mode; leaving it forgets them.
            self.running = [None; 128];
        }
        // The first boundary at or after `tick`.
        let boundary = |tick: f64| {
            if unit <= 0.0 {
                tick
            } else {
                origin + ((tick - origin) / unit - 1e-6).ceil() * unit
            }
        };
        let sample_of = |tick: f64| {
            (((tick - start) / tps).round().max(0.0) as usize).min(frames.saturating_sub(1)) as u32
        };
        let push = |out: &mut noodle_engine::EventsOut<'_>, time: u32, kind| {
            let _ = out.push(Event { time, kind });
        };

        // Plays what is due before `tick`.
        let flush = |me: &mut Self, out: &mut noodle_engine::EventsOut<'_>, tick: f64| {
            while let Some(w) = me.pop_due(tick) {
                push(out, sample_of(w.target), w.event);
            }
        };

        for event in event_inputs[Ps::NOTES] {
            let now = start + f64::from(event.time) * tps;
            // Anything due at or before this event comes first. (A target
            // exactly at the event's tick plays with it, in order.)
            flush(self, out, now + 1e-9);
            let at = boundary(now);
            let immediate = at - now < 1e-6 || unit <= 0.0;
            match event.kind {
                EventKind::NoteOn { note, key, .. } if toggle && unit > 0.0 => {
                    let key = usize::from(key.min(127));
                    match self.running[key] {
                        Some(playing) => {
                            self.running[key] = None;
                            if !self.cancel(playing) {
                                let off = EventKind::NoteOff {
                                    note: noodle_engine::NoteId(playing),
                                    velocity: 0.0,
                                };
                                if immediate {
                                    push(out, event.time, off);
                                } else {
                                    self.insert(at, off);
                                }
                            }
                        }
                        None => {
                            self.running[key] = Some(note.0);
                            if immediate {
                                push(out, event.time, event.kind);
                            } else {
                                self.insert(at, event.kind);
                            }
                        }
                    }
                }
                // A release does nothing to a switch.
                EventKind::NoteOff { .. } if toggle && unit > 0.0 => {}
                EventKind::NoteOn { .. } if immediate => push(out, event.time, event.kind),
                EventKind::NoteOn { .. } => self.insert(at, event.kind),
                EventKind::NoteOff { note, .. } => {
                    // Released before it started: it still plays one unit.
                    let target = match self.start_of(note.0) {
                        Some(on) => on + unit,
                        None => at,
                    };
                    if target - now < 1e-6 {
                        push(out, event.time, event.kind);
                    } else {
                        self.insert(target, event.kind);
                    }
                }
                EventKind::Expression { note, .. } => {
                    if self.start_of(note.0).is_none() {
                        push(out, event.time, event.kind);
                    }
                }
                EventKind::Midi(_) | EventKind::Channel { .. } => push(out, event.time, event.kind),
            }
        }
        flush(self, out, end);
    }

    fn reset(&mut self) {
        *self = Self::new(self.sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::NoteId;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    /// At 120 beats per minute in 4/4.
    const BEAT: u32 = 24_000;
    const BAR: u32 = 4 * BEAT;

    fn on(time: u32, id: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.8,
            },
        }
    }

    fn off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    fn harness(grid: f32, toggle: bool) -> Harness {
        let mut h =
            Harness::new(&LaunchQuantise, &Config::new(), &[], RATE, BEAT as usize).unwrap();
        h.set(Ps::GRID, grid);
        h.set(Ps::MODE, if toggle { 1.0 } else { 0.0 });
        h
    }

    /// Runs `beats` blocks of one beat, sending `events` (given in absolute
    /// samples) in the right block, and returns every output as (absolute
    /// sample, "on" or "off", note).
    fn run(h: &mut Harness, beats: u32, events: &[Event]) -> Vec<(u32, &'static str, u32)> {
        let mut heard = Vec::new();
        for b in 0..beats {
            let base = b * BEAT;
            let block: Vec<Event> = events
                .iter()
                .filter(|e| e.time >= base && e.time < base + BEAT)
                .map(|e| Event {
                    time: e.time - base,
                    kind: e.kind,
                })
                .collect();
            h.send_events(Ps::NOTES, &block);
            h.run(BEAT as usize).unwrap();
            for e in h.events(Ps::OUT) {
                match e.kind {
                    EventKind::NoteOn { note, .. } => heard.push((base + e.time, "on", note.0)),
                    EventKind::NoteOff { note, .. } => heard.push((base + e.time, "off", note.0)),
                    _ => heard.push((base + e.time, "other", 0)),
                }
            }
        }
        heard
    }

    #[test]
    fn a_press_waits_for_the_next_bar() {
        let mut h = harness(3.0, false);
        let heard = run(&mut h, 8, &[on(30_000, 1, 36)]);
        assert_eq!(heard, [(BAR, "on", 1)]);
    }

    #[test]
    fn a_press_on_the_boundary_plays_at_once() {
        let mut h = harness(3.0, false);
        let heard = run(&mut h, 8, &[on(BAR, 1, 36)]);
        assert_eq!(heard, [(BAR, "on", 1)]);
    }

    #[test]
    fn the_release_is_quantised_too() {
        let mut h = harness(3.0, false);
        let heard = run(&mut h, 12, &[on(30_000, 1, 36), off(BAR + 5_000, 1)]);
        assert_eq!(heard, [(BAR, "on", 1), (2 * BAR, "off", 1)]);
    }

    #[test]
    fn a_tap_before_the_start_still_plays_a_whole_bar() {
        let mut h = harness(3.0, false);
        let heard = run(&mut h, 12, &[on(30_000, 1, 36), off(40_000, 1)]);
        assert_eq!(heard, [(BAR, "on", 1), (2 * BAR, "off", 1)]);
    }

    #[test]
    fn the_grid_sets_how_long_a_press_waits() {
        for (grid, at) in [(1.0, BEAT), (2.0, 2 * BEAT), (4.0, 2 * BAR), (5.0, 4 * BAR)] {
            let mut h = harness(grid, false);
            let heard = run(&mut h, 20, &[on(100, 1, 36)]);
            assert_eq!(heard, [(at, "on", 1)], "grid {grid}");
        }
    }

    #[test]
    fn off_passes_everything_straight_through() {
        let mut h = harness(0.0, false);
        let heard = run(&mut h, 2, &[on(100, 1, 36), off(30_000, 1)]);
        assert_eq!(heard, [(100, "on", 1), (30_000, "off", 1)]);
    }

    #[test]
    fn toggle_starts_on_a_bar_and_stops_on_a_later_one() {
        let mut h = harness(3.0, true);
        let heard = run(
            &mut h,
            16,
            &[
                on(30_000, 1, 36),
                off(31_000, 1),
                on(BAR + 10_000, 2, 36),
                off(BAR + 11_000, 2),
            ],
        );
        assert_eq!(heard, [(BAR, "on", 1), (2 * BAR, "off", 1)]);
    }

    #[test]
    fn a_second_press_before_the_start_cancels_the_launch() {
        let mut h = harness(3.0, true);
        let heard = run(&mut h, 12, &[on(10_000, 1, 36), on(50_000, 2, 36)]);
        assert!(heard.is_empty(), "{heard:?}");
        // The key is free again: the next press launches.
        let heard = run(&mut h, 6, &[on(10_000, 3, 36)]);
        assert_eq!(heard.len(), 1);
    }

    #[test]
    fn toggled_keys_are_independent() {
        let mut h = harness(3.0, true);
        let heard = run(&mut h, 8, &[on(10_000, 1, 36), on(20_000, 2, 37)]);
        assert_eq!(heard, [(BAR, "on", 1), (BAR, "on", 2)]);
    }

    #[test]
    fn raw_midi_is_not_held() {
        let mut h = harness(3.0, false);
        let bend = Event {
            time: 77,
            kind: EventKind::Midi([0xE0, 0, 64]),
        };
        let heard = run(&mut h, 1, &[bend]);
        assert_eq!(heard, [(77, "other", 0)]);
    }

    #[test]
    fn leaving_toggle_mode_forgets_the_switches() {
        let mut h = harness(3.0, true);
        run(&mut h, 4, &[on(10_000, 1, 36)]);
        h.set(Ps::MODE, 0.0);
        run(&mut h, 1, &[]);
        h.set(Ps::MODE, 1.0);
        // The key is a fresh switch: this press starts a note, not stops one.
        let heard = run(&mut h, 8, &[on(10_000, 2, 36)]);
        assert_eq!(heard.iter().filter(|e| e.1 == "on").count(), 1, "{heard:?}");
        assert!(heard.iter().all(|e| e.1 != "off"), "{heard:?}");
    }
}
