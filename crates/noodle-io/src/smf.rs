//! Standard MIDI Files: a pure data layer (`read_smf`, `write_smf`) on `midly`,
//! and a mapping between a file and the project's [`MidiClip`]s and
//! [`TempoMap`] (`import`, `export`).
//!
//! MIDI 1.0 notes and controllers only. SMPTE time division, format 2, sysex
//! and text events other than the track name are not carried.

use std::fmt;

use midly::num::{u4, u7, u14, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use noodle_core::group::{PORT_NAME, TRACK_INPUT};
use noodle_core::{
    Clip, ClipContent, ClipId, ControlKind, ControlPoint, ExpressionKind, MidiClip, MidiNote,
    NodeId, NoteExpression, Project, SignatureChange, TICKS_PER_QUARTER, TempoChange, TempoMap,
    Tick, TimeSignature, Value,
};

/// Why a file could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmfError(pub String);

impl fmt::Display for SmfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SmfError {}

/// One channel message of a file track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
    },
    Controller {
        number: u8,
        value: u8,
    },
    /// 14 bit, 8192 at the centre.
    PitchBend(u16),
    Pressure(u8),
    PolyPressure {
        key: u8,
        value: u8,
    },
    Program(u8),
}

/// A message at a tick on a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// In [`TICKS_PER_QUARTER`].
    pub tick: i64,
    pub channel: u8,
    pub msg: Msg,
}

/// One track of a file, with its ticks already at [`TICKS_PER_QUARTER`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmfTrack {
    pub name: Option<String>,
    pub events: Vec<Event>,
}

/// A file, read.
#[derive(Clone, Debug, PartialEq)]
pub struct SmfFile {
    /// Tempo changes: tick and quarter notes per minute.
    pub tempos: Vec<(i64, f64)>,
    /// Time signature changes: tick, numerator, denominator.
    pub signatures: Vec<(i64, u8, u8)>,
    /// Tracks that hold channel messages. A format 0 file is split by channel.
    pub tracks: Vec<SmfTrack>,
    /// Events that were not carried (sysex, text, and so on).
    pub dropped: usize,
}

fn rescale(tick: u64, per_quarter: u64) -> i64 {
    ((tick * TICKS_PER_QUARTER as u64 + per_quarter / 2) / per_quarter) as i64
}

/// Reads a Standard MIDI File.
pub fn read_smf(bytes: &[u8]) -> Result<SmfFile, SmfError> {
    let smf = Smf::parse(bytes).map_err(|e| SmfError(format!("not a MIDI file ({e})")))?;
    let per_quarter = match smf.header.timing {
        Timing::Metrical(t) => u64::from(t.as_int()).max(1),
        Timing::Timecode(..) => {
            return Err(SmfError(
                "this file counts in SMPTE time, which isn't supported".into(),
            ));
        }
    };
    if smf.header.format == Format::Sequential {
        return Err(SmfError(
            "format 2 MIDI files (separate songs) aren't supported".into(),
        ));
    }
    let mut file = SmfFile {
        tempos: Vec::new(),
        signatures: Vec::new(),
        tracks: Vec::new(),
        dropped: 0,
    };
    for (index, track) in smf.tracks.iter().enumerate() {
        let mut tick = 0u64;
        let mut name: Option<String> = None;
        let mut events = Vec::new();
        for event in track {
            tick += u64::from(event.delta.as_int());
            let at = rescale(tick, per_quarter);
            match event.kind {
                TrackEventKind::Midi { channel, message } => {
                    let msg = match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() == 0 => {
                            Msg::NoteOff { key: key.as_int() }
                        }
                        MidiMessage::NoteOn { key, vel } => Msg::NoteOn {
                            key: key.as_int(),
                            velocity: vel.as_int(),
                        },
                        MidiMessage::NoteOff { key, .. } => Msg::NoteOff { key: key.as_int() },
                        MidiMessage::Controller { controller, value } => Msg::Controller {
                            number: controller.as_int(),
                            value: value.as_int(),
                        },
                        MidiMessage::PitchBend { bend } => Msg::PitchBend(bend.0.as_int()),
                        MidiMessage::ChannelAftertouch { vel } => Msg::Pressure(vel.as_int()),
                        MidiMessage::Aftertouch { key, vel } => Msg::PolyPressure {
                            key: key.as_int(),
                            value: vel.as_int(),
                        },
                        MidiMessage::ProgramChange { program } => Msg::Program(program.as_int()),
                    };
                    events.push(Event {
                        tick: at,
                        channel: channel.as_int(),
                        msg,
                    });
                }
                TrackEventKind::Meta(MetaMessage::TrackName(raw)) => {
                    name.get_or_insert_with(|| String::from_utf8_lossy(raw).trim().to_string());
                }
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                    let micros = micros.as_int().max(1);
                    file.tempos.push((at, 60_000_000.0 / f64::from(micros)));
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => {
                    let den = 1u32.checked_shl(u32::from(d)).unwrap_or(0);
                    file.signatures.push((at, n, den.min(255) as u8));
                }
                TrackEventKind::Meta(
                    MetaMessage::EndOfTrack
                    | MetaMessage::MidiChannel(_)
                    | MetaMessage::MidiPort(_),
                ) => {}
                _ => file.dropped += 1,
            }
        }
        if events.is_empty() {
            continue;
        }
        if smf.header.format == Format::SingleTrack {
            // Format 0: one file track, split by channel.
            let mut channels: Vec<u8> = events.iter().map(|e| e.channel).collect();
            channels.sort_unstable();
            channels.dedup();
            for channel in channels {
                file.tracks.push(SmfTrack {
                    name: Some(match &name {
                        Some(n) if channels_len(&events) == 1 => n.clone(),
                        Some(n) => format!("{n} (channel {})", channel + 1),
                        None => format!("Channel {}", channel + 1),
                    }),
                    events: events
                        .iter()
                        .filter(|e| e.channel == channel)
                        .copied()
                        .collect(),
                });
            }
        } else {
            file.tracks.push(SmfTrack {
                name: name.or_else(|| Some(format!("Track {}", index + 1))),
                events,
            });
        }
    }
    file.tempos.sort_by_key(|t| t.0);
    file.signatures.sort_by_key(|s| s.0);
    Ok(file)
}

