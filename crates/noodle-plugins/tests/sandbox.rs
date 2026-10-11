//! The test plugin running in a plugin host process: it sounds the same as
//! in-process, survives its host dying or hanging, and its editor and state
//! capture work across the process boundary.
//!
//! Sandboxing is a process-wide setting, so these tests take turns.

mod common;

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use common::*;
use noodle_core::NodeId;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Event, EventKind, NodeType, NoteId, Shape};
use noodle_plugins::{SandboxCommand, sandbox_status, set_sandbox};

static TURN: Mutex<()> = Mutex::new(());

/// Holds the turn, and sandboxing on for as long as it lives.
struct Sandboxed {
    _turn: MutexGuard<'static, ()>,
}

fn sandboxed() -> Sandboxed {
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    set_sandbox(Some(SandboxCommand::program(env!(
        "CARGO_BIN_EXE_noodle-plugin-scan"
    ))));
    Sandboxed { _turn: turn }
}

impl Drop for Sandboxed {
    fn drop(&mut self) {
        set_sandbox(None);
    }
}

const RATE: f32 = 48_000.0;
const FRAMES: usize = 256;
const IN: usize = 0;
const GAIN: usize = 1;

fn harness(node: &dyn NodeType) -> Harness {
    Harness::new(node, &Config::new(), &[(IN, Shape::STEREO)], RATE, FRAMES).expect("harness")
}

fn note(time: u32, id: u32, on: bool) -> Event {
    Event {
        time,
        kind: if on {
            EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key: 69,
                velocity: 0.8,
            }
        } else {
            EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            }
        },
    }
}

/// Several blocks of a ramp through the plugin with a note and parameter
/// changes, as the samples of the first output channel.
fn render(h: &mut Harness) -> Vec<f32> {
    let mut samples = Vec::new();
    for block in 0..12usize {
        {
            let mut input = h.input(IN, FRAMES);
            for channel in 0..2 {
                for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
                    *s = ((block * FRAMES + i) as f32 * 0.013).sin() * 0.4;
                }
            }
        }
        h.set(GAIN, 1.0 - block as f32 * 0.05);
        let events = match block {
            2 => vec![note(40, 1, true)],
            8 => vec![note(100, 1, false)],
            _ => vec![],
        };
        h.send_events(0, &events);
        h.run(FRAMES).unwrap();
        samples.extend_from_slice(h.output(0).lane(0, 0));
    }
    samples
}

fn pid_of(node: NodeId) -> u32 {
    sandbox_status(node).and_then(|s| s.pid).expect("a host")
}

fn signal(pid: u32, signal: &str) {
    #[cfg(unix)]
    {
        // (The process may be gone already, in the test that kills every
        // replacement.)
        let _ = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
    }
    #[cfg(windows)]
    {
        let _ = signal;
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
            .unwrap();
    }
}

fn until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out: {what} ({:?})",
            sandbox_status(NodeId(0))
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_plugin_in_a_host_process_sounds_exactly_like_one_in_process() {
    let catalog = catalog();
    let node_type = node_type(&catalog);
    // In-process first, while no other test has sandboxing on.
    let expected = {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        render(&mut harness(node_type))
    };
    assert!(
        expected.iter().any(|s| s.abs() > 0.1),
        "the reference is silent"
    );

    let _sandbox = sandboxed();
    let mut h = harness(node_type);
    let status = sandbox_status(NodeId(0)).expect("the node runs in a host");
    assert!(status.running);
    let got = render(&mut h);
    assert_eq!(got, expected);
    let status = sandbox_status(NodeId(0)).unwrap();
    assert_eq!(status.dropped_blocks, 0);
    assert_eq!(status.restarts, 0);
}

