use std::sync::Arc;

use noodle_core::{
    AutomationLane, AutomationPoint, Command, Config, Connection, Curve, History, Node, TempoMap,
    TimeSignature, Value,
};

use super::*;
use crate::{
    Context, Instance, Io, Layout, Memory, NodeError, NodeInfo, NodeType, ParamInfo, Replacement,
    Replacements, Setup, TapSpec,
};

struct TestType {
    info: NodeInfo,
    layout: Layout,
}

impl NodeType for TestType {
    fn info(&self) -> &NodeInfo {
        &self.info
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(self.layout.clone())
    }

    fn loop_input(&self, _config: &Config) -> Option<&'static str> {
        (self.info.id == "delayish").then_some("in")
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(Silent))
    }
}

struct Silent;

impl crate::Node for Silent {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}

    fn process_output(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let mut add = |id: &'static str, version: u32, layout: Layout| {
        registry.register(TestType {
            info: NodeInfo {
                id,
                version,
                name: id,
                category: "Test",
            },
            layout,
        });
    };
    add("source", 1, Layout::realtime().output("out", "Out"));
    add(
        "live",
        1,
        Layout::realtime().output("out", "Out").nondeterministic(),
    );
    add(
        "thru",
        1,
        Layout::realtime()
            .input("in", "In")
            .param("amount", "Amount", ParamInfo::new(0.0, 1.0, 0.5))
            .output("out", "Out"),
    );
    add(
        "offline",
        1,
        Layout::offline().input("in", "In").output("out", "Out"),
    );
    add(
        "offline_param",
        1,
        Layout::offline()
            .input("in", "In")
            .param("amount", "Amount", ParamInfo::new(0.0, 1.0, 0.5).offset())
            .output("out", "Out"),
    );
    add(
        "dry",
        1,
        Layout::realtime().output("out", "Out").memoryless(),
    );
    add(
        "wire",
        1,
        Layout::realtime()
            .input("in", "In")
            .output("out", "Out")
            .memoryless(),
    );
    add(
        "short",
        1,
        Layout::realtime()
            .input("in", "In")
            .output("out", "Out")
            .bounded(100),
    );
    add(
        "long",
        1,
        Layout::realtime()
            .input("in", "In")
            .output("out", "Out")
            .bounded(250),
    );
    add(
        "join",
        1,
        Layout::realtime()
            .input("a", "A")
            .input("b", "B")
            .output("out", "Out")
            .memoryless(),
    );
    add(
        "heavy_source",
        1,
        Layout::realtime()
            .output("out", "Out")
            .memoryless()
            .cost(50),
    );
    add(
        "heavy",
        1,
        Layout::realtime()
            .input("in", "In")
            .output("out", "Out")
            .memoryless()
            .cost(50),
    );
    add(
        "delayish",
        1,
        Layout::realtime().input("in", "In").output("out", "Out"),
    );
    registry
}

struct Rig {
    project: Project,
    history: History,
    max_frames: usize,
    /// Nodes the session is feeding live.
    live: Vec<NodeId>,
    min_cost: u32,
}

impl Rig {
    fn new() -> Self {
        Self {
            project: Project::new(),
            history: History::new(),
            max_frames: 128,
            live: Vec::new(),
            min_cost: 0,
        }
    }

    fn apply(&mut self, command: Command) {
        self.history.apply(&mut self.project, command).unwrap();
    }

    fn add(&mut self, id: u64, type_id: &str) -> NodeId {
        let id = NodeId(id);
        self.apply(Command::AddNode {
            id,
            node: Node::new(type_id),
        });
        id
    }

    fn wire(&mut self, from: NodeId, to: NodeId, port: &str) {
        self.apply(Command::Connect(Connection {
            from: Endpoint::new(from, "out"),
            to: Endpoint::new(to, port),
        }));
    }

