use std::collections::HashMap;

use noodle_core::{Command, Connection, Node, Project};

use super::*;
use crate::{Context, Instance, Io, NodeInfo, ParamInfo, Setup};

/// A node type for tests: a fixed layout, and optionally a fixed output shape
/// instead of broadcasting. Config `broken` makes its layout fail.
struct TestType {
    info: NodeInfo,
    layout: Layout,
    output: Option<Shape>,
}

impl NodeType for TestType {
    fn info(&self) -> &NodeInfo {
        &self.info
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        match config.get("broken") {
            Some(_) => Err(NodeError::config("broken on purpose")),
            None => Ok(self.layout.clone()),
        }
    }

    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = match self.output {
            Some(shape) => shape,
            None => Shape::broadcast_all(inputs.iter().copied())?,
        };
        Ok(vec![shape; layout.outputs.len()])
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(Silent))
    }
}

struct Silent;

impl crate::Node for Silent {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let mut add = |id: &'static str, layout: Layout, output: Option<Shape>| {
        registry.register(TestType {
            info: NodeInfo {
                id,
                version: 1,
                name: id,
                category: "Test",
            },
            layout,
            output,
        });
    };
    let out = || Layout::realtime().output("out", "Out");
    add("source", out(), None);
    add("poly8", out(), Some(Shape::new(8, 1)));
    add("poly4", out(), Some(Shape::new(4, 1)));
    add(
        "thru",
        Layout::realtime()
            .input("in", "In")
            .param("amount", "Amount", ParamInfo::new(0.0, 1.0, 0.5))
            .output("out", "Out"),
        None,
    );
    add(
        "sum",
        Layout::realtime()
            .input("a", "A")
            .input("b", "B")
            .output("out", "Out"),
        None,
    );
    add(
        "notes",
        Layout::realtime()
            .event_input("notes", "Notes")
            .output("out", "Out"),
        None,
    );
    add(
        "offline",
        Layout::offline().input("in", "In").output("out", "Out"),
        None,
    );
    registry
}

fn add(project: &mut Project, node: Node) -> NodeId {
    let id = project.new_node_id();
    Command::AddNode { id, node }.apply(project).unwrap();
    id
}

fn wire(project: &mut Project, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
    let connection = Connection {
        from: Endpoint::new(from, from_port),
        to: Endpoint::new(to, to_port),
    };
    Command::Connect(connection).apply(project).unwrap();
}

/// Compiles and checks that the schedule is valid (see [`check`]).
fn compile_checked(project: &Project) -> (Schedule, Vec<Diagnostic>) {
    let (schedule, diagnostics) = compile(project.graph(), &registry());
    check(project.graph(), &schedule, &diagnostics);
    (schedule, diagnostics)
}

/// Checks a schedule by simulating it: every buffer an input reads must
/// still hold the output it's wired to, outputs must not share buffers with
/// the node's inputs or each other, and buffers must be big enough.
fn check(graph: &Graph, schedule: &Schedule, diagnostics: &[Diagnostic]) {
    let scheduled: HashMap<NodeId, &ScheduledNode> =
        schedule.nodes.iter().map(|n| (n.id, n)).collect();
    let mut holds: HashMap<BufferId, Endpoint> = HashMap::new();

    for node in &schedule.nodes {
        for (port, source) in node.layout.inputs.iter().zip(&node.inputs) {
            let input = Endpoint::new(node.id, port.key.as_ref());
            match source {
                InputSource::Buffer(b) => {
                    let wired = graph.source(&input).expect("buffer input must be wired");
                    assert_eq!(holds.get(b), Some(wired), "{input} read a stale buffer");
                }
                InputSource::Value(_) => {
                    // A wire from a scheduled node can only be ignored if a
                    // diagnostic says why.
                    if let Some(wired) = graph.source(&input)
                        && scheduled.contains_key(&wired.node)
                    {
                        assert!(
                            diagnostics
                                .iter()
                                .any(|d| d.location == Location::Wire(input.clone())),
                            "{input} ignored its wire without a diagnostic"
                        );
                    }
                }
            }
        }

        let mut outputs = node.outputs.clone();
        outputs.sort();
        outputs.dedup();
        assert_eq!(outputs.len(), node.outputs.len(), "outputs share a buffer");
        for (b, (port, shape)) in node
            .outputs
            .iter()
            .zip(node.layout.outputs.iter().zip(&node.output_shapes))
        {
            assert!(
                !node.inputs.contains(&InputSource::Buffer(*b)),
                "output aliases an input"
            );
            assert!(
                schedule.buffer_lanes[b.0] >= shape.lanes(),
                "buffer too small"
            );
            holds.insert(*b, Endpoint::new(node.id, port.key.as_ref()));
        }
    }
}

