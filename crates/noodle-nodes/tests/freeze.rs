//! Offline nodes and freeze, end to end: keys follow what the render depends
//! on, offline nodes render from the cache and again when something upstream
//! changes, and a frozen node plays what the live graph would.

use std::path::PathBuf;
use std::time::Duration;

use noodle_core::{
    AudioClip, Clip, ClipContent, Command, Connection, Endpoint, History, Node, NodeId, Project,
    Tick,
};
use noodle_engine::{
    Analysis, Controller, OUTPUT_ID, Processor, Progress, Registry, Settings, TargetKind,
    TempoTable, engine,
};
use noodle_io::{CacheStore, write_wav};
use noodle_nodes::{
    FreezeError, Freezer, RenderRequest, TRACK_INPUT_ID, TargetState, freeze, register_library,
    register_library_blocking,
};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};
/// Several cache chunks, and not a whole number of them.
const FRAMES: usize = 5 * 4096 + 123;
const REVERSE: &str = "noodle.offline.reverse";

struct Rig {
    project: Project,
    history: History,
    base: PathBuf,
    freezer: Freezer,
}

fn setup(name: &str) -> Rig {
    let root = std::env::temp_dir().join(format!("noodle-freeze-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let samples: Vec<f32> = (0..FRAMES)
        .flat_map(|i| {
            let x = (i as f32 * 0.05).sin() * 0.5 * (1.0 - i as f32 / FRAMES as f32);
            [x, -x * 0.5]
        })
        .collect();
    write_wav(&root.join("tone.wav"), &samples, 2, RATE).unwrap();
    let store = CacheStore::open(root.join("cache")).unwrap();
    Rig {
        project: Project::new(),
        history: History::new(),
        base: root,
        freezer: Freezer::new(store),
    }
}

impl Rig {
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

    fn wire(&mut self, from: (NodeId, &str), to: (NodeId, &str)) {
        self.apply(Command::Connect(Connection {
            from: Endpoint::new(from.0, from.1),
            to: Endpoint::new(to.0, to.1),
        }));
    }

    /// Track input -> `middle` (if any) -> Output, playing `tone.wav`.
    fn clip_chain(&mut self, middle: Option<&str>) -> (NodeId, Option<NodeId>, NodeId) {
        let track = self.add(1, TRACK_INPUT_ID);
        let out = self.add(3, OUTPUT_ID);
        let mid = middle.map(|kind| self.add(2, kind));
        match mid {
            Some(mid) => {
                self.wire((track, "audio"), (mid, "in"));
                self.wire((mid, "out"), (out, "in"));
            }
            None => self.wire((track, "audio"), (out, "in")),
        }
        let id = self.project.new_clip_id();
        let clip = Clip::audio(track, Tick(0), "tone.wav", FRAMES as u64);
        self.apply(Command::AddClip { id, clip });
        (track, mid, out)
    }

    fn request(&self) -> RenderRequest {
        RenderRequest {
            project: self.project.clone(),
            base: self.base.clone(),
            settings: SETTINGS,
            frames: FRAMES,
            extend_registry: None,
        }
    }

    fn analyze(&self) -> Analysis {
        let mut registry = Registry::with_builtins();
        let _ = register_library_blocking(&mut registry);
        self.freezer
            .analyze(&self.project, &registry, SETTINGS, FRAMES, &self.base)
    }

    fn freeze(&self) -> noodle_nodes::FreezeReport {
        freeze(&self.freezer, &self.request(), &Progress::new()).unwrap()
    }

    /// Plays the project through the live engine with the cache standing in,
    /// block by block, and returns what came out. `pace` sleeps between
    /// blocks, which a streaming player needs to keep ahead.
    fn play(&self, blocking: bool, pace: Option<Duration>) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        let library = if blocking {
            register_library_blocking(&mut registry)
        } else {
            register_library(&mut registry)
        };
        let table = TempoTable::new(self.project.tempo_map(), SETTINGS.sample_rate);
        library
            .clips
            .update(&self.project, &table, RATE, &self.base);
        let analysis = self
            .freezer
            .analyze(&self.project, &registry, SETTINGS, FRAMES, &self.base);
        let plan = self.freezer.plan(&analysis, SETTINGS, FRAMES, blocking);
        let (mut controller, mut processor) = engine(SETTINGS).unwrap();
        controller.update_project_replacing(&self.project, &registry, &plan.replacements);
        if !blocking {
            // The players read ahead on their own threads; let them start.
            std::thread::sleep(Duration::from_millis(100));
        }
        run(&mut controller, &mut processor, pace)
    }

    /// The project played with no cache at all.
    fn live(&self) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        let library = register_library_blocking(&mut registry);
        let table = TempoTable::new(self.project.tempo_map(), SETTINGS.sample_rate);
        library
            .clips
            .update(&self.project, &table, RATE, &self.base);
        let (mut controller, mut processor) = engine(SETTINGS).unwrap();
        controller.update_project(&self.project, &registry);
        run(&mut controller, &mut processor, None)
    }
}

