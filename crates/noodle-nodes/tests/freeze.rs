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
    Analysis, Controller, OUTPUT_ID, Pass, Processor, Progress, Registry, Settings, TargetKind,
    TempoTable, engine,
};
use noodle_io::{CacheStore, write_wav};
use noodle_nodes::{
    FreezeError, Freezer, RenderRequest, TRACK_INPUT_ID, TargetState, freeze, freeze_passes,
    register_library, register_library_blocking,
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

    /// Makes the clip's warp loop over `region`.
    fn warp_loop(&mut self, clip: noodle_core::ClipId, region: noodle_core::LoopRegion) {
        let command = self
            .project
            .warp_edit(clip, |warp| warp.loop_region = Some(region))
            .unwrap();
        self.apply(command);
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
        let plan = self
            .freezer
            .plan(&analysis, SETTINGS, FRAMES, blocking, None);
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
    let before = rig
        .freezer
        .plan(&rig.analyze(), SETTINGS, FRAMES, true, None);
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
    let plan = rig
        .freezer
        .plan(&rig.analyze(), SETTINGS, FRAMES, true, None);
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
    let plan = rig.freezer.plan(&analysis, SETTINGS, FRAMES, true, None);
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
    let plan = rig
        .freezer
        .plan(&rig.analyze(), SETTINGS, FRAMES, true, None);
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
    let plan = rig.freezer.plan(&analysis, SETTINGS, FRAMES, true, None);
    assert_eq!(plan.state(frozen), Some(&TargetState::Missing));
    // Not rendered yet: it plays live, so nothing is replaced.
    assert!(plan.replacements.is_empty());
    assert_close(&rig.play(true, None), &live, "frozen but not rendered");

    assert_eq!(rig.freeze().rendered, [frozen]);
    let plan = rig
        .freezer
        .plan(&rig.analyze(), SETTINGS, FRAMES, true, None);
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
    let plan = rig
        .freezer
        .plan(&rig.analyze(), SETTINGS, FRAMES, true, None);
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

#[test]
fn an_offline_node_after_a_spectral_chain_renders_again_when_the_chain_changes() {
    let mut rig = setup("spectral");
    let track = rig.add(1, TRACK_INPUT_ID);
    let analyze = rig.add(2, "noodle.spectral.analyze");
    let gate = rig.add(3, "noodle.spectral.gate");
    let synth = rig.add(4, "noodle.spectral.synthesize");
    let reverse = rig.add(5, REVERSE);
    let out = rig.add(6, OUTPUT_ID);
    rig.wire((track, "audio"), (analyze, "in"));
    rig.wire((analyze, "spectrum"), (gate, "spectrum"));
    rig.wire((gate, "out"), (synth, "spectrum"));
    rig.wire((synth, "out"), (reverse, "in"));
    rig.wire((reverse, "out"), (out, "in"));
    let id = rig.project.new_clip_id();
    let clip = Clip::audio(track, Tick(0), "tone.wav", FRAMES as u64);
    rig.apply(Command::AddClip { id, clip });

    assert_eq!(rig.freeze().rendered, [offline_kind(reverse)]);
    assert!(rig.freeze().rendered.is_empty(), "nothing changed");

    // A knob on a spectral node, upstream of the offline node only through
    // spectral wires and the synthesizer.
    rig.apply(Command::SetParam {
        node: gate,
        key: "threshold".into(),
        value: Some(-20.0),
    });
    let plan = rig
        .freezer
        .plan(&rig.analyze(), SETTINGS, FRAMES, true, None);
    assert_eq!(
        plan.state(offline_kind(reverse)),
        Some(&TargetState::Missing)
    );
    assert_eq!(rig.freeze().rendered, [offline_kind(reverse)]);
}

/// The key of the Reverse node after a track playing `tone.wav`, once
/// `change` has edited the track's clip (or the project around it).
fn key_after(change: impl FnOnce(&mut Rig, noodle_core::ClipId)) -> noodle_core::CacheKey {
    let mut rig = setup("clip-key");
    let (_, reverse, _) = rig.clip_chain(Some(REVERSE));
    let id = rig.project.clips().next().unwrap().0;
    change(&mut rig, id);
    rig.analyze().output_key(reverse.unwrap(), 0).unwrap()
}

fn edit_audio(rig: &mut Rig, id: noodle_core::ClipId, change: impl FnOnce(&mut AudioClip)) {
    let mut clip = rig.project.clip(id).unwrap().clone();
    let ClipContent::Audio(audio) = &mut clip.content else {
        panic!("an audio clip");
    };
    change(audio);
    rig.apply(Command::SetClip { id, clip });
}

#[test]
fn what_a_looping_or_extended_audio_clip_plays_is_in_the_key() {
    let plain = key_after(|_, _| {});
    // A section dragged out past its window plays longer.
    let extent = key_after(|rig, id| {
        edit_audio(rig, id, |audio| audio.extent = Some(Tick(1920)));
    });
    assert_ne!(extent, plain);
    let longer = key_after(|rig, id| {
        edit_audio(rig, id, |audio| audio.extent = Some(Tick(3840)));
    });
    assert_ne!(longer, extent);

    // The part of a warped clip that repeats.
    let looped = |start: f64, end: f64| {
        key_after(move |rig, id| {
            rig.warp_loop(id, noodle_core::LoopRegion { start, end });
        })
    };
    assert_ne!(looped(0.0, 2.0), plain);
    assert_ne!(looped(0.0, 2.0), looped(0.0, 4.0));
    assert_ne!(looped(0.0, 2.0), looped(1.0, 2.0));
}

#[test]
fn a_midi_clips_loop_is_in_the_key() {
    let key = |change: fn(&mut noodle_core::MidiClip)| {
        let mut rig = setup("midi-loop");
        let (track, reverse, _) = rig.clip_chain(Some(REVERSE));
        let id = rig.project.new_clip_id();
        let mut clip = Clip::midi(track, Tick(0), Tick(3840));
        let ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!()
        };
        midi.notes
            .push(noodle_core::MidiNote::new(Tick(0), Tick(120), 60));
        change(midi);
        rig.apply(Command::AddClip { id, clip });
        rig.analyze().output_key(reverse.unwrap(), 0).unwrap()
    };
    let straight = key(|_| {});
    let looped = key(|midi| {
        midi.loop_on = true;
        midi.loop_start = Tick(0);
        midi.loop_len = Tick(960);
    });
    assert_ne!(looped, straight);
    let shorter = key(|midi| {
        midi.loop_on = true;
        midi.loop_start = Tick(0);
        midi.loop_len = Tick(480);
    });
    assert_ne!(shorter, looped);
    let later = key(|midi| {
        midi.loop_on = true;
        midi.loop_start = Tick(240);
        midi.loop_len = Tick(960);
    });
    assert_ne!(later, looped);
}

#[test]
fn a_track_fed_live_is_not_cached_and_the_others_still_are() {
    let mut rig = setup("live-track");
    let (track, reverse, _) = rig.clip_chain(Some(REVERSE));
    let reverse = reverse.unwrap();
    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    let analyze = |live: &dyn Fn(NodeId) -> bool| {
        rig.freezer
            .analyze_known(&rig.project, &registry, SETTINGS, FRAMES, &rig.base, live)
            .0
    };
    // Hashes the file, as the app does before it renders.
    rig.freezer
        .hash_files(&[rig.base.join("tone.wav")], &Progress::new())
        .unwrap();
    let live = |node| library.clips.is_live(node);
    assert!(analyze(&live).output_key(reverse, 0).is_ok());

    library.clips.set_monitoring(track, true);
    let analysis = analyze(&live);
    assert!(matches!(
        analysis.output_key(track, 0),
        Err(noodle_engine::Uncacheable::Live)
    ));
    assert!(analysis.output_key(reverse, 0).is_err(), "what follows it");
    library.clips.set_monitoring(track, false);
    assert!(analyze(&live).output_key(reverse, 0).is_ok());

    // A key of the piano roll's keyboard held down is live too.
    library.clips.audition(track, 60, true);
    assert!(analyze(&live).output_key(reverse, 0).is_err());
    library.clips.audition(track, 60, false);
    assert!(analyze(&live).output_key(reverse, 0).is_ok());
}

#[test]
fn the_warp_preview_is_in_the_key_of_a_warped_clip() {
    let warped = |rig: &mut Rig| {
        let mut registry = Registry::with_builtins();
        let library = register_library_blocking(&mut registry);
        rig.freezer
            .hash_files(&[rig.base.join("tone.wav")], &Progress::new())
            .unwrap();
        let (_, reverse, _) = rig.clip_chain(Some(REVERSE));
        let id = rig.project.clips().next().unwrap().0;
        let command = rig
            .project
            .warp_edit(id, |warp| {
                warp.mode = noodle_core::WarpMode::Grain;
                warp.tempo = noodle_core::TempoCurve::constant(90.0);
                warp.tempo_conversion = true;
            })
            .unwrap();
        rig.apply(command);
        let key = |preview: bool| {
            library.clips.set_preview(preview);
            rig.freezer
                .analyze(&rig.project, &registry, SETTINGS, FRAMES, &rig.base)
                .output_key(reverse.unwrap(), 0)
                .unwrap()
        };
        (key(false), key(true))
    };
    let (normal, preview) = warped(&mut setup("preview-key"));
    assert_ne!(normal, preview);
}

// The playback cache's automatic targets: top-level groups, cut before their
// outputs' stages, and keyed by the start of the pass (ADR 0010).

/// Blocks of output skipped after a seek, which fades out and back in.
const FADE_SKIP: usize = 2 * 512 * 2;

/// A second group like [`synth_group`]'s, at other node IDs.
fn second_group(rig: &mut Rig) -> (NodeId, NodeId) {
    let osc = rig.add(11, "noodle.osc.saw");
    let gain = rig.add(12, noodle_nodes::GAIN_ID);
    let out = rig.add(13, OUTPUT_ID);
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
    (group, osc)
}

fn pass_at(rig: &Rig, tick: i64) -> Pass {
    let table = TempoTable::new(rig.project.tempo_map(), SETTINGS.sample_rate);
    Pass::new(Tick(tick), &table, FRAMES)
}

impl Rig {
    fn freeze_passes(&self, passes: &[Pass]) -> noodle_nodes::FreezeReport {
        freeze_passes(&self.freezer, &self.request(), passes, &Progress::new()).unwrap()
    }

    fn plan_for(&self, pass: &Pass) -> noodle_nodes::FreezePlan {
        self.freezer
            .plan(&self.analyze(), SETTINGS, FRAMES, true, Some(pass))
    }

    /// The project played from `pass`'s start after a seek, with the cache
    /// standing in for the automatic targets or not at all. The first blocks,
    /// where the seek fades, are dropped.
    fn play_pass(&self, pass: &Pass, cached: bool) -> Vec<f32> {
        self.play_seek(pass, cached.then_some(pass))
    }

    /// [`play_pass`](Self::play_pass) that seeks to `seek`'s start but takes
    /// the cache entries of `entries`' (none: live).
    fn play_seek(&self, seek: &Pass, entries: Option<&Pass>) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        let library = register_library_blocking(&mut registry);
        let table = TempoTable::new(self.project.tempo_map(), SETTINGS.sample_rate);
        library
            .clips
            .update(&self.project, &table, RATE, &self.base);
        let (mut controller, mut processor) = engine(SETTINGS).unwrap();
        if let Some(entries) = entries {
            let plan = self.plan_for(entries);
            controller.update_project_replacing(&self.project, &registry, &plan.replacements);
        } else {
            controller.update_project(&self.project, &registry);
        }
        controller.transport().seek(seek.tick);
        run(&mut controller, &mut processor, None)[FADE_SKIP..].to_vec()
    }

    fn set_group_gain(&mut self, group: NodeId, db: f32) {
        let port = self.project.graph().group_ports(group).outputs[0].clone();
        self.apply(Command::SetParam {
            node: port.node,
            key: port.control_key(noodle_core::group::GAIN),
            value: Some(db),
        });
    }
}

