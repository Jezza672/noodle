//! Clips linked across tracks share one warp, so they stay sample-aligned
//! through a slide and a warp: four mics on one take play as one.

use std::path::PathBuf;

use noodle_core::{
    Clip, ClipContent, ClipWarp, Command, Config, Connection, Endpoint, History, Node, NodeId,
    Project, Tick, Value, WarpMarker,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::{TRACK_INPUT_ID, render_project_with_clips};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};
const FADE: usize = 300;
/// How loud each mic is, so a clip that strayed would not add up to the sum.
const LEVELS: [f32; 4] = [1.0, 2.0, 3.0, 4.0];

fn ramp(frame: f64) -> f32 {
    (frame / 10_000.0) as f32
}

fn folder() -> PathBuf {
    let dir = std::env::temp_dir().join("noodle-linked-clips");
    std::fs::create_dir_all(&dir).unwrap();
    for (mic, level) in LEVELS.iter().enumerate() {
        let samples: Vec<f32> = (0..192_000)
            .flat_map(|i| [ramp(i as f64) * level, 0.0])
            .collect();
        write_wav(&dir.join(format!("mic{mic}.wav")), &samples, 2, RATE).unwrap();
    }
    dir
}

/// Four tracks wired to one output, a clip of one mic each, the first of them
/// warped (as a take that was warped before the others were added).
fn four_mics() -> (Project, History, [noodle_core::ClipId; 4]) {
    let mut project = Project::new();
    let mut history = History::new();
    let (out, mix) = (NodeId(10), NodeId(11));
    let mut commands = vec![
        Command::AddNode {
            id: out,
            node: Node::new(OUTPUT_ID),
        },
        Command::AddNode {
            id: mix,
            node: Node::new("noodle.util.mix")
                .with_config(Config::new().with("inputs", Value::Int(4))),
        },
        Command::Connect(Connection {
            from: Endpoint::new(mix, "out"),
            to: Endpoint::new(out, "in"),
        }),
    ];
    for mic in 0..4u64 {
        let track = NodeId(mic + 1);
        commands.push(Command::AddNode {
            id: track,
            node: Node::new(TRACK_INPUT_ID),
        });
        commands.push(Command::Connect(Connection {
            from: Endpoint::new(track, "audio"),
            to: Endpoint::new(mix, format!("in{}", mic + 1)),
        }));
    }
    let warp = project.next_warp_id();
    commands.push(Command::AddWarp {
        id: warp,
        warp: ClipWarp::at_tempo(120.0),
    });
    let mut ids = [noodle_core::ClipId(0); 4];
    for (mic, slot) in ids.iter_mut().enumerate() {
        let id = project.new_clip_id();
        *slot = id;
        let mut clip = Clip::audio(
            NodeId(mic as u64 + 1),
            Tick(0),
            format!("mic{mic}.wav"),
            4 * 48_000,
        );
        if let (0, ClipContent::Audio(audio)) = (mic, &mut clip.content) {
            audio.warp = Some(warp);
        }
        commands.push(Command::AddClip { id, clip });
    }
    history
        .apply(&mut project, Command::Batch(commands))
        .unwrap();
    (project, history, ids)
}

#[test]
fn four_linked_mics_stay_sample_aligned_through_a_slide_and_a_warp() {
    let dir = folder();
    let (mut project, mut history, ids) = four_mics();

    // Link them: the mics all take the first one's warp.
    let link = project.link_command(&ids).unwrap();
    history.apply(&mut project, link).unwrap();
    let warps: Vec<_> = ids
        .iter()
        .map(|&id| project.warp_of(id).unwrap().0)
        .collect();
    assert!(warps.iter().all(|&w| w == warps[0]), "{warps:?}");

    // Slide the take a beat (half a second) later: every member moves.
    let slide = Command::Batch(
        ids.iter()
            .map(|&id| {
                let mut clip = project.clip(id).unwrap().clone();
                clip.start = Tick(960);
                Command::SetClip { id, clip }
            })
            .collect(),
    );
    history.apply(&mut project, slide).unwrap();

    // Warp it from the first mic's markers: one edit, and it reaches them all.
    let warp = project
        .warp_edit(ids[0], |warp| {
            warp.set_marker(WarpMarker {
                seconds: 1.0,
                beat: 2.0,
            });
            warp.set_marker(WarpMarker {
                seconds: 2.0,
                beat: 5.0,
            });
        })
        .unwrap();
    assert!(matches!(warp, Command::SetWarp { .. }), "{warp:?}");
    history.apply(&mut project, warp).unwrap();

    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(&project, &mut registry, &dir, SETTINGS, 6 * 48_000).unwrap();
    assert!(rendered.render.diagnostics.is_empty());
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    let samples = &rendered.render.samples;
    let total: f32 = LEVELS.iter().sum();
    // Where the warp puts source frame `f - 24000`: the mics add up to the one
    // ramp, times the sum of their levels, wherever they are in the warp.
    let source = |f: f64| {
        let t = f - 24_000.0;
        if t < 48_000.0 {
            t
        } else if t < 120_000.0 {
            48_000.0 + (t - 48_000.0) * 2.0 / 3.0
        } else {
            96_000.0 + (t - 120_000.0)
        }
    };
    for f in (24_000 + 4 * FADE)..(24_000 + 216_000 - 2) {
        let got = samples[f * 2];
        let want = ramp(source(f as f64)) * total;
        assert!(
            (got - want).abs() < 2e-4,
            "frame {f} is {got}, wanted {want}"
        );
    }

    // One undo takes the warp edit off all four.
    history.undo(&mut project).unwrap();
    assert!(project.warp(warps[0]).unwrap().markers.is_empty());
}

#[test]
fn unlinked_mics_with_their_own_warps_drift_apart_when_one_is_warped() {
    // The same four clips, each with a warp of its own: warping one leaves
    // the others where they were, which is what linking is for.
    let (mut project, mut history, ids) = four_mics();
    let second = project.next_warp_id();
    let mut clip = project.clip(ids[1]).unwrap().clone();
    if let ClipContent::Audio(audio) = &mut clip.content {
        audio.warp = Some(second);
    }
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddWarp {
                    id: second,
                    warp: ClipWarp::at_tempo(120.0),
                },
                Command::SetClip { id: ids[1], clip },
            ]),
        )
        .unwrap();
    let edit = project
        .warp_edit(ids[0], |warp| {
            warp.set_marker(WarpMarker {
                seconds: 1.0,
                beat: 2.5,
            });
        })
        .unwrap();
    history.apply(&mut project, edit).unwrap();
    assert!(project.warp_of(ids[1]).unwrap().1.markers.is_empty());
}
