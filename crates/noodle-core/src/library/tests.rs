use super::*;
use crate::group::{GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, MUTE, PORT_NAME, SOLO};
use crate::{
    Command, Config, Connection, Endpoint, History, Node, NodeId, Position, Project, Value,
};

/// Layout for tests: the type plus its `inputs` config, like a Mix.
fn layout(node: &Node) -> Option<String> {
    let inputs = node.config.get("inputs").cloned().unwrap_or(Value::Int(0));
    Some(format!("{}/{inputs:?}", node.type_id))
}

fn named(kind: &str, name: &str) -> Node {
    Node::new(kind).with_config(Config::new().with(PORT_NAME, Value::Text(name.into())))
}

/// A group `Synth` with an input `pitch`, an oscillator, a filter and an
/// output `out`. `swap` creates the nodes in the other order, so IDs differ.
fn synth(project: &mut Project, history: &mut History, cutoff: f32, swap: bool) -> NodeId {
    let ids: Vec<NodeId> = (0..5).map(|_| project.new_node_id()).collect();
    let group = ids[0];
    let (a, b, c, d) = if swap {
        (ids[4], ids[3], ids[2], ids[1])
    } else {
        (ids[1], ids[2], ids[3], ids[4])
    };
    let (input, osc, filter, output) = (a, b, c, d);
    let mut commands = vec![
        Command::AddNode {
            id: group,
            node: Node::new(GROUP)
                .with_config(Config::new().with(PORT_NAME, Value::Text("Synth".into())))
                .at(1000.0, 500.0),
        },
        Command::AddNode {
            id: input,
            node: named(GROUP_INPUT, "pitch").in_group(group).at(900.0, 500.0),
        },
        Command::AddNode {
            id: osc,
            node: Node::new("osc").in_group(group).at(1100.0, 500.0),
        },
        Command::AddNode {
            id: filter,
            node: Node::new("filter")
                .with_param("cutoff", cutoff)
                .in_group(group),
        },
        Command::AddNode {
            id: output,
            node: named(GROUP_OUTPUT, "out")
                .with_param(GAIN, -6.0)
                .with_param(MUTE, 1.0)
                .with_param(SOLO, 1.0)
                .in_group(group),
        },
    ];
    for (from, port, to, key) in [
        (input, "out", osc, "frequency"),
        (osc, "out", filter, "in"),
        (filter, "out", output, "in"),
    ] {
        commands.push(Command::Connect(Connection {
            from: Endpoint::new(from, port),
            to: Endpoint::new(to, key),
        }));
    }
    history.apply(project, Command::Batch(commands)).unwrap();
    group
}

fn setup() -> (Project, History) {
    (Project::new(), History::new())
}

#[test]
fn a_template_drops_the_outside_and_the_boundary_controls() {
    let (mut project, mut history) = setup();
    let group = synth(&mut project, &mut history, 800.0, false);
    // A wire from outside into the group, and a lane on the output.
    let outside = project.new_node_id();
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddNode {
                    id: outside,
                    node: Node::new("osc"),
                },
                Command::Connect(Connection {
                    from: Endpoint::new(outside, "out"),
                    to: Endpoint::new(group, "pitch"),
                }),
            ]),
        )
        .unwrap();
    let template = GroupTemplate::from_group(project.graph(), group, "Synth").unwrap();
    let graph = template.graph();
    assert_eq!(graph.nodes().count(), 5);
    assert_eq!(graph.connections().count(), 3, "no wire to the outside");
    let root = graph.nodes().find(|(_, n)| n.parent.is_none()).unwrap().1;
    assert_eq!(root.position, Position::default());
    for (_, node) in graph.nodes().filter(|(_, n)| n.type_id == GROUP_OUTPUT) {
        assert!(node.params.is_empty(), "gain, mute and solo are not saved");
    }
}

