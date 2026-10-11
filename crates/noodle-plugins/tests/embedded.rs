//! Editors embedded in a window the host makes, with a fake window in place
//! of a native one. Windows are process-wide, so this is a binary of its own.

mod common;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::*;
use noodle_core::NodeId;
use noodle_engine::{Config, NodeType, Setup};
use noodle_plugins::{
    EditorWindow, ParentWindow, capture, close_editor, editor_is_open, open_editor,
    set_window_factory,
};

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

fn build(node_type: &dyn NodeType, node: NodeId) -> noodle_engine::Instance {
    node_type
        .instantiate(&Setup {
            node,
            config: &Config::new(),
            sample_rate: 48_000.0,
            max_frames: 256,
            input_shapes: &[],
            output_shapes: &[],
            seed: 0,
            position: 0,
        })
        .expect("instantiate")
}

#[test]
fn an_editor_embeds_in_the_hosts_window_and_follows_its_size_and_closing() {
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

    let catalog = catalog();
    let node_type = node_type(&catalog);
    let node = NodeId(9_201);
    let instance = build(node_type, node);

    open_editor(node).expect("the plugin embeds");
    assert!(editor_is_open(node));
    {
        let log = lock();
        assert_eq!(log.made, 1);
        assert!(log.shown);
        // The plugin's own size (200 x 100), resizable.
        assert_eq!(log.sizes[0], (200, 100, true));
    }
    // The plugin's timer runs, and its request for a bigger window is carried
    // out (the plugin accepted our window as its parent, or it wouldn't have
    // shown).
    until("the window is resized", || lock().sizes.contains(&(300, 150, true)));

    // The user dragging the window's edge asks the plugin, which snaps the
    // size to multiples of 10, and the window follows.
    lock().dragged_to = Some((425, 263));
    until("the window snaps to the plugin's size", || {
        lock().sizes.contains(&(420, 260, true))
    });
    assert!(editor_is_open(node));

    // Closing the window the host made closes the editor.
    lock().closed = true;
    until("the editor notices the window closed", || !editor_is_open(node));
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
    // Settings made in the editor are still read from the running plugin.
    assert!(capture(node).is_ok());
    drop(instance);
}
