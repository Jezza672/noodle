//! M1's "done when", driven through the whole app as a user would: build a
//! patch from scratch in the node editor, tweak it while it plays, save it,
//! and reopen it exactly as it was.
//!
//! Playback uses ALSA's `null` device, which exists wherever ALSA does,
//! sound card or not. Elsewhere the test still builds, tweaks, saves and
//! reopens, just without playing.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::time::Duration;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use noodle_core::{Endpoint, NodeId, Project};
use noodle_engine::OUTPUT_ID;

use crate::app::App;
use crate::editor::socket_on_screen;
use crate::session::{Nodes, Session};

type H = Harness<'static, App>;

/// While audio plays the app keeps repainting, so these tests step a few
/// frames rather than `run` until it settles.
fn harness(app: App) -> H {
    let mut h = Harness::builder()
        .with_size(Vec2::new(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

/// Presses the primary button at the first point, moves through the rest
/// and releases at the last.
fn drag(h: &mut H, path: &[Pos2]) {
    h.event(Event::PointerMoved(path[0]));
    h.step();
    h.event(Event::PointerButton {
        pos: path[0],
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for &p in &path[1..] {
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(Event::PointerButton {
        pos: *path.last().unwrap(),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
}

/// Shift+A at `at`, then picks the first search result for `query`.
fn add_node(h: &mut H, at: Pos2, query: &str) -> NodeId {
    let before: Vec<NodeId> = nodes(h);
    h.hover_at(at);
    h.step();
    h.key_press_modifiers(Modifiers::SHIFT, Key::A);
    h.run_steps(3);
    h.event(Event::Text(query.into()));
    h.run_steps(3);
    h.key_press(Key::Enter);
    h.run_steps(3);
    let added: Vec<NodeId> = nodes(h)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(added.len(), 1, "searching for {query:?} added one node");
    added[0]
}

fn nodes(h: &H) -> Vec<NodeId> {
    let graph = h.state().session().project().graph();
    graph.nodes().map(|(id, _)| id).collect()
}

/// Drags a wire from an output socket to an input socket.
fn wire(h: &mut H, from: (NodeId, &str), to: (NodeId, &str)) {
    let from = socket(h, from.0, true, from.1);
    let to = socket(h, to.0, false, to.1);
    let path: Vec<Pos2> = (0..=10).map(|i| from.lerp(to, i as f32 / 10.0)).collect();
    drag(h, &path);
}

/// The meter's smoothed RMS, read through a reader of the test's own so the
/// editor's peaks are left alone.
#[cfg(target_os = "linux")]
fn rms(h: &H, meter: NodeId) -> f32 {
    let levels = h.state().session().telemetry().meter_reader().meter(meter);
    levels.expect("the meter is running")[0].rms
}

fn socket(h: &H, node: NodeId, output: bool, key: &str) -> Pos2 {
    let app = h.state();
    socket_on_screen(app.editor(), app.session(), node, output, key).expect("the port is drawn")
}

/// Drags the properties panel's slider for `label` sideways.
fn drag_slider(h: &mut H, label: &str, dx: f32) {
    // The rightmost one is the properties panel's, should a node draw one
    // too.
    let rect = h
        .query_all_by_label(label)
        .map(|node| node.rect())
        .max_by(|a, b| a.center().x.total_cmp(&b.center().x))
        .unwrap_or_else(|| panic!("no {label:?} slider"));
    let start = rect.center();
    let path: Vec<Pos2> = (0..=10)
        .map(|i| start + Vec2::new(dx * i as f32 / 10.0, 0.0))
        .collect();
    drag(h, &path);
}

fn param(project: &Project, node: NodeId, key: &str) -> Option<f32> {
    project.graph().node(node)?.params.get(key).copied()
}

/// Lets the devices run for a while, as frames go by.
#[cfg(target_os = "linux")]
fn play_for(h: &mut H, time: Duration) {
    let frames = 10;
    for _ in 0..frames {
        std::thread::sleep(time / frames);
        h.run_steps(3);
    }
}

/// Plays on ALSA's null device. Not its null input too: the null devices
/// aren't clocked, so input and output drift apart and every block counts as
/// an input glitch.
#[cfg(target_os = "linux")]
fn null_devices() -> noodle_io::AudioConfig {
    noodle_io::AudioConfig {
        output: Some("alsa:null".into()),
        ..noodle_io::AudioConfig::default()
    }
}

fn open(path: &Path) -> Session {
    Session::open(Nodes::all(), path).expect("the project opens")
}

#[test]
fn build_a_patch_play_and_tweak_it_save_and_reopen_it() {
    let dir = tempfile::tempdir().unwrap();
    crate::prefs::init(dir.path().join("prefs.ron"));
    // An empty project with a file, so Save doesn't open a file dialog.
    let path = dir.path().join("patch.ron");
    std::fs::write(&path, Project::new().to_ron()).unwrap();
    #[cfg_attr(not(target_os = "linux"), expect(unused_mut))]
    let mut session = open(&path);
    #[cfg(target_os = "linux")]
    session.set_audio_config(null_devices());
    let mut h = harness(App::new(session));

    // Build: a sine wired into the output, all through the editor.
    let sine = add_node(&mut h, Pos2::new(250.0, 400.0), "sine");
    let output = add_node(&mut h, Pos2::new(650.0, 430.0), "output");
    // A meter shows the sound really goes through the engine.
    let meter = add_node(&mut h, Pos2::new(650.0, 560.0), "meter");
    wire(&mut h, (sine, "out"), (output, "in"));
    wire(&mut h, (sine, "out"), (meter, "in"));
    {
        let session = h.state().session();
        let graph = session.project().graph();
        assert_eq!(graph.node(sine).unwrap().type_id, "noodle.osc.sine");
        assert_eq!(graph.node(output).unwrap().type_id, OUTPUT_ID);
        assert_eq!(graph.node(meter).unwrap().type_id, "noodle.view.meter");
        assert_eq!(
            graph.source(&Endpoint::new(output, "in")),
            Some(&Endpoint::new(sine, "out")),
            "the editor wired the sine to the output"
        );
        assert_eq!(
            graph.source(&Endpoint::new(meter, "in")),
            Some(&Endpoint::new(sine, "out")),
        );
        assert!(
            session.diagnostics().is_empty(),
            "{:?}",
            session.diagnostics()
        );
    }

    // Play, and tweak the sine while it plays.
    #[cfg(target_os = "linux")]
    {
        h.key_press(Key::Space);
        h.run_steps(3);
        let session = h.state().session();
        assert!(session.is_playing(), "{:?}", session.message());
        assert_eq!(session.input_problem(), None);
        play_for(&mut h, Duration::from_millis(200));
        // A full-scale sine settles at about 0.707.
        assert!(rms(&h, meter) > 0.3, "the sine reaches the meter");
    }
    let select = socket(&h, sine, true, "out") - Vec2::new(60.0, 0.0);
    drag(&mut h, &[select]);
    assert_eq!(h.state().editor().active, Some(sine));
    let before = param(h.state().session().project(), sine, "frequency");
    drag_slider(&mut h, "Frequency", 15.0);
    let tweaked = param(h.state().session().project(), sine, "frequency")
        .expect("dragging the slider set the frequency");
    assert_ne!(Some(tweaked), before);
    #[cfg(target_os = "linux")]
    {
        play_for(&mut h, Duration::from_millis(200));
        let session = h.state().session();
        assert!(session.is_playing(), "{:?}", session.message());
        // Underruns, input glitches and device errors all land here.
        assert_eq!(session.message(), None, "playback was clean");
        assert!(rms(&h, meter) > 0.3, "still playing after the tweak");
    }

    // Save, and reopen it as it was.
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    h.run_steps(3);
    let built = h.state().session().project().to_ron();
    assert!(!h.state().session().is_dirty());
    drop(h);

    let reopened = open(&path);
    assert_eq!(reopened.project().to_ron(), built);
    assert_eq!(param(reopened.project(), sine, "frequency"), Some(tweaked));
    assert!(!reopened.is_dirty());
    let h = harness(App::new(reopened));
    // The editor draws the reopened patch.
    socket(&h, output, false, "in");
}

#[cfg(target_os = "linux")]
#[test]
fn the_transport_plays_pauses_rewinds_and_follows_tempo_edits() {
    use noodle_core::{Command, TempoMap, Tick, TimeSignature};

    use crate::session::Edit;

    let dir = tempfile::tempdir().unwrap();
    crate::prefs::init(dir.path().join("prefs.ron"));
    let mut session = Session::new(Nodes::all());
    session.set_audio_config(null_devices());
    let mut h = harness(App::new(session));
    h.key_press(Key::Space);
    h.run_steps(3);
    assert!(h.state().session().is_playing());

    let playhead = |h: &H| h.state().session().playhead();
    play_for(&mut h, Duration::from_millis(300));
    assert!(playhead(&h) > Tick(0), "the playhead moves while playing");

    // Paused, it holds still.
    h.state_mut().session_mut().set_transport_running(false);
    play_for(&mut h, Duration::from_millis(100));
    let held = playhead(&h);
    play_for(&mut h, Duration::from_millis(200));
    assert_eq!(playhead(&h), held);

    h.state_mut().session_mut().rewind();
    play_for(&mut h, Duration::from_millis(200));
    assert_eq!(playhead(&h), Tick(0));

    // A new project starts at the beginning, running. The null device isn't
    // clocked, so the playhead runs far ahead; it should drop back.
    h.state_mut().session_mut().set_transport_running(true);
    play_for(&mut h, Duration::from_millis(200));
    h.state_mut().session_mut().set_transport_running(false);
    play_for(&mut h, Duration::from_millis(100));
    let far = playhead(&h);
    h.state_mut().session_mut().new_project();
    let mut lowest = far;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(2));
        lowest = lowest.min(playhead(&h));
    }
    assert!(
        lowest < Tick(far.0 / 2),
        "rewound from {far:?}, lowest {lowest:?}"
    );
    assert!(h.state().session().transport_running());

    // Stopping keeps the playhead; it can be moved while stopped, and
    // playing starts from there.
    h.state_mut().session_mut().set_transport_running(false);
    h.state_mut().session_mut().seek(Tick(7680));
    // The audio thread takes the seek at its next block.
    for _ in 0..100 {
        if h.state().session().playhead() == Tick(7680) {
            break;
        }
        play_for(&mut h, Duration::from_millis(10));
    }
    let session = h.state_mut().session_mut();
    session.stop();
    assert_eq!(session.playhead(), Tick(7680));
    session.seek(Tick(3840));
    assert_eq!(session.playhead(), Tick(3840));
    session.play();
    assert!(session.is_playing(), "{:?}", session.message());
    session.set_transport_running(false);
    play_for(&mut h, Duration::from_millis(200));
    assert_eq!(playhead(&h), Tick(3840), "playing started from the seek");
    h.state_mut().session_mut().set_transport_running(true);

    // A tempo edit reaches the engine.
    let fast = TempoMap::constant(240.0, TimeSignature::COMMON).unwrap();
    h.state_mut()
        .session_mut()
        .edit([Edit::Apply(Command::SetTempoMap(fast.clone()))]);
    play_for(&mut h, Duration::from_millis(200));
    assert!(h.state().session().tempo_in_engine() == Some(&fast));
}

#[cfg(target_os = "linux")]
#[test]
fn a_clip_on_a_track_plays_and_follows_edits() {
    use noodle_core::{Clip, Command, Connection, Node, Tick};
    use noodle_engine::OUTPUT_ID;

    use crate::session::Edit;

    let dir = tempfile::tempdir().unwrap();
    crate::prefs::init(dir.path().join("prefs.ron"));
    // Half a second of a constant, so the meter reads it exactly.
    let frames = 24_000;
    noodle_io::write_wav(&dir.path().join("tone.wav"), &vec![0.5; frames], 1, 48_000).unwrap();
    let path = dir.path().join("song.ron");
    std::fs::write(&path, Project::new().to_ron()).unwrap();
    let mut session = open(&path);
    session.set_audio_config(null_devices());

    let new = |session: &Session, type_id: &str| (session.new_node_id(), Node::new(type_id));
    let (track, track_node) = new(&session, "noodle.track.input");
    session.edit([Edit::Apply(Command::AddNode {
        id: track,
        node: track_node,
    })]);
    let (output, output_node) = new(&session, OUTPUT_ID);
    session.edit([Edit::Apply(Command::AddNode {
        id: output,
        node: output_node,
    })]);
    session.edit([Edit::Apply(Command::Connect(Connection {
        from: Endpoint::new(track, "audio"),
        to: Endpoint::new(output, "in"),
    }))]);
    session.play();
    assert!(session.is_playing(), "{:?}", session.message());
    // The null device isn't clocked, so playback races ahead of the disk
    // thread; hold the timeline at the start, where the clip will begin.
    session.set_transport_running(false);
    session.rewind();
    // Added while playing, so it reaches the track input as an edit.
    let clip = noodle_core::ClipId(1);
    session.edit([Edit::Apply(Command::AddClip {
        id: clip,
        clip: Clip::audio(track, Tick(0), "tone.wav", frames as u64),
    })]);
    let mut h = harness(App::new(session));
    // The track input opens a stream for the clip once it is scheduled.
    // Generous, since a busy machine can be slow to open the file.
    let mut streams = 0;
    for _ in 0..200 {
        play_for(&mut h, Duration::from_millis(50));
        streams = h.state().session().clip_status(track).streams;
        if streams > 0 {
            break;
        }
    }
    assert_eq!(streams, 1, "a stream is ready for the clip");
    assert!(h.state().session().clip_problems().is_empty());

    // A clip whose file is missing is reported, and the rest still plays.
    let missing = noodle_core::ClipId(2);
    h.state_mut()
        .session_mut()
        .edit([Edit::Apply(Command::AddClip {
            id: missing,
            clip: Clip::audio(track, Tick(1920), "gone.wav", 100),
        })]);
    let problems = h.state().session().clip_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].clip, missing);

    // Saving elsewhere resolves relative clips against the new folder, where
    // the tone isn't, so it is reported too.
    let other = tempfile::tempdir().unwrap();
    assert!(
        h.state_mut()
            .session_mut()
            .save_as(&other.path().join("moved.ron"))
    );
    assert_eq!(h.state().session().clip_problems().len(), 2);

    // Stopping leaves nothing scheduled, so nothing to report.
    h.state_mut().session_mut().stop();
    assert!(h.state().session().clip_problems().is_empty());
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not needed".into())
    }
}

/// Drops `file` with the pointer at `at`.
fn drop_file(h: &mut H, at: Pos2, file: &Path) {
    h.event(Event::PointerMoved(at));
    h.step();
    h.input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(file.to_owned())));
    h.step();
    h.run();
}

/// M2's "done when", driven through the whole app as a user would: arrange
/// clips on tracks, automate a parameter, look at the mixer, and mix down,
/// offline and live, checking the levels.
#[test]
fn arrange_clips_on_tracks_automate_a_parameter_and_mix_down() {
    use noodle_core::group::{GROUP_OUTPUT, MUTE};
    use noodle_core::{Curve, Tick};

    const RATE: usize = 48_000;
    const BEAT: usize = RATE / 2;
    let dir = tempfile::tempdir().unwrap();
    crate::prefs::init(dir.path().join("prefs.ron"));
    // Four beats of a constant each, so the mix reads exactly.
    let tone = dir.path().join("tone.wav");
    let quiet = dir.path().join("quiet.wav");
    noodle_io::write_wav(&tone, &vec![0.5; 4 * BEAT], 1, RATE as u32).unwrap();
    noodle_io::write_wav(&quiet, &vec![0.25; 4 * BEAT], 1, RATE as u32).unwrap();
    let path = dir.path().join("song.ron");
    std::fs::write(&path, Project::new().to_ron()).unwrap();
    #[allow(unused_mut)]
    let mut session = open(&path);
    #[cfg(target_os = "linux")]
    session.set_audio_config(null_devices());
    let mut h = harness(App::new(session));

    // Two tracks, with a clip dropped on each lane at the first beat.
    for _ in 0..2 {
        h.get_by_label("+ Add track").click();
        h.run();
    }
    let lane_at = |h: &H, name: &str| {
        let label = h.get_by_label(name).rect();
        Pos2::new(label.left() - 12.0 + 150.0 + 10.0, label.top() + 20.0)
    };
    let first = lane_at(&h, "Track 1");
    drop_file(&mut h, first, &tone);
    let second = lane_at(&h, "Track 2");
    drop_file(&mut h, second, &quiet);
    let clips: Vec<_> = h
        .state()
        .session()
        .project()
        .clips()
        .map(|(_, clip)| (clip.node, clip.start))
        .collect();
    assert_eq!(clips.len(), 2, "{clips:?}");
    assert!(clips.iter().all(|&(_, start)| start == Tick(0)));

    // Track 2 mutes itself from the third beat, through a lane made in the
    // header menu and clicked into the row.
    let outputs: Vec<NodeId> = h
        .state()
        .session()
        .project()
        .graph()
        .nodes()
        .filter(|(_, node)| node.type_id == GROUP_OUTPUT)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(outputs.len(), 2);
    h.get_all_by_label("~").nth(1).unwrap().click();
    h.run();
    h.get_by_label("Automate Mute").click();
    h.run();
    let (lane, _) = {
        let project = h.state().session().project();
        let found = outputs
            .iter()
            .find_map(|&out| project.lane_for(&Endpoint::new(out, MUTE)))
            .expect("the lane was made");
        (found.0, found.1.clone())
    };
    let row = h.state().timeline().automation().row(lane).unwrap();
    let spot = Pos2::new(row.left() + 2.0 * 60.0, row.top() + 8.0);
    h.event(Event::PointerMoved(spot));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: spot,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run();
    let points: Vec<_> = h
        .state()
        .session()
        .project()
        .lane(lane)
        .unwrap()
        .points
        .iter()
        .map(|p| (p.tick, p.value, p.curve))
        .collect();
    assert_eq!(
        points,
        vec![(Tick(0), 0.0, Curve::Hold), (Tick(1920), 1.0, Curve::Hold)]
    );

    // The mixer shows both tracks.
    h.get_by_label("View").click();
    h.run();
    h.get_by_label("Mixer").click();
    h.run();
    h.get_by_label("Track 1");
    h.get_by_label("Track 2");

    // Offline, the mix is both clips for two beats, then the first alone.
    let project = h.state().session().project().clone();
    let mut registry = noodle_engine::Registry::with_builtins();
    let settings = noodle_engine::Settings {
        sample_rate: RATE as f32,
        max_frames: 512,
        channels: 2,
    };
    let rendered = noodle_nodes::render_project_with_clips(
        &project,
        &mut registry,
        dir.path(),
        settings,
        4 * BEAT,
    )
    .unwrap();
    assert!(rendered.render.diagnostics.is_empty());
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    assert_eq!(rendered.underruns, 0);
    let level = |from: usize, to: usize, want: f32| {
        // Clear of the clips' ends and the mute's smoothed step.
        for frame in from + 3_000..to - 3_000 {
            let got = rendered.render.samples[frame * 2];
            assert!(
                (got - want).abs() < 2e-3,
                "frame {frame} is {got}, wanted {want}"
            );
        }
    };
    level(0, 2 * BEAT, 0.75);
    level(2 * BEAT, 4 * BEAT, 0.5);

    // Live, both clips get a stream and nothing is reported.
    #[cfg(target_os = "linux")]
    {
        h.state_mut().session_mut().play();
        assert!(
            h.state().session().is_playing(),
            "{:?}",
            h.state().session().message()
        );
        h.state_mut().session_mut().set_transport_running(false);
        h.state_mut().session_mut().rewind();
        let inputs: Vec<NodeId> = project.clips().map(|(_, clip)| clip.node).collect();
        let mut ready = false;
        for _ in 0..200 {
            play_for(&mut h, Duration::from_millis(50));
            ready = inputs
                .iter()
                .all(|&node| h.state().session().clip_status(node).streams == 1);
            if ready {
                break;
            }
        }
        assert!(ready, "both clips have a stream");
        assert!(h.state().session().clip_problems().is_empty());
        h.state_mut().session_mut().stop();
    }
}