#[test]
fn using_a_template_adds_an_unlinked_copy_that_remembers_its_source() {
    let (mut project, mut history) = setup();
    let group = synth(&mut project, &mut history, 800.0, false);
    let template = GroupTemplate::from_group(project.graph(), group, "Synth").unwrap();
    let before = project.graph().nodes().count();
    let (copy, command) = template.instantiate(
        "user:template:synth",
        None,
        Position { x: 50.0, y: 60.0 },
        || project.new_node_id(),
    );
    history.apply(&mut project, command).unwrap();
    let graph = project.graph();
    assert_eq!(graph.nodes().count(), before + 5);
    let node = graph.node(copy).unwrap();
    assert_eq!(node.template.as_deref(), Some("user:template:synth"));
    assert_eq!(node.position, Position { x: 50.0, y: 60.0 });
    assert_eq!(graph.group_ports(copy).inputs[0].name, "pitch");
    assert_eq!(graph.descendants(copy).len(), 4);
    // The copy is a fresh group: editing it leaves the original alone.
    let osc = graph
        .descendants(copy)
        .into_iter()
        .find(|&id| graph.node(id).unwrap().type_id == "filter")
        .unwrap();
    history
        .apply(
            &mut project,
            Command::SetParam {
                node: osc,
                key: "cutoff".into(),
                value: Some(1.0),
            },
        )
        .unwrap();
    let original = GroupTemplate::from_group(project.graph(), group, "Synth").unwrap();
    assert_eq!(original, template);
    // One undo step removes all of it.
    history.undo(&mut project).unwrap();
    history.undo(&mut project).unwrap();
    assert_eq!(project.graph().nodes().count(), before);
}

#[test]
fn a_track_is_the_hidden_template_and_create_track_instantiates_it() {
    assert!(track_template().hidden);
    let library = Library::in_memory();
    assert!(library.templates().all(|(_, t)| !t.hidden), "not listed");
    assert!(library.get(TRACK).is_some(), "but there");
    let (mut project, mut history) = setup();
    let (group, command) =
        crate::group::create_track(None, Position::default(), || project.new_node_id());
    history.apply(&mut project, command).unwrap();
    let node = project.graph().node(group).unwrap();
    assert_eq!(node.template.as_deref(), Some(TRACK));
    assert_eq!(project.graph().descendants(group).len(), 3);
}

#[test]
fn topology_ignores_ids_but_not_wiring_or_layout() {
    let (mut project, mut history) = setup();
    let a = synth(&mut project, &mut history, 800.0, false);
    let b = synth(&mut project, &mut history, 100.0, true);
    let graph = project.graph();
    let (ta, tb) = (
        topology::topology(graph, a, &layout),
        topology::topology(graph, b, &layout),
    );
    assert_eq!(ta.key, tb.key, "IDs, positions and values don't count");
    // Config that shapes ports does.
    let filter = graph
        .descendants(b)
        .into_iter()
        .find(|&id| graph.node(id).unwrap().type_id == "filter")
        .unwrap();
    history
        .apply(
            &mut project,
            Command::SetConfig {
                node: filter,
                key: "inputs".into(),
                value: Some(Value::Int(3)),
            },
        )
        .unwrap();
    let changed = topology::topology(project.graph(), b, &layout);
    assert_ne!(ta.key, changed.key);
    // So does one more wire between the same nodes.
    let c = synth(&mut project, &mut history, 800.0, false);
    let graph = project.graph();
    let inside = |kind: &str| {
        graph
            .descendants(c)
            .into_iter()
            .find(|&id| graph.node(id).unwrap().type_id == kind)
            .unwrap()
    };
    let wire = Command::Connect(Connection {
        from: Endpoint::new(inside("osc"), "out"),
        to: Endpoint::new(inside("filter"), "cutoff"),
    });
    history.apply(&mut project, wire).unwrap();
    assert_ne!(ta.key, topology::topology(project.graph(), c, &layout).key);
}

#[test]
fn symmetric_nodes_are_ordered_by_id() {
    let (mut project, mut history) = setup();
    let make = |project: &mut Project, history: &mut History| {
        let group = project.new_node_id();
        let (x, y, out) = (
            project.new_node_id(),
            project.new_node_id(),
            project.new_node_id(),
        );
        history
            .apply(
                project,
                Command::Batch(vec![
                    Command::AddNode {
                        id: group,
                        node: Node::new(GROUP),
                    },
                    Command::AddNode {
                        id: x,
                        node: Node::new("osc")
                            .with_param("frequency", 1.0)
                            .in_group(group),
                    },
                    Command::AddNode {
                        id: y,
                        node: Node::new("osc")
                            .with_param("frequency", 2.0)
                            .in_group(group),
                    },
                    Command::AddNode {
                        id: out,
                        node: Node::new("mix")
                            .with_config(Config::new().with("inputs", Value::Int(2)))
                            .in_group(group),
                    },
                ]),
            )
            .unwrap();
        (group, x, y)
    };
    let (g, x, y) = make(&mut project, &mut history);
    let order = topology::topology(project.graph(), g, &layout).order;
    let at = |id| order.iter().position(|&o| o == id).unwrap();
    assert!(at(x) < at(y), "the lower ID of two twins comes first");
}