fn run(controller: &mut Controller, processor: &mut Processor, pace: Option<Duration>) -> Vec<f32> {
    let mut out = vec![0.0; FRAMES * 2];
    for (i, block) in out.chunks_mut(512 * 2).enumerate() {
        processor.process(block);
        controller.maintain();
        if let Some(pace) = pace
            && i % 4 == 3
        {
            std::thread::sleep(pace);
        }
    }
    out
}

fn assert_close(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: lengths");
    let worst = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-5, "{what}: samples differ by {worst}");
}

/// `samples` (stereo interleaved) played backwards frame by frame.
fn reversed(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(2)
        .rev()
        .flat_map(|frame| frame.iter().copied())
        .collect()
}

fn offline_kind(node: NodeId) -> TargetKind {
    TargetKind::Offline(node)
}

#[test]
fn an_offline_node_plays_its_cached_render() {
    let mut rig = setup("reverse");
    let (_, reverse, _) = rig.clip_chain(Some(REVERSE));
    let reverse = reverse.unwrap();
    let original = {
        let mut plain = setup("reverse-plain");
        plain.clip_chain(None);
        plain.live()
    };
    assert!(original.iter().any(|&s| s.abs() > 0.01));

    // Until it is rendered an offline node plays silence.
    let before = rig.freezer.plan(&rig.analyze(), SETTINGS, FRAMES, true);
    assert_eq!(
        before.state(offline_kind(reverse)),
        Some(&TargetState::Missing)
    );
    assert!(rig.play(true, None).iter().all(|&s| s == 0.0));

    let report = rig.freeze();
    assert_eq!(report.rendered, [offline_kind(reverse)]);
    let expected = reversed(&original);
    assert_close(&rig.play(true, None), &expected, "blocking player");

    // Already rendered: nothing to do the second time.
    let again = rig.freeze();
    assert!(again.rendered.is_empty());
    assert_eq!(again.cached, [offline_kind(reverse)]);
}

#[test]
fn an_offline_node_renders_again_when_something_upstream_changes() {
    let mut rig = setup("upstream");
    let (track, reverse, _) = rig.clip_chain(Some(REVERSE));
    let reverse = reverse.unwrap();
    rig.freeze();
    let first = rig.play(true, None);
    let (clip_id, clip) = rig
        .project
        .clips_on(track)
        .map(|(id, clip)| (id, clip.clone()))
        .next()
        .unwrap();

    // Turn the clip down: the key changes, so the old render no longer
    // stands in, and a new one is made.
    let ClipContent::Audio(audio) = clip.content.clone() else {
        unreachable!()
    };
    let quieter = Clip {
        content: ClipContent::Audio(AudioClip { gain: 0.5, ..audio }),
        ..clip
    };
    rig.apply(Command::SetClip {
        id: clip_id,
        clip: quieter,
    });
    let plan = rig.freezer.plan(&rig.analyze(), SETTINGS, FRAMES, true);
    assert_eq!(
        plan.state(offline_kind(reverse)),
        Some(&TargetState::Missing)
    );
    assert_eq!(rig.freeze().rendered, [offline_kind(reverse)]);
    let second = rig.play(true, None);
    let half: Vec<f32> = first.iter().map(|s| s * 0.5).collect();
    assert_close(&second, &half, "after the clip was turned down");

    // Undo brings the first render back without rendering anything.
    rig.history.undo(&mut rig.project).unwrap();
    assert!(rig.freeze().rendered.is_empty());
    assert_close(&rig.play(true, None), &first, "after undo");
}

