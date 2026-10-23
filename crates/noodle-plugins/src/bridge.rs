//! The host side every plugin format shares.
//!
//! A [`Bridge`] turns one block of a node into the calls a plugin needs, and
//! the plugin's answer back into outputs. A [`Format`] is only what differs
//! between CLAP, VST3 and AU: how a parameter change, a note and the
//! transport are handed over, and how the plugin is asked to process.
//!
//! The bridge owns:
//!
//! - the bus buffers, and the copy of the node's inputs into them and of the
//!   plugin's outputs out of them;
//! - chunking a block into runs of at most `max_frames`;
//! - the time-ordered merge of every note input, with parameters sampled every
//!   [`PARAM_STEP`] frames while a wire modulates them, and one rule for how
//!   many events fit (parameter changes leave [`NOTE_RESERVE`] free for notes);
//! - which key each held note was played with, so a note-off can say;
//! - [`musical_position`], the one reckoning of beats and bars;
//! - the quiet state: while a plugin has nothing to do, it isn't called, and
//!   its outputs are flagged silent (see "Plugins" in the architecture notes).
//!
//! Everything here runs on the audio thread: nothing allocates.

use noodle_core::{TICKS_PER_QUARTER, TimeSignature};
use noodle_engine::{Context, Event, EventKind, Io, Node};

/// While a wire drives a parameter, it's sampled this often (in frames).
pub const PARAM_STEP: usize = 32;

/// The most events (notes and parameter changes) handed to a plugin per
/// block. The rest are dropped, so the audio thread never allocates. (The
/// events a plugin writes go to a buffer of the same size; one that writes
/// more in a block makes that buffer grow, which a plugin that busy would
/// need to be told not to do.)
pub(crate) const MAX_EVENTS: usize = 4096;

/// Room kept for note events: parameter changes stop this far short of
/// [`MAX_EVENTS`], so a flood of modulation can't push out a note-off and
/// leave a note stuck.
pub(crate) const NOTE_RESERVE: usize = 512;

/// Notes remembered so a note-off can say which key it is for.
pub(crate) const MAX_NOTES: usize = 256;

/// How long a plugin that reports no status must output exact zeros, with
/// nothing going in, before it is taken to be done (a delay line holding an
/// echo longer than this would be cut off).
const QUIET_SECONDS: f32 = 4.0;

/// One bus of a plugin: a buffer per channel, and which channels are known to
/// hold only zeros.
pub(crate) struct Bus {
    pub channels: Vec<Vec<f32>>,
    /// A bit per channel (the first 64). For an input bus the bridge sets it
    /// from the node's silence flags; for an output bus the format sets it
    /// for the channels the plugin says it left silent.
    pub silent: u64,
}

impl Bus {
    pub fn new(channels: u32, frames: usize) -> Self {
        Self {
            channels: (0..channels.max(1)).map(|_| vec![0.0; frames]).collect(),
            silent: 0,
        }
    }
}

/// The buses of a plugin, as the format sees them while it processes.
pub(crate) struct Buffers {
    pub inputs: Vec<Bus>,
    pub outputs: Vec<Bus>,
}

impl Buffers {
    /// A bus for each of the channel counts, with room for `frames`.
    pub fn new(
        inputs: impl IntoIterator<Item = u32>,
        outputs: impl IntoIterator<Item = u32>,
        frames: usize,
    ) -> Self {
        Self {
            inputs: inputs.into_iter().map(|c| Bus::new(c, frames)).collect(),
            outputs: outputs.into_iter().map(|c| Bus::new(c, frames)).collect(),
        }
    }
}

/// What the transport looks like `start` frames into a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Position {
    pub playing: bool,
    pub bpm: f64,
    pub sample_rate: f64,
    /// The timeline position, in samples.
    pub samples: f64,
    /// Quarter notes from the start of the timeline.
    pub beats: f64,
    pub seconds: f64,
    /// The start of the bar `beats` is in, in quarter notes.
    pub bar_start: f64,
    /// Which bar of the current time signature `beats` is in, from 0. (The
    /// transport only knows where the current signature began, so bars of
    /// earlier signatures aren't counted.)
    pub bar_number: i32,
    pub signature: TimeSignature,
    /// Frames this node has been given since it was made, whether or not the
    /// plugin ran for them.
    pub steady: u64,
}