/// About half a second at 120 bpm, not a whole number of cycles of the tones.
const SEEK_TICK: i64 = 1000;

#[test]
fn a_group_is_cached_per_pass_start_and_plays_what_a_seek_there_plays() {
    let mut rig = setup("auto");
    let (group, osc, _) = synth_group(&mut rig);
    let auto = TargetKind::Auto(group);
    let analysis = rig.analyze();
    assert!(analysis.target(auto).is_some());
    // Without a pass, the automatic targets are not part of the plan.
    let plan = rig.freezer.plan(&analysis, SETTINGS, FRAMES, true, None);
    assert_eq!(plan.state(auto), None);

    let (start, later) = (pass_at(&rig, 0), pass_at(&rig, SEEK_TICK));
    assert_eq!(
        rig.plan_for(&later).state(auto),
        Some(&TargetState::Missing)
    );
    assert!(rig.plan_for(&later).replacements.is_empty());

    // One entry per start: rendering the later one leaves the first missing.
    assert_eq!(rig.freeze_passes(&[later]).rendered, [auto]);
    assert_eq!(rig.plan_for(&later).state(auto), Some(&TargetState::Ready));
    assert_eq!(
        rig.plan_for(&start).state(auto),
        Some(&TargetState::Missing)
    );
    assert_eq!(rig.freeze_passes(&[later]).cached, [auto]);
    assert_eq!(rig.freeze_passes(&[start, later]).rendered, [auto]);

    for pass in [&start, &later] {
        let live = rig.play_pass(pass, false);
        assert!(live.iter().any(|&s| s.abs() > 0.01));
        assert_close(&rig.play_pass(pass, true), &live, "a cached pass");
    }
    // A pass is not served by another start's entry: the sine's phase starts
    // at its start, and the entry for 0 is in the wrong place at the later one.
    let wrong = rig.play_seek(&later, Some(&start));
    let right = rig.play_pass(&later, false);
    assert!(wrong.iter().zip(&right).any(|(a, b)| (a - b).abs() > 0.01));

    // The nodes that made the sound no longer run.
    let mut registry = Registry::with_builtins();
    let _ = register_library_blocking(&mut registry);
    let lanes: Vec<_> = rig.project.lanes().collect();
    let (schedule, _) = noodle_engine::compile_replacing(
        rig.project.graph(),
        &lanes,
        &registry,
        &rig.plan_for(&later).replacements,
    );
    assert!(schedule.nodes.iter().all(|n| n.id != osc));
    assert!(
        schedule
            .nodes
            .iter()
            .any(|n| n.node_type.info().id == noodle_nodes::CACHED_ID)
    );
}

