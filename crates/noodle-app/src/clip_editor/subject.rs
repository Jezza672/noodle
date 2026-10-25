//! What the editor edits: an audio clip on the timeline, or the sample of a
//! Sampler. They are the same kind of thing (a stretch of a file, with a gain
//! envelope, an optional warp and a loop), so the editor, its panel and the
//! commands that change them are written once over a [`Subject`].
//!
//! A clip keeps these in the project (its section, its warp). A Sampler keeps
//! the scalar ones as parameters (`start`, `end` and the loop, as fractions
//! of the sample, so they can be modulated) and the rest as config text: the
//! `warp` (a [`ClipWarp`]) and the `gain_env` (gain points counting millionths
//! of the sample). [`Subject::read`] and the edits below turn between the two
//! shapes, so a drag that moves the lit part, the loop or a marker is one
//! command either way.
//!
//! The Sampler's loop is a sustain loop in the sample's own seconds, kept in
//! the parameters. In a draft it appears as the warp's `loop_region` (in the
//! warp's beats), when it is at least [`MIN_LOOP_BEATS`] long, so the warped
//! editor draws and drags it like a clip's; the stored warp never holds one.

use noodle_core::{
    AudioClip, Clip, ClipContent, ClipId, ClipWarp, Command, GainPoint, LoopRegion, MIN_LOOP_BEATS,
    Node, NodeId, Project, TempoCurve, Tick, Value, gain_points_from_text, gain_points_to_text,
};
use noodle_nodes::{GAIN_ENV_KEY, GAIN_ENV_SCALE, SAMPLE_KEY, SAMPLER_ID, WARP_KEY};

use crate::clip_panel::Span;

/// The tempo a clip gets when a loop is made on a clip with no warp: fast, so
/// a beat is a quarter second and the loop can be as short as the engine
/// plays one.
pub(crate) const LOOP_TEMPO: f64 = 240.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Subject {
    Clip(ClipId),
    Sampler(NodeId),
}

impl From<ClipId> for Subject {
    fn from(id: ClipId) -> Self {
        Self::Clip(id)
    }
}

/// What the editor shows of a [`Subject`].
#[derive(Clone, Debug)]
pub(crate) struct Section {
    /// The part of the file that plays, and the gain envelope. A Sampler's is
    /// made from its parameters; its `source` is the sample's hash.
    pub audio: AudioClip,
    /// The warp, if there is one. For a Sampler, the loop is in it (see the
    /// module's note).
    pub warp: Option<ClipWarp>,
    /// The loop, in seconds of the file, if it is on.
    pub looping: Option<(f64, f64)>,
    /// The clip's place on the timeline; zero for a Sampler.
    pub start: Tick,
}

impl Section {
    /// Whether the editor shows this as a plain stretch of file, in seconds,
    /// rather than on beats.
    pub fn is_plain(&self) -> bool {
        self.warp.as_ref().is_none_or(ClipWarp::is_natural)
    }
}

/// The Sampler's parameter, as a fraction.
fn param(node: &Node, key: &str, default: f32) -> f32 {
    node.params.get(key).copied().unwrap_or(default)
}