fn order(schedule: &Schedule) -> Vec<NodeId> {
    schedule.nodes.iter().map(|n| n.id).collect()
}

fn find(schedule: &Schedule, id: NodeId) -> &ScheduledNode {
    schedule.nodes.iter().find(|n| n.id == id).unwrap()
}

#[test]
fn orders_nodes_by_dependency() {
    let mut p = Project::new();
    let c = add(&mut p, Node::new("thru"));
    let b = add(&mut p, Node::new("thru"));
    let a = add(&mut p, Node::new("source"));
    wire(&mut p, a, "out", b, "in");
    wire(&mut p, b, "out", c, "in");
    let (schedule, diagnostics) = compile_checked(&p);
    assert_eq!(order(&schedule), [a, b, c]);
    assert!(diagnostics.is_empty());
}

#[test]
fn reuses_buffers_along_a_chain() {
    let mut p = Project::new();
    let mut previous = add(&mut p, Node::new("source"));
    for _ in 0..5 {
        let next = add(&mut p, Node::new("thru"));
        wire(&mut p, previous, "out", next, "in");
        previous = next;
    }
    let (schedule, _) = compile_checked(&p);
    assert_eq!(schedule.buffer_lanes.len(), 2);
}

#[test]
fn keeps_a_buffer_until_its_last_reader() {
    let mut p = Project::new();
    let source = add(&mut p, Node::new("source"));
    let a = add(&mut p, Node::new("thru"));
    let b = add(&mut p, Node::new("thru"));
    let c = add(&mut p, Node::new("thru"));
    wire(&mut p, source, "out", a, "in");
    wire(&mut p, a, "out", b, "in");
    wire(&mut p, source, "out", c, "in");
    // `check` would fail if b's output overwrote the source before c ran.
    compile_checked(&p);
}

#[test]
fn unconnected_inputs_hold_project_values_or_defaults() {
    let mut p = Project::new();
    let set = add(&mut p, Node::new("thru").with_param("amount", 0.25));
    let default = add(&mut p, Node::new("thru"));
    let (schedule, _) = compile_checked(&p);
    assert_eq!(
        find(&schedule, set).inputs,
        [InputSource::Value(0.0), InputSource::Value(0.25)]
    );
    assert_eq!(find(&schedule, default).inputs[1], InputSource::Value(0.5));
}

#[test]
fn broadcasts_shapes_downstream() {
    let mut p = Project::new();
    let poly = add(&mut p, Node::new("poly8"));
    let lfo = add(&mut p, Node::new("source"));
    let thru = add(&mut p, Node::new("thru"));
    wire(&mut p, poly, "out", thru, "in");
    wire(&mut p, lfo, "out", thru, "amount");
    let (schedule, _) = compile_checked(&p);
    let thru = find(&schedule, thru);
    assert_eq!(thru.input_shapes, [Shape::new(8, 1), Shape::MONO]);
    assert_eq!(thru.output_shapes, [Shape::new(8, 1)]);
}

#[test]
fn blames_the_wire_that_breaks_broadcasting() {
    let mut p = Project::new();
    let eight = add(&mut p, Node::new("poly8"));
    let four = add(&mut p, Node::new("poly4"));
    let sum = add(&mut p, Node::new("sum"));
    let after = add(&mut p, Node::new("thru"));
    wire(&mut p, eight, "out", sum, "a");
    wire(&mut p, four, "out", sum, "b");
    wire(&mut p, sum, "out", after, "in");
    let (schedule, diagnostics) = compile_checked(&p);

    assert_eq!(
        diagnostics,
        [Diagnostic::wire(
            &Endpoint::new(sum, "b"),
            Problem::Shape(ShapeError(Shape::new(8, 1), Shape::new(4, 1)))
        )]
    );
    assert!(!order(&schedule).contains(&sum));
    // Downstream of the broken node acts as unconnected.
    assert_eq!(find(&schedule, after).inputs[0], InputSource::Value(0.0));
}