fn channels_len(events: &[Event]) -> usize {
    let mut c: Vec<u8> = events.iter().map(|e| e.channel).collect();
    c.sort_unstable();
    c.dedup();
    c.len()
}

/// Writes a format 1 file at [`TICKS_PER_QUARTER`]: a conductor track with the
/// tempos and signatures, then each of `file.tracks`.
pub fn write_smf(file: &SmfFile) -> Vec<u8> {
    let header = Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(TICKS_PER_QUARTER as u16)),
    );
    let mut smf = Smf::new(header);

    let mut conductor: Vec<(i64, u8, TrackEventKind)> = Vec::new();
    for &(tick, bpm) in &file.tempos {
        let micros = (60_000_000.0 / bpm).round().clamp(1.0, 16_777_215.0) as u32;
        conductor.push((
            tick,
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros))),
        ));
    }
    for &(tick, n, d) in &file.signatures {
        let pow = d.max(1).ilog2() as u8;
        conductor.push((
            tick,
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(n, pow, 24, 8)),
        ));
    }
    smf.tracks.push(to_track(conductor, Some("Tempo")));

    for track in &file.tracks {
        let mut events: Vec<(i64, u8, TrackEventKind)> = Vec::new();
        for e in &track.events {
            let channel = u4::new(e.channel.min(15));
            let message = match e.msg {
                Msg::NoteOn { key, velocity } => MidiMessage::NoteOn {
                    key: u7::new(key.min(127)),
                    vel: u7::new(velocity.clamp(1, 127)),
                },
                Msg::NoteOff { key } => MidiMessage::NoteOff {
                    key: u7::new(key.min(127)),
                    vel: u7::new(0),
                },
                Msg::Controller { number, value } => MidiMessage::Controller {
                    controller: u7::new(number.min(127)),
                    value: u7::new(value.min(127)),
                },
                Msg::PitchBend(v) => MidiMessage::PitchBend {
                    bend: midly::PitchBend(u14::new(v.min(16383))),
                },
                Msg::Pressure(v) => MidiMessage::ChannelAftertouch {
                    vel: u7::new(v.min(127)),
                },
                Msg::PolyPressure { key, value } => MidiMessage::Aftertouch {
                    key: u7::new(key.min(127)),
                    vel: u7::new(value.min(127)),
                },
                Msg::Program(p) => MidiMessage::ProgramChange {
                    program: u7::new(p.min(127)),
                },
            };
            // Note offs sort before note ons at one tick, so a repeated key
            // doesn't glue its notes together.
            let order = match e.msg {
                Msg::NoteOff { .. } => 0,
                Msg::NoteOn { .. } => 2,
                _ => 1,
            };
            events.push((e.tick, order, TrackEventKind::Midi { channel, message }));
        }
        smf.tracks.push(to_track(events, track.name.as_deref()));
    }
    let mut out = Vec::new();
    smf.write_std(&mut out).expect("writing to a Vec");
    out
}

