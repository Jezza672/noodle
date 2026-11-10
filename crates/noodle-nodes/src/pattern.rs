//! The Pattern Player node: a sampler for MIDI clips.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use noodle_core::MidiClip;
use noodle_engine::{
    Config, Context, Event, EventKind, EventsOut, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, ParamInfo, Ports, Setup, Unit,
};

use crate::clip::{AUDITION_ID, ClipFeeds};
use crate::clock::{DIVISION_TICKS, DIVISIONS};

/// Plays a MIDI clip on its `notes` output, the way the Sampler plays a
/// sample. The clips are the player's own (their `node` is the player, and the
/// timeline never shows them); each is a *slot*, picked by `slot`, and is
/// edited in the piano roll or the step grid like any other MIDI clip. A
/// step sequencer is a short clip played in Step or Loop mode.
///
/// Modes:
/// - *Loop* plays the region over and over. With `sync` on *Transport* the
///   position is read from the timeline, so the pattern stays on the bar
///   through tempo changes and seeks, and is silent while the transport is
///   stopped; on *Free* it starts when it is switched on and runs at the
///   tempo even with the transport stopped. `play` says what switches it
///   on: *Always*, only while a note is held on `trigger` (*Held*), or a
///   note on `trigger` toggling it (*Toggle*).
/// - *One-shot*: a note on `trigger` plays the region once, ignoring the
///   note-off; another note restarts it.
/// - *Gate*: like One-shot, but the note-off ends the pattern at once.
/// - *Step*: each rising edge on `advance` (and each note on `trigger`)
///   plays the next window of one `step` and wraps at the region's end. A
///   rising edge on `reset` goes back to the first.
///
/// `reset` also restarts a Free loop, a one-shot, and (for Transport sync)
/// counts the bar from that moment. The region is `length` steps of `step`
/// from step `start`; `rate` plays it faster or slower (2/3 and 3/2 move
/// between straight and triplet feel). A note that starts in the region
/// plays, cut at the region's end. `transpose` shifts the notes, and with
/// `keytrack` on the key played on `trigger` shifts them further, relative to
/// `root`. `velocity` scales their velocity.
///
/// `gate` is high while any pattern note sounds and `out` holds the velocity
/// of the latest, so either can drive a VCA or an envelope. `out` is the
/// LFO's output key, so Replace node keeps what an LFO modulated.
///
/// Changing the pattern, the slot or the region ends the notes that are
/// sounding. Chords are several notes; the player has one pattern at a time
/// and `gate` and `out` are mono.
pub struct PatternPlayer {
    feeds: ClipFeeds,
}

impl PatternPlayer {
    pub fn new(feeds: &ClipFeeds) -> Self {
        Self {
            feeds: feeds.clone(),
        }
    }
}

pub const PATTERN_ID: &str = noodle_core::group::PATTERN_PLAYER;

pub const MODES: [&str; 4] = ["Loop", "One-shot", "Gate", "Step"];
const LOOP: u8 = 0;
const ONE_SHOT: u8 = 1;
const GATE: u8 = 2;
const STEP: u8 = 3;

/// What switches a Loop on: `Always` is 0.
const PLAY_HELD: u8 = 1;
const PLAY_TOGGLE: u8 = 2;

/// The `rate` choices and the speed each plays at.
pub const RATE_NAMES: [&str; 7] = ["1/4", "1/2", "2/3", "1", "3/2", "2", "4"];
const RATES: [f64; 7] = [0.25, 0.5, 2.0 / 3.0, 1.0, 1.5, 2.0, 4.0];

/// The default `step`: a sixteenth note.
const DEFAULT_STEP: f32 = 4.0;
/// The longest region, in steps.
pub const MAX_STEPS: f32 = 256.0;
/// The most slots a player picks between.
pub const MAX_SLOTS: usize = 64;
/// The default root of `keytrack`: middle C.
const DEFAULT_ROOT: f32 = 60.0;

#[derive(Ports)]
struct PatternPorts {
    #[event_input("trigger", "Trigger")]
    trigger: (),
    #[input("advance", "Advance")]
    advance: (),
    #[input("reset", "Reset")]
    reset: (),
    #[param("mode", "Mode", ParamInfo::choice(MODES))]
    mode: (),
    #[param("play", "Play", ParamInfo::choice(["Always", "Held", "Toggle"]))]
    play: (),
    #[param("sync", "Sync", ParamInfo::choice(["Transport", "Free"]))]
    sync: (),
    #[param("rate", "Rate", ParamInfo::choice(RATE_NAMES).default_to(3.0))]
    rate: (),
    #[param("step", "Step", ParamInfo::choice(DIVISIONS).default_to(DEFAULT_STEP))]
    step: (),
    #[param("start", "Start", ParamInfo::new(0.0, MAX_STEPS - 1.0, 0.0).smoothing(0.0))]
    start: (),
    #[param("length", "Length", ParamInfo::new(1.0, MAX_STEPS, 16.0).smoothing(0.0))]
    length: (),
    #[param("slot", "Slot", ParamInfo::new(0.0, MAX_SLOTS as f32 - 1.0, 0.0).smoothing(0.0))]
    slot: (),
    #[param(
        "transpose",
        "Transpose",
        ParamInfo::new(-48.0, 48.0, 0.0).unit(Unit::Semitones).smoothing(0.0)
    )]
    transpose: (),
    #[param("root", "Root", ParamInfo::new(0.0, 127.0, DEFAULT_ROOT).unit(Unit::Note).smoothing(0.0))]
    root: (),
    #[param("keytrack", "Keytrack", ParamInfo::choice(["Off", "On"]))]
    keytrack: (),
    #[param("velocity", "Velocity", ParamInfo::new(0.0, 2.0, 1.0))]
    velocity: (),
    #[event_output("notes", "Notes")]
    notes: (),
    #[control_output("gate", "Gate")]
    gate: (),
    #[control_output("out", "Out")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: PATTERN_ID,
    version: 1,
    name: "Pattern Player",
    category: "Events",
};