/// Where the transport is `start` frames into the block. A bar is
/// `numerator` beats of a 1/`denominator` note each (6/8 is three quarter
/// notes long, 2/2 four), counted from where the current signature began.
pub(crate) fn musical_position(ctx: &Context, start: usize, steady: u64) -> Position {
    let t = &ctx.transport;
    let advance = if t.playing { start as f64 } else { 0.0 };
    let rate = f64::from(ctx.sample_rate);
    let per_quarter = TICKS_PER_QUARTER as f64;
    let beats = t.tick / per_quarter + advance / rate * t.bpm / 60.0;
    let origin = t.signature_start / per_quarter;
    let numerator = f64::from(t.signature.numerator.max(1));
    let denominator = f64::from(t.signature.denominator.max(1));
    let bar_length = numerator * 4.0 / denominator;
    let bar = ((beats - origin) / bar_length).floor().max(0.0);
    Position {
        playing: t.playing,
        bpm: t.bpm,
        sample_rate: rate,
        samples: t.position as f64 + advance,
        beats,
        seconds: (t.position as f64 + advance) / rate,
        bar_start: origin + bar * bar_length,
        bar_number: bar as i32,
        signature: t.signature,
        steady,
    }
}

/// How a plugin answered a call to process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// It wants to keep running (CLAP's `Continue` and `Tail`).
    Continue,
    /// The format has no word on it either way.
    Unknown,
    /// Every output is silent this block, which says nothing about the next
    /// (a delay between echoes is silent). The plugin is done once its tail,
    /// in frames, has gone by with nothing coming in.
    SilentTail(u32),
    /// It is done if its outputs are all zero (CLAP's `ContinueIfNotQuiet`).
    MaybeQuiet,
    /// It needs nothing more until something comes in: input, a note or a
    /// parameter change (CLAP's `Sleep`).
    Sleep,
    /// The call failed: the outputs are zeros.
    Failed,
}

/// What differs between plugin formats.
pub(crate) trait Format {
    /// A chunk starts: clears the lists of events. Returns whether the
    /// format has input of its own for the chunk (an edit made in the plugin's
    /// editor), which wakes a quiet plugin.
    fn begin(&mut self, n: usize) -> bool;

    /// A parameter has `value` (already clamped, and rounded if stepped) from
    /// frame `at` of the chunk. `slot` is the position of the parameter in
    /// the list the bridge was made with.
    fn param(&mut self, slot: usize, at: usize, value: f32);

    /// A note event at frame `at` of the chunk, on note input `port`. `key` is
    /// the channel and key the note was played with, if the bridge remembers it
    /// (it does for a note-on, and until the note-off).
    fn note(&mut self, port: usize, event: &Event, at: usize, key: Option<(u8, u8)>);

    /// The transport at the chunk's start (also how many frames the node has
    /// been given, for the plugin's steady clock).
    fn transport(&mut self, position: &Position);

    /// Runs the plugin on the `n` frames in `buffers.inputs`, leaving its
    /// output in `buffers.outputs` (and `silent` set on the channels it says
    /// it left silent).
    fn process(&mut self, n: usize, buffers: &mut Buffers) -> Status;

    /// Passes on the notes the plugin played as `(port, frame in the chunk,
    /// event)`.
    fn drain_note_outputs(&mut self, emit: &mut dyn FnMut(usize, u32, EventKind));

    /// The latency the plugin reported when it was loaded, in samples.
    fn latency(&self) -> u32;

    /// The plugin has gone quiet and won't be called until something wakes it.
    fn sleep(&mut self) {}

    /// Clears the plugin's state.
    fn reset(&mut self);
}

/// A parameter with a port, as the bridge samples it.
pub(crate) struct ParamSlot {
    /// The input's position in the layout, or none for a parameter nothing is
    /// wired to (the format drives it some other way).
    pub input: Option<usize>,
    pub min: f32,
    pub max: f32,
    pub stepped: bool,
    /// The last value sent (the default until one is), so a port nobody
    /// touched leaves the plugin's own, or restored, value alone.
    pub last: f32,
}