    fn analysis(&self, frames: u64) -> Analysis {
        let no_files = |_: &str| None;
        analyze(
            &self.project,
            &registry(),
            &CacheEnv {
                project: &self.project,
                sample_rate: 48_000.0,
                frames,
                max_frames: self.max_frames,
                live: &|node| self.live.contains(&node),
                file_key: &no_files,
                min_cost: self.min_cost,
            },
        )
    }

    fn key(&self, node: NodeId) -> Result<CacheKey, Uncacheable> {
        self.analysis(1000).output_key(node, 0)
    }
}

/// source -> thru -> offline.
fn chain() -> (Rig, NodeId, NodeId, NodeId) {
    let mut rig = Rig::new();
    let source = rig.add(1, "source");
    let thru = rig.add(2, "thru");
    let offline = rig.add(3, "offline");
    rig.wire(source, thru, "in");
    rig.wire(thru, offline, "in");
    (rig, source, thru, offline)
}

#[test]
fn keys_are_stable_and_follow_everything_upstream() {
    let (mut rig, source, thru, offline) = chain();
    let keys = |rig: &Rig| [source, thru, offline].map(|n| rig.key(n).unwrap());
    let before = keys(&rig);
    assert_eq!(before, keys(&rig));
    assert_ne!(before[0], before[1]);

    // A parameter in the middle changes it and everything after it, and
    // leaves what's before alone.
    rig.apply(Command::SetParam {
        node: thru,
        key: "amount".into(),
        value: Some(0.9),
    });
    let after = keys(&rig);
    assert_eq!(after[0], before[0]);
    assert_ne!(after[1], before[1]);
    assert_ne!(after[2], before[2]);

    // Undo gives the old keys back, which is how an undone edit finds its
    // old render.
    rig.history.undo(&mut rig.project).unwrap();
    assert_eq!(keys(&rig), before);
}

#[test]
fn the_config_the_tempo_and_the_length_are_in_the_key() {
    let (mut rig, source, _, offline) = chain();
    let base = rig.key(offline).unwrap();
    assert_ne!(rig.analysis(2000).output_key(offline, 0).unwrap(), base);

    let fast = TempoMap::constant(180.0, TimeSignature::COMMON).unwrap();
    rig.apply(Command::SetTempoMap(fast));
    assert_ne!(rig.key(offline).unwrap(), base);
    rig.history.undo(&mut rig.project).unwrap();
    assert_eq!(rig.key(offline).unwrap(), base);

    rig.apply(Command::SetConfig {
        node: source,
        key: "x".into(),
        value: Some(Value::Int(1)),
    });
    assert_ne!(rig.key(offline).unwrap(), base);
}

#[test]
fn two_identical_nodes_have_different_keys() {
    // A node's seed comes from its ID, so identical nodes may sound different.
    let mut rig = Rig::new();
    let a = rig.add(1, "source");
    let b = rig.add(2, "source");
    assert_ne!(rig.key(a).unwrap(), rig.key(b).unwrap());
}

#[test]
fn automation_lane_points_are_in_the_key() {
    let (mut rig, _, thru, offline) = chain();
    let base = rig.key(offline).unwrap();
    let lane = |value: f32| AutomationLane {
        target: Endpoint::new(thru, "amount"),
        points: vec![AutomationPoint {
            tick: noodle_core::Tick(0),
            value,
            curve: Curve::Linear,
        }],
    };
    let id = rig.project.new_lane_id();
    rig.apply(Command::AddLane {
        id,
        lane: lane(0.2),
    });
    let with_lane = rig.key(offline).unwrap();
    assert_ne!(with_lane, base);
    rig.apply(Command::SetLane {
        id,
        lane: lane(0.3),
    });
    assert_ne!(rig.key(offline).unwrap(), with_lane);
}

