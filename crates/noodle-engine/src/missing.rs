//! The stand-in for a plugin that is not installed.
//!
//! A project that uses a plugin the machine does not have keeps the node:
//! its type, config (with the plugin's saved state) and parameter values stay
//! exactly as they were, so reopening the project where the plugin exists
//! restores it. Meanwhile the node is a pass-through (audio input *n* to
//! audio output *n*; notes are dropped) so the rest of the mix still sounds.
//!
//! Ports come from what the project uses: the keys of the wires that touch
//! the node and the parameters it stores values for. A plugin's port keys
//! follow one scheme, which says what kind of port each key is.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

use noodle_core::{Config, Graph, Node as GraphNode, NodeId};

use crate::{
    Context, Instance, Io, Layout, Memory, Node, NodeError, NodeInfo, NodeType, ParamInfo, Setup,
};

/// The prefixes of the node types that plugins register under.
const PLUGIN_PREFIXES: [&str; 3] = ["clap.", "vst3.", "au."];

/// Whether `type_id` names a plugin node type.
pub fn is_plugin_type(type_id: &str) -> bool {
    PLUGIN_PREFIXES.iter().any(|p| type_id.starts_with(p))
}

/// Node IDs must be `'static`; a missing plugin's is leaked once and reused.
fn intern(id: &str) -> &'static str {
    static IDS: OnceLock<Mutex<BTreeSet<&'static str>>> = OnceLock::new();
    let mut ids = IDS
        .get_or_init(Default::default)
        .lock()
        .expect("the intern table is never poisoned");
    if let Some(known) = ids.get(id) {
        return known;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    ids.insert(leaked);
    leaked
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    AudioIn,
    EventIn,
    Param,
    AudioOut,
    EventOut,
}

fn numbered(key: &str, stem: &str) -> bool {
    key.strip_prefix(stem)
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
}

fn input_kind(key: &str) -> Kind {
    if numbered(key, "in") {
        Kind::AudioIn
    } else if numbered(key, "notes") {
        Kind::EventIn
    } else {
        Kind::Param
    }
}

fn output_kind(key: &str) -> Kind {
    if numbered(key, "notes_out") {
        Kind::EventOut
    } else {
        Kind::AudioOut
    }
}

/// Orders `in`, `in2`, `in3`, ... by number.
fn order_key(key: &str) -> (usize, String) {
    let digits: String = key.chars().filter(char::is_ascii_digit).collect();
    (digits.parse().unwrap_or(1), key.to_owned())
}

/// A node type for the missing plugin `info.id`, with the ports the project
/// uses on `node`. `None` if `node` is not a plugin.
pub fn placeholder(graph: &Graph, id: NodeId, node: &GraphNode) -> Option<MissingPlugin> {
    if !is_plugin_type(&node.type_id) {
        return None;
    }
    let mut ports: BTreeSet<(bool, String)> = BTreeSet::new();
    for c in graph.connections() {
        if c.to.node == id {
            ports.insert((false, c.to.port));
        }
        if c.from.node == id {
            ports.insert((true, c.from.port));
        }
    }
    for key in node.params.keys() {
        ports.insert((false, key.clone()));
    }
    let mut inputs: Vec<(Kind, String)> = Vec::new();
    let mut outputs: Vec<(Kind, String)> = Vec::new();
    for (is_output, key) in ports {
        if is_output {
            outputs.push((output_kind(&key), key));
        } else {
            inputs.push((input_kind(&key), key));
        }
    }
    let sort = |v: &mut Vec<(Kind, String)>| v.sort_by_key(|(_, k)| order_key(k));
    sort(&mut inputs);
    sort(&mut outputs);
    Some(MissingPlugin {
        info: NodeInfo {
            id: intern(&node.type_id),
            version: 1,
            name: intern(&format!("Missing plugin {}", node.type_id)),
            category: "Plugins",
        },
        inputs,
        outputs,
    })
}

/// A stand-in for a plugin that isn't installed, keeping its wires and values.
pub struct MissingPlugin {
    info: NodeInfo,
    inputs: Vec<(Kind, String)>,
    outputs: Vec<(Kind, String)>,
}

impl NodeType for MissingPlugin {
    fn info(&self) -> &NodeInfo {
        &self.info
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        let mut layout = Layout::realtime();
        // Audio inputs first, so output n passes input n.
        for (kind, key) in &self.inputs {
            if *kind == Kind::AudioIn {
                layout = layout.input(Cow::Owned(key.clone()), Cow::Owned(key.clone()));
            }
        }
        for (kind, key) in &self.inputs {
            match kind {
                Kind::Param => {
                    layout = layout.param(
                        Cow::Owned(key.clone()),
                        Cow::Owned(key.clone()),
                        ParamInfo::new(0.0, 1.0, 0.0),
                    );
                }
                Kind::EventIn => {
                    layout = layout.event_input(Cow::Owned(key.clone()), Cow::Owned(key.clone()));
                }
                _ => {}
            }
        }
        for (kind, key) in &self.outputs {
            layout = match kind {
                Kind::EventOut => {
                    layout.event_output(Cow::Owned(key.clone()), Cow::Owned(key.clone()))
                }
                _ => layout.output(Cow::Owned(key.clone()), Cow::Owned(key.clone())),
            };
        }
        // An unknown plugin passes sound through unchanged.
        Ok(layout.memory(Memory::None))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let audio_in = self
            .inputs
            .iter()
            .filter(|(k, _)| *k == Kind::AudioIn)
            .count();
        Ok(Instance::realtime(PassThrough { audio_in }))
    }
}

struct PassThrough {
    audio_in: usize,
}

impl Node for PassThrough {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        for (i, output) in io.outputs.iter_mut().enumerate() {
            let shape = output.shape();
            for voice in 0..shape.voices {
                for channel in 0..shape.channels {
                    if i < self.audio_in {
                        let input = &io.inputs[i];
                        output
                            .lane_mut(voice, channel)
                            .copy_from_slice(input.lane(voice, channel));
                    } else {
                        output.silence(voice, channel);
                    }
                }
            }
        }
    }
}