#[test]
fn a_fader_move_leaves_a_groups_entry_alone_and_an_edit_inside_replaces_it() {
    let mut rig = setup("fader");
    let (group, osc, _) = synth_group(&mut rig);
    let auto = TargetKind::Auto(group);
    let pass = pass_at(&rig, 0);
    rig.freeze_passes(&[pass]);
    let key = |rig: &Rig| {
        let target = rig.analyze().target(auto).unwrap().clone();
        let (node, output) = target.outputs[0];
        rig.analyze().pass_key(node, output, pass.tick).unwrap()
    };
    let before = key(&rig);

    // Gain and mute are applied after the cut: the entry stands, and the
    // sound has the fader's effect.
    rig.set_group_gain(group, -12.0);
    assert_eq!(key(&rig), before);
    assert_eq!(rig.plan_for(&pass).state(auto), Some(&TargetState::Ready));
    assert!(rig.freeze_passes(&[pass]).rendered.is_empty());
    let live = rig.play_pass(&pass, false);
    assert_close(&rig.play_pass(&pass, true), &live, "after the fader moved");
    let port = rig.project.graph().group_ports(group).outputs[0].clone();
    rig.apply(Command::SetParam {
        node: port.node,
        key: port.control_key(noodle_core::group::MUTE),
        value: Some(1.0),
    });
    assert_eq!(key(&rig), before);
    assert_eq!(rig.plan_for(&pass).state(auto), Some(&TargetState::Ready));

    // A node inside is not.
    rig.apply(Command::SetParam {
        node: osc,
        key: "frequency".into(),
        value: Some(440.0),
    });
    assert_ne!(key(&rig), before);
    assert_eq!(rig.plan_for(&pass).state(auto), Some(&TargetState::Missing));
    assert_eq!(rig.freeze_passes(&[pass]).rendered, [auto]);
}

