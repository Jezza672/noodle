//! Replace node: change a node's type, keeping its ID, so its wires, its
//! automation lanes and the clips it plays stay with it. The whole change is
//! one `Command::Batch`, so one undo step puts everything back.
//!
//! Ports are mapped like this:
//!
//! - Audio, event and output ports keep their key if the new type has it,
//!   otherwise they go to the port at the same position among its kind.
//! - A parameter keeps its key if the new type has it. Between two plugins
//!   keys mean nothing (both number their parameters from the plugin's own
//!   IDs), so they are matched by name, ignoring case and punctuation.
//! - A parameter whose range differs keeps its *position* along the slider,
//!   in its stored value and in the points of its lane.
//!
//! Whatever has no match loses its wire or lane (and a stored value), and
//! the report names it. A stand-in for a missing plugin only knows port keys,
//! not names, so it can only be matched by key.

use noodle_core::{
    AutomationLane, Command, Config, Connection, Endpoint, Node, NodeId, Project, Value,
};
use noodle_engine::{InputKind, InputPort, Layout, Registry, is_plugin_type};
use noodle_plugins::PARAMS_KEY;

/// What a replace kept and what it could not.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Wires, lanes and stored values that moved to the new type.
    pub kept: usize,
    /// What had no match, by the old port's name.
    pub dropped: Vec<String>,
}

impl Report {
    pub fn summary(&self, node_name: &str) -> String {
        if self.dropped.is_empty() {
            format!(
                "Replaced with {node_name}; kept {} connection(s)",
                self.kept
            )
        } else {
            format!(
                "Replaced with {node_name}; kept {}, dropped {}",
                self.kept,
                self.dropped.join(", ")
            )
        }
    }
}

fn normalise(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Which of the node's ports a key is.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Side {
    Input,
    Output,
    EventIn,
    EventOut,
}

/// What `map_port` has decided: each old port's answer (so a port asked
/// about twice, for its wire and its lane, gets the same one), and the new
/// parameters already taken.
#[derive(Default)]
struct Taken {
    done: std::collections::HashMap<(u8, String), Option<String>>,
    used: std::collections::HashSet<String>,
}

/// The key of the new layout's port that takes over `key` (of `side`) from
/// the old layout, if any.
fn map_port(
    old: &Layout,
    new: &Layout,
    side: Side,
    key: &str,
    plugins: bool,
    taken: &mut Taken,
) -> Option<String> {
    let side_key = (side as u8, key.to_owned());
    if let Some(done) = taken.done.get(&side_key) {
        return done.clone();
    }
    let found = match side {
        Side::Input => {
            let port = old.inputs.iter().find(|p| p.key == key)?;
            map_input(old, new, port, plugins)
        }
        Side::Output => by_position(&old.outputs, &new.outputs, key),
        Side::EventIn => by_position(&old.event_inputs, &new.event_inputs, key),
        Side::EventOut => by_position(&old.event_outputs, &new.event_outputs, key),
    }?;
    if side == Side::Input && matches!(kind_of(new, &found), Some(InputKind::Param(_))) {
        // Two old parameters can't share a new one.
        if !taken.used.insert(found.clone()) {
            taken.done.insert(side_key, None);
            return None;
        }
    }
    taken.done.insert(side_key, Some(found.clone()));
    Some(found)
}

fn kind_of<'a>(layout: &'a Layout, key: &str) -> Option<&'a InputKind> {
    layout.inputs.iter().find(|p| p.key == key).map(|p| &p.kind)
}

fn by_position(
    old: &[noodle_engine::Port],
    new: &[noodle_engine::Port],
    key: &str,
) -> Option<String> {
    if let Some(port) = new.iter().find(|p| p.key == key) {
        return Some(port.key.to_string());
    }
    let index = old.iter().position(|p| p.key == key)?;
    new.get(index).map(|p| p.key.to_string())
}

