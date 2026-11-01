//! Audio Unit editors, embedded in a fake window. Apple's delay brings a Cocoa
//! view of its own (the generic-view fallback has no test unit that needs it).
//! AppKit views need the main thread, so this test has no harness: `main`
//! adopts the main thread as the plugin thread, as the app does, and pumps it
//! while it waits. Windows are process-wide, so it is a binary of its own.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::common::scanner;
use noodle_core::NodeId;
use noodle_engine::{Config, NodeType, Setup};
use noodle_plugins::{
    Catalog, EditorWindow, ParentWindow, capture, close_editor, editor_is_open, is_running,
    open_editor, scan_file, set_window_factory,
};
use objc2::rc::Retained;
use objc2_app_kit::NSView;

/// `aufx` `dely` `appl`: Apple's delay.
const DELAY: &str = "61756678-64656c79-6170706c";

#[derive(Default)]
struct Log {
    made: usize,
    sizes: Vec<(u32, u32, bool)>,
    shown: bool,
    dropped: usize,
    /// How many views the window held when it was dropped.
    subviews_when_dropped: Option<usize>,
    /// The user's doing.
    closed: bool,
}

struct Fake {
    log: Arc<Mutex<Log>>,
    view: Retained<NSView>,
    size: (u32, u32),
}

fn log(log: &Mutex<Log>) -> std::sync::MutexGuard<'_, Log> {
    log.lock().unwrap_or_else(PoisonError::into_inner)
}

impl EditorWindow for Fake {
    fn parent(&self) -> ParentWindow {
        ParentWindow::Cocoa(Retained::as_ptr(&self.view) as *mut c_void)
    }
    fn set_size(&mut self, width: u32, height: u32, resizable: bool) {
        self.size = (width, height);
        log(&self.log).sizes.push((width, height, resizable));
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn show(&mut self) {
        log(&self.log).shown = true;
    }
    fn hide(&mut self) {
        log(&self.log).shown = false;
    }
    fn is_closed(&self) -> bool {
        log(&self.log).closed
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let mut log = log(&self.log);
        log.dropped += 1;
        // SAFETY: a plain query of a view we own.
        log.subviews_when_dropped = Some(unsafe { self.view.subviews() }.len());
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        noodle_plugins::pump();
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn run() {
    assert!(noodle_plugins::adopt_current_thread(|| {}));
    // SAFETY: the main thread, as `main` runs on it.
    let mtm = unsafe { objc2_foundation::MainThreadMarker::new_unchecked() };
    objc2_app_kit::NSApplication::sharedApplication(mtm);
    a_unit_opens_its_editor_in_the_window_and_gives_it_back();
    println!("a_unit_opens_its_editor_in_the_window_and_gives_it_back ... ok");
}

fn a_unit_opens_its_editor_in_the_window_and_gives_it_back() {
    let shared = Arc::new(Mutex::new(Log::default()));
    let for_factory = Arc::clone(&shared);
    assert!(set_window_factory(move |_| {
        log(&for_factory).made += 1;
        // SAFETY: a plain view; it is made on the thread that uses it.
        let view = unsafe { NSView::new(objc2_foundation::MainThreadMarker::new_unchecked()) };
        Some(Box::new(Fake {
            log: Arc::clone(&for_factory),
            view,
            size: (0, 0),
        }))
    }));

    let file = scan_file(
        &scanner(),
        std::path::Path::new(&format!("audiounit:{DELAY}")),
    );
    assert_eq!(file.error, None);
    let catalog = Catalog::new(&[file]);
    let node_type = catalog.iter().next().expect("the delay is in the catalog");
    let node = NodeId(9_501);
    let config = Config::new();
    let instance = node_type
        .instantiate(&Setup {
            node,
            config: &config,
            sample_rate: 48_000.0,
            max_frames: 256,
            input_shapes: &[],
            output_shapes: &[],
            seed: 0,
            position: 0,
        })
        .expect("instantiate");
    assert!(is_running(node));
    assert!(!editor_is_open(node));

    open_editor(node).expect("the editor opens");
    assert!(editor_is_open(node));
    {
        let log = log(&shared);
        assert_eq!(log.made, 1);
        assert!(log.shown);
        let (width, height, _) = *log.sizes.last().expect("the window was sized");
        assert!(width > 0 && height > 0, "{width} x {height}");
    }

    // What the unit saves, and its parameter values, are readable while the
    // editor is up.
    let captured = capture(node).expect("capture");
    assert!(!captured.state.is_empty());
    assert!(!captured.params.is_empty());

    // The user closes the window: the editor follows.
    log(&shared).closed = true;
    until("the editor closes", || !editor_is_open(node));
    {
        let log = log(&shared);
        assert_eq!(log.dropped, 1);
        assert_eq!(
            log.subviews_when_dropped,
            Some(0),
            "the unit's view leaves the window before it goes"
        );
    }

    // And it opens again.
    log(&shared).closed = false;
    open_editor(node).expect("the editor opens again");
    assert!(editor_is_open(node));
    close_editor(node);
    until("the editor closes", || !editor_is_open(node));

    drop(instance);
    assert!(!is_running(node));
    assert!(open_editor(node).is_err());
}
