use std::collections::HashMap;

use noodle_core::{Command, Connection, Node, Project};

use super::*;
use crate::{Context, Instance, Io, NodeInfo, ParamInfo, Setup};

/// A node type for tests with a fixed layout. Config `broken` makes its
/// layout fail.
struct TestType {
    info: NodeInfo,
    layout: Layout,
    outputs: Outputs,
    loop_input: Option<&'static str>,
}

#[derive(Clone, Copy)]
enum Outputs {
    /// Every output is the broadcast of the inputs.
    Broadcast,
    Fixed(Shape),
    /// Output 0 is the broadcast of the inputs and output 1 is a stereo
    /// version of it, so the two need differently sized buffers.
    Split,
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

    fn loop_input(&self, _config: &Config) -> Option<&'static str> {
        self.loop_input
    }

    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let broadcast = Shape::broadcast_all(inputs.iter().copied())?;
        Ok(match self.outputs {
            Outputs::Broadcast => vec![broadcast; layout.outputs.len()],
            Outputs::Fixed(shape) => vec![shape; layout.outputs.len()],
            Outputs::Split => vec![broadcast, Shape::new(broadcast.voices, 2)],
        })
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
    let mut add = |id: &'static str, layout: Layout, outputs: Outputs| {
        registry.register(TestType {
            info: NodeInfo {
                id,
                version: 1,
                name: id,
                category: "Test",
            },
            layout,
            outputs,
            loop_input: None,
        });
    };
    let out = || Layout::realtime().output("out", "Out");
    add("source", out(), Outputs::Broadcast);
    add("poly8", out(), Outputs::Fixed(Shape::new(8, 1)));
    add("poly4", out(), Outputs::Fixed(Shape::new(4, 1)));
    add(
        "thru",
        Layout::realtime()
            .input("in", "In")
            .param("amount", "Amount", ParamInfo::new(0.0, 1.0, 0.5))
            .output("out", "Out"),
        Outputs::Broadcast,
    );
    add(
        "sum",
        Layout::realtime()
            .input("a", "A")
            .input("b", "B")
            .output("out", "Out"),
        Outputs::Broadcast,
    );
    add(
        "notes",
        Layout::realtime()
            .event_input("notes", "Notes")
            .output("out", "Out"),
        Outputs::Broadcast,
    );
    add(
        "offline",
        Layout::offline().input("in", "In").output("out", "Out"),
        Outputs::Broadcast,
    );
    add(
        "split",
        Layout::realtime()
            .input("in", "In")
            .output("low", "Low")
            .output("high", "High"),
        Outputs::Split,
    );
    add(
        "note_source",
        Layout::realtime().event_output("notes", "Notes"),
        Outputs::Broadcast,
    );
    add(
        "note_thru",
        Layout::realtime()
            .event_input("notes", "Notes")
            .event_output("a", "A")
            .event_output("b", "B"),
        Outputs::Broadcast,
    );
    // Like a Delay: `in` is read after the output is written.
    registry.register(TestType {
        info: NodeInfo {
            id: "delayish",
            version: 1,
            name: "delayish",
            category: "Test",
        },
        layout: Layout::realtime()
            .input("in", "In")
            .param("time", "Time", ParamInfo::new(0.0, 1.0, 0.5))
            .event_input("notes", "Notes")
            .output("out", "Out")
            .event_output("echo", "Echo"),
        outputs: Outputs::Broadcast,
        loop_input: Some("in"),
    });
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

