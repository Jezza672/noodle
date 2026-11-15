//! What a clip dropped on a Sampler (or a Grains or Wavetable node) or into the
//! graph becomes.

use noodle_core::{ClipId, Command, NodeId, Value};
use noodle_nodes::{GRAINS_ID, SAMPLE_KEY, SAMPLER_ID, SampleRef, TABLE_KEY, WAVETABLE_ID};

use crate::session::Session;

/// The sample a clip plays, imported into the project's samples.
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    pub reference: SampleRef,
    /// The key number of the sample's fundamental, if it has a pitch.
    pub pitch: Option<f32>,
}

/// Imports the file an audio clip plays and works out the window of it the
/// clip plays, as fractions of the file.
pub fn from_clip(session: &Session, clip: ClipId) -> Result<Dropped, String> {
    let project = session.project();
    let audio = project
        .clip(clip)
        .and_then(|c| c.as_audio())
        .ok_or("Only audio clips can be used as a sample.")?;
    let path = audio.file(session.directory().unwrap_or(std::path::Path::new("")));
    let store = session.samples();
    let hash = store
        .import(&path)
        .map_err(|e| format!("Couldn't read {}: {e}", audio.source))?;
    let frames = store.get(&hash).map_or(0, |sample| sample.frames()).max(1) as f64;
    let start = (audio.offset as f64 / frames).clamp(0.0, 1.0) as f32;
    let end = ((audio.offset + audio.length) as f64 / frames).clamp(0.0, 1.0) as f32;
    let reference = SampleRef {
        hash,
        start,
        end: end.max(start),
    };
    let pitch = pitch_of(session, &reference);
    Ok(Dropped { reference, pitch })
}

/// The fundamental of the part of a sample a reference names, from its first
/// moments.
pub fn pitch_of(session: &Session, reference: &SampleRef) -> Option<f32> {
    session.samples().pitch(reference)
}

/// Rounds a root to hundredths of a semitone, so the stored value reads well.
fn root_value(key: f32) -> f32 {
    (key * 100.0).round() / 100.0
}

/// What a node that plays a dropped sample takes it as: the config key that
/// names the sample, and whether the node has a `root` and the Sampler's
/// region (a Sampler), only a `root` (Grains) or neither (a Wavetable).
fn target(session: &Session, node: NodeId) -> (&'static str, bool, bool) {
    let type_id = session
        .project()
        .graph()
        .node(node)
        .map(|n| n.type_id.as_str());
    match type_id {
        Some(WAVETABLE_ID) => (TABLE_KEY, false, false),
        Some(GRAINS_ID) => (SAMPLE_KEY, true, false),
        Some(SAMPLER_ID) | None => (SAMPLE_KEY, true, true),
        Some(_) => (SAMPLE_KEY, false, false),
    }
}

/// The commands that give the Sampler `node` a whole sample: the sample, and
/// the root from its fundamental when it has one. A sample with no pitch
/// leaves the root as it was. One undo step.
pub fn choose(session: &Session, node: NodeId, hash: &str) -> Command {
    let (key, rooted, _) = target(session, node);
    let mut commands = vec![Command::SetConfig {
        node,
        key: key.to_owned(),
        value: Some(Value::Text(hash.to_owned())),
    }];
    if rooted {
        commands.extend(
            pitch_of(session, &SampleRef::whole(hash)).map(|key| Command::SetParam {
                node,
                key: "root".into(),
                value: Some(root_value(key)),
            }),
        );
    }
    Command::Batch(commands)
}

/// The commands that give the Sampler `node` a clip's part of a sample:
/// the sample, `start` and `end` from the clip, and the root. One undo step.
pub fn onto_sampler(session: &Session, node: NodeId, dropped: &Dropped) -> Command {
    let r = &dropped.reference;
    let (key, rooted, regioned) = target(session, node);
    if !regioned {
        // A Grains or Wavetable node takes the clip's part of the file as its
        // sample, the window being part of the reference.
        let mut commands = vec![Command::SetConfig {
            node,
            key: key.to_owned(),
            value: Some(Value::Text(r.to_text())),
        }];
        if rooted {
            commands.extend(dropped.pitch.map(|key| Command::SetParam {
                node,
                key: "root".into(),
                value: Some(root_value(key)),
            }));
        }
        return Command::Batch(commands);
    }
    let mut commands = vec![
        Command::SetConfig {
            node,
            key: SAMPLE_KEY.to_owned(),
            value: Some(Value::Text(r.hash.clone())),
        },
        Command::SetParam {
            node,
            key: "start".into(),
            value: (r.start > 0.0).then_some(r.start),
        },
        Command::SetParam {
            node,
            key: "end".into(),
            value: (r.end < 1.0).then_some(r.end),
        },
    ];
    commands.extend(dropped.pitch.map(|key| Command::SetParam {
        node,
        key: "root".into(),
        value: Some(root_value(key)),
    }));
    Command::Batch(commands)
}