#[test]
fn changing_a_node_inside_one_group_renders_only_that_group_again() {
    let mut rig = setup("two-groups");
    let (first, _, _) = synth_group(&mut rig);
    let (second, saw) = second_group(&mut rig);
    let pass = pass_at(&rig, 0);
    let both = rig.freeze_passes(&[pass]);
    assert_eq!(
        both.rendered,
        [TargetKind::Auto(first), TargetKind::Auto(second)]
    );
    assert_eq!(rig.freeze_passes(&[pass]).rendered, []);

    rig.apply(Command::SetParam {
        node: saw,
        key: "frequency".into(),
        value: Some(220.0),
    });
    let again = rig.freeze_passes(&[pass]);
    assert_eq!(again.rendered, [TargetKind::Auto(second)]);
    assert_eq!(again.cached, [TargetKind::Auto(first)]);
    let live = rig.play_pass(&pass, false);
    assert_close(&rig.play_pass(&pass, true), &live, "two cached groups");
}

#[test]
fn a_group_that_cant_be_cached_is_left_live_and_the_others_still_are() {
    let mut rig = setup("auto-live");
    let input = rig.add(31, noodle_engine::INPUT_ID);
    let gain = rig.add(32, noodle_nodes::GAIN_ID);
    let out = rig.add(33, OUTPUT_ID);
    rig.wire((input, "out"), (gain, "in"));
    rig.wire((gain, "out"), (out, "in"));
    let project = rig.project.clone();
    let mut next = project.next_node_id().0;
    let (live_group, command) = noodle_core::group::group_nodes(&project, &[input, gain], || {
        next += 1;
        NodeId(next - 1)
    })
    .unwrap();
    rig.apply(command);
    let (synth, _, _) = synth_group(&mut rig);
    let analysis = rig.analyze();
    assert!(analysis.target(TargetKind::Auto(live_group)).is_none());
    assert!(analysis.target(TargetKind::Auto(synth)).is_some());
}