/// Checks a schedule by simulating it, for signal and event buffers alike:
/// every buffer an input reads must still hold the output it's wired to, a
/// node's outputs must not share buffers with its inputs or each other,
/// buffer IDs must exist, and signal buffers must be big enough.
fn check(graph: &Graph, schedule: &Schedule, diagnostics: &[Diagnostic]) {
    let scheduled: HashMap<NodeId, &ScheduledNode> =
        schedule.nodes.iter().map(|n| (n.id, n)).collect();
    // A wire from a scheduled node can only be ignored if a diagnostic says why.
    let assert_explained = |input: &Endpoint| {
        if let Some(wired) = graph.source(input)
            && scheduled.contains_key(&wired.node)
        {
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.location == Location::Wire(input.clone())),
                "{input} ignored its wire without a diagnostic"
            );
        }
    };
    // Every node runs once, whole, or twice, output half then input half.
    let mut seen: HashMap<NodeId, Phase> = HashMap::new();
    for node in &schedule.nodes {
        let before = seen.insert(node.id, node.phase);
        match (before, node.phase) {
            (None, Phase::Whole | Phase::Output) | (Some(Phase::Output), Phase::Input) => {}
            other => panic!("{}: steps out of order: {other:?}", node.id),
        }
    }
    assert!(
        seen.values().all(|&p| p != Phase::Output),
        "an output half without its input half"
    );
    let mut holds: HashMap<BufferId, Endpoint> = HashMap::new();
    let mut event_holds: HashMap<EventBufferId, Endpoint> = HashMap::new();

    for node in &schedule.nodes {
        for (port, source) in node.layout.inputs.iter().zip(&node.inputs) {
            let input = Endpoint::new(node.id, port.key.as_ref());
            match source {
                InputSource::Buffer(b) | InputSource::Modulated(b, _) => {
                    let wired = graph.source(&input).expect("buffer input must be wired");
                    assert_eq!(holds.get(b), Some(wired), "{input} read a stale buffer");
                }
                InputSource::Value(_) => assert_explained(&input),
                InputSource::Absent => {
                    assert_ne!(
                        node.phase,
                        Phase::Whole,
                        "{input} is absent in a whole node"
                    );
                }
            }
        }
        if node.phase == Phase::Input {
            assert!(node.event_inputs.iter().all(Option::is_none));
            assert!(node.event_outputs.is_empty());
            continue;
        }
        for (port, source) in node.layout.event_inputs.iter().zip(&node.event_inputs) {
            let input = Endpoint::new(node.id, port.key.as_ref());
            match source {
                Some(b) => {
                    let wired = graph.source(&input).expect("event input must be wired");
                    assert_eq!(
                        event_holds.get(b),
                        Some(wired),
                        "{input} read a stale event buffer"
                    );
                }
                None => assert_explained(&input),
            }
        }

        assert_distinct(&node.outputs, "outputs share a buffer");
        for (b, (port, shape)) in node
            .outputs
            .iter()
            .zip(node.layout.outputs.iter().zip(&node.output_shapes))
        {
            assert!(b.0 < schedule.buffer_lanes.len(), "no buffer {b:?}");
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

        assert_distinct(&node.event_outputs, "event outputs share a buffer");
        for (b, port) in node.event_outputs.iter().zip(&node.layout.event_outputs) {
            assert!(b.0 < schedule.event_buffers, "no event buffer {b:?}");
            assert!(
                !node.event_inputs.contains(&Some(*b)),
                "event output aliases an event input"
            );
            event_holds.insert(*b, Endpoint::new(node.id, port.key.as_ref()));
        }
    }
}

fn assert_distinct<T: Ord + Clone>(items: &[T], message: &str) {
    let mut sorted = items.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), items.len(), "{message}");
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
fn a_loop_through_a_loop_breaker_runs_it_in_two_halves() {
    let mut p = Project::new();
    let src = add(&mut p, Node::new("source"));
    let mix = add(&mut p, Node::new("sum"));
    let delay = add(&mut p, Node::new("delayish"));
    wire(&mut p, src, "out", mix, "a");
    wire(&mut p, mix, "out", delay, "in");
    wire(&mut p, delay, "out", mix, "b");
    let (schedule, diagnostics) = compile_checked(&p);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let steps: Vec<_> = schedule.nodes.iter().map(|n| (n.id, n.phase)).collect();
    // The output half writes first, the input half reads last.
    assert_eq!(
        steps,
        [
            (src, Phase::Whole),
            (delay, Phase::Output),
            (mix, Phase::Whole),
            (delay, Phase::Input),
        ]
    );
    let output = &schedule.nodes[1];
    assert_eq!(output.inputs[0], InputSource::Absent);
    assert_eq!(output.outputs.len(), 1);
    let input = &schedule.nodes[3];
    assert!(matches!(input.inputs[0], InputSource::Buffer(_)));
    assert_eq!(input.inputs[1], InputSource::Absent);
    assert!(input.outputs.is_empty());
}

#[test]
fn a_loop_breaker_that_is_not_on_a_loop_runs_whole() {
    let mut p = Project::new();
    let src = add(&mut p, Node::new("source"));
    let delay = add(&mut p, Node::new("delayish"));
    wire(&mut p, src, "out", delay, "in");
    let (schedule, diagnostics) = compile_checked(&p);
    assert!(diagnostics.is_empty());
    assert_eq!(find(&schedule, delay).phase, Phase::Whole);
}

#[test]
fn a_loop_into_a_breakers_other_input_is_still_dropped() {
    let mut p = Project::new();
    let delay = add(&mut p, Node::new("delayish"));
    let thru = add(&mut p, Node::new("thru"));
    wire(&mut p, delay, "out", thru, "in");
    wire(&mut p, thru, "out", delay, "time");
    let (_, diagnostics) = compile_checked(&p);
    assert_eq!(
        diagnostics,
        [Diagnostic::wire(
            &Endpoint::new(delay, "time"),
            Problem::Loop
        )]
    );
}