#[test]
fn live_input_and_loops_cant_be_cached_and_neither_can_what_follows() {
    let mut rig = Rig::new();
    let live = rig.add(1, "live");
    let thru = rig.add(2, "thru");
    let offline = rig.add(3, "offline");
    rig.wire(live, thru, "in");
    rig.wire(thru, offline, "in");
    assert!(matches!(
        rig.key(live),
        Err(Uncacheable::NotDeterministic(_))
    ));
    let Err(Uncacheable::Upstream(node, _)) = rig.key(offline) else {
        panic!("the offline node follows live input");
    };
    assert_eq!(node, thru);
    let analysis = rig.analysis(1000);
    let target = analysis.target(TargetKind::Offline(offline)).unwrap();
    assert!(analysis.blocked(target).is_some());

    // A feedback loop through a delay-like node.
    let mut rig = Rig::new();
    let delay = rig.add(1, "delayish");
    let thru = rig.add(2, "thru");
    rig.wire(delay, thru, "in");
    rig.wire(thru, delay, "in");
    assert_eq!(rig.key(delay), Err(Uncacheable::Loop));
    assert!(matches!(rig.key(thru), Err(Uncacheable::Upstream(..))));
}

#[test]
fn targets_are_the_offline_nodes_and_the_frozen_ones_upstream_first() {
    let (mut rig, _, thru, offline) = chain();
    rig.apply(Command::SetFrozen {
        node: thru,
        frozen: true,
    });
    let analysis = rig.analysis(1000);
    let kinds: Vec<_> = analysis.targets.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [TargetKind::Frozen(thru), TargetKind::Offline(offline)]
    );
    assert_eq!(analysis.targets[0].outputs, [(thru, 0)]);
    assert!(analysis.nodes[&offline].offline);
}

/// The nodes a graph keeps after replacing and tapping.
fn kept(rig: &Rig, replace: &[(u64, &str)], taps: &[(u64, &str)]) -> Vec<(u64, String)> {
    let source: Arc<dyn NodeType> = Arc::new(TestType {
        info: NodeInfo {
            id: "cached",
            version: 1,
            name: "cached",
            category: "Test",
        },
        layout: Layout::realtime().output("out", "Out"),
    });
    let sink: Arc<dyn NodeType> = Arc::new(TestType {
        info: NodeInfo {
            id: "tap",
            version: 1,
            name: "tap",
            category: "Test",
        },
        layout: Layout::realtime().input("in", "In"),
    });
    let replacements: Replacements = replace
        .iter()
        .map(|&(node, port)| {
            (
                (NodeId(node), port.to_string()),
                Replacement {
                    node_type: Arc::clone(&source),
                    config: Config::new(),
                },
            )
        })
        .collect();
    let taps: Vec<_> = taps
        .iter()
        .map(|&(node, port)| TapSpec {
            endpoint: Endpoint::new(NodeId(node), port),
            node_type: Arc::clone(&sink),
        })
        .collect();
    let rewritten = crate::replace::rewrite(rig.project.graph(), &replacements, &taps);
    rewritten
        .graph
        .nodes()
        .map(|(id, node)| (id.0, node.type_id.clone()))
        .filter(|(id, _)| *id < 1 << 62)
        .collect()
}

#[test]
fn replacing_an_output_drops_what_only_fed_it() {
    let (mut rig, source, _, offline) = chain();
    let out = rig.add(4, "thru");
    rig.wire(offline, out, "in");
    // Replacing the thru in the middle drops it and the source before it,
    // and keeps what follows.
    let kept_ids: Vec<u64> = kept(&rig, &[(2, "out")], &[])
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(kept_ids, [3, 4]);

    // Something else reading the source keeps it alive.
    let side = rig.add(5, "thru");
    rig.wire(source, side, "in");
    let kept_ids: Vec<u64> = kept(&rig, &[(2, "out")], &[])
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(kept_ids, [1, 3, 4, 5]);
}

#[test]
fn taps_cut_the_graph_down_to_what_feeds_them() {
    let (mut rig, _, _, offline) = chain();
    let out = rig.add(4, "thru");
    rig.wire(offline, out, "in");
    let kept_ids: Vec<u64> = kept(&rig, &[], &[(2, "out")])
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(kept_ids, [1, 2]);
    // A tap on a replaced output watches the replacement, and nothing
    // upstream of it runs.
    let kept_ids = kept(&rig, &[(2, "out")], &[(2, "out")]);
    assert!(kept_ids.is_empty(), "{kept_ids:?}");
}