#[test]
fn only_top_level_groups_that_arent_frozen_are_targets() {
    let mut rig = setup("auto-which");
    let (group, osc, _) = synth_group(&mut rig);
    // A group inside a group is part of the outer one's cluster.
    let project = rig.project.clone();
    let mut next = project.next_node_id().0;
    let (inner, command) = noodle_core::group::group_nodes(&project, &[osc], || {
        next += 1;
        NodeId(next - 1)
    })
    .unwrap();
    rig.apply(command);
    let analysis = rig.analyze();
    assert!(analysis.target(TargetKind::Auto(group)).is_some());
    assert!(analysis.target(TargetKind::Auto(inner)).is_none());

    // A group the user froze is theirs.
    rig.apply(Command::SetFrozen {
        node: group,
        frozen: true,
    });
    let analysis = rig.analyze();
    assert!(analysis.target(TargetKind::Auto(group)).is_none());
    assert!(analysis.target(TargetKind::Frozen(group)).is_some());
}

#[test]
fn an_entry_grows_with_the_project_instead_of_rendering_again() {
    let mut rig = setup("grow");
    let (group, _, _) = synth_group(&mut rig);
    let auto = TargetKind::Auto(group);
    let frozen_node = rig.add(21, noodle_nodes::GAIN_ID);
    rig.apply(Command::SetFrozen {
        node: frozen_node,
        frozen: true,
    });
    let pass = pass_at(&rig, 0);
    rig.freeze_passes(&[pass]);

    // A project a little longer is served by the same entries: a real-time
    // node's output up to a frame doesn't depend on how long the render goes.
    let longer = FRAMES + 20_000;
    let analysis = rig.analyze();
    let mut longer_pass = pass;
    longer_pass.frames = longer;
    let plan = rig
        .freezer
        .plan(&analysis, SETTINGS, longer, true, Some(&longer_pass));
    assert_eq!(plan.state(auto), Some(&TargetState::Ready));
    assert_eq!(
        plan.state(TargetKind::Frozen(frozen_node)),
        Some(&TargetState::Ready)
    );

    // Past the end of the entry it must grow: rendered again, under the same
    // key, and then ready.
    let way_longer = 40 * RATE as usize;
    longer_pass.frames = way_longer;
    let plan = rig
        .freezer
        .plan(&analysis, SETTINGS, way_longer, true, Some(&longer_pass));
    assert_eq!(plan.state(auto), Some(&TargetState::Missing));
    let mut request = rig.request();
    request.frames = way_longer;
    let report = freeze_passes(&rig.freezer, &request, &[longer_pass], &Progress::new()).unwrap();
    assert!(report.rendered.contains(&auto));
    let plan = rig
        .freezer
        .plan(&analysis, SETTINGS, way_longer, true, Some(&longer_pass));
    assert_eq!(plan.state(auto), Some(&TargetState::Ready));
}