#[test]
fn shapes_settle_round_a_loop() {
    let mut p = Project::new();
    let poly = add(&mut p, Node::new("poly4"));
    let mix = add(&mut p, Node::new("sum"));
    let delay = add(&mut p, Node::new("delayish"));
    wire(&mut p, poly, "out", mix, "a");
    wire(&mut p, mix, "out", delay, "in");
    wire(&mut p, delay, "out", mix, "b");
    let (schedule, diagnostics) = compile_checked(&p);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // The delay's output was assumed mono at first; the loop made it 4 voices.
    for step in &schedule.nodes {
        assert_eq!(step.output_shapes, [Shape::new(4, 1)], "{}", step.id);
    }
    assert_eq!(find(&schedule, delay).input_shapes[0], Shape::new(4, 1));
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

#[test]
fn each_output_gets_its_own_buffer_and_lifetime() {
    let mut p = Project::new();
    let source = add(&mut p, Node::new("source"));
    let split = add(&mut p, Node::new("split"));
    let early = add(&mut p, Node::new("thru"));
    let late = add(&mut p, Node::new("thru"));
    wire(&mut p, source, "out", split, "in");
    wire(&mut p, split, "low", early, "in");
    // `late` runs after `early`, so `high` must outlive `low`.
    wire(&mut p, early, "out", late, "amount");
    wire(&mut p, split, "high", late, "in");
    let (schedule, diagnostics) = compile_checked(&p);
    assert!(diagnostics.is_empty());

    let split = find(&schedule, split);
    assert_eq!(split.output_shapes, [Shape::MONO, Shape::STEREO]);
    let [low, high] = split.outputs[..] else {
        panic!()
    };
    assert_ne!(low, high);
    assert_eq!(find(&schedule, early).inputs[0], InputSource::Buffer(low));
    assert_eq!(find(&schedule, late).inputs[0], InputSource::Buffer(high));
    assert_eq!(find(&schedule, late).output_shapes, [Shape::STEREO]);
}

#[test]
fn routes_keeps_and_reuses_event_buffers() {
    let mut p = Project::new();
    let source = add(&mut p, Node::new("note_source"));
    let first = add(&mut p, Node::new("note_thru"));
    let second = add(&mut p, Node::new("note_thru"));
    let sink = add(&mut p, Node::new("notes"));
    let late_sink = add(&mut p, Node::new("notes"));
    wire(&mut p, source, "notes", first, "notes");
    wire(&mut p, first, "a", second, "notes");
    wire(&mut p, second, "a", sink, "notes");
    // Fan-out: the source is also read last of all.
    wire(&mut p, source, "notes", late_sink, "notes");
    let (schedule, diagnostics) = compile_checked(&p);
    assert!(diagnostics.is_empty());

    let source_buffer = find(&schedule, source).event_outputs[0];
    assert_eq!(
        find(&schedule, late_sink).event_inputs,
        [Some(source_buffer)]
    );
    let first = find(&schedule, first);
    assert_eq!(first.event_inputs, [Some(source_buffer)]);
    assert_ne!(first.event_outputs[0], first.event_outputs[1]);
    // Five event outputs, but finished and unread buffers get reused.
    assert!(schedule.event_buffers < 5, "{}", schedule.event_buffers);
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
    let registry = registry();
    let types = [
        "source",
        "poly8",
        "poly4",
        "thru",
        "sum",
        "split",
        "note_source",
        "note_thru",
        "notes",
        "delayish",
    ];
    // Each fixture's outputs and inputs, as (port key, is an event port).
    type Ports = Vec<(String, bool)>;
    let ports: HashMap<&str, (Ports, Ports)> = types
        .iter()
        .map(|&type_id| {
            let layout = registry
                .get(type_id)
                .unwrap()
                .layout(&Config::new())
                .unwrap();
            let signal_in = layout.inputs.iter().map(|p| (p.key.to_string(), false));
            let signal_out = layout.outputs.iter().map(|p| (p.key.to_string(), false));
            let events_in = layout
                .event_inputs
                .iter()
                .map(|p| (p.key.to_string(), true));
            let events_out = layout
                .event_outputs
                .iter()
                .map(|p| (p.key.to_string(), true));
            let outputs = signal_out.chain(events_out).collect();
            let inputs = signal_in.chain(events_in).collect();
            (type_id, (outputs, inputs))
        })
        .collect();
    let mut rng = XorShift(0x5eed);

    for _ in 0..500 {
        let mut p = Project::new();
        let nodes: Vec<(NodeId, &str)> = (0..2 + rng.below(30))
            .map(|_| {
                let type_id = types[rng.below(types.len())];
                (add(&mut p, Node::new(type_id)), type_id)
            })
            .collect();
        for _ in 0..rng.below(nodes.len() * 3) {
            let (from, from_type) = nodes[rng.below(nodes.len())];
            let (to, to_type) = nodes[rng.below(nodes.len())];
            let outputs = &ports[from_type].0;
            if outputs.is_empty() {
                continue;
            }
            let (from_port, is_event) = &outputs[rng.below(outputs.len())];
            // Mostly pick an input of the same kind; occasionally any input,
            // to exercise kind mismatches.
            let inputs: Vec<&(String, bool)> = ports[to_type]
                .1
                .iter()
                .filter(|(_, kind)| rng.below(10) == 0 || kind == is_event)
                .collect();
            if !inputs.is_empty() {
                let (to_port, _) = inputs[rng.below(inputs.len())];
                wire(&mut p, from, from_port, to, to_port);
            }
        }
        compile_checked(&p);
    }
}