fn to_track<'a>(
    mut events: Vec<(i64, u8, TrackEventKind<'a>)>,
    name: Option<&'a str>,
) -> Vec<TrackEvent<'a>> {
    events.sort_by_key(|e| (e.0, e.1));
    let mut out = Vec::with_capacity(events.len() + 2);
    if let Some(name) = name {
        out.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(name.as_bytes())),
        });
    }
    let mut last = 0i64;
    for (tick, _, kind) in events {
        let tick = tick.max(0);
        out.push(TrackEvent {
            delta: u28::new((tick - last).min(0x0FFF_FFFF) as u32),
            kind,
        });
        last = tick;
    }
    out.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    out
}

// ---- Mapping --------------------------------------------------------------

/// A file track as a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTrack {
    pub name: String,
    pub clip: MidiClip,
}

/// What a file becomes.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub tracks: Vec<ImportedTrack>,
    /// The file's own tempo and meter, when it has either.
    pub tempo_map: Option<TempoMap>,
    /// What could not be carried over, for the notice.
    pub notes: Vec<String>,
}

fn bend_to_value(raw: u16) -> f32 {
    let centred = i32::from(raw) - 8192;
    if centred >= 0 {
        centred as f32 / 8191.0
    } else {
        centred as f32 / 8192.0
    }
}

fn value_to_bend(value: f32) -> u16 {
    let v = value.clamp(-1.0, 1.0);
    let centred = if v >= 0.0 { v * 8191.0 } else { v * 8192.0 };
    (8192 + centred.round() as i32).clamp(0, 16383) as u16
}

fn to_7bit(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// The file's tempo and meter as a map, with meter changes that fall inside
/// a bar moved to the next bar line (the project puts signatures on bars).
fn tempo_map(file: &SmfFile, notes: &mut Vec<String>) -> Option<TempoMap> {
    if file.tempos.is_empty() && file.signatures.is_empty() {
        return None;
    }
    let mut tempos: Vec<TempoChange> = Vec::new();
    for &(tick, bpm) in &file.tempos {
        let bpm = bpm.clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM);
        match tempos.last_mut() {
            Some(last) if last.tick.0 == tick => last.bpm = bpm,
            _ => tempos.push(TempoChange {
                tick: Tick(tick),
                bpm,
            }),
        }
    }
    match tempos.first() {
        None => tempos.push(TempoChange {
            tick: Tick::ZERO,
            bpm: 120.0,
        }),
        Some(first) if first.tick != Tick::ZERO => tempos.insert(
            0,
            TempoChange {
                tick: Tick::ZERO,
                bpm: 120.0,
            },
        ),
        _ => {}
    }
    let mut signatures = vec![SignatureChange {
        bar: 0,
        signature: TimeSignature::COMMON,
    }];
    // Where the current signature began: its bar and tick.
    let (mut region_bar, mut region_tick) = (0u32, 0i64);
    let mut snapped = false;
    for &(tick, num, den) in &file.signatures {
        let signature = TimeSignature {
            numerator: num,
            denominator: den,
        };
        if !signature.is_valid() {
            continue;
        }
        let per_bar = signatures.last().expect("one").signature.ticks_per_bar();
        let into = tick - region_tick;
        let bars = (into + per_bar - 1) / per_bar;
        if into % per_bar != 0 {
            snapped = true;
        }
        let bar = region_bar + bars as u32;
        if bar == region_bar {
            signatures.last_mut().expect("one").signature = signature;
        } else {
            signatures.push(SignatureChange { bar, signature });
            region_bar = bar;
            region_tick += bars * per_bar;
        }
    }
    if snapped {
        notes.push("meter changes inside a bar moved to the next bar".into());
    }
    TempoMap::new(tempos, signatures).ok()
}