// The tiers (ADR 0010): which entries serve which starts, and the loop rule.

/// A group of the track playing `tone.wav` into `middle` (if any), with
/// `time` set on it, and an output after it.
fn clip_group(rig: &mut Rig, middle: Option<(&str, f32)>) -> NodeId {
    let track = rig.add(1, TRACK_INPUT_ID);
    let out = rig.add(3, OUTPUT_ID);
    let id = rig.project.new_clip_id();
    let clip = Clip::audio(track, Tick(0), "tone.wav", FRAMES as u64);
    rig.apply(Command::AddClip { id, clip });
    let mut inside = vec![track];
    let mut last = track;
    let mut port = "audio";
    if let Some((kind, time)) = middle {
        let node = rig.add(2, kind);
        rig.apply(Command::SetParam {
            node,
            key: "time".into(),
            value: Some(time),
        });
        rig.wire((last, port), (node, "in"));
        inside.push(node);
        (last, port) = (node, "out");
    }
    rig.wire((last, port), (out, "in"));
    let project = rig.project.clone();
    let mut next = project.next_node_id().0;
    let (group, command) = noodle_core::group::group_nodes(&project, &inside, || {
        next += 1;
        NodeId(next - 1)
    })
    .unwrap();
    rig.apply(command);
    group
}

fn table(rig: &Rig) -> TempoTable {
    TempoTable::new(rig.project.tempo_map(), SETTINGS.sample_rate)
}

/// Ticks in a second at the default 120 bpm.
const SECOND: i64 = 1920;

#[test]
fn a_position_pure_cluster_is_one_entry_for_every_start() {
    let mut rig = setup("pure");
    let group = clip_group(&mut rig, None);
    let auto = TargetKind::Auto(group);
    let analysis = rig.analyze();
    let target = analysis.target(auto).unwrap();
    assert_eq!(analysis.memory(target), noodle_engine::Memory::None);

    let (start, later) = (pass_at(&rig, 0), pass_at(&rig, 300));
    // Both starts want the entry from 0, and it is rendered once.
    let report = rig.freeze_passes(&[start, later]);
    assert_eq!(report.rendered, [auto]);
    for pass in [&start, &later, &pass_at(&rig, 123)] {
        assert_eq!(rig.plan_for(pass).state(auto), Some(&TargetState::Ready));
    }
    let live = rig.play_pass(&later, false);
    assert!(live.iter().any(|&s| s.abs() > 0.01));
    assert_close(&rig.play_pass(&later, true), &live, "a seek into the entry");
}

#[test]
fn a_bounded_cluster_is_warmed_from_before_the_start_and_serves_a_range() {
    let mut rig = setup("bounded");
    let delay = (noodle_nodes::DELAY_ID, 0.1);
    let group = clip_group(&mut rig, Some(delay));
    let auto = TargetKind::Auto(group);
    let analysis = rig.analyze();
    let target = analysis.target(auto).unwrap();
    assert_eq!(
        analysis.memory(target),
        noodle_engine::Memory::Bounded(2000)
    );

    // Near the start, the entry from 0 serves everything: a delayed clip is
    // what a seek into a continuous play sounds like.
    let (first, second) = (pass_at(&rig, 300), pass_at(&rig, 500));
    assert_eq!(rig.freeze_passes(&[first]).rendered, [auto]);
    assert_eq!(rig.plan_for(&second).state(auto), Some(&TargetState::Ready));
    let cached = rig.play_pass(&second, true);
    let fresh = rig.play_pass(&second, false);
    // The delay is 0.1 s: a pass that began fresh at the seek is silent for
    // the first part, while the entry carries the clip from before the seek.
    let early = 3_000 * 2;
    assert!(fresh[..early].iter().all(|s| s.abs() < 1e-6));
    assert!(cached[..early].iter().any(|s| s.abs() > 0.01));
    let late = 6_000 * 2;
    assert_close(&cached[late..], &fresh[late..], "after the delay");

    // Far from 0 (past what the entry from 0 holds), the entry starts on a bar before the start less what the
    // cluster remembers (2 s), and serves the starts in the same stretch.
    let far = |ticks: i64| pass_at(&rig, 20 * SECOND + ticks);
    let spec = analysis.entries(target, &far(100))[0];
    assert_eq!(
        spec.origin,
        Tick(9 * 3840),
        "38500 ticks less 2 s, on the 3840-tick bar grid"
    );
    assert!(spec.origin_frame + 2 * RATE as u64 <= far(100).frame);
    assert_eq!(rig.freeze_passes(&[far(100)]).rendered, [auto]);
    assert_eq!(
        rig.plan_for(&far(500)).state(auto),
        Some(&TargetState::Ready)
    );
    assert_eq!(
        rig.plan_for(&far(2 * SECOND)).state(auto),
        Some(&TargetState::Missing),
        "a start the entry has not warmed for"
    );
}