#[test]
fn drops_a_wire_that_closes_a_loop() {
    let mut p = Project::new();
    let a = add(&mut p, Node::new("thru"));
    let b = add(&mut p, Node::new("thru"));
    wire(&mut p, a, "out", b, "in");
    wire(&mut p, b, "out", a, "in");
    let (schedule, diagnostics) = compile_checked(&p);
    assert_eq!(order(&schedule), [a, b]);
    assert_eq!(
        diagnostics,
        [Diagnostic::wire(&Endpoint::new(a, "in"), Problem::Loop)]
    );
}

#[test]
fn reports_unknown_types_ports_and_params() {
    let mut p = Project::new();
    let ghost = add(&mut p, Node::new("ghost"));
    let source = add(&mut p, Node::new("source"));
    let thru = add(&mut p, Node::new("thru").with_param("volume", 1.0));
    wire(&mut p, ghost, "out", thru, "amount");
    wire(&mut p, source, "nope", thru, "in");
    let (_, diagnostics) = compile_checked(&p);
    assert_eq!(
        diagnostics,
        [
            Diagnostic::node(ghost, Problem::UnknownNodeType("ghost".into())),
            Diagnostic::node(thru, Problem::UnknownParam("volume".into())),
            Diagnostic::wire(
                &Endpoint::new(thru, "in"),
                Problem::UnknownPort(Endpoint::new(source, "nope"))
            ),
        ]
    );
}

#[test]
fn rejects_wires_between_audio_and_event_ports() {
    let mut p = Project::new();
    let source = add(&mut p, Node::new("source"));
    let notes = add(&mut p, Node::new("notes"));
    wire(&mut p, source, "out", notes, "notes");
    let (schedule, diagnostics) = compile_checked(&p);
    assert_eq!(
        diagnostics,
        [Diagnostic::wire(
            &Endpoint::new(notes, "notes"),
            Problem::KindMismatch
        )]
    );
    assert_eq!(find(&schedule, notes).event_inputs, [None]);
}

#[test]
fn leaves_out_offline_nodes_and_broken_config() {
    let mut p = Project::new();
    let offline = add(&mut p, Node::new("offline"));
    let config = Config::new().with("broken", noodle_core::Value::Bool(true));
    let broken = add(&mut p, Node::new("source").with_config(config));
    let (schedule, diagnostics) = compile_checked(&p);
    assert!(schedule.nodes.is_empty());
    assert_eq!(
        diagnostics,
        [
            Diagnostic::node(offline, Problem::OfflineUnsupported),
            Diagnostic::node(
                broken,
                Problem::Node(NodeError::config("broken on purpose"))
            ),
        ]
    );
}

/// A small, seeded random number generator, so failures reproduce.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[test]
fn random_graphs_compile_to_valid_schedules() {
    let types = ["source", "poly8", "poly4", "thru", "sum"];
    let inputs: HashMap<&str, &[&str]> = HashMap::from([
        ("source", &[][..]),
        ("poly8", &[][..]),
        ("poly4", &[][..]),
        ("thru", &["in", "amount"][..]),
        ("sum", &["a", "b"][..]),
    ]);
    let mut rng = XorShift(0x5eed);

    for _ in 0..300 {
        let mut p = Project::new();
        let nodes: Vec<(NodeId, &str)> = (0..2 + rng.below(30))
            .map(|_| {
                let type_id = types[rng.below(types.len())];
                (add(&mut p, Node::new(type_id)), type_id)
            })
            .collect();
        for _ in 0..rng.below(nodes.len() * 2) {
            let (from, _) = nodes[rng.below(nodes.len())];
            let (to, to_type) = nodes[rng.below(nodes.len())];
            let ports = inputs[to_type];
            if !ports.is_empty() {
                wire(&mut p, from, "out", to, ports[rng.below(ports.len())]);
            }
        }
        compile_checked(&p);
    }
}