fn map_input(old: &Layout, new: &Layout, port: &InputPort, plugins: bool) -> Option<String> {
    match &port.kind {
        InputKind::Audio => {
            let audio = |l: &Layout| -> Vec<String> {
                l.inputs
                    .iter()
                    .filter(|p| p.kind == InputKind::Audio)
                    .map(|p| p.key.to_string())
                    .collect()
            };
            let (old_audio, new_audio) = (audio(old), audio(new));
            if new_audio.iter().any(|k| *k == port.key) {
                return Some(port.key.to_string());
            }
            let index = old_audio.iter().position(|k| *k == port.key)?;
            new_audio.get(index).cloned()
        }
        InputKind::Param(_) => {
            if !plugins
                && let Some(same) = new
                    .inputs
                    .iter()
                    .find(|p| p.key == port.key && matches!(p.kind, InputKind::Param(_)))
            {
                return Some(same.key.to_string());
            }
            // "Filter / Cutoff" matches "Filter / Cutoff" first, then any
            // "Cutoff" whatever its module.
            let params = || {
                new.inputs
                    .iter()
                    .filter(|p| matches!(p.kind, InputKind::Param(_)))
            };
            let leaf = |name: &str| normalise(name.rsplit('/').next().unwrap_or(name));
            let (full, last) = (normalise(&port.name), leaf(&port.name));
            if last.is_empty() {
                return None;
            }
            params()
                .find(|p| normalise(&p.name) == full)
                .or_else(|| params().find(|p| leaf(&p.name) == last))
                .map(|p| p.key.to_string())
        }
    }
}

/// Moves `value` of the old parameter to the same place along the new one's
/// travel.
fn rescale(old: &InputKind, new: &InputKind, value: f32) -> f32 {
    match (old, new) {
        (InputKind::Param(a), InputKind::Param(b)) if (a.min, a.max) != (b.min, b.max) => {
            b.value_at(a.position(value))
        }
        _ => value,
    }
}