#[test]
fn an_unbounded_cluster_is_served_only_at_its_own_start() {
    let mut rig = setup("unbounded");
    let (group, _, _) = synth_group(&mut rig);
    let analysis = rig.analyze();
    let target = analysis.target(TargetKind::Auto(group)).unwrap();
    assert_eq!(analysis.memory(target), noodle_engine::Memory::Unbounded);
    for ticks in [0, 300, 5 * SECOND] {
        let pass = pass_at(&rig, ticks);
        let specs = analysis.entries(target, &pass);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].origin, pass.tick);
    }
}

/// `frames` of the project played in a loop from `from` to `to`, from its
/// start, with the cache standing in for `entries`' automatic targets.
fn play_loop(
    rig: &Rig,
    entries: Option<&Pass>,
    (from, to): (Tick, Tick),
    frames: usize,
) -> Vec<f32> {
    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    library
        .clips
        .update(&rig.project, &table(rig), RATE, &rig.base);
    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    match entries {
        Some(pass) => {
            let plan = rig.plan_for(pass);
            controller.update_project_replacing(&rig.project, &registry, &plan.replacements);
        }
        None => {
            controller.update_project(&rig.project, &registry);
        }
    }
    controller.transport().set_loop(Some((from, to)));
    let mut out = vec![0.0; frames * 2];
    for block in out.chunks_mut(512 * 2) {
        processor.process(block);
        controller.maintain();
    }
    out
}

#[test]
fn a_loops_bounded_tail_crosses_the_wrap_and_an_unbounded_cluster_resets() {
    // 0.2 s: the lap, with a 0.1 s delay inside it.
    let lap_ticks = SECOND / 5;
    let lap_frames = RATE as usize / 5;
    let region = (Tick(0), Tick(lap_ticks));
    let laps = |samples: &[f32], lap: usize| {
        let start = (lap * lap_frames) * 2;
        samples[start..start + lap_frames * 2].to_vec()
    };

    let mut rig = setup("loop-bounded");
    let group = clip_group(&mut rig, Some((noodle_nodes::DELAY_ID, 0.1)));
    let auto = TargetKind::Auto(group);
    let pass = pass_at(&rig, 0).looping(region.0, region.1);
    let pass = Pass {
        frames: lap_frames,
        ..pass
    };
    let analysis = rig.analyze();
    let target = analysis.target(auto).unwrap();
    let specs = analysis.entries(target, &pass);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].frames, lap_frames, "the entry is one lap");
    // A pass starting inside the loop is served by the same steady lap.
    let inside = Pass {
        frames: lap_frames / 2,
        ..pass_at(&rig, lap_ticks / 2).looping(region.0, region.1)
    };
    assert_eq!(analysis.entries(target, &inside), specs);
    rig.freeze_passes(&[pass]);
    assert_eq!(rig.plan_for(&pass).state(auto), Some(&TargetState::Ready));
    assert_eq!(rig.plan_for(&inside).state(auto), Some(&TargetState::Ready));

    let live = play_loop(&rig, None, region, 5 * lap_frames);
    let cached = play_loop(&rig, Some(&pass), region, 5 * lap_frames);
    // The delay hands the end of the clip's first lap to the start of the
    // second, in the live loop and in the cache. A cluster reset at the wrap
    // would be silent there.
    let second = laps(&live, 1);
    assert!(second[..lap_frames].iter().any(|&s| s.abs() > 0.01));
    // The first lap has not played the loop's end yet: it is silent for the
    // delay, as a live first lap is, and not the steady lap.
    assert!(
        laps(&live, 0)[..lap_frames / 2 * 2]
            .iter()
            .all(|s| s.abs() < 1e-6)
    );
    for lap in 0..5 {
        assert_close(
            &laps(&cached, lap),
            &laps(&live, lap),
            &format!("lap {lap} carries the tail"),
        );
    }

    // An unbounded cluster resets at the wrap: every lap is the first, and
    // the last milliseconds fade out.
    let mut rig = setup("loop-unbounded");
    let (group, _, _) = synth_group(&mut rig);
    let auto = TargetKind::Auto(group);
    let pass = Pass {
        frames: lap_frames,
        ..pass_at(&rig, 0).looping(region.0, region.1)
    };
    let analysis = rig.analyze();
    let target = analysis.target(auto).unwrap();
    assert!(
        analysis
            .entries(
                target,
                &Pass {
                    frames: lap_frames / 2,
                    ..pass_at(&rig, lap_ticks / 2).looping(region.0, region.1)
                }
            )
            .is_empty(),
        "from the middle of a loop it stays live"
    );
    rig.freeze_passes(&[pass]);
    assert_eq!(rig.plan_for(&pass).state(auto), Some(&TargetState::Ready));
    let cached = play_loop(&rig, Some(&pass), region, 5 * lap_frames);
    let one = laps(&cached, 1);
    assert!(one.iter().any(|&s| s.abs() > 0.01));
    for lap in 2..4 {
        assert_close(&laps(&cached, lap), &one, "every lap starts fresh");
    }
    let end = &one[one.len() - 4..];
    assert!(
        end.iter().all(|s| s.abs() < 0.01),
        "faded out at the wrap: {end:?}"
    );
    // The sine runs on across the wrap in a live loop.
    let live = play_loop(&rig, None, region, 5 * lap_frames);
    assert!(
        laps(&live, 2)
            .iter()
            .zip(&one)
            .any(|(a, b)| (a - b).abs() > 0.01)
    );
}

