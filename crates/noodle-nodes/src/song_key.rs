//! The Song Key node.

use noodle_core::{Key, Mode};
use noodle_engine::{
    CacheEnv, Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup, Uncacheable,
};

use crate::scale_quantise::ROOT_NAMES;

/// The timeline's key as signals: where a marker sets a key, wire it into
/// Scale Quantise or Transpose to make a part follow it. It is the source
/// node that Tempo is for the tempo (ADR 0008): nothing follows the key
/// unless a wire shows it.
///
/// - `from` is the key the part was written in (a major or minor key).
/// - `root` is the timeline's tonic, 0 (C) to 11 (B): wire it into Scale
///   Quantise's `root`.
/// - `scale` is 0 for a major key and 1 for a minor one, the indices of
///   Scale Quantise's `Major` and `Minor`: wire it into its `scale`. (Markers
///   name only major and minor keys, so no other mode comes out.)
/// - `shift` is the semitones from `from` to the timeline's key, the short
///   way (-6 to 5), as a clip's Transpose does, so relative keys match (A
///   minor to C major is no shift): wire it into Transpose's `semitones`.
///
/// The outputs are the values themselves, which Scale Quantise's and
/// Transpose's parameters take as they are. It reads the key at the start
/// of each block, so a key change at a marker reaches it at the next block
/// (like Tempo). It works with the transport stopped.
pub struct SongKey;

pub const SONG_KEY_ID: &str = "noodle.mod.key";

#[derive(Ports)]
#[ports(memoryless)]
struct SongKeyPorts {
    #[param("from", "From", ParamInfo::choice(key_names()))]
    from: (),
    #[control_output("root", "Root")]
    root: (),
    #[control_output("scale", "Scale")]
    scale: (),
    #[control_output("shift", "Shift")]
    shift: (),
}

const fn key_names() -> [&'static str; 24] {
    [
        "C major", "C# major", "D major", "D# major", "E major", "F major", "F# major", "G major",
        "G# major", "A major", "A# major", "B major", "C minor", "C# minor", "D minor", "D# minor",
        "E minor", "F minor", "F# minor", "G minor", "G# minor", "A minor", "A# minor", "B minor",
    ]
}

/// The key `from` stands for: 0 to 11 are the major keys from C, 12 to 23 the
/// minor keys.
pub(crate) fn key_from_choice(choice: f32) -> Key {
    let n = choice.round().clamp(0.0, 23.0) as u8;
    Key::new(n % 12, if n < 12 { Mode::Major } else { Mode::Minor })
}

static INFO: NodeInfo = NodeInfo {
    id: SONG_KEY_ID,
    version: 1,
    name: "Song Key",
    category: "Modulation",
};

impl NodeType for SongKey {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        debug_assert_eq!(ROOT_NAMES.len(), 12);
        Ok(SongKeyPorts::layout())
    }

    /// Reads only the transport, so a rebuilt copy picks up where the old
    /// one was.
    fn stateless(&self) -> bool {
        true
    }

    /// The output follows the timeline's key, which is not in the graph: the
    /// project's and every marker's.
    fn cache_inputs(
        &self,
        _node: noodle_core::NodeId,
        _config: &Config,
        env: &CacheEnv<'_>,
        key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), Uncacheable> {
        key.str(&format!("{:?}", env.project.key()));
        for marker in env.project.markers() {
            if let Some(k) = marker.key {
                key.i64(marker.at.0).str(&format!("{k:?}"));
            }
        }
        Ok(())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SongKeyNode))
    }
}

struct SongKeyNode;

impl Node for SongKeyNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let key = ctx.transport.key;
        let from = io.inputs[SongKeyPorts::FROM].lane(0, 0);
        let (root, rest) = io.outputs.split_at_mut(SongKeyPorts::SCALE);
        let (scale, shift) = rest.split_at_mut(1);
        let root = root[SongKeyPorts::ROOT].lane_mut(0, 0);
        let scale = scale[0].lane_mut(0, 0);
        let shift = shift[0].lane_mut(0, 0);
        let mode = match key.mode {
            Mode::Major => 0.0,
            Mode::Minor => 1.0,
        };
        for i in 0..root.len() {
            root[i] = f32::from(key.tonic);
            scale[i] = mode;
            shift[i] = key_from_choice(from[i]).shift_to(key) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_are_the_keys_in_order() {
        for (n, name) in key_names().iter().enumerate() {
            let key = key_from_choice(n as f32);
            assert_eq!(&key.to_string(), name, "{n}");
        }
    }
}