impl ParamSlot {
    pub fn new(input: Option<usize>, min: f32, max: f32, stepped: bool, default: f32) -> Self {
        let default = default.clamp(min, max);
        Self {
            input,
            min,
            max,
            stepped,
            last: if stepped { default.round() } else { default },
        }
    }
}

struct NoteKey {
    port: u16,
    note: u32,
    channel: u8,
    key: u8,
}

/// A plugin node's audio-thread half, for any [`Format`].
pub(crate) struct Bridge<F: Format> {
    format: F,
    buffers: Buffers,
    params: Vec<ParamSlot>,
    notes: Vec<NoteKey>,
    /// Where each note input's merge has got to.
    cursors: Vec<usize>,
    /// Events handed over in the chunk so far.
    events: usize,
    max_frames: usize,
    /// Frames given to the plugin's node, running or not.
    steady: u64,
    /// Whether the plugin plays notes itself: it is never taken to be quiet.
    plays_notes: bool,
    asleep: bool,
    /// Frames of silence in and exact zeros out, for a format that doesn't
    /// say when its plugin is done.
    zero_frames: usize,
    /// Per output bus, the channels that were silent for every chunk of the
    /// block so far.
    block_silent: Vec<u64>,
}

impl<F: Format> Bridge<F> {
    pub fn new(
        format: F,
        buffers: Buffers,
        params: Vec<ParamSlot>,
        note_inputs: usize,
        plays_notes: bool,
        max_frames: usize,
    ) -> Self {
        let outputs = buffers.outputs.len();
        Self {
            format,
            buffers,
            params,
            notes: Vec::with_capacity(MAX_NOTES),
            cursors: vec![0; note_inputs],
            events: 0,
            max_frames,
            steady: 0,
            plays_notes,
            asleep: false,
            zero_frames: 0,
            block_silent: vec![0; outputs],
        }
    }

    /// Runs frames `start..start + n` of the block.
    fn chunk(&mut self, ctx: &Context, io: &mut Io<'_, '_>, start: usize, n: usize) {
        let position = musical_position(ctx, start, self.steady);
        self.events = 0;
        let mut woke = self.format.begin(n);
        self.build_events(io, start, n);
        woke |= self.events > 0;

        // The inputs: which lanes are known to be silent.
        let mut input_silent = true;
        for (port, bus) in self.buffers.inputs.iter_mut().enumerate() {
            bus.silent = 0;
            let Some(input) = io.inputs.get(port) else {
                continue;
            };
            for channel in 0..bus.channels.len() {
                if input.is_silent(0, channel) {
                    if channel < 64 {
                        bus.silent |= 1 << channel;
                    }
                } else {
                    input_silent = false;
                }
            }
        }

        if self.asleep && input_silent && !woke {
            self.steady += n as u64;
            return self.write_silence(io, start, n);
        }
        self.asleep = false;

        for (port, bus) in self.buffers.inputs.iter_mut().enumerate() {
            let Some(input) = io.inputs.get(port) else {
                continue;
            };
            for (channel, buffer) in bus.channels.iter_mut().enumerate() {
                buffer[..n].copy_from_slice(&input.lane(0, channel)[start..start + n]);
            }
        }
        self.asleep = false;

        self.format.transport(&position);
        let status = self.format.process(n, &mut self.buffers);
        self.steady += n as u64;
        if status == Status::Failed {
            return self.write_silence(io, start, n);
        }

        // The outputs, and which channels are silent: those the plugin says
        // so of, and any that are all zeros anyway.
        let mut output_zero = true;
        for (port, bus) in self.buffers.outputs.iter().enumerate() {
            let out = &mut io.outputs[port];
            let mut silent = bus.silent;
            for (channel, buffer) in bus.channels.iter().enumerate() {
                out.lane_mut(0, channel)[start..start + n].copy_from_slice(&buffer[..n]);
                let flagged = channel < 64 && silent & (1 << channel) != 0;
                if flagged || buffer[..n].iter().all(|&x| x == 0.0) {
                    if channel < 64 {
                        silent |= 1 << channel;
                    }
                } else {
                    output_zero = false;
                }
            }
            self.block_silent[port] &= silent;
        }
        let mut played = false;
        if !io.event_outputs.is_empty() {
            let outputs = &mut *io.event_outputs;
            self.format.drain_note_outputs(&mut |port, time, kind| {
                played = true;
                // A port the plugin didn't declare goes to the first.
                let port = port.min(outputs.len() - 1);
                let _ = outputs[port].push(Event {
                    time: time.saturating_add(start as u32),
                    kind,
                });
            });
        }

        // Quiet: only when nothing came in, and the plugin has nothing to say.
        if !input_silent || woke || played || self.plays_notes {
            self.zero_frames = 0;
            return;
        }
        let done = match status {
            Status::Sleep => true,
            Status::MaybeQuiet => output_zero,
            Status::Unknown | Status::SilentTail(_) => {
                self.zero_frames = if output_zero { self.zero_frames + n } else { 0 };
                let limit = match status {
                    Status::SilentTail(tail) => tail as f32,
                    _ => QUIET_SECONDS * ctx.sample_rate,
                };
                self.zero_frames as f32 >= limit
            }
            Status::Continue | Status::Failed => false,
        };
        if done {
            self.asleep = true;
            self.format.sleep();
        }
    }

