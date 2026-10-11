//! VST3 editors, embedded in a fake window, and the two things a VST3
//! editor depends on: its edits reaching the processor and the messages
//! between the plugin's halves.
//!
//! The test plugin's editor plays the user when it is attached (see
//! `examples/test_vst3.rs`): it turns `Gain` down to a quarter, asks for a
//! 300 x 150 window and sends the processor a message that sets `bias` to
//! 0.75, which the processor answers so the controller turns `Invert` on.
//!
//! Windows are process-wide, so this is a binary of its own, with one test.

mod common;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::*;
use noodle_core::{Command, NodeId};
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Shape};
use noodle_plugins::{
    EditorWindow, ParentWindow, capture, close_editor, editor_is_open, is_running, open_editor,
    set_window_factory,
};

const GAIN: u32 = 1;
const INVERT: u32 = 2;

/// What happened to the fake windows, in order.
#[derive(Default)]
struct Log {
    made: usize,
    sizes: Vec<(u32, u32, bool)>,
    shown: bool,
    dropped: usize,
    /// The user's doing.
    closed: bool,
    dragged_to: Option<(u32, u32)>,
}

struct Fake {
    log: Arc<Mutex<Log>>,
    size: (u32, u32),
}

impl Fake {
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl EditorWindow for Fake {
    fn parent(&self) -> ParentWindow {
        ParentWindow::X11(0xED17)
    }
    fn set_size(&mut self, width: u32, height: u32, resizable: bool) {
        self.size = (width, height);
        self.log().sizes.push((width, height, resizable));
    }
    fn size(&self) -> (u32, u32) {
        self.log().dragged_to.unwrap_or(self.size)
    }
    fn show(&mut self) {
        self.log().shown = true;
    }
    fn hide(&mut self) {
        self.log().shown = false;
    }
    fn is_closed(&self) -> bool {
        self.log().closed
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.log().dropped += 1;
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_with_input(h: &mut Harness, value: f32) -> Vec<f32> {
    {
        let mut input = h.input(0, 256);
        for channel in 0..input.shape().channels {
            input.lane_mut(0, channel).fill(value);
        }
    }
    h.run(256).unwrap();
    h.output(0).lane(0, 0).to_vec()
}

#[test]
fn the_editor_embeds_and_its_edits_and_messages_reach_the_processor() {
    let log = Arc::new(Mutex::new(Log::default()));
    let made = Arc::clone(&log);
    assert!(set_window_factory(move |_title| {
        made.lock().unwrap_or_else(PoisonError::into_inner).made += 1;
        Some(Box::new(Fake {
            log: Arc::clone(&made),
            size: (0, 0),
        }))
    }));
    let lock = || log.lock().unwrap_or_else(PoisonError::into_inner);

    let catalog = vst3_catalog();
    let node_type = vst3_node_type(&catalog);
    // The harness builds the node as node 0.
    let node = NodeId(0);
    let mut h = Harness::new(
        node_type,
        &Config::new(),
        &[(0, Shape::STEREO)],
        48_000.0,
        256,
    )
    .expect("harness");
    assert!(is_running(node));

    // Untouched: Gain's port default is a gain of 1, and no bias.
    assert!(
        run_with_input(&mut h, 1.0)
            .iter()
            .all(|&s| (s - 1.0).abs() < 1e-6)
    );

    // A port the user set reaches the processor only; capturing tells the
    // controller first, so it doesn't store its stale default over the port.
    h.set(1, 0.2);
    run_with_input(&mut h, 1.0);
    let before = capture(node).expect("capture");
    let gain = before.params.iter().find(|(p, _)| *p == GAIN).unwrap().1;
    assert!((gain - 0.2).abs() < 1e-6, "{gain}");
    h.set(1, 0.5);
    run_with_input(&mut h, 1.0);

    open_editor(node).expect("the plugin embeds");
    assert!(editor_is_open(node));
    {
        let log = lock();
        assert_eq!(log.made, 1);
        assert!(log.shown);
        // The plugin's own size, resizable.
        assert_eq!(log.sizes[0], (200, 100, true));
    }
    // The plugin asked for 300 x 150 while it attached, and the thread
    // carries that out when it next looks.
    until("the window is resized", || {
        lock().sizes.contains(&(300, 150, true))
    });
    // The user dragging the window's edge asks the plugin, which snaps the
    // size to multiples of ten.
    lock().dragged_to = Some((425, 263));
    until("the window snaps to the plugin's size", || {
        lock().sizes.contains(&(420, 260, true))
    });

    // The edit made in the editor (Gain to a quarter, a gain of 0.5) is heard,
    // though the Gain port was never touched; and the message set the bias.
    let heard = run_with_input(&mut h, 1.0);
    assert!(
        heard.iter().all(|&s| (s - (0.5 + 0.75)).abs() < 1e-6),
        "{:?}",
        &heard[..4]
    );
    // And it stays: the untouched port doesn't put the old value back.
    let again = run_with_input(&mut h, 0.0);
    assert!(again.iter().all(|&s| (s - 0.75).abs() < 1e-6));
    assert!(
        run_with_input(&mut h, 1.0)
            .iter()
            .all(|&s| (s - 1.25).abs() < 1e-6)
    );

    // Captured: the processor's state has the bias that came by message, and
    // the parameters have Gain from the edit and Invert from the answer.
    let captured = capture(node).expect("capture");
    let bias = f32::from_le_bytes(captured.state[16..20].try_into().unwrap());
    assert_eq!(bias, 0.75, "the message reached the processor");
    let value = |id| captured.params.iter().find(|(p, _)| *p == id).unwrap().1;
    assert_eq!(value(GAIN), 0.25);
    assert_eq!(value(INVERT), 1.0, "the answer reached the controller");
    let Command::Batch(commands) = captured.commands(node, node_type.descriptor()) else {
        panic!("a batch");
    };
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::SetParam { key, value: Some(v), .. } if key == "p1" && *v == 0.25
    )));

    // Closing the window the host made closes the editor.
    lock().closed = true;
    until("the editor notices the window closed", || {
        !editor_is_open(node)
    });
    until("the window is dropped", || lock().dropped == 1);
    assert!(!lock().shown);

    // Opened again, it gets a fresh window.
    {
        let mut log = lock();
        log.closed = false;
        log.dragged_to = None;
    }
    open_editor(node).expect("opens again");
    assert_eq!(lock().made, 2);
    assert!(editor_is_open(node));
    close_editor(node);
    until("closed from the node", || !editor_is_open(node));
    until("second window dropped", || lock().dropped == 2);

    // Dropping the node takes its editor with it.
    open_editor(node).expect("opens a third time");
    drop(h);
    assert!(!is_running(node));
    assert!(open_editor(node).is_err());
    until("the open editor goes with the plugin", || {
        lock().dropped == 3
    });
}