#[test]
fn each_output_of_a_node_has_its_own_key() {
    let node = KeyBuilder::new("node").u64(1).finish();
    assert_ne!(output_key(&node, 0), output_key(&node, 1));
    assert_eq!(output_key(&node, 1), output_key(&node, 1));
}

#[test]
fn an_offline_node_with_a_modulated_parameter_is_keyed_by_what_modulates_it() {
    let mut rig = Rig::new();
    let source = rig.add(1, "source");
    let offline = rig.add(2, "offline_param");
    rig.wire(source, offline, "in");
    let bare = rig.key(offline).unwrap();
    let modulator = rig.add(3, "source");
    rig.wire(modulator, offline, "amount");
    let modulated = rig.key(offline).expect("an offline node can take a wire");
    assert_ne!(bare, modulated);
    // Another modulator is another key.
    let other = rig.add(4, "source");
    rig.wire(other, offline, "amount");
    assert_ne!(modulated, rig.key(offline).unwrap());
}

fn memory(rig: &Rig, node: NodeId) -> Memory {
    rig.analysis(1000).nodes[&node].memory
}

#[test]
fn memory_adds_along_a_chain_and_takes_the_longest_of_parallel_branches() {
    let mut rig = Rig::new();
    let source = rig.add(1, "dry");
    let a = rig.add(2, "short");
    let b = rig.add(3, "long");
    let c = rig.add(4, "short");
    let join = rig.add(5, "join");
    let after = rig.add(6, "wire");
    // dry -> short -> long -> short is a chain of 100 + 250 + 100 ms.
    rig.wire(source, a, "in");
    rig.wire(a, b, "in");
    rig.wire(b, c, "in");
    assert_eq!(memory(&rig, source), Memory::None);
    assert_eq!(memory(&rig, a), Memory::Bounded(100));
    assert_eq!(memory(&rig, b), Memory::Bounded(350));
    assert_eq!(memory(&rig, c), Memory::Bounded(450));
    // Parallel branches meet at a join: the longer one, 450 against 100.
    let fast = rig.add(7, "short");
    rig.wire(source, fast, "in");
    rig.wire(c, join, "a");
    rig.wire(fast, join, "b");
    assert_eq!(memory(&rig, join), Memory::Bounded(450));
    // A node with no memory adds none.
    rig.wire(join, after, "in");
    assert_eq!(memory(&rig, after), Memory::Bounded(450));
}

#[test]
fn unbounded_memory_carries_downstream_and_so_does_a_loop() {
    let mut rig = Rig::new();
    let dry = rig.add(1, "dry");
    let forever = rig.add(2, "thru");
    let after = rig.add(3, "short");
    let beside = rig.add(4, "short");
    let join = rig.add(5, "join");
    rig.wire(dry, forever, "in");
    rig.wire(forever, after, "in");
    rig.wire(dry, beside, "in");
    rig.wire(after, join, "a");
    rig.wire(beside, join, "b");
    // `thru` doesn't say, so it is unbounded; the sum and the join keep it.
    assert_eq!(memory(&rig, forever), Memory::Unbounded);
    assert_eq!(memory(&rig, after), Memory::Unbounded);
    assert_eq!(memory(&rig, beside), Memory::Bounded(100));
    assert_eq!(memory(&rig, join), Memory::Unbounded);

    // A feedback loop is unbounded even through nodes with no memory.
    let mut rig = Rig::new();
    let delay = rig.add(1, "delayish");
    let wire = rig.add(2, "wire");
    rig.wire(delay, wire, "in");
    rig.wire(wire, delay, "in");
    assert_eq!(memory(&rig, delay), Memory::Unbounded);
    assert_eq!(memory(&rig, wire), Memory::Unbounded);
}