#[test]
fn a_host_that_is_killed_is_replaced_and_the_node_is_silent_meanwhile() {
    let _sandbox = sandboxed();
    let catalog = catalog();
    let mut h = harness(node_type(&catalog));
    let reference = |h: &mut Harness| {
        {
            let mut input = h.input(IN, FRAMES);
            for channel in 0..2 {
                input.lane_mut(0, channel).fill(0.5);
            }
        }
        h.run(FRAMES).unwrap();
        h.output(0).lane(0, 0).to_vec()
    };
    assert!(reference(&mut h).iter().all(|&s| (s - 0.5).abs() < 1e-6));

    let first = pid_of(NodeId(0));
    signal(first, "KILL");
    // Blocks are silent until the replacement is up, and then sound again.
    let mut silent_blocks = 0;
    until("the host comes back", Duration::from_secs(20), || {
        let block = reference(&mut h);
        if block.iter().all(|&s| s == 0.0) {
            silent_blocks += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
        let status = sandbox_status(NodeId(0)).unwrap();
        status.running && status.restarts >= 1 && block.iter().all(|&s| (s - 0.5).abs() < 1e-6)
    });
    assert!(silent_blocks > 0, "the dead host's blocks weren't silent");
    let status = sandbox_status(NodeId(0)).unwrap();
    assert_ne!(status.pid, Some(first));
    assert!(status.last_error.is_some());
}

#[cfg(unix)]
#[test]
fn a_host_that_stops_answering_is_killed_and_replaced() {
    let _sandbox = sandboxed();
    let catalog = catalog();
    let mut h = harness(node_type(&catalog));
    h.run(FRAMES).unwrap();
    let first = pid_of(NodeId(0));
    signal(first, "STOP");
    until("the hung host is replaced", Duration::from_secs(30), || {
        h.run(FRAMES).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let status = sandbox_status(NodeId(0)).unwrap();
        status.running && status.restarts >= 1
    });
    assert_ne!(sandbox_status(NodeId(0)).unwrap().pid, Some(first));
    // The stopped process was killed, not left behind: there is nothing
    // to continue.
    let still_there = std::process::Command::new("kill")
        .args(["-0", &first.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!still_there.success());
}

#[test]
fn a_host_that_keeps_dying_is_given_up_on() {
    let _sandbox = sandboxed();
    let catalog = catalog();
    let mut h = harness(node_type(&catalog));
    h.run(FRAMES).unwrap();
    until("the node gives up", Duration::from_secs(60), || {
        let status = sandbox_status(NodeId(0)).unwrap();
        if status.failed {
            return true;
        }
        if status.running
            && let Some(pid) = status.pid
        {
            signal(pid, "KILL");
        }
        h.run(FRAMES).unwrap();
        false
    });
    // Silent, and not a crash of ours.
    h.run(FRAMES).unwrap();
    assert!(h.output(0).lane(0, 0).iter().all(|&s| s == 0.0));
}

#[test]
fn the_state_and_editor_of_a_sandboxed_plugin_can_be_reached() {
    let _sandbox = sandboxed();
    let catalog = catalog();
    let node = NodeId(9_100);
    let node_type = node_type(&catalog);
    let instance = node_type
        .instantiate(&noodle_engine::Setup {
            node,
            config: &Config::new(),
            sample_rate: RATE,
            max_frames: FRAMES,
            input_shapes: &[],
            output_shapes: &[],
            seed: 0,
            position: 0,
        })
        .expect("instantiate");
    assert!(noodle_plugins::is_running(node));
    let captured = noodle_plugins::capture(node).expect("capture");
    assert!(!captured.state.is_empty());
    assert!(captured.params.iter().any(|(id, _)| *id == 1));

    if cfg!(target_os = "linux") {
        noodle_plugins::open_editor(node).expect("open the editor");
        until("the editor shows", Duration::from_secs(10), || {
            noodle_plugins::editor_is_open(node)
        });
        // The test plugin's editor turns the gain down once its timer ticks.
        until("the editor's edit lands", Duration::from_secs(10), || {
            noodle_plugins::capture(node).is_ok_and(|c| {
                c.params
                    .iter()
                    .any(|(id, v)| *id == 1 && (*v - 0.25).abs() < 1e-6)
            })
        });
        noodle_plugins::close_editor(node);
        until("the editor closes", Duration::from_secs(10), || {
            !noodle_plugins::editor_is_open(node)
        });
    } else {
        assert!(noodle_plugins::open_editor(node).is_err());
    }

    drop(instance);
    until("the node is forgotten", Duration::from_secs(5), || {
        !noodle_plugins::is_running(node)
    });
}

#[test]
fn a_host_that_cannot_load_the_plugin_fails_the_node_instead_of_hanging() {
    let _sandbox = sandboxed();
    let catalog = catalog();
    let node_type = node_type(&catalog);
    // A path that isn't a plugin, under the same descriptor.
    let broken = noodle_plugins::PluginNodeType::new(
        std::env::temp_dir().join("no-such-plugin.clap"),
        node_type.descriptor().clone(),
    );
    let started = Instant::now();
    let result = Harness::new(&broken, &Config::new(), &[], RATE, FRAMES);
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn an_offline_render_through_the_hook_waits_for_hosts_and_matches_in_process() {
    use noodle_core::Project;
    use noodle_engine::{Progress, Settings};
    use noodle_nodes::{RenderRequest, render_streaming};

    let project = Project::from_ron(
        r#"(
    format: 1,
    graph: (
        nodes: {
            1: (type: "noodle.osc.sine", params: {"frequency": 440.0}, position: (x: 0.0, y: 0.0)),
            2: (type: "clap.noodle.test.gain-sine", params: {"p1": 0.5}, position: (x: 200.0, y: 0.0)),
            3: (type: "noodle.io.output", position: (x: 400.0, y: 0.0)),
        },
        connections: [
            (from: (node: 1, port: "out"), to: (node: 2, port: "in")),
            (from: (node: 2, port: "out"), to: (node: 3, port: "in")),
        ],
    ),
)"#,
    )
    .unwrap();
    let render = |catalog: &noodle_plugins::Catalog| {
        let request = RenderRequest {
            project: project.clone(),
            base: std::env::temp_dir(),
            settings: Settings {
                sample_rate: 48_000.0,
                max_frames: 512,
                channels: 2,
            },
            // Many blocks, so a host that wasn't waited for would drop some.
            frames: 96_000,
            extend_registry: Some(catalog.extender()),
        };
        let mut samples = Vec::new();
        let report = render_streaming(&request, &Progress::new(), |chunk| {
            samples.extend_from_slice(chunk);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(report.diagnostics.is_empty());
        samples
    };
    let catalog = catalog();
    let expected = {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        render(&catalog)
    };
    assert!(expected.iter().any(|s| s.abs() > 0.1));
    let _sandbox = sandboxed();
    assert_eq!(render(&catalog), expected);
}