/// The batch that turns `node` into a `new_type`, and what it kept.
pub fn plan(
    project: &Project,
    registry: &Registry,
    node: NodeId,
    new_type: &str,
) -> Result<(Command, Report), String> {
    let old_node: &Node = project.graph().node(node).ok_or("that node is gone")?;
    if old_node.type_id == new_type {
        return Err("the node already has that type".into());
    }
    let old_type = registry
        .get(&old_node.type_id)
        .ok_or_else(|| format!("unknown node type `{}`", old_node.type_id))?;
    let new_node_type = registry
        .get(new_type)
        .ok_or_else(|| format!("unknown node type `{new_type}`"))?;
    let old_layout = old_type
        .layout(&old_node.config)
        .map_err(|e| format!("{e:?}"))?;
    let plugins = is_plugin_type(&old_node.type_id) && is_plugin_type(new_type);
    let initial = new_node_type.initial_config();
    // A plugin shows all its parameters when it has no `params` setting: the
    // matching looks at every one, and the setting is made from the matches.
    let mut open = initial.clone();
    open.remove(PARAMS_KEY);
    let new_layout = new_node_type.layout(&open).map_err(|e| format!("{e:?}"))?;

    let mut report = Report::default();
    let mut taken = Taken::default();
    let mut disconnects = Vec::new();
    let mut connects = Vec::new();
    let mut commands_after = Vec::new();
    let mut new_param_keys: Vec<String> = Vec::new();

    let name_of = |layout: &Layout, side: Side, key: &str| -> String {
        let found = match side {
            Side::Input => layout
                .inputs
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.name.to_string()),
            Side::Output => layout
                .outputs
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.name.to_string()),
            Side::EventIn => layout
                .event_inputs
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.name.to_string()),
            Side::EventOut => layout
                .event_outputs
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.name.to_string()),
        };
        found.unwrap_or_else(|| key.to_owned())
    };
    let input_side = |key: &str| {
        if old_layout.event_inputs.iter().any(|p| p.key == key) {
            Side::EventIn
        } else {
            Side::Input
        }
    };
    let output_side = |key: &str| {
        if old_layout.event_outputs.iter().any(|p| p.key == key) {
            Side::EventOut
        } else {
            Side::Output
        }
    };

    for c in project.graph().connections() {
        if c.to.node == node {
            let side = input_side(&c.to.port);
            disconnects.push(c.to.clone());
            match map_port(
                &old_layout,
                &new_layout,
                side,
                &c.to.port,
                plugins,
                &mut taken,
            ) {
                Some(key) => {
                    if matches!(kind_of(&new_layout, &key), Some(InputKind::Param(_))) {
                        new_param_keys.push(key.clone());
                    }
                    connects.push(Command::Connect(Connection {
                        from: c.from.clone(),
                        to: Endpoint::new(node, key),
                    }));
                    report.kept += 1;
                }
                None => report.dropped.push(name_of(&old_layout, side, &c.to.port)),
            }
        } else if c.from.node == node {
            let side = output_side(&c.from.port);
            disconnects.push(c.to.clone());
            match map_port(
                &old_layout,
                &new_layout,
                side,
                &c.from.port,
                plugins,
                &mut taken,
            ) {
                Some(key) => {
                    connects.push(Command::Connect(Connection {
                        from: Endpoint::new(node, key),
                        to: c.to.clone(),
                    }));
                    report.kept += 1;
                }
                None => report
                    .dropped
                    .push(name_of(&old_layout, side, &c.from.port)),
            }
        }
    }

    // Stored parameter values.
    let mut values = Vec::new();
    for (key, &value) in &old_node.params {
        commands_after.push(Command::SetParam {
            node,
            key: key.clone(),
            value: None,
        });
        let mapped = map_port(
            &old_layout,
            &new_layout,
            Side::Input,
            key,
            plugins,
            &mut taken,
        );
        match mapped {
            Some(new_key) => {
                let old_kind = kind_of(&old_layout, key).expect("a stored value has a port");
                let value = rescale(old_kind, kind_of(&new_layout, &new_key).unwrap(), value);
                if matches!(kind_of(&new_layout, &new_key), Some(InputKind::Param(_))) {
                    new_param_keys.push(new_key.clone());
                }
                values.push((new_key, value));
                report.kept += 1;
            }
            None => report.dropped.push(name_of(&old_layout, Side::Input, key)),
        }
    }

    // Lanes.
    let mut lanes = Vec::new();
    for (id, lane) in project.lanes().filter(|(_, l)| l.target.node == node) {
        let key = &lane.target.port;
        match map_port(
            &old_layout,
            &new_layout,
            Side::Input,
            key,
            plugins,
            &mut taken,
        ) {
            Some(new_key) => {
                let old_kind = kind_of(&old_layout, key).expect("a lane has a port");
                let new_kind = kind_of(&new_layout, &new_key).unwrap();
                let mut moved: AutomationLane = lane.clone();
                moved.target = Endpoint::new(node, new_key.clone());
                for p in &mut moved.points {
                    p.value = rescale(old_kind, new_kind, p.value);
                }
                if matches!(new_kind, InputKind::Param(_)) {
                    new_param_keys.push(new_key);
                }
                lanes.push(Command::SetLane { id, lane: moved });
                report.kept += 1;
            }
            None => {
                lanes.push(Command::RemoveLane { id });
                report.dropped.push(format!(
                    "{} (automation)",
                    name_of(&old_layout, Side::Input, key)
                ));
            }
        }
    }

    // The batch: unplug, change the type and config, plug in again.
    let mut batch: Vec<Command> = disconnects
        .into_iter()
        .map(|input| Command::Disconnect { input })
        .collect();
    batch.extend(lanes);
    batch.push(Command::SetNodeType {
        node,
        type_id: new_type.to_owned(),
    });
    for (key, _) in old_node.config.iter() {
        batch.push(Command::SetConfig {
            node,
            key: key.to_owned(),
            value: None,
        });
    }
    let mut config: Config = initial;
    if is_plugin_type(new_type) {
        new_param_keys.sort();
        new_param_keys.dedup();
        let ids: Vec<&str> = new_param_keys
            .iter()
            .filter_map(|k| k.strip_prefix('p'))
            .collect();
        let text = if ids.is_empty() {
            "none".to_owned()
        } else {
            ids.join(",")
        };
        config.set(PARAMS_KEY, Value::Text(text));
    }
    for (key, value) in config.iter() {
        batch.push(Command::SetConfig {
            node,
            key: key.to_owned(),
            value: Some(value.clone()),
        });
    }
    batch.extend(commands_after);
    for (key, value) in values {
        batch.push(Command::SetParam {
            node,
            key,
            value: Some(value),
        });
    }
    batch.push(Command::SetPreset { node, preset: None });
    batch.push(Command::SetPortOrder {
        node,
        order: Vec::new(),
    });
    batch.extend(connects);
    Ok((Command::Batch(batch), report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_plugins::{AudioPort, Descriptor, ParamDesc, PluginFile, PluginFormat};

    fn param(id: u32, module: &str, name: &str, min: f64, max: f64) -> ParamDesc {
        ParamDesc {
            id,
            name: name.into(),
            module: module.into(),
            min,
            max,
            default: min,
            stepped: false,
        }
    }

    fn plugin(id: &str, params: Vec<ParamDesc>) -> Descriptor {
        Descriptor {
            format: PluginFormat::Clap,
            id: id.into(),
            name: id.into(),
            vendor: "Nobody".into(),
            instrument: false,
            audio_inputs: vec![AudioPort {
                name: "main".into(),
                channels: 2,
            }],
            audio_outputs: vec![AudioPort {
                name: "main".into(),
                channels: 2,
            }],
            note_inputs: 0,
            note_outputs: 0,
            params,
        }
    }

    /// Two reverbs whose parameter IDs disagree but whose names overlap.
    fn session() -> Session {
        let files = vec![PluginFile {
            path: "/no/such.clap".into(),
            len: 0,
            modified: 0,
            plugins: vec![
                plugin(
                    "a.verb",
                    vec![
                        param(1, "", "Mix", 0.0, 1.0),
                        param(2, "Space", "Decay", 0.0, 10.0),
                        param(3, "", "Pre-delay", 0.0, 100.0),
                    ],
                ),
                plugin(
                    "b.verb",
                    vec![
                        param(7, "", "Decay", 0.0, 20.0),
                        param(8, "", "MIX", 0.0, 1.0),
                    ],
                ),
            ],
            error: None,
        }];
        let mut nodes = Nodes::all();
        nodes.set_plugins(noodle_plugins::Catalog::new(&files));
        Session::new(nodes)
    }

    #[test]
    fn replacing_a_plugin_maps_wires_values_and_lanes_by_name() {
        let mut session = session();
        let mut verb = Node::new("clap.a.verb").at(100.0, 0.0);
        verb.config.set(PARAMS_KEY, Value::Text("1,2,3".into()));
        verb.params.insert("p2".into(), 5.0);
        verb.params.insert("p3".into(), 40.0);
        let (src, verb_id, out, lfo) = (
            session.new_node_id(),
            session.new_node_id(),
            session.new_node_id(),
            session.new_node_id(),
        );
        let lane = session.project().next_lane_id();
        let wire = |a: NodeId, ap: &str, b: NodeId, bp: &str| {
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(a, ap),
                to: Endpoint::new(b, bp),
            }))
        };
        session.edit([
            Edit::Apply(Command::AddNode {
                id: src,
                node: Node::new("noodle.osc.sine"),
            }),
            Edit::Apply(Command::AddNode {
                id: verb_id,
                node: verb,
            }),
            Edit::Apply(Command::AddNode {
                id: out,
                node: Node::new(noodle_engine::OUTPUT_ID),
            }),
            Edit::Apply(Command::AddNode {
                id: lfo,
                node: Node::new("noodle.osc.sine"),
            }),
            wire(src, "out", verb_id, "in"),
            wire(verb_id, "out", out, "in"),
            wire(lfo, "out", verb_id, "p1"),
            Edit::Apply(Command::AddLane {
                id: lane,
                lane: AutomationLane::new(
                    Endpoint::new(verb_id, "p2"),
                    vec![noodle_core::AutomationPoint {
                        tick: noodle_core::Tick(0),
                        value: 5.0,
                        curve: Default::default(),
                    }],
                ),
            }),
        ]);

        session.edit([Edit::ReplaceNode {
            node: verb_id,
            type_id: "clap.b.verb".into(),
        }]);

        let p = session.project();
        let node = p.graph().node(verb_id).expect("same ID");
        assert_eq!(node.type_id, "clap.b.verb");
        // Mix (p1 -> p8) keeps its wire; Decay (p2 -> p7) keeps its lane and
        // its value, moved to the same place along the longer range;
        // Pre-delay has no match and is reported.
        let connected =
            |to: (NodeId, &str)| p.graph().source(&Endpoint::new(to.0, to.1)).map(|e| e.node);
        assert_eq!(connected((verb_id, "in")), Some(src));
        assert_eq!(connected((verb_id, "p8")), Some(lfo));
        assert_eq!(connected((out, "in")), Some(verb_id));
        assert_eq!(p.lane(lane).unwrap().target, Endpoint::new(verb_id, "p7"));
        assert_eq!(p.lane(lane).unwrap().points[0].value, 10.0);
        assert_eq!(node.params.get("p7"), Some(&10.0));
        assert!(!node.params.contains_key("p3"));
        assert_eq!(
            node.config.get(PARAMS_KEY),
            Some(&Value::Text("7,8".into()))
        );
        assert!(session.message().unwrap().contains("Pre-delay"));
        assert!(
            !session
                .diagnostics()
                .iter()
                .any(|d| format!("{d:?}").contains("Unknown"))
        );

        // One undo step puts everything back.
        session.undo();
        let p = session.project();
        assert_eq!(p.graph().node(verb_id).unwrap().type_id, "clap.a.verb");
        assert_eq!(p.lane(lane).unwrap().target, Endpoint::new(verb_id, "p2"));
        assert_eq!(
            p.graph()
                .source(&Endpoint::new(verb_id, "p1"))
                .map(|e| e.node),
            Some(lfo)
        );
    }

    #[test]
    fn a_missing_plugin_stays_in_the_project_and_passes_sound_through() {
        // The sine and Output are wired through a plugin nobody has.
        let project = Project::from_ron(
            r#"(format: 1, graph: (nodes: {
                1: (type: "noodle.osc.sine", position: (x: 0.0, y: 0.0)),
                2: (type: "clap.gone.plugin", params: {"p4": 0.25}, config: {"state": "abc"}, position: (x: 100.0, y: 0.0)),
                3: (type: "noodle.io.output", position: (x: 200.0, y: 0.0)),
            }, connections: [
                (from: (node: 1, port: "out"), to: (node: 2, port: "in")),
                (from: (node: 2, port: "out"), to: (node: 3, port: "in")),
            ]))"#,
        )
        .unwrap();
        let mut session = Session::with_project(Nodes::all(), project, None);
        session.recompile();
        let node = session
            .project()
            .graph()
            .node(noodle_core::NodeId(2))
            .unwrap();
        // Nothing of the node is lost.
        assert_eq!(node.type_id, "clap.gone.plugin");
        assert_eq!(node.params.get("p4"), Some(&0.25));
        assert!(node.config.get("state").is_some());
        // The editor can lay it out, and compiling raises no unknown type.
        let layout = session
            .registry()
            .get("clap.gone.plugin")
            .expect("a stand-in")
            .layout(&node.config)
            .unwrap();
        assert_eq!(layout.inputs.len(), 2);
        assert_eq!(layout.outputs.len(), 1);
        assert!(
            !session
                .diagnostics()
                .iter()
                .any(|d| format!("{d:?}").contains("UnknownNodeType")),
            "{:?}",
            session.diagnostics()
        );
    }
}