    /// Writes zeros for the chunk. The block's silent flags are left as they
    /// are: zeros don't take a lane's flag away.
    fn write_silence(&self, io: &mut Io<'_, '_>, start: usize, n: usize) {
        for out in io.outputs.iter_mut() {
            for channel in 0..out.shape().channels {
                out.lane_mut(0, channel)[start..start + n].fill(0.0);
            }
        }
    }

    /// Fills the chunk's parameter changes and note events, in time order.
    fn build_events(&mut self, io: &Io<'_, '_>, start: usize, n: usize) {
        self.sample_params(io, start, 0);
        let mut next_step = PARAM_STEP;
        // Every note input is read, merged into one time-ordered stream (the
        // plugin wants its events in order whatever port they are for).
        self.cursors.iter_mut().for_each(|c| *c = 0);
        loop {
            let mut pick: Option<(usize, usize)> = None;
            for (port, cursor) in self.cursors.iter_mut().enumerate() {
                let events = io.event_inputs.get(port).copied().unwrap_or(&[]);
                while let Some(event) = events.get(*cursor) {
                    let time = event.time as usize;
                    if time < start {
                        *cursor += 1;
                    } else {
                        if time < start + n && pick.is_none_or(|(_, t)| time < t) {
                            pick = Some((port, time));
                        }
                        break;
                    }
                }
            }
            let Some((port, time)) = pick else {
                break;
            };
            let event = &io.event_inputs[port][self.cursors[port]];
            self.cursors[port] += 1;
            let at = time - start;
            while next_step <= at {
                self.sample_params(io, start, next_step);
                next_step += PARAM_STEP;
            }
            if self.events >= MAX_EVENTS {
                break;
            }
            self.events += 1;
            let key = self.remember(port as u16, event);
            self.format.note(port, event, at, key);
        }
        while next_step < n {
            self.sample_params(io, start, next_step);
            next_step += PARAM_STEP;
        }
    }

    /// Sends the parameters whose value at `start + at` differs from what the
    /// plugin has. Unmodulated ones are only looked at at `at == 0`.
    fn sample_params(&mut self, io: &Io<'_, '_>, start: usize, at: usize) {
        for (index, slot) in self.params.iter_mut().enumerate() {
            let Some(input) = slot.input else { continue };
            let input = &io.inputs[input];
            let value = match input.constant() {
                Some(value) => {
                    if at != 0 {
                        continue;
                    }
                    value
                }
                None => input.lane(0, 0)[start + at],
            };
            let mut value = value.clamp(slot.min, slot.max);
            if slot.stepped {
                value = value.round();
            }
            if value == slot.last || !value.is_finite() {
                continue;
            }
            if self.events >= MAX_EVENTS - NOTE_RESERVE {
                return;
            }
            slot.last = value;
            self.events += 1;
            self.format.param(index, at, value);
        }
    }