#[test]
fn a_tail_longer_than_the_loop_gets_an_entry_for_each_lap_until_it_is_steady() {
    // A 0.1 s delay in a 0.04 s loop: the clip's first laps reach the output
    // two and a half laps late, so the first laps all differ.
    let lap_ticks = SECOND / 25;
    let lap_frames = RATE as usize / 25;
    let region = (Tick(0), Tick(lap_ticks));
    let mut rig = setup("loop-long-tail");
    let group = clip_group(&mut rig, Some((noodle_nodes::DELAY_ID, 0.1)));
    let auto = TargetKind::Auto(group);
    let pass = Pass {
        frames: lap_frames,
        ..pass_at(&rig, 0).looping(region.0, region.1)
    };
    let analysis = rig.analyze();
    let target = analysis.target(auto).unwrap();
    let required = analysis.requirements(target, &pass);
    // The cluster remembers 2 s: about 50 laps, then the steady lap.
    assert!(required.len() >= 50, "lap 1, the laps up to steady, steady");
    let keys: std::collections::HashSet<_> = required
        .iter()
        .map(|specs| specs[0].key(&noodle_core::KeyBuilder::new("t").finish()))
        .collect();
    assert_eq!(keys.len(), required.len(), "an entry each");

    let report = rig.freeze_passes(&[pass]);
    assert_eq!(report.rendered.len(), required.len(), "a render each");
    assert_eq!(rig.plan_for(&pass).state(auto), Some(&TargetState::Ready));
    assert!(rig.freeze_passes(&[pass]).rendered.is_empty(), "all stored");

    let laps = 60;
    let live = play_loop(&rig, None, region, laps * lap_frames);
    let cached = play_loop(&rig, Some(&pass), region, laps * lap_frames);
    let lap =
        |samples: &[f32], i: usize| samples[i * lap_frames * 2..(i + 1) * lap_frames * 2].to_vec();
    // The tone only fills the first part of each lap's input, so the laps
    // differ as the tail builds, and the live loop and the cache agree on all.
    assert!(
        lap(&live, 1)
            .iter()
            .zip(lap(&live, 3))
            .any(|(a, b)| (a - b).abs() > 0.01),
        "the early laps differ"
    );
    for i in 0..laps {
        assert_close(&lap(&cached, i), &lap(&live, i), &format!("lap {i}"));
    }
}
