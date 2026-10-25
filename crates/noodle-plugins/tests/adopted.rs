//! The plugin thread adopted from the application's UI thread (what the app
//! does on macOS and Windows): other threads' requests wait for `pump`, the
//! adopted thread's own requests run at once, and plugin timers run in
//! `pump`.
//!
//! One test, because adopting is for the whole process and must come first.

mod common;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
use noodle_core::NodeId;
use noodle_engine::{Config, NodeType, Setup};
use noodle_plugins::{
    adopt_current_thread, capture, close_editor, editor_is_open, open_editor, pump,
};

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

fn gain(node: NodeId) -> f64 {
    capture(node)
        .unwrap()
        .params
        .iter()
        .find(|(id, _)| *id == 1)
        .unwrap()
        .1
}

#[test]
fn an_adopted_thread_runs_the_plugins_and_their_timers_when_it_pumps() {
    let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = wakes.clone();
    assert!(adopt_current_thread(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    assert!(!adopt_current_thread(|| {}), "only once");
    assert_eq!(pump(), None, "nothing running yet");

    let catalog = catalog();
    let node_type = node_type(&catalog);

    // On the adopted thread itself, nothing waits for a pump.
    let local = NodeId(9_101);
    let instance = build(node_type, local);
    // Loaded, but with no timer registered and no editor open, there is
    // nothing to pump for: the app's window can sleep.
    assert_eq!(pump(), None, "an idle plugin needs no pump");
    open_editor(local).expect("opens at once");
    assert!(editor_is_open(local));
    // Its timer only runs when we pump.
    let deadline = Instant::now() + Duration::from_secs(10);
    while gain(local) != 0.25 {
        assert!(Instant::now() < deadline, "the timer never ran in pump");
        pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pump().is_some_and(|d| d <= Duration::from_millis(50)));
    close_editor(local);
    drop(instance);

    // From another thread, a request waits until we pump.
    let (done, finished) = mpsc::channel();
    let remote = NodeId(9_102);
    let params = std::thread::scope(|scope| {
        scope.spawn(move || {
            let instance = build(node_type, remote);
            let captured = capture(remote).unwrap();
            drop(instance);
            done.send(captured.params.len()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "the remote request never ran");
            pump();
            if let Ok(params) = finished.recv_timeout(Duration::from_millis(10)) {
                break params;
            }
        }
    });
    assert_eq!(params, 2);
    assert!(wakes.load(std::sync::atomic::Ordering::Relaxed) > 0);
}
