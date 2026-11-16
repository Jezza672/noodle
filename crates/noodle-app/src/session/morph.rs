//! Morph between presets: a graph edit, nothing more. It applies preset A to
//! a group, adds one Morph node inside it with an output wired into every
//! parameter whose value differs between A and B, and exposes the node's
//! `position` as a group input named `morph`, all as one undo step.
//!
//! Parameters inside nested groups are morphed too: a wire cannot cross a
//! group boundary, so each nested group that holds differing parameters gets
//! a Morph node of its own, driven by a group input named `morph` that is
//! wired, one level up, from the parent's `morph` input. Levels that only
//! lead to such a group get the input and the pass-through wire, no node.
//!
//! Nothing is modulated unseen: the wires are real, and a parameter that
//! already has a wire or a lane stays out (two sources into one input were
//! never allowed), as does all config. Taking a parameter out of the morph is
//! deleting its wire.

use noodle_core::group::{GROUP, GROUP_INPUT};
use noodle_core::library::GroupPreset;
use noodle_core::spare;
use noodle_core::{Command, Endpoint, Frame, FrameId, Node, NodeId, Position, Project};
use noodle_engine::{InputKind, ParamKind, Registry};
use noodle_nodes::{MORPH_ID, MORPH_POSITION, MORPH_Y, MorphRow, morph_config, morph_output_key};

/// The name of the group input the node's `position` is exposed as.
pub const MORPH_INPUT: &str = "morph";
/// The name of the second input, `y`, of a four-preset morph.
pub const MORPH_INPUT_Y: &str = "morph y";

/// The IDs the command's new things take: for each level it morphs, a Morph
/// node, a frame and (if the group has no input boundary yet) a boundary.
pub struct Ids {
    nodes: Vec<NodeId>,
    frames: Vec<FrameId>,
}

impl Ids {
    /// How many levels a morph of `group` may touch: the group and every
    /// group inside it.
    pub fn levels(graph: &noodle_core::Graph, group: NodeId) -> usize {
        1 + nested_groups(graph, group).len()
    }

    /// Enough IDs for `levels` levels.
    pub fn new(
        levels: usize,
        mut node: impl FnMut() -> NodeId,
        mut frame: impl FnMut() -> FrameId,
    ) -> Self {
        Self {
            nodes: (0..2 * levels).map(|_| node()).collect(),
            frames: (0..levels).map(|_| frame()).collect(),
        }
    }

    fn node(&mut self) -> Result<NodeId, String> {
        self.nodes.pop().ok_or_else(|| "out of node IDs".to_owned())
    }

    fn frame(&mut self) -> Result<FrameId, String> {
        self.frames
            .pop()
            .ok_or_else(|| "out of frame IDs".to_owned())
    }
}

/// Every group inside `group`, at any depth, parents before children.
fn nested_groups(graph: &noodle_core::Graph, group: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut queue = vec![group];
    while let Some(parent) = queue.pop() {
        for (id, node) in graph.children(Some(parent)) {
            if node.type_id == GROUP {
                found.push(id);
                queue.push(id);
            }
        }
    }
    found
}

/// The name of the group input that is the only thing feeding `target`: a
/// control set in a group's panel, not a modulation. The Morph takes such a
/// parameter over (its wire is replaced, so the control no longer does
/// anything). A control passed down through nested groups (an input of the
/// morphed `group` wired to an input of a group inside it, and so on down to
/// the parameter) counts, as long as nothing outside `group` feeds the first
/// one; the name is the one in `group`'s panel.
fn only_from_the_boundary(
    graph: &noodle_core::Graph,
    group: NodeId,
    level: NodeId,
    target: &Endpoint,
) -> Option<String> {
    let source = graph.source(target)?;
    let boundary = graph.node(source.node)?;
    if boundary.type_id != GROUP_INPUT || boundary.parent != Some(level) {
        return None;
    }
    let port = graph
        .group_ports(level)
        .inputs
        .into_iter()
        .find(|p| p.node == source.node && p.key() == source.port)?;
    let outside = graph.source(&Endpoint::new(level, port.name.clone()));
    match outside {
        None => Some(port.name),
        // Fed from the level above: a control of that group passed down.
        Some(from) if level != group => {
            let parent = graph.node(level)?.parent?;
            only_from_the_boundary(graph, group, parent, &Endpoint::new(level, port.name)).filter(
                |_| {
                    graph
                        .node(from.node)
                        .is_some_and(|n| n.parent == Some(parent))
                },
            )
        }
        Some(_) => None,
    }
}

