//! How fast warped clips render: a minute of drum-like audio through each warp
//! mode, offline, and the speed relative to real time. Run with
//! `cargo bench -p noodle-nodes --bench warp`.
//!
//! The stretching modes run on a worker thread ahead of the playhead, so what
//! matters is the stretcher's own throughput: to play live without dropouts
//! it has to render many times faster than real time (the ring holds a third
//! of a second), and a project can play many warped clips at once. The first
//! table is the stretcher alone, on audio in memory, and the second the whole
//! offline render of one clip, which also decodes the file and mixes the
//! track, and so says little about the modes.

use std::time::Instant;

use noodle_core::{
    Clip, ClipContent, ClipWarp, Command, Connection, Endpoint, History, Node, NodeId, Project,
    Tick, WarpMarker, WarpMode,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::{StretchSource, Stretcher, TRACK_INPUT_ID, WarpMap, render_project_with_clips};
use std::sync::Arc;

const RATE: u32 = 48_000;
const SECONDS: usize = 60;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};

/// A minute of stereo: a noise burst every 0.25 s over a chord.
fn source() -> Vec<f32> {
    let frames = SECONDS * RATE as usize;
    let mut seed = 1u32;
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let t = i as f32 / RATE as f32;
        let tone = [220.0, 277.2, 329.6]
            .iter()
            .map(|f| (t * f * std::f32::consts::TAU).sin())
            .sum::<f32>()
            * 0.08;
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (seed >> 8) as f32 / (1 << 23) as f32 - 1.0;
        let since = (i % (RATE as usize / 4)) as f32 / RATE as f32;
        let hit = noise * (-since / 0.02).exp() * 0.6;
        out.push(tone + hit);
        out.push(tone - hit);
    }
    out
}

fn project(mode: WarpMode, length: u64) -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let (track, out) = (NodeId(1), NodeId(2));
    for (id, kind) in [(track, TRACK_INPUT_ID), (out, OUTPUT_ID)] {
        history
            .apply(
                &mut project,
                Command::AddNode {
                    id,
                    node: Node::new(kind),
                },
            )
            .unwrap();
    }
    history
        .apply(
            &mut project,
            Command::Connect(Connection {
                from: Endpoint::new(track, "audio"),
                to: Endpoint::new(out, "in"),
            }),
        )
        .unwrap();
    let id = project.new_clip_id();
    let warp_id = project.next_warp_id();
    let mut clip = Clip::audio(track, Tick(0), "bench.wav", length);
    let ClipContent::Audio(audio) = &mut clip.content else {
        unreachable!("not an audio clip")
    };
    audio.warp = Some(warp_id);
    // Recorded at 100 bpm on a 120 bpm timeline, with a marker that bends the
    // speed part-way.
    let mut warp = ClipWarp::at_tempo(100.0);
    warp.mode = mode;
    warp.set_marker(WarpMarker {
        seconds: 10.0,
        beat: 14.0,
    });
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddWarp { id: warp_id, warp },
                Command::AddClip { id, clip },
            ]),
        )
        .unwrap();
    project
}

struct Memory(Vec<f32>);

impl StretchSource for Memory {
    fn channels(&self) -> usize {
        2
    }

    fn read(&mut self, start: i64, out: &mut [f32]) {
        let frames = (self.0.len() / 2) as i64;
        for i in 0..out.len() / 2 {
            let at = start + i as i64;
            let frame = if (0..frames).contains(&at) {
                &self.0[at as usize * 2..at as usize * 2 + 2]
            } else {
                &[0.0, 0.0]
            };
            out[i * 2..i * 2 + 2].copy_from_slice(frame);
        }
    }
}

/// The stretcher alone, over a minute of audio played at 1.2 times its length
/// with a speed change part-way.
fn stretcher_alone() {
    println!("the stretcher alone: {SECONDS} s of stereo stretched to about 72 s");
    println!("{:<8} {:>9} {:>14}", "mode", "wall time", "x real time");
    let data = source();
    let frames = (SECONDS * RATE as usize) as f64;
    let out_len = frames * 1.2;
    let map = Arc::new(WarpMap::from_knots(vec![
        (0.0, 0.0),
        (out_len * 0.4, frames * 0.5),
        (out_len, frames),
    ]));
    for (name, beats) in [("Grain", false), ("Beats", true)] {
        let started = Instant::now();
        let onsets = if beats {
            let mono: Vec<f32> = (0..data.len() / 2)
                .map(|i| (data[i * 2] + data[i * 2 + 1]) / 2.0)
                .collect();
            noodle_io::detect_onsets(&mono, RATE)
        } else {
            Vec::new()
        };
        let mut stretcher = Stretcher::new(
            Memory(data.clone()),
            map.clone(),
            &onsets,
            out_len as u64,
            RATE as f32,
        );
        let mut buffer = vec![0.0f32; 2 * 4096];
        let mut total = 0usize;
        loop {
            let n = stretcher.read(&mut buffer);
            total += n;
            if n < 4096 {
                break;
            }
        }
        let wall = started.elapsed().as_secs_f64();
        println!(
            "{:<8} {:>8.2}s {:>13.0}x",
            name,
            wall,
            total as f64 / f64::from(RATE) / wall
        );
    }
    println!();
}

fn main() {
    stretcher_alone();
    let dir = std::env::temp_dir().join("noodle-bench-warp");
    std::fs::create_dir_all(&dir).unwrap();
    write_wav(&dir.join("bench.wav"), &source(), 2, RATE).unwrap();
    let length = (SECONDS * RATE as usize) as u64;
    println!("{SECONDS} s of stereo through one warped clip, rendered offline");
    println!("{:<8} {:>9} {:>14}", "mode", "wall time", "x real time");
    for mode in [WarpMode::Repitch, WarpMode::Grain, WarpMode::Beats] {
        let project = project(mode, length);
        // The clip is shortened by 100/120 by the tempo conversion.
        let frames = (SECONDS as f64 * 100.0 / 120.0 * f64::from(RATE)) as usize;
        let mut registry = Registry::with_builtins();
        let started = Instant::now();
        let rendered =
            render_project_with_clips(&project, &mut registry, &dir, SETTINGS, frames).unwrap();
        let wall = started.elapsed().as_secs_f64();
        assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
        let rendered_seconds = frames as f64 / f64::from(RATE);
        println!(
            "{:<8} {:>8.2}s {:>13.0}x",
            format!("{mode:?}"),
            wall,
            rendered_seconds / wall
        );
    }
}