#[test]
fn replacing_the_file_under_the_same_name_gives_a_new_key() {
    let mut rig = setup("file");
    let (_, reverse, _) = rig.clip_chain(Some(REVERSE));
    let reverse = reverse.unwrap();
    let key = |rig: &Rig| rig.analyze().output_key(reverse, 0).unwrap();
    let before = key(&rig);
    assert_eq!(before, key(&rig));

    let louder: Vec<f32> = (0..FRAMES * 2).map(|i| (i as f32 * 0.01).sin()).collect();
    let path = rig.base.join("tone.wav");
    write_wav(&path, &louder, 2, RATE).unwrap();
    // Same name; make sure the stat check sees a change even on a coarse clock.
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    assert_ne!(before, key(&rig));
}

#[test]
fn live_input_cant_be_cached() {
    let mut rig = setup("live");
    let input = rig.add(1, noodle_engine::INPUT_ID);
    let reverse = rig.add(2, REVERSE);
    let out = rig.add(3, OUTPUT_ID);
    rig.wire((input, "out"), (reverse, "in"));
    rig.wire((reverse, "out"), (out, "in"));
    let analysis = rig.analyze();
    let plan = rig.freezer.plan(&analysis, SETTINGS, FRAMES, true);
    let Some(TargetState::Blocked(why)) = plan.state(offline_kind(reverse)) else {
        panic!("expected the offline node to be blocked");
    };
    assert!(why.to_string().contains("same every time"), "{why}");
    // It is a compile error on the node, and the node plays silence.
    assert_eq!(plan.diagnostics.len(), 1);
    assert!(matches!(
        plan.diagnostics[0].problem,
        noodle_engine::Problem::NotCacheable(_)
    ));
    assert!(
        plan.replacements
            .contains_key(&(reverse, "out".to_string()))
    );
    assert!(matches!(
        freeze(&rig.freezer, &rig.request(), &Progress::new())
            .unwrap()
            .blocked
            .as_slice(),
        [(TargetKind::Offline(_), _)]
    ));
}

#[test]
fn a_missing_file_is_not_cached() {
    let mut rig = setup("missing");
    let (_, reverse, _) = rig.clip_chain(Some(REVERSE));
    std::fs::remove_file(rig.base.join("tone.wav")).unwrap();
    let plan = rig.freezer.plan(&rig.analyze(), SETTINGS, FRAMES, true);
    assert!(matches!(
        plan.state(offline_kind(reverse.unwrap())),
        Some(TargetState::Blocked(_))
    ));
}

#[test]
fn the_streaming_player_matches_the_blocking_one() {
    let mut rig = setup("streaming");
    rig.clip_chain(Some(REVERSE));
    rig.freeze();
    let blocking = rig.play(true, None);
    let streamed = rig.play(false, Some(Duration::from_millis(15)));
    assert_close(&streamed, &blocking, "streamed against blocking");
}

#[test]
fn cancelling_a_freeze_leaves_nothing_in_the_cache() {
    let mut rig = setup("cancel");
    rig.clip_chain(Some(REVERSE));
    let progress = Progress::new();
    progress.cancel();
    let result = freeze(&rig.freezer, &rig.request(), &progress);
    assert!(matches!(result, Err(FreezeError::Cancelled)));
    assert_eq!(rig.freezer.store().total_bytes().unwrap(), 0);
}