// ---------------------------------------------------------------------
// The patterns, handed from the project to the audio thread.
// ---------------------------------------------------------------------

/// One note of a pattern, in ticks from the start of its clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternNote {
    pub start: i64,
    /// The first tick after the note: never before `start + 1`, never past
    /// the end of its clip.
    pub end: i64,
    pub key: u8,
    pub velocity: f32,
}

/// One clip of a player, ready to play: its notes sorted by start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternSlot {
    pub length: i64,
    pub notes: Vec<PatternNote>,
}

impl PatternSlot {
    /// The notes of `clip` that play: those it holds after its takes are
    /// comped, that start inside it.
    pub fn from_midi(clip: &MidiClip) -> Self {
        let length = clip.length.0.max(1);
        let mut notes: Vec<PatternNote> = clip
            .played()
            .into_iter()
            .filter(|n| n.start.0 >= 0 && n.start.0 < length)
            .map(|n| PatternNote {
                start: n.start.0,
                end: n.end().0.min(length).max(n.start.0 + 1),
                key: n.key,
                velocity: n.velocity,
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        Self { length, notes }
    }
}

/// Every slot of a player, in the order of their clips' IDs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternSet {
    pub slots: Vec<PatternSlot>,
}

/// What the UI side and the player node share: the newest set of slots.
///
/// The node never waits and never frees: it reads the version with an atomic
/// load and, only when that changed, takes the new set with a `try_lock`
/// (which gives up rather than wait). Replaced sets are parked here until
/// nothing else holds them, so the last reference is always dropped on the
/// UI side.
pub(crate) struct PatternShared {
    latest: Mutex<Arc<PatternSet>>,
    version: AtomicU64,
    parked: Mutex<Vec<Arc<PatternSet>>>,
    /// The keys held down from the piano roll's keyboard, one bit each.
    pub audition: [AtomicU64; 2],
}

impl PatternShared {
    pub fn new() -> Self {
        Self {
            latest: Mutex::new(Arc::new(PatternSet::default())),
            version: AtomicU64::new(0),
            parked: Mutex::new(Vec::new()),
            audition: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    /// Makes `set` the newest, if it differs from it.
    pub fn set(&self, set: PatternSet) {
        let mut latest = self.latest.lock().expect("pattern lock");
        if **latest == set {
            return;
        }
        let old = std::mem::replace(&mut *latest, Arc::new(set));
        self.version.fetch_add(1, Ordering::Release);
        drop(latest);
        let mut parked = self.parked.lock().expect("pattern lock");
        parked.retain(|p| Arc::strong_count(p) > 1);
        parked.push(old);
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// The newest set and its version, or `None` if the UI side is in the
    /// middle of replacing it.
    pub fn try_latest(&self) -> Option<(u64, Arc<PatternSet>)> {
        let latest = self.latest.try_lock().ok()?;
        Some((self.version(), Arc::clone(&latest)))
    }

    pub fn set_audition(&self, key: u8, on: bool) {
        let key = key.min(127);
        let bit = 1u64 << (key % 64);
        let word = &self.audition[usize::from(key / 64)];
        if on {
            word.fetch_or(bit, Ordering::Relaxed);
        } else {
            word.fetch_and(!bit, Ordering::Relaxed);
        }
    }

    /// Whether there are no slots.
    pub fn is_empty(&self) -> bool {
        self.latest.lock().expect("pattern lock").slots.is_empty()
    }

    pub fn is_live(&self) -> bool {
        self.audition
            .iter()
            .any(|word| word.load(Ordering::Relaxed) != 0)
    }

    pub fn latest(&self) -> Arc<PatternSet> {
        Arc::clone(&self.latest.lock().expect("pattern lock"))
    }
}

// ---------------------------------------------------------------------
// The node type.
// ---------------------------------------------------------------------

impl NodeType for PatternPlayer {
    fn pulses_on_beat(&self) -> bool {
        true
    }

    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(PatternPorts::layout())
    }

    /// The player's clips live in the project, so they are keyed here, as the
    /// track input keys its own.
    fn cache_inputs(
        &self,
        node: noodle_core::NodeId,
        _config: &Config,
        env: &noodle_engine::CacheEnv<'_>,
        key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), noodle_engine::Uncacheable> {
        for (id, clip) in env.project.patterns_of(node) {
            if let Some(midi) = clip.as_midi() {
                key.u64(id.0);
                crate::clip::key_midi(key, midi);
            }
        }
        Ok(())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shared = self.feeds.pattern_shared(setup.node);
        let salt = (setup.seed ^ (setup.seed >> 32)) as u32 & 0xFF;
        Ok(Instance::realtime(PatternNode::new(shared, salt)))
    }
}

// ---------------------------------------------------------------------
// The node.
// ---------------------------------------------------------------------

/// The most notes sounding at once.
const MAX_SOUNDING: usize = 64;
/// The most trigger notes held at once.
const MAX_HELD: usize = 16;

#[derive(Clone, Copy, Default)]
struct Sounding {
    active: bool,
    id: u32,
    /// The sample (counting from the node's start) the note ends on.
    off_at: u64,
}

/// The parameters of a block.
#[derive(Clone, Copy)]
struct Params {
    mode: u8,
    play: u8,
    free: bool,
    rate: f64,
    /// The start of the region, its length and the step, in ticks.
    ws: f64,
    len: f64,
    step: f64,
    steps: usize,
    slot: usize,
    transpose: i32,
    root: i32,
    keytrack: bool,
    velocity: f32,
}

struct PatternNode {
    shared: Arc<PatternShared>,
    set: Arc<PatternSet>,
    version: u64,
    audition_held: [u64; 2],
    /// Distinguishes this player's note IDs from another's.
    salt: u32,
    sample_rate: f32,
    /// Samples since the start, to date note-offs in later blocks.
    now: u64,
    sounding: [Sounding; MAX_SOUNDING],
    held: [u32; MAX_HELD],
    held_count: usize,
    latched: bool,
    trigger_key: u8,
    /// Whether a One-shot or Gate is playing.
    active: bool,
    /// Free running: the pattern position at the next block's first sample.
    carry: f64,
    /// Transport sync: the tick the bar is counted from.
    origin: f64,
    expected_tick: Option<f64>,
    was_playing: bool,
    /// Step mode: the window that plays next.
    step_index: usize,
    /// How many times the pattern has restarted, to tell repeats apart.
    repeat: u32,
    adv_high: bool,
    rst_high: bool,
    /// What the pattern was last block, to end notes when it changes.
    signature: (u8, usize, i64, i64, i64),
    count: u32,
    last_velocity: f32,
}

impl PatternNode {
    fn new(shared: Arc<PatternShared>, salt: u32) -> Self {
        let set = shared.latest();
        let version = shared.version();
        let mut node = Self::fresh(shared, salt);
        node.set = set;
        node.version = version;
        node
    }

    /// A node with no pattern yet and nothing playing; takes no lock.
    fn fresh(shared: Arc<PatternShared>, salt: u32) -> Self {
        let (set, version) = (Arc::new(PatternSet::default()), 0);
        Self {
            shared,
            set,
            version,
            audition_held: [0; 2],
            salt,
            sample_rate: 48_000.0,
            now: 0,
            sounding: [Sounding::default(); MAX_SOUNDING],
            held: [0; MAX_HELD],
            held_count: 0,
            latched: false,
            trigger_key: 60,
            active: false,
            carry: 0.0,
            origin: 0.0,
            expected_tick: None,
            was_playing: false,
            step_index: 0,
            repeat: 0,
            adv_high: false,
            rst_high: false,
            signature: (0, 0, 0, 0, 0),
            count: 0,
            last_velocity: 0.0,
        }
    }

    fn note_id(&self, index: usize, repeat: u32) -> u32 {
        self.salt << 21 | (repeat & 0x1FF) << 12 | (index as u32 & 0xFFF)
    }

    fn refresh(&mut self) {
        if self.shared.version() != self.version
            && let Some((version, set)) = self.shared.try_latest()
        {
            self.set = set;
            self.version = version;
            // The notes of the old pattern are not the new one's.
            self.signature.1 = usize::MAX;
        }
    }

    fn put(out: &mut EventsOut<'_>, frames: usize, time: usize, kind: EventKind) {
        let _ = out.insert(Event {
            time: time.min(frames.saturating_sub(1)) as u32,
            kind,
        });
    }

    fn all_off(&mut self, out: &mut EventsOut<'_>, frames: usize, time: usize) {
        for s in self.sounding.iter_mut().filter(|s| s.active) {
            Self::put(
                out,
                frames,
                time,
                EventKind::NoteOff {
                    note: NoteId(s.id),
                    velocity: 0.0,
                },
            );
            s.active = false;
        }
    }

    /// Ends the notes that are due before sample `to` of this block.
    fn flush_offs(&mut self, out: &mut EventsOut<'_>, frames: usize, to: usize) {
        let limit = self.now + to as u64;
        for s in self
            .sounding
            .iter_mut()
            .filter(|s| s.active && s.off_at < limit)
        {
            let time = s.off_at.saturating_sub(self.now) as usize;
            Self::put(
                out,
                frames,
                time,
                EventKind::NoteOff {
                    note: NoteId(s.id),
                    velocity: 0.0,
                },
            );
            s.active = false;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_note(
        &mut self,
        p: &Params,
        pps: f64,
        out: &mut EventsOut<'_>,
        frames: usize,
        time: usize,
        index: usize,
        note: &PatternNote,
        rel_start: f64,
    ) {
        let Some(slot) = self.sounding.iter().position(|s| !s.active) else {
            return;
        };
        let key = (i32::from(note.key)
            + p.transpose
            + if p.keytrack {
                i32::from(self.trigger_key) - p.root
            } else {
                0
            })
        .clamp(0, 127) as u8;
        let velocity = (note.velocity * p.velocity).clamp(0.0, 1.0);
        let end_rel = (note.end as f64).min(p.ws + p.len) - p.ws;
        let ticks = (end_rel - rel_start).max(1.0);
        let id = self.note_id(index, self.repeat);
        self.sounding[slot] = Sounding {
            active: true,
            id,
            off_at: self.now + time as u64 + (ticks / pps).ceil().max(1.0) as u64,
        };
        Self::put(
            out,
            frames,
            time,
            EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity,
            },
        );
    }

    /// Whether a Loop (or a One-shot, or a Gate) is playing now.
    fn linear_active(&self, p: &Params, playing: bool) -> bool {
        match p.mode {
            LOOP => {
                let on = match p.play {
                    PLAY_HELD => self.held_count > 0,
                    PLAY_TOGGLE => self.latched,
                    _ => true,
                };
                on && (p.free || playing)
            }
            ONE_SHOT | GATE => self.active,
            _ => false,
        }
    }

    /// Plays the notes that start in samples `from..to`, where the pattern
    /// position (in ticks from the start of the region) is `base + i * pps`
    /// at sample `i`.
    #[allow(clippy::too_many_arguments)]
    fn generate(
        &mut self,
        p: &Params,
        set: &PatternSet,
        base: f64,
        pps: f64,
        playing: bool,
        out: &mut EventsOut<'_>,
        frames: usize,
        from: usize,
        to: usize,
    ) {
        if from < to && self.linear_active(p, playing) {
            let c0 = base + from as f64 * pps;
            let c1 = base + to as f64 * pps;
            let looping = p.mode == LOOP;
            if let Some(slot) = set.slots.get(p.slot.min(set.slots.len().saturating_sub(1))) {
                let (k0, k1) = if looping {
                    (
                        (c0 / p.len).floor() as i64,
                        ((c1 / p.len).ceil() as i64) - 1,
                    )
                } else {
                    (0, 0)
                };
                for k in k0.max(0)..=k1.max(k0.max(0)) {
                    let offset = k as f64 * p.len;
                    let lo = c0 - offset;
                    let hi = c1 - offset;
                    if !looping && (hi <= 0.0 || lo >= p.len) {
                        break;
                    }
                    let first = slot.notes.partition_point(|n| (n.start as f64 - p.ws) < lo);
                    for (index, note) in slot.notes.iter().enumerate().skip(first) {
                        let rel = note.start as f64 - p.ws;
                        if rel >= hi || rel >= p.len {
                            break;
                        }
                        if rel < 0.0 {
                            continue;
                        }
                        let time = (((offset + rel - base) / pps).ceil().max(0.0) as usize)
                            .clamp(from, to - 1);
                        self.repeat = k as u32;
                        self.start_note(p, pps, out, frames, time, index, note, rel);
                    }
                }
            }
            if !looping && c1 >= p.len {
                self.active = false;
            }
        }
        if from < to {
            self.flush_offs(out, frames, to);
        }
    }

    /// Plays the next window of Step mode at `time`.
    fn step_fire(
        &mut self,
        p: &Params,
        set: &PatternSet,
        pps: f64,
        out: &mut EventsOut<'_>,
        frames: usize,
        time: usize,
    ) {
        let w = self.step_index % p.steps.max(1);
        self.step_index = (w + 1) % p.steps.max(1);
        let Some(slot) = set.slots.get(p.slot.min(set.slots.len().saturating_sub(1))) else {
            return;
        };
        let lo = p.ws + w as f64 * p.step;
        let hi = lo + p.step;
        let first = slot.notes.partition_point(|n| (n.start as f64) < lo);
        self.repeat = self.repeat.wrapping_add(1);
        for (index, note) in slot.notes.iter().enumerate().skip(first) {
            if note.start as f64 >= hi {
                break;
            }
            self.start_note(
                p,
                pps,
                out,
                frames,
                time,
                index,
                note,
                note.start as f64 - p.ws,
            );
        }
    }

    fn press(&mut self, id: u32) {
        if self.held_count < MAX_HELD && !self.held[..self.held_count].contains(&id) {
            self.held[self.held_count] = id;
            self.held_count += 1;
        }
    }

    fn release(&mut self, id: u32) -> bool {
        match self.held[..self.held_count].iter().position(|&h| h == id) {
            Some(i) => {
                self.held.copy_within(i + 1..self.held_count, i);
                self.held_count -= 1;
                true
            }
            None => false,
        }
    }

    /// Starts the pattern from its beginning at sample `time`.
    fn restart(&mut self, base: &mut f64, pps: f64, time: usize) {
        *base = -(time as f64) * pps;
        self.origin = f64::NAN;
    }

    fn play_audition(&mut self, out: &mut EventsOut<'_>, frames: usize) {
        for word in 0..2 {
            let wanted = self.shared.audition[word].load(Ordering::Relaxed);
            let mut changed = wanted ^ self.audition_held[word];
            while changed != 0 {
                let bit = changed.trailing_zeros();
                changed &= changed - 1;
                let key = word as u32 * 64 + bit;
                let note = NoteId(AUDITION_ID | key);
                let kind = if wanted >> bit & 1 == 1 {
                    EventKind::NoteOn {
                        note,
                        channel: 0,
                        key: key as u8,
                        velocity: 0.8,
                    }
                } else {
                    EventKind::NoteOff {
                        note,
                        velocity: 0.0,
                    }
                };
                if out.insert(Event { time: 0, kind }).is_ok() {
                    self.audition_held[word] ^= 1 << bit;
                }
            }
        }
        let _ = frames;
    }
}

impl Node for PatternNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            outputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        self.sample_rate = ctx.sample_rate;
        self.refresh();
        let set = Arc::clone(&self.set);
        let frames = ctx.frames;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let choice = |port: usize, max: usize| (first(port).round().max(0.0) as usize).min(max);
        let step_index = choice(PatternPorts::STEP, DIVISION_TICKS.len() - 1);
        let step = DIVISION_TICKS[step_index];
        let steps = (first(PatternPorts::LENGTH).round().clamp(1.0, MAX_STEPS) as usize).max(1);
        let start = first(PatternPorts::START)
            .round()
            .clamp(0.0, MAX_STEPS - 1.0);
        let p = Params {
            mode: choice(PatternPorts::MODE, 3) as u8,
            play: choice(PatternPorts::PLAY, 2) as u8,
            free: choice(PatternPorts::SYNC, 1) == 1,
            rate: RATES[choice(PatternPorts::RATE, RATES.len() - 1)],
            ws: f64::from(start) * step,
            len: steps as f64 * step,
            step,
            steps,
            slot: choice(PatternPorts::SLOT, MAX_SLOTS - 1),
            transpose: first(PatternPorts::TRANSPOSE).round() as i32,
            root: first(PatternPorts::ROOT).round() as i32,
            keytrack: first(PatternPorts::KEYTRACK) >= 0.5,
            velocity: first(PatternPorts::VELOCITY).clamp(0.0, 2.0),
        };
        let out = &mut event_outputs[PatternPorts::NOTES];
        let transport = ctx.transport;
        let tps = transport.bpm * 960.0 / 60.0 / f64::from(ctx.sample_rate);
        let pps = tps * p.rate;

        // A change of what is played ends the notes of the old one.
        let slot_len = set
            .slots
            .get(p.slot.min(set.slots.len().saturating_sub(1)))
            .map_or(0, |s| s.length);
        let signature = (p.mode, p.slot, p.ws as i64, p.len as i64, slot_len);
        let mut changed = self.signature != signature;
        // Transport sync follows the timeline: a stop or a jump leaves the
        // sounding notes hanging, so they are ended.
        let sync_transport = p.mode == LOOP && !p.free;
        if sync_transport {
            let jumped = self
                .expected_tick
                .is_some_and(|e| (e - transport.tick).abs() > 1.0);
            if !transport.playing && self.was_playing || (transport.playing && jumped) {
                changed = true;
            }
        }
        if changed {
            self.all_off(out, frames, 0);
            if p.mode != signature_mode(self.signature) {
                self.active = false;
                self.latched = false;
            }
            self.signature = signature;
        }
        if transport.playing && !self.was_playing {
            self.step_index = 0;
        }
        self.expected_tick = transport
            .playing
            .then_some(transport.tick + frames as f64 * tps);
        self.was_playing = transport.playing;
        if self.origin.is_nan() {
            self.origin = transport.tick;
        }

        // The pattern position of sample 0.
        let mut base = if sync_transport {
            (transport.tick - self.origin) * p.rate
        } else {
            self.carry
        };

        let advance = inputs[PatternPorts::ADVANCE].lane(0, 0);
        let reset = inputs[PatternPorts::RESET].lane(0, 0);
        let mut triggers = event_inputs[PatternPorts::TRIGGER].iter().peekable();
        let mut seg = 0usize;
        for i in 0..=frames {
            let mut act = i == frames;
            let mut adv_edge = false;
            let mut rst_edge = false;
            if i < frames {
                let adv = advance.get(i).copied().unwrap_or(0.0) >= 0.5;
                adv_edge = adv && !self.adv_high;
                self.adv_high = adv;
                let rst = reset.get(i).copied().unwrap_or(0.0) >= 0.5;
                rst_edge = rst && !self.rst_high;
                self.rst_high = rst;
                act = adv_edge && p.mode == STEP
                    || rst_edge
                    || triggers.peek().is_some_and(|e| e.time as usize <= i);
            }
            if !act {
                continue;
            }
            self.generate(&p, &set, base, pps, transport.playing, out, frames, seg, i);
            seg = i;
            if i == frames {
                break;
            }
            if rst_edge {
                self.all_off(out, frames, i);
                self.step_index = 0;
                if sync_transport {
                    self.origin = transport.tick + i as f64 * tps;
                    base = 0.0;
                } else {
                    self.restart(&mut base, pps, i);
                    self.origin = 0.0;
                }
            }
            if adv_edge && p.mode == STEP {
                self.step_fire(&p, &set, pps, out, frames, i);
            }
            while let Some(event) = triggers.next_if(|e| e.time as usize <= i) {
                match event.kind {
                    EventKind::NoteOn { note, key, .. } => {
                        self.press(note.0);
                        self.trigger_key = key;
                        match p.mode {
                            LOOP => {
                                let was_on = self.linear_active(&p, transport.playing);
                                if p.play == PLAY_TOGGLE {
                                    self.latched = !self.latched;
                                }
                                let now_on = self.linear_active(&p, transport.playing);
                                if now_on && !was_on && p.free {
                                    self.restart(&mut base, pps, i);
                                    self.origin = 0.0;
                                }
                                if was_on && !now_on {
                                    self.all_off(out, frames, i);
                                }
                            }
                            ONE_SHOT | GATE => {
                                self.all_off(out, frames, i);
                                self.restart(&mut base, pps, i);
                                self.origin = 0.0;
                                self.active = true;
                            }
                            _ => self.step_fire(&p, &set, pps, out, frames, i),
                        }
                    }
                    EventKind::NoteOff { note, .. } => {
                        let last = self.release(note.0) && self.held_count == 0;
                        match p.mode {
                            LOOP if last && p.play == PLAY_HELD => {
                                self.all_off(out, frames, i);
                            }
                            GATE if last => {
                                self.all_off(out, frames, i);
                                self.active = false;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        if !sync_transport {
            self.carry = base + frames as f64 * pps;
        }

        // `gate` and `out` follow the notes that were put out.
        let (gate, level) = {
            let (head, tail) = outputs.split_at_mut(PatternPorts::OUT);
            (&mut head[PatternPorts::GATE], &mut tail[0])
        };
        let (gate, level) = (gate.lane_mut(0, 0), level.lane_mut(0, 0));
        let mut at = 0;
        for event in out.as_slice() {
            let time = (event.time as usize).min(frames);
            fill(gate, level, at, time, self.count, self.last_velocity);
            at = time;
            match event.kind {
                EventKind::NoteOn { velocity, .. } => {
                    self.count += 1;
                    self.last_velocity = velocity;
                }
                EventKind::NoteOff { .. } => self.count = self.count.saturating_sub(1),
                _ => {}
            }
        }
        fill(gate, level, at, frames, self.count, self.last_velocity);

        self.play_audition(out, frames);
        self.now += frames as u64;
    }

    fn reset(&mut self) {
        // Playback state goes back to a new instance's; the pattern is kept,
        // so the audio thread takes no lock and allocates nothing here.
        self.audition_held = [0; 2];
        self.now = 0;
        self.sounding = [Sounding::default(); MAX_SOUNDING];
        self.held = [0; MAX_HELD];
        self.held_count = 0;
        self.latched = false;
        self.trigger_key = 60;
        self.active = false;
        self.carry = 0.0;
        self.origin = 0.0;
        self.expected_tick = None;
        self.was_playing = false;
        self.step_index = 0;
        self.repeat = 0;
        self.adv_high = false;
        self.rst_high = false;
        self.signature = (0, 0, 0, 0, 0);
        self.count = 0;
        self.last_velocity = 0.0;
    }
}

fn signature_mode(signature: (u8, usize, i64, i64, i64)) -> u8 {
    signature.0
}

fn fill(gate: &mut [f32], level: &mut [f32], from: usize, to: usize, count: u32, velocity: f32) {
    let high = f32::from(count > 0);
    gate[from..to].fill(high);
    level[from..to].fill(velocity);
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{MidiNote, Tick};
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    /// At 120 bpm a quarter note is 24,000 samples: a tick is 25 samples.
    const TICK: usize = 25;

    fn slot(notes: &[(i64, i64, u8)], length: i64) -> PatternSlot {
        PatternSlot::from_midi(&MidiClip {
            length: Tick(length),
            notes: notes
                .iter()
                .map(|&(start, len, key)| MidiNote::new(Tick(start), Tick(len), key))
                .collect(),
            ..MidiClip::new(Tick(length))
        })
    }

    struct Rig {
        harness: Harness,
        shared: Arc<PatternShared>,
        blocks: usize,
    }

    fn rig(slots: Vec<PatternSlot>, params: &[(usize, f32)]) -> Rig {
        let feeds = ClipFeeds::default();
        let node = PatternPlayer::new(&feeds);
        let shared = feeds.pattern_shared(noodle_core::NodeId(0));
        shared.set(PatternSet { slots });
        let mut harness = Harness::new(
            &node,
            &Config::new(),
            &[
                (PatternPorts::ADVANCE, noodle_engine::Shape::MONO),
                (PatternPorts::RESET, noodle_engine::Shape::MONO),
            ],
            RATE,
            480,
        )
        .unwrap();
        for &(port, value) in params {
            harness.set(port, value);
        }
        Rig {
            harness,
            shared,
            blocks: 0,
        }
    }

    impl Rig {
        /// Runs `blocks` blocks of 480 frames and returns every event with
        /// its sample time.
        fn run(&mut self, blocks: usize) -> Vec<(usize, EventKind)> {
            let mut all = Vec::new();
            for _ in 0..blocks {
                self.harness.run(480).unwrap();
                all.extend(
                    self.harness
                        .events(PatternPorts::NOTES)
                        .iter()
                        .map(|e| (self.blocks * 480 + e.time as usize, e.kind)),
                );
                self.blocks += 1;
            }
            all
        }
    }

    fn ons(events: &[(usize, EventKind)]) -> Vec<(usize, u8)> {
        events
            .iter()
            .filter_map(|(t, k)| match k {
                EventKind::NoteOn { key, .. } => Some((*t, *key)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_loop_plays_the_region_on_the_transport() {
        // Two notes in a 1/16 x 4 region (960 ticks): a loop of one beat.
        let pattern = slot(&[(0, 100, 60), (480, 100, 64)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::LENGTH, 4.0), (PatternPorts::STEP, 4.0)],
        );
        let events = r.run(100);
        let on = ons(&events);
        assert_eq!(
            &on[..4],
            [
                (0, 60),
                (480 * TICK, 64),
                (960 * TICK, 60),
                (1440 * TICK, 64)
            ]
        );
    }

    #[test]
    fn a_loop_is_silent_while_the_transport_is_stopped() {
        let pattern = slot(&[(0, 100, 60)], 960);
        let mut r = rig(vec![pattern], &[(PatternPorts::LENGTH, 4.0)]);
        r.harness.set_playing(false);
        assert!(ons(&r.run(20)).is_empty());
    }

    #[test]
    fn notes_end_where_the_region_does() {
        // A note longer than the region is cut at its end.
        let pattern = slot(&[(0, 4000, 60)], 4000);
        let mut r = rig(vec![pattern], &[(PatternPorts::LENGTH, 4.0)]);
        let events = r.run(60);
        let off = events
            .iter()
            .find(|(_, k)| matches!(k, EventKind::NoteOff { .. }))
            .map(|(t, _)| *t)
            .unwrap();
        assert!((off as i64 - 960 * TICK as i64).abs() <= 1, "{off}");
    }

    #[test]
    fn a_free_loop_runs_with_the_transport_stopped() {
        let pattern = slot(&[(0, 100, 60)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::LENGTH, 4.0), (PatternPorts::SYNC, 1.0)],
        );
        r.harness.set_playing(false);
        let on = ons(&r.run(100));
        assert!(on.len() >= 2, "{on:?}");
    }

    #[test]
    fn rate_speeds_the_pattern_up() {
        let pattern = slot(&[(0, 100, 60)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::LENGTH, 4.0), (PatternPorts::RATE, 5.0)],
        );
        let on = ons(&r.run(60));
        assert_eq!(on[1].0, 480 * TICK);
    }

    #[test]
    fn transpose_shifts_the_notes() {
        let pattern = slot(&[(0, 100, 60)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::LENGTH, 4.0), (PatternPorts::TRANSPOSE, 7.0)],
        );
        assert_eq!(ons(&r.run(2))[0].1, 67);
    }

    #[test]
    fn gate_and_out_follow_the_notes() {
        let pattern = slot(&[(0, 240, 60)], 960);
        let mut r = rig(vec![pattern], &[(PatternPorts::LENGTH, 4.0)]);
        r.run(1);
        let gate = r.harness.output(PatternPorts::GATE).lane(0, 0).to_vec();
        assert_eq!((gate[0], gate[479]), (1.0, 1.0));
        r.run(12);
        let gate = r.harness.output(PatternPorts::GATE).lane(0, 0).to_vec();
        assert_eq!(gate[479], 0.0);
        let level = r.harness.output(PatternPorts::OUT).lane(0, 0)[479];
        assert!(level > 0.0);
    }

    #[test]
    fn a_replaced_pattern_ends_the_old_notes() {
        let long = slot(&[(0, 960, 60)], 960);
        let mut r = rig(vec![long], &[(PatternPorts::LENGTH, 4.0)]);
        r.run(2);
        r.shared.set(PatternSet {
            slots: vec![slot(&[(480, 100, 62)], 960)],
        });
        let events = r.run(2);
        assert!(
            events
                .iter()
                .any(|(_, k)| matches!(k, EventKind::NoteOff { .. }))
        );
    }

    #[test]
    fn a_set_unchanged_keeps_its_version() {
        let shared = PatternShared::new();
        let set = PatternSet {
            slots: vec![slot(&[(0, 10, 60)], 960)],
        };
        shared.set(set.clone());
        let v = shared.version();
        shared.set(set);
        assert_eq!(shared.version(), v);
    }

    fn note_on(time: u32, id: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 1.0,
            },
        }
    }

    fn note_off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    #[test]
    fn a_one_shot_plays_once_and_ignores_the_note_off() {
        let pattern = slot(&[(0, 100, 60), (240, 100, 62)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::MODE, 1.0), (PatternPorts::LENGTH, 4.0)],
        );
        assert!(ons(&r.run(2)).is_empty());
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_on(0, 1, 36)]);
        let first = r.run(1);
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_off(0, 1)]);
        let rest = r.run(60);
        r.harness.send_events(PatternPorts::TRIGGER, &[]);
        let on = ons(&first.into_iter().chain(rest).collect::<Vec<_>>());
        assert_eq!(on.iter().map(|o| o.1).collect::<Vec<_>>(), [60, 62]);
    }