#[test]
fn a_node_preset_sets_stored_values_only_and_is_one_undo_step() {
    let (mut project, mut history) = setup();
    let id = project.new_node_id();
    let other = project.new_node_id();
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddNode {
                    id,
                    node: Node::new("filter")
                        .with_param("cutoff", 300.0)
                        .with_param("resonance", 0.9)
                        .at(5.0, 5.0),
                },
                Command::AddNode {
                    id: other,
                    node: Node::new("osc"),
                },
                Command::Connect(Connection {
                    from: Endpoint::new(other, "out"),
                    to: Endpoint::new(id, "in"),
                }),
            ]),
        )
        .unwrap();
    let source = Node::new("filter").with_param("cutoff", 1200.0);
    let preset = NodePreset::capture(&source, &layout(&source).unwrap(), "Open");
    let command = preset
        .apply(project.graph(), id, "user:node:open", &layout)
        .unwrap();
    history.apply(&mut project, command).unwrap();
    let node = project.graph().node(id).unwrap();
    assert_eq!(node.params.get("cutoff"), Some(&1200.0));
    assert!(
        !node.params.contains_key("resonance"),
        "back to its default"
    );
    assert_eq!(node.preset.as_deref(), Some("user:node:open"));
    assert_eq!(node.position, Position { x: 5.0, y: 5.0 });
    assert_eq!(project.graph().connections().count(), 1, "wires stay");
    assert!(preset.matches(node));
    // Edited since: no longer matches.
    history
        .apply(
            &mut project,
            Command::SetParam {
                node: id,
                key: "cutoff".into(),
                value: Some(1.0),
            },
        )
        .unwrap();
    assert!(!preset.matches(project.graph().node(id).unwrap()));
    history.undo(&mut project).unwrap();
    history.undo(&mut project).unwrap();
    let node = project.graph().node(id).unwrap();
    assert_eq!(node.params.get("cutoff"), Some(&300.0));
    assert_eq!(node.preset, None);
}

#[test]
fn a_node_preset_is_refused_for_another_type_or_layout() {
    let (mut project, mut history) = setup();
    let id = project.new_node_id();
    let node = Node::new("mix").with_config(Config::new().with("inputs", Value::Int(2)));
    history
        .apply(&mut project, Command::AddNode { id, node })
        .unwrap();
    let wrong_type = NodePreset::capture(&Node::new("filter"), "filter/Int(0)", "x");
    assert_eq!(
        wrong_type.apply(project.graph(), id, "e", &layout),
        Err(PresetError::WrongNode)
    );
    let three = Node::new("mix").with_config(Config::new().with("inputs", Value::Int(3)));
    let wrong_ports = NodePreset::capture(&three, &layout(&three).unwrap(), "x");
    assert!(!wrong_ports.applies_to(project.graph().node(id).unwrap(), &layout));
}