fn config_text(node: &Node, key: &str) -> String {
    match node.config.get(key) {
        Some(Value::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

fn sampler(project: &Project, id: NodeId) -> Option<&Node> {
    project
        .graph()
        .node(id)
        .filter(|node| node.type_id == SAMPLER_ID)
}

/// The Sampler `node`'s section of a file `span` long.
fn sampler_section(node: &Node, span: Span) -> Section {
    let frames = span.frames as f64;
    let rate = f64::from(span.rate.max(1));
    let (start, end) = (
        f64::from(param(node, "start", 0.0)),
        f64::from(param(node, "end", 1.0)),
    );
    let offset = (start * frames).round() as u64;
    let length = ((end * frames).round() as u64).saturating_sub(offset);
    let mut audio = AudioClip::new(config_text(node, SAMPLE_KEY), length);
    audio.offset = offset;
    audio.gain_points = gain_points_from_text(&config_text(node, GAIN_ENV_KEY));
    let total = frames / rate;
    let looping = (param(node, "loop", 0.0) >= 0.5).then(|| {
        (
            f64::from(param(node, "loop_start", 0.0)) * total,
            f64::from(param(node, "loop_end", 1.0)) * total,
        )
    });
    let warp = ClipWarp::from_text(&config_text(node, WARP_KEY)).map(|mut warp| {
        warp.loop_region = looping
            .map(|(a, b)| LoopRegion {
                start: warp.tempo.beats_at(a),
                end: warp.tempo.beats_at(b),
            })
            .filter(|region| region.beats() >= MIN_LOOP_BEATS);
        warp
    });
    Section {
        audio,
        warp,
        looping,
        start: Tick::ZERO,
    }
}

fn set_param(node: NodeId, key: &str, value: f32) -> Command {
    Command::SetParam {
        node,
        key: key.to_owned(),
        value: Some(value),
    }
}

fn set_text(node: NodeId, key: &str, text: String) -> Command {
    Command::SetConfig {
        node,
        key: key.to_owned(),
        value: (!text.is_empty()).then_some(Value::Text(text)),
    }
}

fn batch(mut commands: Vec<Command>) -> Option<Command> {
    match commands.len() {
        0 => None,
        1 => commands.pop(),
        _ => Some(Command::Batch(commands)),
    }
}

impl Subject {
    /// What the subject is now, for a file of `span`. `None` if it has gone
    /// (or a clip is not audio).
    pub fn read(self, project: &Project, span: Span) -> Option<Section> {
        match self {
            Self::Clip(id) => {
                let clip = project.clip(id)?;
                let audio = clip.as_audio()?.clone();
                let warp = project.warp_of(id).map(|(_, warp)| warp.clone());
                let looping = warp.as_ref().and_then(ClipWarp::loop_seconds);
                Some(Section {
                    audio,
                    warp,
                    looping,
                    start: clip.start,
                })
            }
            Self::Sampler(id) => Some(sampler_section(sampler(project, id)?, span)),
        }
    }

    /// The stored warp, without a Sampler's loop.
    pub fn warp(self, project: &Project) -> Option<ClipWarp> {
        match self {
            Self::Clip(id) => project.warp_of(id).map(|(_, warp)| warp.clone()),
            Self::Sampler(id) => ClipWarp::from_text(&config_text(sampler(project, id)?, WARP_KEY)),
        }
    }

    /// The command that applies `change` to the warp, making one that plays
    /// at the clip's natural speed (at the project's tempo at the start, so
    /// its beats are the timeline's) if there is none. A clip's warp may be
    /// shared with other sections, which fork it (see [`Project::warp_edit`]).
    /// A Sampler's loop is not reached from here, only the warp.
    pub fn warp_edit(
        self,
        project: &Project,
        change: impl FnOnce(&mut ClipWarp),
    ) -> Option<Command> {
        match self {
            Self::Clip(id) => project.warp_edit(id, change).ok(),
            Self::Sampler(id) => edit_sampler(project, id, None, |warp, _| change(warp)),
        }
    }

    /// Like [`warp_edit`](Self::warp_edit), for a change that also moves the
    /// part of the file that plays, or (for a Sampler) the loop.
    pub fn section_edit(
        self,
        project: &Project,
        span: Span,
        change: impl FnOnce(&mut ClipWarp, &mut AudioClip),
    ) -> Option<Command> {
        match self {
            Self::Clip(id) => project.section_edit(id, change).ok(),
            Self::Sampler(id) => edit_sampler(project, id, Some(span), change),
        }
    }

    /// The command that gives the subject the part of the file in `audio`
    /// (its offset and length), leaving its warp alone. For a clip, the whole
    /// audio replaces the clip's, so fades and the rest go with it.
    pub fn set_audio(self, project: &Project, span: Span, audio: AudioClip) -> Option<Command> {
        match self {
            Self::Clip(id) => {
                let clip = project.clip(id)?;
                Some(Command::SetClip {
                    id,
                    clip: Clip {
                        content: ClipContent::Audio(audio),
                        ..clip.clone()
                    },
                })
            }
            Self::Sampler(id) => {
                let node = sampler(project, id)?;
                let current = sampler_section(node, span).audio;
                batch(sampler_audio_commands(id, &current, &audio, span))
            }
        }
    }

    /// The command that gives the subject the part of the file in `audio`
    /// and, if `looping` is given, moves the loop to those seconds of the
    /// file in the same step: the lit part slid or trimmed with its loop
    /// along.
    pub fn slide(
        self,
        project: &Project,
        span: Span,
        audio: AudioClip,
        looping: Option<(f64, f64)>,
    ) -> Option<Command> {
        let Some(seconds) = looping else {
            return self.set_audio(project, span, audio);
        };
        match self {
            Self::Clip(_) => self.section_edit(project, span, |warp, section| {
                *section = audio;
                warp.loop_region = Some(LoopRegion {
                    start: warp.tempo.beats_at(seconds.0),
                    end: warp.tempo.beats_at(seconds.1),
                })
                .filter(|region| region.beats() >= MIN_LOOP_BEATS)
                .or(warp.loop_region);
            }),
            Self::Sampler(_) => batch(
                [
                    self.set_audio(project, span, audio),
                    self.set_loop(project, span, Some(seconds)),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ),
        }
    }

    /// The command that sets the gain envelope.
    pub fn set_gain_points(self, project: &Project, points: Vec<GainPoint>) -> Option<Command> {
        match self {
            Self::Clip(id) => {
                let clip = project.clip(id)?;
                let mut audio = clip.as_audio()?.clone();
                audio.gain_points = points;
                Some(Command::SetClip {
                    id,
                    clip: Clip {
                        content: ClipContent::Audio(audio),
                        ..clip.clone()
                    },
                })
            }
            Self::Sampler(id) => Some(set_text(id, GAIN_ENV_KEY, gain_points_to_text(&points))),
        }
    }

    /// The command that turns the loop on, over `seconds` of the file, or off.
    /// A clip with no warp gets one that plays it as it is.
    pub fn set_loop(
        self,
        project: &Project,
        span: Span,
        seconds: Option<(f64, f64)>,
    ) -> Option<Command> {
        match self {
            Self::Clip(id) => {
                let fresh = project.warp_of(id).is_none();
                if fresh && seconds.is_none() {
                    return None;
                }
                project
                    .warp_edit(id, |warp| {
                        if fresh && seconds.is_some() {
                            warp.tempo = TempoCurve::constant(LOOP_TEMPO);
                        }
                        warp.loop_region = seconds
                            .map(|(a, b)| LoopRegion {
                                start: warp.tempo.beats_at(a),
                                end: warp.tempo.beats_at(b),
                            })
                            .filter(|region| region.beats() >= MIN_LOOP_BEATS);
                    })
                    .ok()
            }
            Self::Sampler(id) => {
                let node = sampler(project, id)?;
                let total = (span.frames as f64 / f64::from(span.rate.max(1))).max(1e-9);
                let mut commands = vec![set_param(
                    id,
                    "loop",
                    f32::from(u8::from(seconds.is_some())),
                )];
                if let Some((a, b)) = seconds {
                    let (a, b) = ((a / total) as f32, (b / total) as f32);
                    if param(node, "loop_start", 0.0) != a {
                        commands.push(set_param(id, "loop_start", a));
                    }
                    if param(node, "loop_end", 1.0) != b {
                        commands.push(set_param(id, "loop_end", b));
                    }
                }
                batch(commands)
            }
        }
    }
}

/// The commands that move a Sampler's `start` and `end` from the part of the
/// file in `now` to the one in `new`.
fn sampler_audio_commands(
    id: NodeId,
    now: &AudioClip,
    new: &AudioClip,
    span: Span,
) -> Vec<Command> {
    if (now.offset, now.length) == (new.offset, new.length) {
        return Vec::new();
    }
    let frames = span.frames.max(1) as f64;
    let end = (new.offset + new.length).min(span.frames);
    vec![
        set_param(id, "start", (new.offset as f64 / frames) as f32),
        set_param(id, "end", (end as f64 / frames) as f32),
    ]
}

/// The Sampler `id` edited as a clip is: `change` is given the warp (a
/// natural-speed one if there is none) and, when `span` is known, the part
/// of the file that plays. What it leaves different is written back.
fn edit_sampler(
    project: &Project,
    id: NodeId,
    span: Option<Span>,
    change: impl FnOnce(&mut ClipWarp, &mut AudioClip),
) -> Option<Command> {
    let node = sampler(project, id)?;
    let known = span.unwrap_or(Span {
        rate: 48_000,
        frames: 1,
    });
    let section = sampler_section(node, known);
    let default = ClipWarp {
        tempo_conversion: false,
        ..ClipWarp::at_tempo(project.tempo_map().bpm_at(Tick::ZERO))
    };
    let stored = ClipWarp::from_text(&config_text(node, WARP_KEY));
    let mut warp = section.warp.clone().unwrap_or_else(|| default.clone());
    let mut audio = section.audio.clone();
    // A Sampler's loop only reaches the warp when there is a stored one.
    let overlay = warp.loop_region;
    change(&mut warp, &mut audio);

    let mut commands = Vec::new();
    let loop_region = std::mem::take(&mut warp.loop_region);
    let before = stored.clone().unwrap_or(default);
    if warp != before {
        commands.push(set_text(id, WARP_KEY, warp.to_text()));
    }
    if let Some(span) = span {
        commands.extend(sampler_audio_commands(id, &section.audio, &audio, span));
        if loop_region != overlay {
            let total = span.frames as f64 / f64::from(span.rate.max(1));
            match loop_region {
                Some(region) => {
                    let secs = |beat: f64| warp.tempo.seconds_at(beat) / total.max(1e-9);
                    commands.push(set_param(id, "loop", 1.0));
                    commands.push(set_param(id, "loop_start", secs(region.start) as f32));
                    commands.push(set_param(id, "loop_end", secs(region.end) as f32));
                }
                None => commands.push(set_param(id, "loop", 0.0)),
            }
        }
        if audio.gain_points != section.audio.gain_points {
            commands.push(set_text(
                id,
                GAIN_ENV_KEY,
                gain_points_to_text(&audio.gain_points),
            ));
        }
    }
    batch(commands)
}

/// How many ticks of a Sampler's gain envelope the whole sample is.
pub(crate) const ENVELOPE_LENGTH: Tick = Tick(GAIN_ENV_SCALE as i64);

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{Clip, Node};

    const SPAN: Span = Span {
        rate: 48_000,
        frames: 96_000,
    };

    fn run(project: &mut Project, command: impl FnOnce(&Project) -> Option<Command>) {
        let command = command(project);
        command.expect("a command").apply(project).unwrap();
    }

    fn with_sampler() -> (Project, NodeId) {
        let mut project = Project::new();
        let id = NodeId(1);
        Command::AddNode {
            id,
            node: Node::new(SAMPLER_ID),
        }
        .apply(&mut project)
        .unwrap();
        for (key, value) in [
            ("start", 0.25),
            ("end", 0.75),
            ("loop", 1.0),
            ("loop_start", 0.3),
            ("loop_end", 0.6),
        ] {
            run(&mut project, |_| Some(set_param(id, key, value)));
        }
        (project, id)
    }

    fn param_of(project: &Project, id: NodeId, key: &str) -> f32 {
        project.graph().node(id).unwrap().params[key]
    }

    #[test]
    fn a_sampler_reads_as_a_part_of_its_sample_with_a_loop() {
        let (project, id) = with_sampler();
        let section = Subject::Sampler(id).read(&project, SPAN).unwrap();
        assert_eq!(
            (section.audio.offset, section.audio.length),
            (24_000, 48_000)
        );
        let (a, b) = section.looping.unwrap();
        assert!((a - 0.6).abs() < 1e-6 && (b - 1.2).abs() < 1e-6);
        assert!(section.is_plain(), "no warp yet");
    }

    #[test]
    fn a_section_edit_of_a_sampler_writes_back_only_what_changed() {
        let (mut project, id) = with_sampler();
        let subject = Subject::Sampler(id);
        // Slipping the part moves start and end, and nothing else.
        let command = subject.section_edit(&project, SPAN, |_, audio| audio.offset += 4_800);
        let Some(Command::Batch(commands)) = &command else {
            panic!("{command:?}");
        };
        assert_eq!(commands.len(), 2);
        run(&mut project, |_| command);
        assert!((param_of(&project, id, "start") - 0.30).abs() < 1e-6);
        assert!((param_of(&project, id, "end") - 0.80).abs() < 1e-6);
        assert!((param_of(&project, id, "loop_start") - 0.3).abs() < 1e-6);

        // A gain point goes into the envelope setting.
        let point = GainPoint {
            tick: Tick(250_000),
            db: -6.0,
        };
        run(&mut project, |p| {
            subject.section_edit(p, SPAN, |_, audio| audio.gain_points = vec![point])
        });
        let node = project.graph().node(id).unwrap();
        assert_eq!(
            gain_points_from_text(&config_text(node, GAIN_ENV_KEY)),
            [point]
        );
        // Nothing changed, nothing to do.
        assert!(subject.section_edit(&project, SPAN, |_, _| {}).is_none());
    }

    #[test]
    fn a_sampler_stores_the_warp_but_the_loop_stays_in_its_parameters() {
        let (mut project, id) = with_sampler();
        let subject = Subject::Sampler(id);
        run(&mut project, |_| Some(set_param(id, "loop_end", 0.7)));
        run(&mut project, |p| {
            subject.warp_edit(p, |warp| {
                warp.tempo_conversion = true;
                warp.tempo = TempoCurve::constant(90.0);
            })
        });
        let stored = subject.warp(&project).unwrap();
        assert!(stored.tempo_conversion && stored.loop_region.is_none());
        assert_eq!(param_of(&project, id, "loop"), 1.0);
        // Read back, the loop is in the warp's beats (90 bpm: 1.5 a second).
        let section = subject.read(&project, SPAN).unwrap();
        assert!(!section.is_plain(), "it follows the tempo now");
        let region = section.warp.unwrap().loop_region.unwrap();
        assert!((region.start - 0.9).abs() < 1e-6 && (region.end - 2.1).abs() < 1e-6);

        // Pulling the loop's end in the warped editor moves the parameter.
        run(&mut project, |p| {
            subject.section_edit(p, SPAN, |warp, _| {
                warp.loop_region.as_mut().unwrap().end = 1.5;
            })
        });
        let loop_end = f64::from(param_of(&project, id, "loop_end"));
        assert!(
            (loop_end * 2.0 - 1.0).abs() < 1e-5,
            "1.5 beats at 90 bpm is 1 s of 2"
        );
    }

    #[test]
    fn a_loop_on_an_unwarped_clip_makes_a_fast_natural_warp_and_stays_plain() {
        let mut project = Project::new();
        let node = NodeId(1);
        Command::AddNode {
            id: node,
            node: Node::new(noodle_nodes::TRACK_INPUT_ID),
        }
        .apply(&mut project)
        .unwrap();
        let id = project.next_clip_id();
        Command::AddClip {
            id,
            clip: Clip::audio(node, Tick(0), "a.wav", 96_000),
        }
        .apply(&mut project)
        .unwrap();
        let subject = Subject::Clip(id);
        // Turning it off when there is none does nothing.
        assert!(subject.set_loop(&project, SPAN, None).is_none());
        run(&mut project, |p| {
            subject.set_loop(p, SPAN, Some((0.5, 1.25)))
        });
        let (_, warp) = project.warp_of(id).unwrap();
        assert_eq!(warp.tempo.points()[0].bpm, LOOP_TEMPO);
        assert!(!warp.tempo_conversion && warp.is_natural());
        let (a, b) = warp.loop_seconds().unwrap();
        assert!((a - 0.5).abs() < 1e-9 && (b - 1.25).abs() < 1e-9);
        assert!(subject.read(&project, SPAN).unwrap().is_plain());
    }
}