    /// Keeps track of which key each note was played with. Returns the
    /// channel and key an event is about, if known.
    fn remember(&mut self, port: u16, event: &Event) -> Option<(u8, u8)> {
        match event.kind {
            EventKind::NoteOn {
                note, channel, key, ..
            } => {
                self.notes.retain(|n| !(n.port == port && n.note == note.0));
                if self.notes.len() == MAX_NOTES {
                    self.notes.remove(0);
                }
                self.notes.push(NoteKey {
                    port,
                    note: note.0,
                    channel,
                    key,
                });
                Some((channel, key))
            }
            EventKind::NoteOff { note, .. } => {
                let found = self
                    .notes
                    .iter()
                    .position(|n| n.port == port && n.note == note.0)
                    .map(|i| self.notes.remove(i));
                found.map(|n| (n.channel, n.key))
            }
            EventKind::Expression { note, .. } => self
                .notes
                .iter()
                .find(|n| n.port == port && n.note == note.0)
                .map(|n| (n.channel, n.key)),
            _ => None,
        }
    }
}

impl<F: Format + Send + 'static> Node for Bridge<F> {
    fn latency(&self) -> u32 {
        self.format.latency()
    }

    fn process(&mut self, ctx: &Context, mut io: Io<'_, '_>) {
        self.block_silent.fill(u64::MAX);
        let mut start = 0;
        while start < ctx.frames {
            let n = (ctx.frames - start).min(self.max_frames);
            self.chunk(ctx, &mut io, start, n);
            start += n;
        }
        // A lane is flagged only if every chunk of the block left it silent.
        for (port, out) in io.outputs.iter_mut().enumerate() {
            for channel in 0..out.shape().channels {
                let bit = if channel < 64 { 1 << channel } else { 0 };
                if self.block_silent[port] & bit != 0 {
                    out.set_silent(0, channel);
                }
            }
        }
    }

    fn reset(&mut self) {
        self.format.reset();
        self.notes.clear();
        self.asleep = false;
        self.zero_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use noodle_engine::Transport;

    use super::*;

    fn at(beats: f64, signature: (u8, u8), signature_start_beats: f64) -> Position {
        let ctx = Context {
            sample_rate: 48_000.0,
            frames: 256,
            transport: Transport {
                tick: beats * TICKS_PER_QUARTER as f64,
                signature: TimeSignature {
                    numerator: signature.0,
                    denominator: signature.1,
                },
                signature_start: signature_start_beats * TICKS_PER_QUARTER as f64,
                ..Transport::default()
            },
        };
        musical_position(&ctx, 0, 0)
    }

    #[test]
    fn a_six_eight_bar_is_three_quarter_notes() {
        // Tick 3 x 960 is the start of the second bar (the old CLAP reckoning,
        // which took the numerator for quarter notes, said bar 0).
        let p = at(3.0, (6, 8), 0.0);
        assert_eq!((p.beats, p.bar_start, p.bar_number), (3.0, 3.0, 1));
        let p = at(5.5, (6, 8), 0.0);
        assert_eq!((p.bar_start, p.bar_number), (3.0, 1));
    }

    #[test]
    fn a_two_two_bar_is_four_quarter_notes() {
        let p = at(7.9, (2, 2), 0.0);
        assert_eq!((p.bar_start, p.bar_number), (4.0, 1));
        let p = at(3.9, (2, 2), 0.0);
        assert_eq!((p.bar_start, p.bar_number), (0.0, 0));
    }

    #[test]
    fn bars_after_a_signature_change_count_from_where_it_began() {
        // 3/4 from the start of bar 5 of 4/4 (beat 16).
        let p = at(16.0, (3, 4), 16.0);
        assert_eq!((p.bar_start, p.bar_number), (16.0, 0));
        let p = at(20.5, (3, 4), 16.0);
        assert_eq!((p.bar_start, p.bar_number), (19.0, 1));
        // A change that is not on a multiple of the bar length.
        let p = at(11.0, (6, 8), 5.0);
        assert_eq!((p.bar_start, p.bar_number), (11.0, 2));
    }

    #[test]
    fn a_playing_transport_moves_on_within_the_block() {
        let mut ctx = Context {
            sample_rate: 48_000.0,
            frames: 48_000,
            transport: Transport {
                playing: true,
                ..Transport::default()
            },
        };
        // 120 bpm: half a second is a beat.
        let p = musical_position(&ctx, 24_000, 7);
        assert_eq!(
            (p.beats, p.seconds, p.samples, p.steady),
            (1.0, 0.5, 24_000.0, 7)
        );
        ctx.transport.playing = false;
        assert_eq!(musical_position(&ctx, 24_000, 0).beats, 0.0);
    }

    /// A format whose plugin writes `script[call]` (a constant) on its calls
    /// and answers with `status`, counting the calls.
    struct Scripted {
        script: Vec<f32>,
        status: Status,
        calls: usize,
    }

    impl Format for Scripted {
        fn begin(&mut self, _n: usize) -> bool {
            false
        }
        fn param(&mut self, _slot: usize, _at: usize, _value: f32) {}
        fn note(&mut self, _port: usize, _event: &Event, _at: usize, _key: Option<(u8, u8)>) {}
        fn transport(&mut self, _position: &Position) {}
        fn process(&mut self, n: usize, buffers: &mut Buffers) -> Status {
            let value = self.script.get(self.calls).copied().unwrap_or(0.0);
            self.calls += 1;
            buffers.outputs[0].channels[0][..n].fill(value);
            self.status
        }
        fn drain_note_outputs(&mut self, _emit: &mut dyn FnMut(usize, u32, EventKind)) {}
        fn latency(&self) -> u32 {
            0
        }
        fn reset(&mut self) {}
    }

    /// Runs `blocks` blocks of silence in; returns the plugin's calls and the
    /// samples out of each block's first frame.
    fn run_silence(script: Vec<f32>, status: Status, blocks: usize) -> (usize, Vec<f32>) {
        use noodle_engine::{Shape, SignalBuffer};
        const FRAMES: usize = 256;
        let format = Scripted {
            script,
            status,
            calls: 0,
        };
        let mut bridge = Bridge::new(
            format,
            Buffers::new([1], [1], FRAMES),
            Vec::new(),
            0,
            false,
            FRAMES,
        );
        let ctx = Context {
            sample_rate: 48_000.0,
            frames: FRAMES,
            transport: Transport::default(),
        };
        let mut input = SignalBuffer::new(Shape::MONO, FRAMES);
        let mut output = SignalBuffer::new(Shape::MONO, FRAMES);
        let mut heard = Vec::new();
        for _ in 0..blocks {
            input.as_out(FRAMES).silence(0, 0);
            let inputs = [input.as_in(FRAMES)];
            let mut outputs = [output.as_out(FRAMES)];
            bridge.process(
                &ctx,
                Io {
                    inputs: &inputs,
                    outputs: &mut outputs,
                    event_inputs: &[],
                    event_outputs: &mut [],
                    spectral_inputs: &[],
                    spectral_outputs: &mut [],
                },
            );
            heard.push(output.as_in(FRAMES).lane(0, 0)[0]);
        }
        (bridge.format.calls, heard)
    }

    #[test]
    fn a_silent_block_between_echoes_does_not_put_a_plugin_to_sleep() {
        // Silent blocks flagged (as VST3 and AU do), then an echo at call 3.
        let script = vec![0.0, 0.0, 0.0, 0.5, 0.0];
        for status in [Status::Unknown, Status::SilentTail(48_000)] {
            let (calls, heard) = run_silence(script.clone(), status, 5);
            assert_eq!(calls, 5, "{status:?}: called every block");
            assert_eq!(heard[3], 0.5, "{status:?}: the echo arrived");
        }
    }

    #[test]
    fn a_plugin_is_left_alone_once_its_tail_has_gone_by() {
        // A tail of 512 frames is two blocks of silence in and out.
        let (calls, _) = run_silence(vec![], Status::SilentTail(512), 40);
        assert_eq!(calls, 2);
        // Sleep is taken at its word at once.
        let (calls, _) = run_silence(vec![], Status::Sleep, 40);
        assert_eq!(calls, 1);
        // No word: the 4 s timeout (750 blocks of 256 frames).
        let (calls, _) = run_silence(vec![], Status::Unknown, 900);
        assert_eq!(calls, 750);
    }
}