/// Turns a file into clips. Each file track becomes one clip starting at its
/// own tick 0, long enough (a whole number of bars of the file's meter) for
/// its last note.
pub fn import(file: &SmfFile) -> Imported {
    let mut notes_out = Vec::new();
    let map = tempo_map(file, &mut notes_out);
    let bars = map.clone().unwrap_or_default();
    let mut tracks = Vec::new();
    let mut lost_poly = 0usize;
    let mut stuck = 0usize;
    for track in &file.tracks {
        let mut events = track.events.clone();
        events.sort_by_key(|e| e.tick);
        let mut notes: Vec<MidiNote> = Vec::new();
        // Notes sounding, by channel and key.
        let mut open: Vec<(u8, u8, usize)> = Vec::new();
        let mut controls: Vec<ControlPoint> = Vec::new();
        let mut poly: Vec<(i64, u8, f32)> = Vec::new();
        let mut end = 0i64;
        for e in &events {
            end = end.max(e.tick);
            match e.msg {
                Msg::NoteOn { key, velocity } => {
                    let mut note = MidiNote::new(Tick(e.tick), Tick(0), key);
                    note.velocity = f32::from(velocity) / 127.0;
                    open.push((e.channel, key, notes.len()));
                    notes.push(note);
                }
                Msg::NoteOff { key } => {
                    if let Some(i) = open
                        .iter()
                        .position(|&(c, k, _)| c == e.channel && k == key)
                    {
                        let (_, _, n) = open.remove(i);
                        notes[n].length = Tick((e.tick - notes[n].start.0).max(1));
                    }
                }
                Msg::Controller { number, value } => controls.push(ControlPoint::new(
                    Tick(e.tick),
                    e.channel,
                    ControlKind::Controller(number),
                    f32::from(value) / 127.0,
                )),
                Msg::PitchBend(raw) => controls.push(ControlPoint::new(
                    Tick(e.tick),
                    e.channel,
                    ControlKind::PitchBend,
                    bend_to_value(raw),
                )),
                Msg::Pressure(v) => controls.push(ControlPoint::new(
                    Tick(e.tick),
                    e.channel,
                    ControlKind::ChannelPressure,
                    f32::from(v) / 127.0,
                )),
                Msg::Program(p) => controls.push(ControlPoint::new(
                    Tick(e.tick),
                    e.channel,
                    ControlKind::ProgramChange,
                    f32::from(p) / 127.0,
                )),
                Msg::PolyPressure { key, value } => {
                    poly.push((e.tick, key, f32::from(value) / 127.0))
                }
            }
        }
        // A stuck note ends at the track's end.
        for (_, _, n) in open {
            stuck += 1;
            notes[n].length = Tick((end - notes[n].start.0).max(1));
        }
        for (tick, key, value) in poly {
            let sounding = notes
                .iter_mut()
                .find(|n| n.key == key && n.start.0 <= tick && tick < n.end().0);
            match sounding {
                Some(note) => note.expressions.push(NoteExpression {
                    tick: Tick(tick - note.start.0),
                    kind: ExpressionKind::Pressure,
                    value,
                    ramp: false,
                }),
                None => lost_poly += 1,
            }
        }
        let last = notes
            .iter()
            .map(|n| n.end().0)
            .chain(controls.iter().map(|c| c.tick.0))
            .max()
            .unwrap_or(0);
        let mut clip = MidiClip::new(Tick(whole_bars(&bars, last)));
        clip.notes = notes;
        clip.controls = controls;
        tracks.push(ImportedTrack {
            name: track.name.clone().unwrap_or_default(),
            clip,
        });
    }
    if file.dropped > 0 {
        notes_out.push(format!(
            "{} sysex or text events were left out",
            file.dropped
        ));
    }
    if lost_poly > 0 {
        notes_out.push(format!(
            "{lost_poly} poly pressure values had no note to go on"
        ));
    }
    if stuck > 0 {
        notes_out.push(format!("{stuck} notes never ended and were cut at the end"));
    }
    Imported {
        tracks,
        tempo_map: map,
        notes: notes_out,
    }
}

/// `tick` rounded up to a whole bar of `map`, and at least one bar.
fn whole_bars(map: &TempoMap, tick: i64) -> i64 {
    let bar = map.position(Tick(tick)).bar;
    let start = map.bar_start(bar);
    let bar = if start.0 >= tick && tick > 0 {
        bar
    } else {
        bar + 1
    };
    map.bar_start(bar.max(1)).0
}