#[test]
fn an_offline_node_has_unbounded_memory() {
    let mut rig = Rig::new();
    let dry = rig.add(1, "dry");
    let offline = rig.add(2, "offline");
    rig.wire(dry, offline, "in");
    assert_eq!(memory(&rig, offline), Memory::Unbounded);
}

#[test]
fn memory_composes_by_the_rules() {
    use Memory::{Bounded, None, Unbounded};
    assert_eq!(None.then(None), None);
    assert_eq!(None.then(Bounded(5)), Bounded(5));
    assert_eq!(Bounded(5).then(Bounded(7)), Bounded(12));
    assert_eq!(Bounded(u32::MAX).then(Bounded(1)), Bounded(u32::MAX));
    assert_eq!(Bounded(5).then(Unbounded), Unbounded);
    assert_eq!(Unbounded.then(None), Unbounded);
    assert_eq!(Bounded(5).alongside(Bounded(7)), Bounded(7));
    assert_eq!(None.alongside(Bounded(1)), Bounded(1));
    assert_eq!(Bounded(u32::MAX).alongside(Unbounded), Unbounded);
}

#[test]
fn the_render_length_is_only_in_the_keys_of_what_reads_the_whole_range() {
    let (rig, source, thru, offline) = chain();
    let key = |frames: u64, node: NodeId| rig.analysis(frames).output_key(node, 0).unwrap();
    // Real-time nodes only read the past: a longer render is a longer
    // version of the same one.
    assert_eq!(key(1000, source), key(2000, source));
    assert_eq!(key(1000, thru), key(2000, thru));
    // The offline node reads its input whole, so its render depends on the
    // length.
    assert_ne!(key(1000, offline), key(2000, offline));
}

#[test]
fn the_block_size_is_in_the_key() {
    let (mut rig, source, _, _) = chain();
    let before = rig.key(source).unwrap();
    rig.max_frames = 256;
    assert_ne!(rig.key(source).unwrap(), before);
}

#[test]
fn a_node_fed_live_is_not_cached_but_its_neighbours_are() {
    let mut rig = Rig::new();
    let track = rig.add(1, "source");
    let other = rig.add(2, "source");
    let after = rig.add(3, "thru");
    rig.wire(track, after, "in");
    let before = rig.key(other).unwrap();
    assert!(rig.key(track).is_ok());
    rig.live.push(track);
    assert_eq!(rig.key(track), Err(Uncacheable::Live));
    assert!(matches!(rig.key(after), Err(Uncacheable::Upstream(n, _)) if n == track));
    // Another node of the same type is not touched.
    assert_eq!(rig.key(other).unwrap(), before);
    rig.live.clear();
    assert!(rig.key(track).is_ok());
}

/// Cuts the playback cache made, by node.
fn cuts(rig: &Rig) -> Vec<NodeId> {
    rig.analysis(1000)
        .targets
        .iter()
        .filter_map(|t| match t.kind {
            TargetKind::Cut(node) => Some(node),
            _ => None,
        })
        .collect()
}

#[test]
fn a_source_that_two_nodes_read_is_one_cut_read_twice() {
    let mut rig = Rig::new();
    rig.min_cost = 40;
    let send = rig.add(1, "heavy_source");
    let a = rig.add(2, "wire");
    let b = rig.add(3, "wire");
    let join = rig.add(4, "join");
    rig.wire(send, a, "in");
    rig.wire(send, b, "in");
    rig.wire(a, join, "a");
    rig.wire(b, join, "b");
    // Both branches are cheap once the send is stored, so the one cut is the
    // send: rendered once, and the replacement at its output feeds both.
    assert_eq!(cuts(&rig), [send]);
    let analysis = rig.analysis(1000);
    let cut = analysis.target(TargetKind::Cut(send)).unwrap();
    assert_eq!(cut.outputs, [(send, 0)]);
}