/// A sine through a gain inside a group, to the output: the group is the
/// thing to freeze.
fn synth_group(rig: &mut Rig) -> (NodeId, NodeId, NodeId) {
    let osc = rig.add(1, "noodle.osc.sine");
    let gain = rig.add(2, noodle_nodes::GAIN_ID);
    let out = rig.add(3, OUTPUT_ID);
    rig.apply(Command::SetParam {
        node: osc,
        key: "frequency".into(),
        value: Some(330.0),
    });
    rig.wire((osc, "out"), (gain, "in"));
    rig.wire((gain, "out"), (out, "in"));
    let project = rig.project.clone();
    let mut next = project.next_node_id().0;
    let (group, command) = noodle_core::group::group_nodes(&project, &[osc, gain], || {
        next += 1;
        NodeId(next - 1)
    })
    .unwrap();
    rig.apply(command);
    (group, osc, out)
}

#[test]
fn a_frozen_group_plays_from_the_cache_and_stops_running_live() {
    let mut rig = setup("group");
    let (group, osc, _) = synth_group(&mut rig);
    let live = rig.live();
    assert!(live.iter().any(|&s| s.abs() > 0.01));

    rig.apply(Command::SetFrozen {
        node: group,
        frozen: true,
    });
    let frozen = TargetKind::Frozen(group);
    let analysis = rig.analyze();
    let plan = rig.freezer.plan(&analysis, SETTINGS, FRAMES, true);
    assert_eq!(plan.state(frozen), Some(&TargetState::Missing));
    // Not rendered yet: it plays live, so nothing is replaced.
    assert!(plan.replacements.is_empty());
    assert_close(&rig.play(true, None), &live, "frozen but not rendered");

    assert_eq!(rig.freeze().rendered, [frozen]);
    let plan = rig.freezer.plan(&rig.analyze(), SETTINGS, FRAMES, true);
    assert_eq!(plan.state(frozen), Some(&TargetState::Ready));
    assert_close(&rig.play(true, None), &live, "frozen");
    assert_close(
        &rig.play(false, Some(Duration::from_millis(15))),
        &live,
        "frozen and streamed",
    );

    // The compiled plan holds the cached player and the output, and none of
    // the nodes that made the sound.
    let mut registry = Registry::with_builtins();
    let _ = register_library_blocking(&mut registry);
    let lanes: Vec<_> = rig.project.lanes().collect();
    let (schedule, _) = noodle_engine::compile_replacing(
        rig.project.graph(),
        &lanes,
        &registry,
        &plan.replacements,
    );
    let ids: Vec<_> = schedule
        .nodes
        .iter()
        .map(|n| n.node_type.info().id)
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&noodle_nodes::CACHED_ID));
    assert!(schedule.nodes.iter().all(|n| n.id != osc));

    // Changing something inside the group makes the render stale; the group
    // plays live again until it is frozen again.
    rig.apply(Command::SetParam {
        node: osc,
        key: "frequency".into(),
        value: Some(440.0),
    });
    let plan = rig.freezer.plan(&rig.analyze(), SETTINGS, FRAMES, true);
    assert_eq!(plan.state(frozen), Some(&TargetState::Missing));
    assert!(plan.replacements.is_empty());
    let retuned = rig.live();
    assert_close(&rig.play(true, None), &retuned, "stale freeze plays live");
    assert_eq!(rig.freeze().rendered, [frozen]);
    assert_close(&rig.play(true, None), &retuned, "refrozen");
}

#[test]
fn chained_offline_nodes_and_freezes_render_upstream_first() {
    let mut rig = setup("chain");
    let (track, first, out) = rig.clip_chain(Some(REVERSE));
    let first = first.unwrap();
    // Track -> reverse -> reverse -> output: the original again.
    let second = rig.add(4, REVERSE);
    rig.wire((first, "out"), (second, "in"));
    rig.wire((second, "out"), (out, "in"));
    let _ = track;
    let original = {
        let mut plain = setup("chain-plain");
        plain.clip_chain(None);
        plain.live()
    };
    let report = rig.freeze();
    assert_eq!(report.rendered, [offline_kind(first), offline_kind(second)]);
    assert_close(&rig.play(true, None), &original, "reverse twice");
}
