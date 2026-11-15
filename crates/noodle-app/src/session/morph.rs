//! Morph between presets: a graph edit, nothing more. It applies preset A to
//! a group, adds one Morph node inside it with an output wired into every
//! parameter whose value differs between A and B, and exposes the node's
//! `position` as a group input named `morph`, all as one undo step.
//!
//! Nothing is modulated unseen: the wires are real, and a parameter that
//! already has a wire or a lane stays out (two sources into one input were
//! never allowed), as does all config. Taking a parameter out of the morph is
//! deleting its wire.

use noodle_core::group::{GROUP, GROUP_INPUT};
use noodle_core::library::GroupPreset;
use noodle_core::spare::{self, Side};
use noodle_core::{Command, Endpoint, Frame, FrameId, Node, NodeId, Position, Project};
use noodle_engine::{InputKind, ParamKind, Registry};
use noodle_nodes::{MORPH_ID, MORPH_POSITION, MorphRow, morph_config, morph_output_key};

/// The name of the group input the node's `position` is exposed as.
pub const MORPH_INPUT: &str = "morph";

/// The IDs the command's new things take.
pub struct Ids {
    pub morph: NodeId,
    pub frame: FrameId,
    /// The group's input boundary node, if it needs one made.
    pub boundary: NodeId,
}

/// What the command did, and what it left out and why.
pub struct Morphed {
    pub command: Command,
    /// How many parameters the Morph drives.
    pub wired: usize,
    pub left_out: Vec<String>,
}

impl Morphed {
    /// One line for the status bar.
    pub fn notice(&self) -> String {
        let mut text = format!("Morphing {} parameters.", self.wired);
        if !self.left_out.is_empty() {
            text.push_str(&format!(" Left out: {}.", self.left_out.join("; ")));
        }
        text
    }
}

/// The command that morphs `group` between presets `a` and `b`. `entry_a` is
/// the library ID of `a`, recorded as the group's preset.
pub fn command(
    project: &Project,
    registry: &Registry,
    group: NodeId,
    a: (&str, &GroupPreset),
    b: &GroupPreset,
    ids: &Ids,
) -> Result<Morphed, String> {
    let graph = project.graph();
    let group_node = graph.node(group).ok_or("the group is gone")?;
    if group_node.type_id != GROUP {
        return Err("only a group can morph between presets".to_owned());
    }
    let layout_of = |node: &Node| registry.layout_signature(node);
    let apply_a =
        a.1.apply(graph, group, a.0, &layout_of)
            .map_err(|e| e.to_string())?;
    if !b.applies_to(graph, group, &layout_of) {
        return Err("the second preset is for a differently built group".to_owned());
    }

    let mut rows: Vec<(Endpoint, MorphRow)> = Vec::new();
    let mut left_out = Vec::new();
    for (id, va, vb) in a.1.paired(b, graph, group) {
        let node = graph.node(id).ok_or("a node is gone")?;
        let node_type = registry
            .get(&node.type_id)
            .ok_or("a node's type is unknown")?;
        let layout = node_type.layout(&node.config).map_err(|e| e.to_string())?;
        let label = node_type.info().name;
        if va.config != vb.config {
            left_out.push(format!("{label}'s config stays as in the first preset"));
        }
        let mut keys: Vec<&String> = va.params.keys().chain(vb.params.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            let Some(input) = layout.inputs.iter().find(|i| i.key == *key) else {
                continue;
            };
            let InputKind::Param(info) = &input.kind else {
                continue;
            };
            let from = va.params.get(key).copied().unwrap_or(info.default);
            let to = vb.params.get(key).copied().unwrap_or(info.default);
            if from.to_bits() == to.to_bits() {
                continue;
            }
            let target = Endpoint::new(id, key.clone());
            let name = format!("{label} {}", input.name);
            if graph.source(&target).is_some() {
                left_out.push(format!("{name} has a wire"));
                continue;
            }
            if project.lane_for(&target).is_some() {
                left_out.push(format!("{name} has an automation lane"));
                continue;
            }
            let row = if info.is_offset() {
                // A wire moves the value along its travel from the base,
                // which preset A has just set: 0.5 is no move.
                let travel = (info.position(to) - info.position(from)).clamp(-1.0, 1.0);
                MorphRow {
                    name,
                    min: -1.0,
                    max: 1.0,
                    log: false,
                    a: 0.5,
                    b: 0.5 + 0.5 * travel,
                    stepped: false,
                }
            } else {
                let (min, max, log, _) = super::automate::range_and_start(info, from);
                MorphRow {
                    name,
                    min,
                    max,
                    log,
                    a: info.position(from),
                    b: info.position(to),
                    stepped: matches!(info.kind, ParamKind::Stepped { .. }),
                }
            };
            rows.push((target, row));
        }
    }
    if rows.is_empty() {
        return Err("the two presets don't differ in anything that can be morphed".to_owned());
    }

    // Left of what is inside, in a frame.
    let (x, y) = graph
        .children(Some(group))
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
    morph.parent = Some(group);

    let mut commands = vec![apply_a];
    commands.push(Command::AddNode {
        id: ids.morph,
        node: morph,
    });
    commands.push(Command::AddFrame {
        id: ids.frame,
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

    // The group input: `morph`, or the first free `morph 2`, ...
    let taken = graph.group_ports(group);
    let name = std::iter::once(MORPH_INPUT.to_owned())
        .chain((2..).map(|i| format!("{MORPH_INPUT} {i}")))
        .find(|n| taken.input(n).is_none())
        .expect("an unbounded range has a free name");
    let (add_port, port_key) =
        spare::add_group_port(graph, group, Side::Input, &name, ids.boundary);
    commands.push(add_port);
    let boundary = graph
        .children(Some(group))
        .find(|(_, n)| n.type_id == GROUP_INPUT)
        .map_or(ids.boundary, |(id, _)| id);
    commands.push(spare::wire(
        Endpoint::new(boundary, port_key),
        Endpoint::new(ids.morph, MORPH_POSITION),
    ));
    for (i, (target, _)) in rows.iter().enumerate() {
        commands.push(spare::wire(
            Endpoint::new(ids.morph, morph_output_key(i)),
            target.clone(),
        ));
    }
    Ok(Morphed {
        command: Command::Batch(commands),
        wired: rows.len(),
        left_out,
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

    fn morph_edit(session: &mut Session, lead: &Lead, a: &GroupPreset, b: &GroupPreset) -> Morphed {
        let ids = Ids {
            morph: session.new_node_id(),
            frame: session.new_frame_id(),
            boundary: session.new_node_id(),
        };
        let morphed = command(
            session.project(),
            session.registry(),
            lead.group,
            ("A", a),
            b,
            &ids,
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

    #[test]
    fn identical_presets_have_nothing_to_morph() {
        let mut session = Session::new(Nodes::all());
        let lead = lead(&mut session);
        let a = GroupPreset::capture(session.project().graph(), lead.group, "A");
        let ids = Ids {
            morph: session.new_node_id(),
            frame: session.new_frame_id(),
            boundary: session.new_node_id(),
        };
        let result = command(
            session.project(),
            session.registry(),
            lead.group,
            ("A", &a),
            &a,
            &ids,
        );
        assert!(result.is_err());
    }
}