#[test]
fn a_group_preset_applies_across_ids_and_skips_boundary_controls() {
    let (mut project, mut history) = setup();
    let a = synth(&mut project, &mut history, 1500.0, false);
    let b = synth(&mut project, &mut history, 100.0, true);
    // The two groups' outputs have gains the preset must leave alone.
    let out = |group| {
        project
            .graph()
            .group_ports(group)
            .output("out")
            .unwrap()
            .node
    };
    let (out_a, out_b) = (out(a), out(b));
    for (node, gain) in [(out_a, -3.0), (out_b, -12.0)] {
        history
            .apply(
                &mut project,
                Command::SetParam {
                    node,
                    key: GAIN.into(),
                    value: Some(gain),
                },
            )
            .unwrap();
    }
    let preset = GroupPreset::capture(project.graph(), a, &layout, "Bright");
    assert!(preset.applies_to(project.graph(), b, &layout));
    assert!(!preset.matches(project.graph(), b, &layout));
    let command = preset
        .apply(project.graph(), b, "user:group:bright", &layout)
        .unwrap();
    history.apply(&mut project, command).unwrap();
    let graph = project.graph();
    let filter = |group| {
        graph
            .descendants(group)
            .into_iter()
            .find(|&id| graph.node(id).unwrap().type_id == "filter")
            .unwrap()
    };
    assert_eq!(graph.node(filter(b)).unwrap().params["cutoff"], 1500.0);
    assert_eq!(
        graph.node(b).unwrap().preset.as_deref(),
        Some("user:group:bright")
    );
    // The output's gain, mute and solo are not part of a preset.
    assert_eq!(graph.node(out_b).unwrap().params[GAIN], -12.0);
    assert_eq!(graph.node(out_b).unwrap().params[MUTE], 1.0);
    assert!(preset.matches(graph, b, &layout));
}

#[test]
fn a_group_preset_is_refused_by_a_different_topology() {
    let (mut project, mut history) = setup();
    let a = synth(&mut project, &mut history, 1500.0, false);
    let preset = GroupPreset::capture(project.graph(), a, &layout, "Bright");
    let other = project.new_node_id();
    history
        .apply(
            &mut project,
            Command::AddNode {
                id: other,
                node: Node::new(GROUP),
            },
        )
        .unwrap();
    assert!(!preset.applies_to(project.graph(), other, &layout));
    assert_eq!(
        preset.apply(project.graph(), other, "x", &layout),
        Err(PresetError::WrongTopology)
    );
}

#[test]
fn items_round_trip_through_ron() {
    let (mut project, mut history) = setup();
    let group = synth(&mut project, &mut history, 800.0, false);
    let items = [
        Item::Template(GroupTemplate::from_group(project.graph(), group, "Synth").unwrap()),
        Item::NodePreset(NodePreset::capture(
            &Node::new("filter").with_param("cutoff", 5.0),
            "l",
            "F",
        )),
        Item::GroupPreset(GroupPreset::capture(project.graph(), group, &layout, "G")),
    ];
    for item in items {
        assert_eq!(Item::from_ron(&item.to_ron()).unwrap(), item);
    }
}

#[test]
fn the_user_store_keeps_one_file_per_entry_and_system_entries_are_read_only() {
    let dir = std::env::temp_dir().join(format!("noodle-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut library = Library::open(Some(dir.clone()));
    let preset = Item::NodePreset(NodePreset::capture(
        &Node::new("filter"),
        "l",
        "Warm / Dark",
    ));
    let first = library.save(preset.clone()).unwrap();
    let second = library.save(preset.clone()).unwrap();
    assert_eq!(first, "user:node:warm-dark");
    assert_eq!(second, "user:node:warm-dark-2");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);

    let reopened = Library::open(Some(dir.clone()));
    assert!(reopened.node_preset(&first).is_some());
    assert!(reopened.problems.is_empty());

    // A broken file is reported and skipped.
    std::fs::write(dir.join("junk.ron"), "not ron").unwrap();
    let reopened = Library::open(Some(dir.clone()));
    assert_eq!(reopened.problems.len(), 1);
    std::fs::remove_file(dir.join("junk.ron")).unwrap();

    library.remove(&second).unwrap();
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    assert!(library.remove(TRACK).is_err());
    assert!(library.overwrite(TRACK, preset).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_copy_of_a_template_with_unnamed_ports_keeps_its_ports_and_topology() {
    let (mut project, mut history) = setup();
    let group = project.new_node_id();
    let (input, osc, output) = (
        project.new_node_id(),
        project.new_node_id(),
        project.new_node_id(),
    );
    // Boundary nodes with no `name`, as an older file may have.
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddNode {
                    id: group,
                    node: Node::new(GROUP),
                },
                Command::AddNode {
                    id: input,
                    node: Node::new(GROUP_INPUT).in_group(group),
                },
                Command::AddNode {
                    id: osc,
                    node: Node::new("osc").in_group(group),
                },
                Command::AddNode {
                    id: output,
                    node: Node::new(GROUP_OUTPUT)
                        .in_group(group)
                        .with_param(GAIN, -3.0),
                },
            ]),
        )
        .unwrap();
    let template = GroupTemplate::from_group(project.graph(), group, "Bare").unwrap();
    let (copy, command) =
        template.instantiate("user:template:bare", None, Position::default(), || {
            project.new_node_id()
        });
    history.apply(&mut project, command).unwrap();
    let graph = project.graph();
    let names = |g| -> Vec<String> {
        graph
            .group_ports(g)
            .inputs
            .iter()
            .map(|p| p.name.clone())
            .collect()
    };
    assert_eq!(names(group), names(copy));
    assert_eq!(
        topology::topology(graph, group, &layout).key,
        topology::topology(graph, copy, &layout).key
    );
}

