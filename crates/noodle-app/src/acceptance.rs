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

/// The meter's smoothed RMS. Not its peak: reading that resets it, and the
/// editor reads it every frame to draw the meter.
#[cfg(target_os = "linux")]
fn rms(h: &H, meter: NodeId) -> f32 {
    let levels = h.state().session().telemetry().meter(meter);
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
    let sine = add_node(&mut h, Pos2::new(250.0, 250.0), "sine");
    let output = add_node(&mut h, Pos2::new(650.0, 300.0), "io.output");
    // A meter shows the sound really goes through the engine.
    let meter = add_node(&mut h, Pos2::new(650.0, 500.0), "meter");
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