/// One track of an export: its clips, with where each starts.
#[derive(Clone, Debug)]
pub struct ExportTrack {
    pub name: String,
    pub clips: Vec<(Tick, MidiClip)>,
}

/// Ramps are written as a point every this many ticks.
const RAMP_STEP: i64 = TICKS_PER_QUARTER / 16;

/// Builds the file for `tracks`, and what it could not carry. Looping clips
/// are unrolled as the engine plays them, and humanise is baked in.
pub fn export(tracks: &[ExportTrack], map: &TempoMap) -> (SmfFile, Vec<String>) {
    let mut file = SmfFile {
        tempos: map.tempos().iter().map(|t| (t.tick.0, t.bpm)).collect(),
        signatures: map
            .signatures()
            .iter()
            .map(|s| {
                (
                    map.bar_start(s.bar).0,
                    s.signature.numerator,
                    s.signature.denominator,
                )
            })
            .collect(),
        tracks: Vec::new(),
        dropped: 0,
    };
    let (mut expressions, mut articulations) = (0usize, 0usize);
    for track in tracks {
        let mut events = Vec::new();
        for (start, clip) in &track.clips {
            let base = start.0;
            for note in clip.played() {
                let at = base + note.start.0 + clip.humanise_shift(&note);
                let velocity = to_7bit(clip.humanise_velocity_of(&note)).max(1);
                events.push(Event {
                    tick: at.max(0),
                    channel: 0,
                    msg: Msg::NoteOn {
                        key: note.key,
                        velocity,
                    },
                });
                events.push(Event {
                    tick: (at + note.length.0.max(1)).max(1),
                    channel: 0,
                    msg: Msg::NoteOff { key: note.key },
                });
                if note.articulation.is_some() {
                    articulations += 1;
                }
                for x in &note.expressions {
                    if x.kind == ExpressionKind::Pressure {
                        events.push(Event {
                            tick: at + x.tick.0,
                            channel: 0,
                            msg: Msg::PolyPressure {
                                key: note.key,
                                value: to_7bit(x.value),
                            },
                        });
                    } else {
                        expressions += 1;
                    }
                }
            }
            let points = clip.played_controls();
            for (i, p) in points.iter().enumerate() {
                let mut push = |tick: i64, value: f32| {
                    let msg = match p.kind {
                        ControlKind::Controller(number) => Msg::Controller {
                            number,
                            value: to_7bit(value),
                        },
                        ControlKind::PitchBend => Msg::PitchBend(value_to_bend(value)),
                        ControlKind::ChannelPressure => Msg::Pressure(to_7bit(value)),
                        ControlKind::ProgramChange => Msg::Program(to_7bit(value)),
                    };
                    events.push(Event {
                        tick: base + tick,
                        channel: p.channel,
                        msg,
                    });
                };
                push(p.tick.0, p.value);
                if p.ramp
                    && p.kind != ControlKind::ProgramChange
                    && let Some(next) = points[i + 1..]
                        .iter()
                        .find(|q| q.channel == p.channel && q.kind == p.kind)
                {
                    let span = next.tick.0 - p.tick.0;
                    let mut at = RAMP_STEP;
                    while at < span {
                        let along = at as f32 / span as f32;
                        push(p.tick.0 + at, p.value + (next.value - p.value) * along);
                        at += RAMP_STEP;
                    }
                }
            }
        }
        if !events.is_empty() {
            file.tracks.push(SmfTrack {
                name: Some(track.name.clone()),
                events,
            });
        }
    }
    let mut notes = Vec::new();
    if expressions > 0 {
        notes.push(format!(
            "{expressions} per-note pitch, timbre, volume or pan values were left out"
        ));
    }
    if articulations > 0 {
        notes.push(format!("{articulations} note articulations were left out"));
    }
    (file, notes)
}