    #[test]
    fn a_gate_stops_at_the_note_off() {
        let pattern = slot(&[(0, 100, 60), (480, 100, 62)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::MODE, 2.0), (PatternPorts::LENGTH, 4.0)],
        );
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_on(0, 1, 36)]);
        r.run(1);
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_off(0, 1)]);
        r.run(1);
        r.harness.send_events(PatternPorts::TRIGGER, &[]);
        let later = r.run(60);
        assert!(ons(&later).is_empty(), "{later:?}");
    }

    #[test]
    fn a_loop_on_held_plays_only_while_a_note_is_down() {
        let pattern = slot(&[(0, 100, 60)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::PLAY, 1.0), (PatternPorts::LENGTH, 4.0)],
        );
        assert!(ons(&r.run(60)).is_empty());
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_on(0, 1, 36)]);
        r.run(1);
        r.harness.send_events(PatternPorts::TRIGGER, &[]);
        assert!(!ons(&r.run(60)).is_empty());
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_off(0, 1)]);
        r.run(1);
        r.harness.send_events(PatternPorts::TRIGGER, &[]);
        assert!(ons(&r.run(100)).is_empty());
    }

    #[test]
    fn step_mode_plays_one_window_per_advance_and_wraps() {
        // Steps of a sixteenth (240 ticks), region of 3 steps: keys 60, 62, 64.
        let pattern = slot(&[(0, 100, 60), (240, 100, 62), (480, 100, 64)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::MODE, 3.0), (PatternPorts::LENGTH, 3.0)],
        );
        let mut keys = Vec::new();
        for _ in 0..4 {
            r.harness.input(PatternPorts::ADVANCE, 480).fill(1.0);
            keys.extend(ons(&r.run(1)).iter().map(|o| o.1));
            r.harness.input(PatternPorts::ADVANCE, 480).fill(0.0);
            r.run(1);
        }
        assert_eq!(keys, [60, 62, 64, 60]);
    }

    #[test]
    fn reset_goes_back_to_the_first_step() {
        let pattern = slot(&[(0, 100, 60), (240, 100, 62)], 960);
        let mut r = rig(
            vec![pattern],
            &[(PatternPorts::MODE, 3.0), (PatternPorts::LENGTH, 2.0)],
        );
        r.harness.input(PatternPorts::ADVANCE, 480).fill(1.0);
        r.run(1);
        r.harness.input(PatternPorts::ADVANCE, 480).fill(0.0);
        r.run(1);
        r.harness.input(PatternPorts::RESET, 480).fill(1.0);
        r.run(1);
        r.harness.input(PatternPorts::RESET, 480).fill(0.0);
        r.harness.input(PatternPorts::ADVANCE, 480).fill(1.0);
        assert_eq!(ons(&r.run(1)).iter().map(|o| o.1).collect::<Vec<_>>(), [60]);
    }

    #[test]
    fn keytrack_transposes_by_the_trigger_key() {
        let pattern = slot(&[(0, 100, 60)], 960);
        let mut r = rig(
            vec![pattern],
            &[
                (PatternPorts::MODE, 1.0),
                (PatternPorts::LENGTH, 4.0),
                (PatternPorts::KEYTRACK, 1.0),
            ],
        );
        r.harness
            .send_events(PatternPorts::TRIGGER, &[note_on(0, 1, 64)]);
        assert_eq!(ons(&r.run(1))[0].1, 64);
    }

    #[test]
    fn ids_of_clip_notes_stay_in_the_clip_note_range() {
        let node = PatternNode::new(Arc::new(PatternShared::new()), 0xFF);
        let id = node.note_id(4095, u32::MAX);
        assert_eq!(id & !crate::clip::CLIP_NOTE_MASK, 0);
    }
}

#[cfg(test)]
mod feeds_tests {
    use super::*;

    /// An offline render updates the feeds before the node is built, so the
    /// patterns must wait for it.
    #[test]
    fn patterns_set_before_the_node_exists_are_kept() {
        use noodle_core::{Clip, ClipContent, Command, Node, Project};
        let mut project = Project::new();
        let id = project.new_node_id();
        Command::AddNode {
            id,
            node: Node::new(PATTERN_ID),
        }
        .apply(&mut project)
        .unwrap();
        let clip = project.new_clip_id();
        Command::AddClip {
            id: clip,
            clip: Clip {
                node: id,
                start: noodle_core::Tick(0),
                content: ClipContent::Midi(MidiClip::new(noodle_core::Tick(960))),
            },
        }
        .apply(&mut project)
        .unwrap();
        let feeds = ClipFeeds::default();
        let table = noodle_engine::TempoTable::new(project.tempo_map(), 48_000.0);
        feeds.update(&project, &table, 48_000, std::path::Path::new("."));
        assert_eq!(feeds.pattern_shared(id).latest().slots.len(), 1);
    }
}