#[test]
fn a_cheap_chain_is_not_cached() {
    let mut rig = Rig::new();
    rig.min_cost = 40;
    let source = rig.add(1, "dry");
    let mut last = source;
    for id in 2..30 {
        let next = rig.add(id, "wire");
        rig.wire(last, next, "in");
        last = next;
    }
    // A fan-out and a join of cheap chains: still nothing worth the bytes.
    let other = rig.add(40, "wire");
    let join = rig.add(41, "join");
    rig.wire(source, other, "in");
    rig.wire(last, join, "a");
    rig.wire(other, join, "b");
    assert_eq!(rig.analysis(1000).targets, []);
    // The same graph is cut everywhere when anything is worth caching.
    rig.min_cost = 0;
    assert!(!cuts(&rig).is_empty());
}

#[test]
fn each_expensive_branch_of_a_join_is_a_cut_and_the_join_is_not() {
    let mut rig = Rig::new();
    rig.min_cost = 40;
    let a = rig.add(1, "heavy_source");
    let b = rig.add(2, "heavy_source");
    let join = rig.add(4, "join");
    let after = rig.add(5, "wire");
    rig.wire(a, join, "a");
    rig.wire(b, join, "b");
    rig.wire(join, after, "in");
    assert_eq!(cuts(&rig), [a, b]);
    // A cheap branch beside an expensive one is read live.
    let mut other = Rig::new();
    other.min_cost = 40;
    let (a, cheap_branch) = (other.add(1, "heavy_source"), other.add(2, "dry"));
    let join = other.add(3, "join");
    other.wire(a, join, "a");
    other.wire(cheap_branch, join, "b");
    assert_eq!(cuts(&other), [a]);
}

#[test]
fn a_cut_does_not_pay_for_what_a_cut_before_it_stores() {
    let mut rig = Rig::new();
    rig.min_cost = 80;
    // Two heavy nodes in a chain cost 100 together; the second, behind a
    // stored first, costs 50 and is not a cut of its own.
    let first = rig.add(1, "heavy_source");
    let second = rig.add(2, "heavy");
    let a = rig.add(3, "wire");
    let b = rig.add(4, "wire");
    let join = rig.add(5, "join");
    rig.wire(first, second, "in");
    rig.wire(second, a, "in");
    rig.wire(second, b, "in");
    rig.wire(a, join, "a");
    rig.wire(b, join, "b");
    assert_eq!(cuts(&rig), [second]);
    // With the first read by two, it is a cut; the branch behind it that
    // still costs 51 is the second cut, and the second heavy node is not
    // its own.
    let mut rig = Rig::new();
    rig.min_cost = 50;
    let first = rig.add(1, "heavy_source");
    let second = rig.add(2, "heavy");
    let a = rig.add(3, "wire");
    let b = rig.add(4, "wire");
    let join = rig.add(5, "join");
    rig.wire(first, second, "in");
    rig.wire(first, a, "in");
    rig.wire(second, b, "in");
    rig.wire(a, join, "a");
    rig.wire(b, join, "b");
    assert_eq!(cuts(&rig), [first, b]);
}

#[test]
fn moving_a_node_changes_neither_the_partition_nor_a_key() {
    let mut rig = Rig::new();
    rig.min_cost = 40;
    let send = rig.add(1, "heavy_source");
    let a = rig.add(2, "wire");
    let b = rig.add(3, "wire");
    rig.wire(send, a, "in");
    rig.wire(send, b, "in");
    let before = rig.analysis(1000);
    for node in [send, a, b] {
        rig.apply(Command::MoveNode {
            node,
            position: noodle_core::Position { x: 300.0, y: 40.0 },
        });
    }
    rig.apply(Command::SetPortOrder {
        node: a,
        order: vec!["out".into(), "in".into()],
    });
    let after = rig.analysis(1000);
    assert_eq!(before.targets, after.targets);
    assert_eq!(
        before.output_key(send, 0).unwrap(),
        after.output_key(send, 0).unwrap()
    );
    assert_eq!(cuts(&rig), [send]);
}