/// The MIDI clips of `project`, one [`ExportTrack`] per track input that has
/// any, in the order of the tracks, named as the timeline names them. `keep`
/// picks the clips that go.
pub fn project_tracks(project: &Project, keep: impl Fn(ClipId, &Clip) -> bool) -> Vec<ExportTrack> {
    let mut nodes: Vec<NodeId> = project
        .graph()
        .nodes()
        .filter(|(_, n)| n.type_id == TRACK_INPUT)
        .map(|(id, _)| id)
        .collect();
    let group_of = |id: NodeId| {
        project
            .graph()
            .node(id)
            .and_then(|n| n.parent)
            .unwrap_or(id)
    };
    nodes.sort_by_key(|&id| {
        let rank = project
            .track_order()
            .iter()
            .position(|&g| g == group_of(id));
        (rank.unwrap_or(usize::MAX), id)
    });
    let mut out = Vec::new();
    for (index, &node) in nodes.iter().enumerate() {
        let mut clips: Vec<(Tick, MidiClip)> = project
            .timeline_clips()
            .filter(|(id, clip)| clip.node == node && keep(*id, clip))
            .filter_map(|(_, clip)| match &clip.content {
                ClipContent::Midi(midi) => Some((clip.start, midi.clone())),
                ClipContent::Audio(_) => None,
            })
            .collect();
        if clips.is_empty() {
            continue;
        }
        clips.sort_by_key(|c| c.0);
        let name = match project
            .graph()
            .node(group_of(node))
            .and_then(|n| n.config.get(PORT_NAME))
        {
            Some(Value::Text(name)) if !name.is_empty() => name.clone(),
            _ => format!("Track {}", index + 1),
        };
        out.push(ExportTrack { name, clips });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_with_everything() -> MidiClip {
        let mut clip = MidiClip::new(Tick(TICKS_PER_QUARTER * 4));
        let mut a = MidiNote::new(Tick(0), Tick(480), 60);
        a.velocity = 100.0 / 127.0;
        a.expressions.push(NoteExpression {
            tick: Tick(120),
            kind: ExpressionKind::Pressure,
            value: 64.0 / 127.0,
            ramp: false,
        });
        let mut b = MidiNote::new(Tick(960), Tick(960), 64);
        b.velocity = 50.0 / 127.0;
        clip.notes = vec![a, b];
        clip.controls = vec![
            ControlPoint::new(Tick(0), 0, ControlKind::Controller(1), 32.0 / 127.0),
            ControlPoint::new(Tick(240), 0, ControlKind::PitchBend, -0.5),
            ControlPoint::new(Tick(480), 0, ControlKind::ChannelPressure, 1.0),
            ControlPoint::new(Tick(960), 0, ControlKind::ProgramChange, 5.0 / 127.0),
        ];
        clip
    }

    fn round_trip(clip: &MidiClip) -> MidiClip {
        let track = ExportTrack {
            name: "Keys".into(),
            clips: vec![(Tick::ZERO, clip.clone())],
        };
        let (file, _) = export(&[track], &TempoMap::default());
        let bytes = write_smf(&file);
        let read = read_smf(&bytes).expect("reads");
        assert_eq!(read.tracks[0].name.as_deref(), Some("Keys"));
        import(&read).tracks.remove(0).clip
    }

    #[test]
    fn a_clip_survives_export_and_import() {
        let clip = clip_with_everything();
        let back = round_trip(&clip);
        assert_eq!(back.notes.len(), 2);
        for (a, b) in clip.notes.iter().zip(&back.notes) {
            assert_eq!((a.start, a.length, a.key), (b.start, b.length, b.key));
            assert!((a.velocity - b.velocity).abs() < 1e-6);
            assert_eq!(a.expressions, b.expressions);
        }
        assert_eq!(back.controls.len(), clip.controls.len());
        for (a, b) in clip.controls.iter().zip(&back.controls) {
            assert_eq!((a.tick, a.channel, a.kind), (b.tick, b.channel, b.kind));
            assert!((a.value - b.value).abs() < 1e-3, "{a:?} {b:?}");
        }
        assert_eq!(back.length, clip.length);
    }

    #[test]
    fn ticks_are_rescaled_with_rounding() {
        assert_eq!(rescale(480, 480), 960);
        assert_eq!(rescale(96, 96), 960);
        assert_eq!(rescale(1, 480), 2);
        assert_eq!(rescale(1, 7), 137);
    }

    fn raw(format: Format, tpq: u16, tracks: Vec<Vec<TrackEvent<'static>>>) -> Vec<u8> {
        let mut smf = Smf::new(Header::new(format, Timing::Metrical(u15::new(tpq))));
        smf.tracks = tracks;
        let mut out = Vec::new();
        smf.write_std(&mut out).unwrap();
        out
    }

    fn ev(delta: u32, channel: u8, message: MidiMessage) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: u4::new(channel),
                message,
            },
        }
    }

    fn on(key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            key: u7::new(key),
            vel: u7::new(90),
        }
    }

    fn off(key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            key: u7::new(key),
            vel: u7::new(0),
        }
    }

    fn end() -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        }
    }

    #[test]
    fn a_format_0_file_splits_by_channel_and_velocity_0_ends_a_note() {
        let bytes = raw(
            Format::SingleTrack,
            480,
            vec![vec![
                ev(0, 0, on(60)),
                ev(0, 9, on(36)),
                ev(240, 0, off(60)),
                ev(240, 9, off(36)),
                end(),
            ]],
        );
        let file = read_smf(&bytes).unwrap();
        assert_eq!(file.tracks.len(), 2);
        let imported = import(&file);
        let melody = &imported.tracks[0].clip;
        assert_eq!(melody.notes[0].length, Tick(480));
        assert_eq!(imported.tracks[1].clip.notes[0].key, 36);
        assert_eq!(melody.length.0, TICKS_PER_QUARTER * 4);
    }

    #[test]
    fn a_stuck_note_ends_at_the_track_end() {
        let bytes = raw(
            Format::Parallel,
            960,
            vec![vec![ev(0, 0, on(60)), ev(960, 0, on(62)), end()]],
        );
        let imported = import(&read_smf(&bytes).unwrap());
        let notes = &imported.tracks[0].clip.notes;
        assert_eq!(notes[0].length, Tick(960));
        assert!(imported.notes.iter().any(|n| n.contains("never ended")));
    }

    #[test]
    fn smpte_and_format_2_are_refused() {
        let mut smf = Smf::new(Header::new(
            Format::Sequential,
            Timing::Metrical(u15::new(480)),
        ));
        smf.tracks = vec![vec![end()]];
        let mut out = Vec::new();
        smf.write_std(&mut out).unwrap();
        assert!(read_smf(&out).is_err());
        assert!(read_smf(b"nonsense").is_err());
    }

    #[test]
    fn the_tempo_map_comes_through() {
        let map = TempoMap::new(
            vec![
                TempoChange {
                    tick: Tick::ZERO,
                    bpm: 100.0,
                },
                TempoChange {
                    tick: Tick(3840),
                    bpm: 150.0,
                },
            ],
            vec![
                SignatureChange {
                    bar: 0,
                    signature: TimeSignature::COMMON,
                },
                SignatureChange {
                    bar: 2,
                    signature: TimeSignature {
                        numerator: 3,
                        denominator: 4,
                    },
                },
            ],
        )
        .unwrap();
        let mut clip = MidiClip::new(Tick(3840));
        clip.notes.push(MidiNote::new(Tick(0), Tick(100), 60));
        let (file, _) = export(
            &[ExportTrack {
                name: "t".into(),
                clips: vec![(Tick::ZERO, clip)],
            }],
            &map,
        );
        let back = import(&read_smf(&write_smf(&file)).unwrap());
        let got = back.tempo_map.unwrap();
        assert_eq!(got.tempos().len(), 2);
        assert!((got.tempos()[1].bpm - 150.0).abs() < 0.01);
        assert_eq!(got.signatures(), map.signatures());
    }

    #[test]
    fn a_meter_change_inside_a_bar_moves_to_the_next_bar() {
        let file = SmfFile {
            tempos: vec![],
            signatures: vec![(TICKS_PER_QUARTER * 5, 3, 4)],
            tracks: vec![],
            dropped: 0,
        };
        let mut notes = Vec::new();
        let map = tempo_map(&file, &mut notes).unwrap();
        assert_eq!(map.signatures()[1].bar, 2);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn poly_pressure_with_no_note_is_counted() {
        let bytes = raw(
            Format::Parallel,
            960,
            vec![vec![
                ev(
                    0,
                    0,
                    MidiMessage::Aftertouch {
                        key: u7::new(60),
                        vel: u7::new(10),
                    },
                ),
                ev(0, 0, on(61)),
                ev(10, 0, off(61)),
                end(),
            ]],
        );
        let imported = import(&read_smf(&bytes).unwrap());
        assert!(imported.notes.iter().any(|n| n.contains("poly pressure")));
    }
}