#[test]
fn a_nested_groups_levels_are_in_a_preset_but_its_mute_and_solo_are_not() {
    let (mut project, mut history) = setup();
    let outer = project.new_node_id();
    let (inner, inner_out, osc) = (
        project.new_node_id(),
        project.new_node_id(),
        project.new_node_id(),
    );
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddNode {
                    id: outer,
                    node: Node::new(GROUP),
                },
                Command::AddNode {
                    id: inner,
                    node: Node::new(GROUP).in_group(outer),
                },
                Command::AddNode {
                    id: osc,
                    node: Node::new("osc").in_group(inner),
                },
                Command::AddNode {
                    id: inner_out,
                    node: named(GROUP_OUTPUT, "out")
                        .in_group(inner)
                        .with_param(GAIN, -9.0)
                        .with_param(MUTE, 1.0),
                },
            ]),
        )
        .unwrap();
    let preset = GroupPreset::capture(project.graph(), outer, &layout, "Layered");
    let set = |gain: f32, mute: f32| {
        Command::Batch(vec![
            Command::SetParam {
                node: inner_out,
                key: GAIN.into(),
                value: Some(gain),
            },
            Command::SetParam {
                node: inner_out,
                key: MUTE.into(),
                value: Some(mute),
            },
        ])
    };
    history.apply(&mut project, set(0.0, 0.0)).unwrap();
    let apply = preset.apply(project.graph(), outer, "x", &layout).unwrap();
    history.apply(&mut project, apply).unwrap();
    let node = project.graph().node(inner_out).unwrap();
    assert_eq!(node.params[GAIN], -9.0, "the layer's level is recalled");
    assert_eq!(node.params[MUTE], 0.0, "its mute is not");
    assert!(preset.matches(project.graph(), outer, &layout));
}

#[test]
fn files_with_a_slug_already_taken_get_their_own() {
    let dir = std::env::temp_dir().join(format!("noodle-library-slugs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut library = Library::open(Some(dir.clone()));
    let item = Item::NodePreset(NodePreset::capture(&Node::new("filter"), "l", "Warm"));
    library.save(item).unwrap();
    // The same file copied in under another name.
    std::fs::copy(dir.join("node-warm.ron"), dir.join("copy-of-warm.ron")).unwrap();
    let reopened = Library::open(Some(dir.clone()));
    let ids: Vec<&str> = reopened.node_presets().map(|(id, _)| id).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "both are reachable");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_template_keeps_a_nested_groups_level_but_not_its_mute() {
    let (mut project, mut history) = setup();
    let outer = project.new_node_id();
    let (inner, inner_out) = (project.new_node_id(), project.new_node_id());
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddNode {
                    id: outer,
                    node: Node::new(GROUP),
                },
                Command::AddNode {
                    id: inner,
                    node: Node::new(GROUP).in_group(outer),
                },
                Command::AddNode {
                    id: inner_out,
                    node: named(GROUP_OUTPUT, "out")
                        .in_group(inner)
                        .with_param(GAIN, -9.0)
                        .with_param(MUTE, 1.0),
                },
            ]),
        )
        .unwrap();
    let template = GroupTemplate::from_group(project.graph(), outer, "Layered").unwrap();
    let graph = template.graph();
    let (_, out) = graph
        .nodes()
        .find(|(_, n)| n.type_id == GROUP_OUTPUT)
        .unwrap();
    assert_eq!(out.params.get(GAIN), Some(&-9.0), "the layer's level stays");
    assert!(!out.params.contains_key(MUTE), "its mute does not");
}
