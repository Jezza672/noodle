use std::sync::Arc;

use noodle_core::{
    AutomationLane, AutomationPoint, Command, Config, Connection, Curve, History, Node, TempoMap,
    TimeSignature, Value,
};

use super::*;
use crate::{
    Context, Instance, Io, Layout, NodeError, NodeInfo, NodeType, ParamInfo, Replacement,
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
        "delayish",
        1,
        Layout::realtime().input("in", "In").output("out", "Out"),
    );
    registry
}

struct Rig {
    project: Project,
    history: History,
}

impl Rig {
    fn new() -> Self {
        Self {
            project: Project::new(),
            history: History::new(),
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
                file_key: &no_files,
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