/// A group input named `base`, or the first free `base 2`, `base 3`, ...
fn free_name(taken: &noodle_core::group::GroupPorts, base: &str) -> String {
    std::iter::once(base.to_owned())
        .chain((2..).map(|i| format!("{base} {i}")))
        .find(|n| taken.input(n).is_none())
        .expect("an unbounded range has a free name")
}

/// What the command did, and what it left out and why.
pub struct Morphed {
    pub command: Command,
    /// How many parameters the Morph drives.
    pub wired: usize,
    pub left_out: Vec<String>,
    /// Group controls whose only job was a parameter the Morph now drives.
    pub taken_over: Vec<String>,
}

impl Morphed {
    /// One line for the status bar.
    pub fn notice(&self) -> String {
        let mut text = format!("Morphing {} parameters.", self.wired);
        if !self.left_out.is_empty() {
            text.push_str(&format!(" Left out: {}.", self.left_out.join("; ")));
        }
        if !self.taken_over.is_empty() {
            text.push_str(&format!(
                " The Morph now drives what these controls did: {}.",
                self.taken_over.join(", ")
            ));
        }
        text
    }
}

/// The command that morphs `group` between presets. `a` is the first, with
/// its library ID, recorded as the group's preset. `others` is the second
/// preset alone for a morph along `position`, or the second, third and fourth
/// for the four corners of an XY pad: B at `position` 1, C at `y` 1, D at
/// both.
pub fn command(
    project: &Project,
    registry: &Registry,
    group: NodeId,
    a: (&str, &GroupPreset),
    others: &[&GroupPreset],
    mut ids: Ids,
) -> Result<Morphed, String> {
    if !matches!(others.len(), 1 | 3) {
        return Err("a morph is between two presets, or four for an XY pad".to_owned());
    }
    let xy = others.len() == 3;
    let graph = project.graph();
    let group_node = graph.node(group).ok_or("the group is gone")?;
    if group_node.type_id != GROUP {
        return Err("only a group can morph between presets".to_owned());
    }
    let layout_of = |node: &Node| registry.layout_signature(node);
    let apply_a =
        a.1.apply(graph, group, a.0, &layout_of)
            .map_err(|e| e.to_string())?;
    if others
        .iter()
        .any(|p| !p.applies_to(graph, group, &layout_of))
    {
        return Err("a preset is for a differently built group".to_owned());
    }

    // The rows of each level: a node's parameters are morphed by the Morph
    // node of the group it is directly in.
    let mut left_out = Vec::new();
    let mut taken_over: Vec<String> = Vec::new();
    let mut levels: std::collections::BTreeMap<NodeId, Vec<(Endpoint, MorphRow)>> =
        std::collections::BTreeMap::new();
    for (id, values) in a.1.paired(others, graph, group) {
        let node = graph.node(id).ok_or("a node is gone")?;
        let Some(level) = node.parent else {
            continue;
        };
        // A group's own boundary nodes hold levels, not parameters to morph.
        let Some(node_type) = registry.get(&node.type_id) else {
            continue;
        };
        let layout = node_type.layout(&node.config).map_err(|e| e.to_string())?;
        let label = node_type.info().name;
        if values.iter().any(|v| v.config != values[0].config) {
            left_out.push(format!("{label}'s config stays as in the first preset"));
        }
        let mut keys: Vec<&String> = values.iter().flat_map(|v| v.params.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            let Some(input) = layout.inputs.iter().find(|i| i.key == *key) else {
                continue;
            };
            let InputKind::Param(info) = &input.kind else {
                continue;
            };
            let at = |v: &noodle_core::library::PresetValues| {
                v.params.get(key).copied().unwrap_or(info.default)
            };
            let mut vals: Vec<f32> = values.iter().map(|v| at(v)).collect();
            if !xy {
                // Two presets: the lower corners repeat the upper.
                vals.extend([vals[0], vals[1]]);
            }
            let from = vals[0];
            if vals.iter().all(|v| v.to_bits() == from.to_bits()) {
                continue;
            }
            let target = Endpoint::new(id, key.clone());
            let name = format!("{label} {}", input.name);
            let control = only_from_the_boundary(graph, group, level, &target);
            if graph.source(&target).is_some() && control.is_none() {
                left_out.push(format!("{name} has a wire"));
                continue;
            }
            if project.lane_for(&target).is_some() {
                left_out.push(format!("{name} has an automation lane"));
                continue;
            }
            if let Some(control) = control
                && !taken_over.contains(&control)
            {
                taken_over.push(control);
            }
            let row = if info.is_offset() {
                // A wire moves the value along its travel from the base,
                // which preset A has just set: 0.5 is no move.
                let travel =
                    |v: f32| 0.5 + 0.5 * (info.position(v) - info.position(from)).clamp(-1.0, 1.0);
                MorphRow {
                    name,
                    min: -1.0,
                    max: 1.0,
                    log: false,
                    a: 0.5,
                    b: travel(vals[1]),
                    c: travel(vals[2]),
                    d: travel(vals[3]),
                    stepped: false,
                }
            } else {
                let (min, max, log, _) = super::automate::range_and_start(info, from);
                MorphRow {
                    name,
                    min,
                    max,
                    log,
                    a: info.position(vals[0]),
                    b: info.position(vals[1]),
                    c: info.position(vals[2]),
                    d: info.position(vals[3]),
                    stepped: matches!(info.kind, ParamKind::Stepped { .. }),
                }
            };
            levels.entry(level).or_default().push((target, row));
        }
    }
    let wired: usize = levels.values().map(Vec::len).sum();
    if wired == 0 {
        return Err("the two presets don't differ in anything that can be morphed".to_owned());
    }

    // The levels that need the `morph` input: those with rows, and every
    // group on the way down to one, so the control reaches it.
    let mut needed: std::collections::BTreeSet<NodeId> = std::collections::BTreeSet::new();
    needed.insert(group);
    for &level in levels.keys() {
        let mut at = Some(level);
        while let Some(g) = at {
            needed.insert(g);
            if g == group {
                break;
            }
            at = graph.node(g).and_then(|n| n.parent);
        }
    }
    // Parents before children, so a child's input is named after its parent's
    // is placed.
    let mut order = vec![group];
    order.extend(
        nested_groups(graph, group)
            .into_iter()
            .filter(|g| needed.contains(g)),
    );

    let mut commands = vec![apply_a];
    let mut feed: std::collections::BTreeMap<NodeId, Vec<Endpoint>> =
        std::collections::BTreeMap::new();
    // Pass 1, per level: the input port(s), the Morph node and its wires.
    let mut inputs: std::collections::BTreeMap<NodeId, (NodeId, Vec<String>, Vec<String>)> =
        std::collections::BTreeMap::new();
    for &level in &order {
        // The group inputs: `morph` (and `morph y` for four presets), or the
        // first free `morph 2`, ... All go on the group's one input boundary.
        let taken = graph.group_ports(level);
        let name = free_name(&taken, MORPH_INPUT);
        let suffix = name[MORPH_INPUT.len()..].to_owned();
        let mut names = vec![name];
        if xy {
            names.push(format!("{MORPH_INPUT_Y}{suffix}"));
        }
        let existing = graph
            .children(Some(level))
            .find(|(_, n)| n.type_id == GROUP_INPUT);
        let mut ports = existing.map_or_else(Vec::new, |(_, n)| n.boundary_ports());
        let mut keys = Vec::new();
        for name in &names {
            let id = ports.iter().map(|&(id, _)| id).max().map_or(1, |id| id + 1);
            ports.push((id, name.clone()));
            keys.push(noodle_core::group::port_key(id));
        }
        let boundary = match existing {
            Some((id, _)) => {
                commands.push(Command::SetConfig {
                    node: id,
                    key: noodle_core::group::PORTS.into(),
                    value: Some(noodle_core::group::ports_value(&ports)),
                });
                id
            }
            None => {
                let x = graph
                    .children(Some(level))
                    .map(|(_, n)| n.position.x)
                    .reduce(f32::min)
                    .map_or(0.0, |x| x - 250.0);
                let mut node =
                    Node::new(GROUP_INPUT).with_config(noodle_core::group::ports_config(&ports));
                node.position = Position { x, y: 0.0 };
                node.parent = Some(level);
                let id = ids.node()?;
                commands.push(Command::AddNode { id, node });
                id
            }
        };
        let boundary_id = boundary;
        inputs.insert(level, (boundary, keys.clone(), names));

        let Some(own) = levels.get(&level) else {
            // No Morph here: the input itself is what the groups below get.
            feed.insert(
                level,
                keys.iter()
                    .map(|k| Endpoint::new(boundary_id, k.clone()))
                    .collect(),
            );
            continue;
        };
        let mut rows = own.clone();
        let passes = order
            .iter()
            .skip(1)
            .any(|&c| graph.node(c).and_then(|n| n.parent) == Some(level));
        let first_pass = rows.len();
        if passes {
            // The groups below follow the Morph's own position, so a
            // slider or pad on it moves them too: rows that give it back.
            let line = |name: &str, a: f32, b: f32, c: f32, d: f32| MorphRow {
                name: name.to_owned(),
                min: 0.0,
                max: 1.0,
                log: false,
                a,
                b,
                c,
                d,
                stepped: false,
            };
            let dummy = Endpoint::new(NodeId(0), "");
            rows.push((
                dummy.clone(),
                line("Position for nested groups", 0.0, 1.0, 0.0, 1.0),
            ));
            if xy {
                rows.push((dummy, line("y for nested groups", 0.0, 0.0, 1.0, 1.0)));
            }
        }
        // Left of what is inside, in a frame.
        let (x, y) = graph
            .children(Some(level))
            .map(|(_, n)| (n.position.x, n.position.y))
            .fold((f32::MAX, f32::MAX), |(x, y), (nx, ny)| {
                (x.min(nx), y.min(ny))
            });
        let (x, y) = if x == f32::MAX {
            (0.0, 0.0)
        } else {
            (x - 340.0, y)
        };
        let mut morph = Node::new(MORPH_ID).at(x, y).with_config(morph_config(
            &rows.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>(),
        ));
        morph.parent = Some(level);
        let morph_id = ids.node()?;
        commands.push(Command::AddNode {
            id: morph_id,
            node: morph,
        });
        commands.push(Command::AddFrame {
            id: ids.frame()?,
            frame: Frame {
                label: "Morph A to B".to_owned(),
                position: Position {
                    x: x - 20.0,
                    y: y - 40.0,
                },
                width: 260.0,
                height: 80.0 + 22.0 * rows.len() as f32,
            },
        });
        let (boundary, keys, _) = &inputs[&level];
        commands.push(spare::wire(
            Endpoint::new(*boundary, keys[0].clone()),
            Endpoint::new(morph_id, MORPH_POSITION),
        ));
        if xy {
            commands.push(spare::wire(
                Endpoint::new(*boundary, keys[1].clone()),
                Endpoint::new(morph_id, MORPH_Y),
            ));
        }
        for (i, (target, _)) in rows.iter().enumerate().take(first_pass) {
            commands.push(spare::wire(
                Endpoint::new(morph_id, morph_output_key(i)),
                target.clone(),
            ));
        }
        feed.insert(
            level,
            (first_pass..rows.len())
                .map(|i| Endpoint::new(morph_id, morph_output_key(i)))
                .collect(),
        );
    }
    // Pass 2: each nested level's input is fed, one level up, by the
    // parent's own `morph` input.
    for &level in order.iter().skip(1) {
        let parent = graph
            .node(level)
            .and_then(|n| n.parent)
            .ok_or("a nested group has no parent")?;
        let (_, _, child_names) = &inputs[&level];
        for (from, name) in feed[&parent].iter().zip(child_names) {
            commands.push(spare::wire(
                from.clone(),
                Endpoint::new(level, name.clone()),
            ));
        }
    }
    Ok(Morphed {
        command: Command::Batch(commands),
        wired,
        left_out,
        taken_over,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_core::group::{GROUP_OUTPUT, ports_config};
    use noodle_core::{Config, Value};
    use noodle_engine::{OUTPUT_ID, Settings};
    use noodle_nodes::{MORPH_ID, SVF_ID};

    const SETTINGS: Settings = Settings {
        sample_rate: 48_000.0,
        max_frames: 512,
        channels: 2,
    };

    fn add(session: &mut Session, node: Node) -> NodeId {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    }

    fn wire(session: &mut Session, from: (NodeId, &str), to: (NodeId, &str)) {
        let wire = spare::wire(Endpoint::new(from.0, from.1), Endpoint::new(to.0, to.1));
        session.edit([Edit::Apply(wire)]);
    }

    fn set(session: &mut Session, node: NodeId, key: &str, value: f32) {
        session.edit([Edit::Apply(Command::SetParam {
            node,
            key: key.into(),
            value: Some(value),
        })]);
    }

    struct Lead {
        group: NodeId,
        filter: NodeId,
        gain: NodeId,
        lfo: NodeId,
    }

    /// A saw through a filter (its resonance driven by an LFO) and a gain,
    /// in a group, to the output.
    fn lead(session: &mut Session) -> Lead {
        let group = add(
            session,
            Node::new(GROUP).with_config(Config::new().with("name", Value::Text("Lead".into()))),
        );
        let saw = add(
            session,
            Node::new("noodle.osc.saw")
                .with_param("frequency", 110.0)
                .in_group(group),
        );
        let lfo = add(
            session,
            Node::new("noodle.osc.sine")
                .with_param("frequency", 2.0)
                .in_group(group),
        );
        let filter = add(session, Node::new(SVF_ID).in_group(group));
        let gain = add(session, Node::new("noodle.util.gain").in_group(group));
        let out = add(
            session,
            Node::new(GROUP_OUTPUT)
                .with_config(ports_config(&[(1, "out".into())]))
                .in_group(group),
        );
        wire(session, (saw, "out"), (filter, "in"));
        wire(session, (lfo, "out"), (filter, "resonance"));
        wire(session, (filter, "low"), (gain, "in"));
        wire(session, (gain, "out"), (out, "p1"));
        let output = add(session, Node::new(OUTPUT_ID));
        wire(session, (group, "out"), (output, "in"));
        Lead {
            group,
            filter,
            gain,
            lfo,
        }
    }

    /// Presets A and B: the cutoff and gain differ, and so does the
    /// resonance, which the LFO drives.
    fn presets(session: &mut Session, lead: &Lead) -> (GroupPreset, GroupPreset) {
        set(session, lead.filter, "cutoff", 500.0);
        set(session, lead.filter, "resonance", 0.1);
        set(session, lead.gain, "gain", -12.0);
        let a = GroupPreset::capture(session.project().graph(), lead.group, "A");
        set(session, lead.filter, "cutoff", 3000.0);
        set(session, lead.filter, "resonance", 0.6);
        set(session, lead.gain, "gain", -3.0);
        let b = GroupPreset::capture(session.project().graph(), lead.group, "B");
        (a, b)
    }

    fn test_ids(session: &mut Session, group: NodeId) -> Ids {
        let levels = Ids::levels(session.project().graph(), group);
        let mut nodes = std::iter::repeat_with(|| session.new_node_id()).take(2 * levels);
        let nodes: Vec<NodeId> = nodes.by_ref().collect();
        let mut frames: Vec<FrameId> = (0..levels).map(|_| session.new_frame_id()).collect();
        let mut nodes = nodes.into_iter();
        Ids::new(levels, || nodes.next().unwrap(), || frames.pop().unwrap())
    }

    fn morph_edit(session: &mut Session, lead: &Lead, a: &GroupPreset, b: &GroupPreset) -> Morphed {
        let ids = test_ids(session, lead.group);
        let morphed = command(
            session.project(),
            session.registry(),
            lead.group,
            ("A", a),
            &[b],
            ids,
        )
        .unwrap();
        session.edit([Edit::Apply(morphed.command.clone())]);
        morphed
    }

    fn render(session: &Session) -> Vec<f32> {
        let mut registry = noodle_engine::Registry::with_builtins();
        let _library = noodle_nodes::register_all(&mut registry);
        let rendered =
            noodle_engine::render_project(session.project(), &registry, SETTINGS, 9_600).unwrap();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        rendered.samples
    }

    fn morph_node(session: &Session) -> NodeId {
        let nodes = session.project().graph().nodes();
        let found = nodes.filter(|(_, n)| n.type_id == MORPH_ID);
        found.map(|(id, _)| id).next().expect("a morph node")
    }

    #[test]
    fn it_wires_exactly_the_differing_parameters_and_skips_a_wired_one() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        let (a, b) = presets(&mut session, &lead);
        let morphed = morph_edit(&mut session, &lead, &a, &b);
        // Cutoff and gain; the resonance has the LFO's wire.
        assert_eq!(morphed.wired, 2);
        assert_eq!(morphed.left_out.len(), 1, "{:?}", morphed.left_out);
        assert!(
            morphed.left_out[0].contains("Resonance"),
            "{:?}",
            morphed.left_out
        );
        let graph = session.project().graph();
        let morph = morph_node(&session);
        for (node, port) in [(lead.filter, "cutoff"), (lead.gain, "gain")] {
            let from = graph.source(&Endpoint::new(node, port)).unwrap();
            assert_eq!(from.node, morph, "{port}");
        }
        // The LFO still drives the resonance.
        assert_eq!(
            graph
                .source(&Endpoint::new(lead.filter, "resonance"))
                .unwrap()
                .node,
            lead.lfo
        );
        // `position` is exposed as the group input `morph`, inside a frame.
        assert!(graph.group_ports(lead.group).input("morph").is_some());
        assert_eq!(session.project().frames().count(), 1);
        // The preset A was applied: the group is at A's values.
        assert_eq!(graph.node(lead.gain).unwrap().params["gain"], -12.0);
    }

    #[test]
    fn it_is_one_undo_step() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        let (a, b) = presets(&mut session, &lead);
        let before = session.project().clone();
        morph_edit(&mut session, &lead, &a, &b);
        session.undo();
        assert_eq!(session.project().graph(), before.graph());
        assert_eq!(session.project().frames().count(), 0);
    }

    #[test]
    fn position_zero_sounds_like_a_and_one_like_b() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        let (a, b) = presets(&mut session, &lead);
        let mut reference_a = Session::new(Nodes::all());
        let ref_lead = self::lead(&mut reference_a);
        let mut reference_b = Session::new(Nodes::all());
        let _ = self::lead(&mut reference_b);
        for (s, p) in [(&mut reference_a, &a), (&mut reference_b, &b)] {
            let graph = s.project().graph();
            let command = p
                .apply(graph, ref_lead.group, "x", &|n: &Node| {
                    noodle_engine::Registry::with_builtins().layout_signature(n)
                })
                .unwrap();
            s.edit([Edit::Apply(command)]);
        }
        // The LFO-driven resonance stays out of the morph, at A's base.
        set(&mut reference_b, ref_lead.filter, "resonance", 0.1);
        morph_edit(&mut session, &lead, &a, &b);
        let morph = morph_node(&session);
        set(&mut session, morph, "position", 0.0);
        let at_a = render(&session);
        set(&mut session, morph, "position", 1.0);
        let at_b = render(&session);
        let want_a = render(&reference_a);
        let want_b = render(&reference_b);
        let close = |x: &[f32], y: &[f32]| {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| (p - q).abs() < 1e-3)
        };
        assert!(close(&at_a, &want_a), "position 0 isn't preset A");
        assert!(close(&at_b, &want_b), "position 1 isn't preset B");
        assert!(!close(&at_a, &at_b), "A and B sound the same");
    }

    struct Nested {
        outer: NodeId,
        top: NodeId,
        mid: NodeId,
        gain: NodeId,
    }

    /// A saw into `mid`, a group that holds only `inner`, a group that holds
    /// a gain; out through the outer group to the output. The saw is at the
    /// top level of the group, the gain two groups down, and the middle group
    /// holds no parameter of its own.
    fn nested(session: &mut Session) -> Nested {
        let named = |n: &str| Config::new().with("name", Value::Text(n.into()));
        let outer = add(session, Node::new(GROUP).with_config(named("Outer")));
        let mid = add(
            session,
            Node::new(GROUP).with_config(named("Mid")).in_group(outer),
        );
        let inner = add(
            session,
            Node::new(GROUP).with_config(named("Inner")).in_group(mid),
        );
        let saw = add(session, Node::new("noodle.osc.saw").in_group(outer));
        let gain = add(session, Node::new("noodle.util.gain").in_group(inner));
        let boundary = |session: &mut Session, parent: NodeId, kind: &str, port: &str| {
            let node = Node::new(kind)
                .with_config(ports_config(&[(1, port.into())]))
                .in_group(parent);
            add(session, node)
        };
        let outer_out = boundary(session, outer, GROUP_OUTPUT, "out");
        let mid_in = boundary(session, mid, GROUP_INPUT, "in");
        let mid_out = boundary(session, mid, GROUP_OUTPUT, "out");
        let inner_in = boundary(session, inner, GROUP_INPUT, "in");
        let inner_out = boundary(session, inner, GROUP_OUTPUT, "out");
        wire(session, (saw, "out"), (mid, "in"));
        let top = add(session, Node::new("noodle.util.gain").in_group(outer));
        wire(session, (mid, "out"), (top, "in"));
        wire(session, (top, "out"), (outer_out, "p1"));
        wire(session, (mid_in, "p1"), (inner, "in"));
        wire(session, (inner, "out"), (mid_out, "p1"));
        wire(session, (inner_in, "p1"), (gain, "in"));
        wire(session, (gain, "out"), (inner_out, "p1"));
        let output = add(session, Node::new(OUTPUT_ID));
        wire(session, (outer, "out"), (output, "in"));
        Nested {
            outer,
            top,
            mid,
            gain,
        }
    }

    #[test]
    fn a_morph_reaches_parameters_in_nested_groups_through_group_inputs() {
        let mut session = Session::new(Nodes::all());
        let n = nested(&mut session);
        set(&mut session, n.top, "gain", -6.0);
        set(&mut session, n.gain, "gain", -12.0);
        let a = GroupPreset::capture(session.project().graph(), n.outer, "A");
        set(&mut session, n.top, "gain", -1.0);
        set(&mut session, n.gain, "gain", -3.0);
        let b = GroupPreset::capture(session.project().graph(), n.outer, "B");
        let reference = |preset: &GroupPreset| {
            let mut s = Session::new(Nodes::all());
            let r = nested(&mut s);
            let command = preset
                .apply(s.project().graph(), r.outer, "x", &|n: &Node| {
                    noodle_engine::Registry::with_builtins().layout_signature(n)
                })
                .unwrap();
            s.edit([Edit::Apply(command)]);
            render(&s)
        };
        let (want_a, want_b) = (reference(&a), reference(&b));

        let ids = test_ids(&mut session, n.outer);
        let morphed = command(
            session.project(),
            session.registry(),
            n.outer,
            ("A", &a),
            &[&b],
            ids,
        )
        .unwrap();
        assert_eq!(morphed.wired, 2, "{:?}", morphed.left_out);
        assert!(morphed.left_out.is_empty(), "{:?}", morphed.left_out);
        let before = session.project().clone();
        session.edit([Edit::Apply(morphed.command)]);
        let after = session.project().clone();
        // One undo step, though it touches three groups.
        session.undo();
        assert_eq!(session.project().graph(), before.graph());
        session.redo();
        assert_eq!(session.project().graph(), after.graph());

        let graph = session.project().graph();
        let morphs: Vec<NodeId> = graph
            .nodes()
            .filter(|(_, n)| n.type_id == MORPH_ID)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            morphs.len(),
            2,
            "one in the outer group, one beside the inner gain"
        );
        // The middle group only passes the control down: an input, no Morph.
        assert!(graph.group_ports(n.mid).input("morph").is_some());
        assert!(
            graph
                .children(Some(n.mid))
                .all(|(_, node)| node.type_id != MORPH_ID)
        );
        let inner_morph = *morphs
            .iter()
            .find(|&&m| graph.node(m).unwrap().parent != Some(n.outer))
            .unwrap();
        let top_morph = *morphs.iter().find(|&&m| m != inner_morph).unwrap();
        assert_eq!(
            graph.source(&Endpoint::new(n.gain, "gain")).unwrap().node,
            inner_morph
        );
        assert_eq!(
            graph.source(&Endpoint::new(n.top, "gain")).unwrap().node,
            top_morph
        );
        let close = |x: &[f32], y: &[f32]| {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| (p - q).abs() < 1e-3)
        };
        set(&mut session, top_morph, "position", 0.0);
        let at_a = render(&session);
        set(&mut session, top_morph, "position", 1.0);
        let at_b = render(&session);
        assert!(close(&at_a, &want_a), "position 0 isn't preset A");
        assert!(close(&at_b, &want_b), "position 1 isn't preset B");
        assert!(!close(&at_a, &at_b));
    }

    #[test]
    fn the_notice_names_the_controls_the_morph_took_over() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        // Cutoff is set by a control of the group, which nothing outside feeds.
        let input = add(
            &mut session,
            Node::new(GROUP_INPUT)
                .with_config(ports_config(&[(1, "Brightness".into())]))
                .in_group(lead.group),
        );
        wire(&mut session, (input, "p1"), (lead.filter, "cutoff"));
        let (a, b) = presets(&mut session, &lead);
        let morphed = morph_edit(&mut session, &lead, &a, &b);
        assert_eq!(morphed.taken_over, vec!["Brightness".to_owned()]);
        assert!(
            morphed.notice().contains("Brightness"),
            "{}",
            morphed.notice()
        );
    }

    #[test]
    fn identical_presets_have_nothing_to_morph() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        let a = GroupPreset::capture(session.project().graph(), lead.group, "A");
        let ids = test_ids(&mut session, lead.group);
        let result = command(
            session.project(),
            session.registry(),
            lead.group,
            ("A", &a),
            &[&a],
            ids,
        );
        assert!(result.is_err());
    }

    #[test]
    fn four_presets_make_a_pad_with_two_group_inputs() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        let (a, b) = presets(&mut session, &lead);
        set(&mut session, lead.filter, "cutoff", 8000.0);
        set(&mut session, lead.gain, "gain", -20.0);
        let c = GroupPreset::capture(session.project().graph(), lead.group, "C");
        set(&mut session, lead.filter, "cutoff", 100.0);
        set(&mut session, lead.gain, "gain", 0.0);
        let d = GroupPreset::capture(session.project().graph(), lead.group, "D");
        let ids = test_ids(&mut session, lead.group);
        let morphed = command(
            session.project(),
            session.registry(),
            lead.group,
            ("A", &a),
            &[&b, &c, &d],
            ids,
        )
        .unwrap();
        session.edit([Edit::Apply(morphed.command)]);
        let graph = session.project().graph();
        let ports = graph.group_ports(lead.group);
        assert!(ports.input("morph").is_some() && ports.input("morph y").is_some());
        let morph = morph_node(&session);
        let y_source = graph.source(&Endpoint::new(morph, "y")).unwrap();
        assert_eq!(graph.node(y_source.node).unwrap().type_id, GROUP_INPUT);
        let node = graph.node(morph).unwrap();
        let rows = noodle_nodes::morph_rows_of(&node.config);
        assert_eq!(rows.len(), 2);
        // The gain row is an offset from A's value, in fractions of the
        // 84 dB travel: -12, -3, -20 and 0 dB at the four corners.
        let gain = rows.iter().find(|r| r.name.contains("Gain")).unwrap();
        let corners = [gain.a, gain.b, gain.c, gain.d].map(|t| gain.at_position(t) * 84.0);
        for (got, want) in corners.iter().zip([0.0, 9.0, -8.0, 12.0]) {
            assert!((got - want).abs() < 0.05, "{corners:?}");
        }
    }

    #[test]
    fn it_morphs_the_first_two_presets_of_a_library_synth() {
        let mut session = Session::new(Nodes::all());
        let template = session
            .library()
            .templates()
            .find(|(_, t)| t.name == "Lead Synth")
            .map(|(id, t)| (id.to_string(), t.clone()))
            .unwrap();
        let mut next = session.new_node_id().0;
        let mut ids = || {
            let id = NodeId(next);
            next += 1;
            id
        };
        let (group, add) =
            template
                .1
                .instantiate(&template.0, None, Position { x: 0.0, y: 0.0 }, &mut ids);
        session.edit([Edit::Apply(add)]);
        let presets: Vec<String> = session
            .presets_for(group)
            .into_iter()
            .take(2)
            .map(|(id, _)| id.to_owned())
            .collect();
        session.edit([Edit::MorphPresets { group, presets }]);
        assert!(
            session
                .project()
                .graph()
                .nodes()
                .any(|(_, n)| n.type_id == MORPH_ID),
            "{:?}",
            session.message()
        );
        // The voices sit in a nested group; it has its own Morph, fed from
        // the group's `morph` input, so the notice no longer says some
        // parameters were left behind.
        let graph = session.project().graph();
        assert!(
            graph
                .nodes()
                .any(|(_, n)| n.type_id == MORPH_ID && n.parent != Some(group)),
            "no Morph inside a nested group"
        );
        assert!(
            !session.message().unwrap_or_default().contains("nested"),
            "{:?}",
            session.message()
        );
    }
}
