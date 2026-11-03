//! Export, end to end: the file holds what playback plays, offline nodes
//! included, over the range asked for, in the format asked for.

use std::path::PathBuf;

use noodle_core::group::create_track;
use noodle_core::spare::{self, MIXER};
use noodle_core::{Clip, Command, Connection, Endpoint, History, Node, NodeId, Project, Tick};
use noodle_engine::{OUTPUT_ID, Progress, Registry, Settings};
use noodle_io::{CacheStore, ExportFormat, decode_file, read_wav, write_wav};
use noodle_nodes::{
    CLIP_SILENCE, ClipRequest, ExportJobError, ExportRequest, Freezer, RenderRequest,
    TRACK_INPUT_ID, TrackExport, export, render_clip, render_project_with_clips, spawn_export,
};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};
/// Several chunks, and not a whole number of them.
const FRAMES: usize = 5 * 16_384 + 123;

struct Rig {
    project: Project,
    history: History,
    root: PathBuf,
    freezer: Freezer,
}

fn setup(name: &str) -> Rig {
    let root = std::env::temp_dir().join(format!("noodle-export-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let samples: Vec<f32> = (0..FRAMES)
        .flat_map(|i| {
            let x = (i as f32 * 0.05).sin() * 0.25 * (1.0 - i as f32 / FRAMES as f32);
            [x, -x * 0.5]
        })
        .collect();
    write_wav(&root.join("tone.wav"), &samples, 2, RATE).unwrap();
    Rig {
        project: Project::new(),
        history: History::new(),
        freezer: Freezer::new(CacheStore::open(root.join("cache")).unwrap()),
        root,
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

    fn set(&mut self, node: NodeId, port: &str, value: f32) {
        self.apply(Command::SetParam {
            node,
            key: port.into(),
            value: Some(value),
        });
    }

    /// Track input -> each of `chain`, in order -> Output, playing `tone.wav`.
    fn chain(&mut self, chain: &[&str]) -> Vec<NodeId> {
        let track = self.add(1, TRACK_INPUT_ID);
        let out = self.add(100, OUTPUT_ID);
        let mut ids = Vec::new();
        let mut last = (track, "audio");
        for (i, kind) in chain.iter().enumerate() {
            let id = self.add(10 + i as u64, kind);
            self.wire(last, (id, "in"));
            last = (id, "out");
            ids.push(id);
        }
        self.wire(last, (out, "in"));
        let id = self.project.new_clip_id();
        self.apply(Command::AddClip {
            id,
            clip: Clip::audio(track, Tick(0), "tone.wav", FRAMES as u64),
        });
        ids
    }

    fn request(&self, name: &str, format: ExportFormat, range: (u64, u64)) -> ExportRequest {
        ExportRequest {
            render: RenderRequest {
                project: self.project.clone(),
                base: self.root.clone(),
                settings: SETTINGS,
                frames: FRAMES,
                extend_registry: None,
            },
            start: range.0,
            end: range.1,
            mixdown: Some(self.root.join(format!("{name}.{}", format.extension()))),
            tracks: Vec::new(),
            format,
            normalize: None,
            true_peak_ceiling: None,
            metadata: Default::default(),
            cue: false,
            freezer: Some(self.freezer.clone()),
            silent_tail: None,
        }
    }

    /// Exports `FRAMES` frames as float WAV and returns the samples.
    fn export(&self, name: &str) -> Vec<f32> {
        self.export_range(name, (0, FRAMES as u64))
    }

    fn export_range(&self, name: &str, range: (u64, u64)) -> Vec<f32> {
        let request = self.request(name, ExportFormat::WavFloat, range);
        let report = export(&request, &Progress::new()).unwrap();
        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.frames, range.1 - range.0);
        read_wav(request.mixdown.as_ref().unwrap()).unwrap().samples
    }

    /// The project rendered by the ordinary offline renderer, which knows
    /// nothing of caches.
    fn plain(&self) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        render_project_with_clips(&self.project, &mut registry, &self.root, SETTINGS, FRAMES)
            .unwrap()
            .render
            .samples
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, x| f32::max(m, x.abs()))
}

#[test]
fn a_plain_project_exports_what_the_renderer_renders() {
    let mut rig = setup("plain");
    rig.chain(&[]);
    let expected = rig.plain();
    assert!(peak(&expected) > 0.1);
    assert_eq!(rig.export("plain"), expected);
}

#[test]
fn a_range_writes_only_that_part() {
    let mut rig = setup("range");
    rig.chain(&[]);
    let whole = rig.export("whole");
    let (start, end) = (20_000usize, 70_001usize);
    let part = rig.export_range("part", (start as u64, end as u64));
    assert_eq!(part, whole[start * 2..end * 2]);
    // Stateful nodes have run from the start: the range is a cut, not a
    // restart.
    let mut echo = setup("range-echo");
    let delay = echo.chain(&["noodle.util.delay"])[0];
    echo.set(delay, "time", 0.1);
    let whole = echo.export("whole");
    let part = echo.export_range("part", (30_000, 60_000));
    assert_eq!(part, whole[60_000..120_000]);
}

#[test]
fn offline_nodes_are_rendered_and_heard() {
    let mut rig = setup("offline");
    let plain = {
        let mut rig = setup("offline-plain");
        rig.chain(&[]);
        rig.plain()
    };
    rig.chain(&["noodle.offline.reverse"]);
    let exported = rig.export("reverse");
    let reversed: Vec<f32> = plain
        .chunks(2)
        .rev()
        .flat_map(|frame| frame.iter().copied())
        .collect();
    assert_eq!(exported, reversed);
    // The renders went into the cache for playback to share.
    assert!(
        rig.root
            .join("cache")
            .read_dir()
            .unwrap()
            .any(|e| e.is_ok())
    );
}

#[test]
fn a_range_past_the_playback_length_still_hears_offline_nodes() {
    const EXTRA: usize = 20_000;
    let mut rig = setup("long-range");
    rig.chain(&["noodle.offline.reverse"]);
    // `request` keys renders by `FRAMES`; the file runs `EXTRA` frames longer.
    let end = (FRAMES + EXTRA) as u64;
    let request = rig.request("long", ExportFormat::WavFloat, (0, end));
    export(&request, &Progress::new()).unwrap();
    let exported = read_wav(request.mixdown.as_ref().unwrap()).unwrap().samples;
    assert_eq!(exported.len(), (FRAMES + EXTRA) * 2);
    // The reverse covers the whole file, so the clip, now at its end, lands
    // at the end. Rendered only to `FRAMES` it would leave this all silent.
    assert_eq!(peak(&exported[..EXTRA * 2]), 0.0);
    assert!(peak(&exported[EXTRA * 2..]) > 0.2);
}

#[test]
fn normalize_brings_the_peak_to_the_level() {
    let mut rig = setup("normalize");
    let ids = rig.chain(&["noodle.offline.normalize"]);
    rig.set(ids[0], "level", -6.0);
    let exported = rig.export("normalize");
    let wanted = 10f32.powf(-6.0 / 20.0);
    assert!(
        (peak(&exported) - wanted).abs() < 1e-4,
        "{}",
        peak(&exported)
    );
}

#[test]
fn a_chain_of_offline_nodes_renders_each_from_the_one_before() {
    let mut rig = setup("chain");
    let ids = rig.chain(&[
        "noodle.offline.reverse",
        "noodle.offline.time_stretch",
        "noodle.offline.normalize",
    ]);
    rig.set(ids[1], "ratio", 1.5);
    rig.set(ids[2], "level", -3.0);
    let exported = rig.export("chain");
    let wanted = 10f32.powf(-3.0 / 20.0);
    assert!(
        (peak(&exported) - wanted).abs() < 1e-4,
        "{}",
        peak(&exported)
    );
    // The stretch really happened: the reversed tone fades in, so slowing
    // it down moves the loud part later than the unstretched render.
    let mut unstretched = setup("chain-plain");
    let ids = unstretched.chain(&["noodle.offline.reverse", "noodle.offline.normalize"]);
    unstretched.set(ids[1], "level", -3.0);
    let other = unstretched.export("chain-plain");
    assert_ne!(exported, other);
}

#[test]
fn a_wire_into_an_offline_nodes_parameter_modulates_it() {
    // The Time Stretch ratio offsets along its travel. A wire carrying
    // silence leaves the base value where it is, so it must sound the same
    // as no wire. (A node that skipped the offset would read the silence as
    // a ratio of nothing.)
    let mut bare = setup("modulated-bare");
    let ids = bare.chain(&["noodle.offline.time_stretch"]);
    bare.set(ids[0], "ratio", 2.0);
    let expected = bare.export("bare");

    let mut wired = setup("modulated-wired");
    let ids = wired.chain(&["noodle.offline.time_stretch"]);
    wired.set(ids[0], "ratio", 2.0);
    // A Gain with nothing wired in makes silence.
    let quiet = wired.add(50, "noodle.util.gain");
    wired.wire((quiet, "out"), (ids[0], "ratio"));
    let wired = wired.export("wired");
    let worst = expected
        .iter()
        .zip(&wired)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-3, "off by {worst}");

    // A wire that is not silent does change it.
    let mut moving = setup("modulated-moving");
    let ids = moving.chain(&["noodle.offline.time_stretch"]);
    moving.set(ids[0], "ratio", 2.0);
    let lfo = moving.add(50, "noodle.mod.lfo");
    moving.set(lfo, "rate", 3.0);
    moving.wire((lfo, "out"), (ids[0], "ratio"));
    assert_ne!(moving.export("moving"), expected);
}

#[test]
fn every_format_holds_the_same_audio() {
    let mut rig = setup("formats");
    rig.chain(&[]);
    let expected = rig.plain();
    for format in ExportFormat::ALL {
        let request = rig.request("formats", format, (0, FRAMES as u64));
        export(&request, &Progress::new()).unwrap();
        let audio = decode_file(request.mixdown.as_ref().unwrap()).unwrap();
        assert_eq!(audio.channels, 2, "{format:?}");
        assert_eq!(audio.sample_rate, RATE, "{format:?}");
        assert_eq!(audio.samples.len(), expected.len(), "{format:?}");
        let tolerance = match format {
            ExportFormat::WavFloat => 0.0,
            ExportFormat::Wav24 | ExportFormat::Flac24 => 3e-7,
            ExportFormat::Wav16 | ExportFormat::Flac16 => 7e-5,
        };
        let worst = expected
            .iter()
            .zip(&audio.samples)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst <= tolerance, "{format:?}: off by {worst}");
    }
}

#[test]
fn an_empty_range_is_refused_and_a_cancelled_export_leaves_no_file() {
    let mut rig = setup("cancel");
    rig.chain(&[]);
    let request = rig.request("never", ExportFormat::WavFloat, (500, 500));
    assert!(matches!(
        export(&request, &Progress::new()),
        Err(ExportJobError::EmptyRange)
    ));

    let request = rig.request("cancelled", ExportFormat::Flac16, (0, FRAMES as u64));
    let progress = Progress::new();
    progress.cancel();
    assert!(matches!(
        export(&request, &progress),
        Err(ExportJobError::Cancelled)
    ));
    assert!(!request.mixdown.as_ref().unwrap().exists());
    let mut partial = request.mixdown.as_ref().unwrap().clone().into_os_string();
    partial.push(".partial");
    assert!(!PathBuf::from(partial).exists());
}

#[test]
fn a_background_export_reports_progress_and_finishes() {
    let mut rig = setup("job");
    rig.chain(&["noodle.offline.reverse"]);
    let request = rig.request("job", ExportFormat::Wav24, (0, FRAMES as u64));
    let mut job = spawn_export(request.clone());
    let report = loop {
        if let Some(done) = job.poll() {
            break done.unwrap().unwrap();
        }
        assert!((0.0..=1.0).contains(&job.fraction()));
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(report.frames, FRAMES as u64);
    assert!(request.mixdown.as_ref().unwrap().exists());
    assert_eq!(job.fraction(), 1.0);
}

// Track export.

/// Two tracks, each playing its own tone, into a mixer into an Output.
/// Returns the track groups, their track inputs, and the mixer.
fn two_tracks(rig: &mut Rig) -> ([NodeId; 2], [NodeId; 2], NodeId) {
    let other: Vec<f32> = (0..FRAMES)
        .flat_map(|i| {
            let x = (i as f32 * 0.013).sin() * 0.3;
            [x * 0.5, x]
        })
        .collect();
    write_wav(&rig.root.join("other.wav"), &other, 2, RATE).unwrap();
    let mixer = rig.project.new_node_id();
    let output = rig.project.new_node_id();
    rig.apply(Command::AddNode {
        id: mixer,
        node: Node::new(MIXER).with_config(
            noodle_core::Config::new().with(spare::MIXER_INPUTS, noodle_core::Value::Int(2)),
        ),
    });
    rig.apply(Command::AddNode {
        id: output,
        node: Node::new(OUTPUT_ID),
    });
    rig.apply(spare::wire(
        Endpoint::new(mixer, "out"),
        Endpoint::new(output, "in"),
    ));
    let (mut groups, mut inputs) = ([NodeId(0); 2], [NodeId(0); 2]);
    for (i, file) in ["tone.wav", "other.wav"].into_iter().enumerate() {
        // The group, its input, the track input, its output.
        let mut ids = Vec::new();
        let mut project = rig.project.clone();
        let (group, command) = create_track(None, Default::default(), || {
            let id = project.new_node_id();
            ids.push(id);
            id
        });
        rig.apply(command);
        rig.apply(Command::SetConfig {
            node: group,
            key: noodle_core::group::PORT_NAME.into(),
            value: Some(noodle_core::Value::Text(format!("Track {}", i + 1))),
        });
        rig.apply(spare::wire(
            Endpoint::new(group, "out"),
            Endpoint::new(mixer, format!("in{}", i + 1)),
        ));
        let clip = rig.project.new_clip_id();
        rig.apply(Command::AddClip {
            id: clip,
            clip: Clip::audio(ids[2], Tick(0), file, FRAMES as u64),
        });
        (groups[i], inputs[i]) = (group, ids[2]);
    }
    (groups, inputs, mixer)
}

impl Rig {
    /// An export of the mixdown and both tracks, as float WAV files.
    fn track_request(&self, groups: [NodeId; 2], name: &str) -> ExportRequest {
        let mut request = self.request(name, ExportFormat::WavFloat, (0, FRAMES as u64));
        request.tracks = groups
            .iter()
            .enumerate()
            .map(|(i, &group)| TrackExport {
                group,
                name: format!("Track {}", i + 1),
                path: self.root.join(format!("{name}-track{}.wav", i + 1)),
            })
            .collect();
        request
    }
}

fn read(path: &std::path::Path) -> Vec<f32> {
    read_wav(path).unwrap().samples
}

#[test]
fn with_an_empty_master_group_the_track_files_sum_to_the_mixdown() {
    let mut rig = setup("tracks-sum");
    let (groups, _, _) = two_tracks(&mut rig);
    let request = rig.track_request(groups, "sum");
    let report = export(&request, &Progress::new()).unwrap();
    assert!(report.is_complete(), "{report:?}");
    assert_eq!(report.frames, FRAMES as u64);
    assert_eq!(report.tracks.len(), 2);

    let mix = read(request.mixdown.as_ref().unwrap());
    let (a, b) = (read(&request.tracks[0].path), read(&request.tracks[1].path));
    // Matching lengths, and not silent.
    assert_eq!(a.len(), mix.len());
    assert_eq!(b.len(), mix.len());
    assert!(peak(&a) > 0.1 && peak(&b) > 0.1);
    assert_ne!(a, b);
    // The golden tolerance (tests/golden.rs).
    for i in 0..mix.len() {
        assert!(
            (a[i] + b[i] - mix[i]).abs() < 1e-4,
            "frame {i}: {} + {} vs {}",
            a[i],
            b[i],
            mix[i]
        );
    }
    // The playback renderer hears the same mix.
    let plain = rig.plain();
    assert!(plain.iter().zip(&mix).all(|(p, m)| (p - m).abs() < 1e-4));
}

#[test]
#[allow(clippy::needless_range_loop)]
fn a_track_file_is_taken_after_gain_and_mute_and_before_the_master() {
    let mut rig = setup("tracks-points");
    let (groups, inputs, mixer) = two_tracks(&mut rig);
    let plain = {
        let request = rig.track_request(groups, "before");
        export(&request, &Progress::new()).unwrap();
        (read(&request.tracks[0].path), read(&request.tracks[1].path))
    };
    // The first track is 6 dB down at its output; the second is muted.
    let out = rig.project.graph().control_port(groups[0]).unwrap();
    rig.set(out.node, &out.control_key("gain"), -6.0);
    rig.set(inputs[1], "mute", 1.0);
    // Something after the mixer: the master group's processing.
    let master = rig.add(900, "noodle.util.gain");
    let output = rig
        .project
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == OUTPUT_ID)
        .map(|(id, _)| id)
        .unwrap();
    rig.apply(Command::Disconnect {
        input: Endpoint::new(output, "in"),
    });
    rig.wire((mixer, "out"), (master, "in"));
    rig.wire((master, "out"), (output, "in"));
    rig.set(master, "gain", -12.0);

    let request = rig.track_request(groups, "after");
    export(&request, &Progress::new()).unwrap();
    let (a, b) = (read(&request.tracks[0].path), read(&request.tracks[1].path));
    let half = 10f32.powf(-6.0 / 20.0);
    for i in 0..a.len() {
        assert!((a[i] - plain.0[i] * half).abs() < 1e-4, "frame {i}");
    }
    assert!(b.iter().all(|&x| x == 0.0), "a muted track is silent");
    assert!(peak(&plain.1) > 0.1);
    // The mixdown has the master's processing on it; the files do not.
    let mix = read(request.mixdown.as_ref().unwrap());
    let master_gain = 10f32.powf(-12.0 / 20.0);
    for i in 0..mix.len() {
        assert!((mix[i] - a[i] * master_gain).abs() < 1e-4, "frame {i}");
    }
}

#[test]
fn normalising_brings_the_mixdown_to_the_target_and_the_tracks_keep_the_sum() {
    let mut rig = setup("tracks-normalise");
    let (groups, _, _) = two_tracks(&mut rig);
    // Long enough to hold a loudness block.
    let mut request = rig.track_request(groups, "norm");
    let before = export(&request, &Progress::new()).unwrap();
    let loud = before.mixdown.unwrap().lufs;
    assert!(loud.is_finite(), "{before:?}");
    assert_eq!(before.gain_db, 0.0);

    request.normalize = Some(loud - 6.0);
    let report = export(&request, &Progress::new()).unwrap();
    assert!((report.gain_db + 6.0).abs() < 1e-9, "{report:?}");
    let mix = report.mixdown.unwrap();
    assert!((mix.lufs - (loud - 6.0)).abs() < 0.01, "{report:?}");
    // The written file measures what the report says.
    let measured = noodle_io::measure_loudness(&read(request.mixdown.as_ref().unwrap()), 2, RATE);
    assert!((measured.lufs - mix.lufs).abs() < 1e-6);
    let (a, b) = (read(&request.tracks[0].path), read(&request.tracks[1].path));
    let m = read(request.mixdown.as_ref().unwrap());
    assert!((0..m.len()).all(|i| (a[i] + b[i] - m[i]).abs() < 1e-4));

    // Normalising without a mixdown file still measures one.
    request.mixdown = None;
    let report = export(&request, &Progress::new()).unwrap();
    assert!((report.mixdown.unwrap().lufs - (loud - 6.0)).abs() < 0.01);
}

#[test]
fn nothing_chosen_and_a_group_without_an_output_are_refused() {
    let mut rig = setup("tracks-refused");
    let (groups, _, _) = two_tracks(&mut rig);
    let mut request = rig.track_request(groups, "refused");
    request.mixdown = None;
    request.tracks.clear();
    assert!(matches!(
        export(&request, &Progress::new()),
        Err(ExportJobError::NothingChosen)
    ));
    // A node that isn't a group has no output to tap.
    request.tracks = vec![TrackExport {
        group: NodeId(99_999),
        name: "Nowhere".into(),
        path: rig.root.join("nowhere.wav"),
    }];
    assert!(matches!(
        export(&request, &Progress::new()),
        Err(ExportJobError::NoOutput(_))
    ));
    assert!(!rig.root.join("nowhere.wav").exists());
}

impl Rig {
    fn clip_request(&self, group: NodeId, name: &str, range: (u64, u64), tail: u64) -> ClipRequest {
        ClipRequest {
            render: RenderRequest {
                project: self.project.clone(),
                base: self.root.clone(),
                settings: SETTINGS,
                frames: FRAMES,
                extend_registry: None,
            },
            group,
            name: name.into(),
            start: range.0,
            end: range.1,
            max_tail: tail,
            path: self.root.join(format!("{name}.wav")),
            freezer: Some(self.freezer.clone()),
        }
    }
}

#[test]
fn a_rendered_clip_is_the_tracks_own_render_and_its_tail_stops_at_silence() {
    let mut rig = setup("clip-match");
    let (groups, _, _) = two_tracks(&mut rig);
    // The whole track, as track export writes it.
    let mut whole = rig.track_request(groups, "whole");
    whole.mixdown = None;
    whole.tracks.truncate(1);
    whole.start = 20_000;
    export(&whole, &Progress::new()).unwrap();
    let whole = read(&whole.tracks[0].path);

    // A range that ends before the audio does: the clip runs on to where the
    // track falls silent, well short of the ten second cap.
    let request = rig.clip_request(groups[0], "clip", (20_000, 50_000), 10 * RATE as u64);
    let report = render_clip(&request, &Progress::new()).unwrap();
    assert!(report.is_complete(), "{report:?}");
    assert!(!report.capped);
    assert!(report.tail > 0 && report.frames == 30_000 + report.tail);
    let clip = read(&request.path);
    assert_eq!(clip.len() as u64, report.frames * 2);
    assert_eq!(clip, whole[..clip.len()]);
    assert!(clip.len() < whole.len());
    // What it left off is silence.
    assert!(whole[clip.len()..].iter().all(|s| s.abs() <= CLIP_SILENCE));
    // The last frame it kept is not.
    assert!(
        clip[clip.len() - 2..]
            .iter()
            .any(|s| s.abs() > CLIP_SILENCE)
    );
}

#[test]
fn a_tail_still_sounding_at_the_cap_is_cut_with_a_fade() {
    let mut rig = setup("clip-cap");
    let (groups, _, _) = two_tracks(&mut rig);
    let request = rig.clip_request(groups[0], "capped", (20_000, 30_000), 5_000);
    let report = render_clip(&request, &Progress::new()).unwrap();
    assert!(report.capped);
    assert_eq!(report.tail, 5_000);
    let clip = read(&request.path);
    assert_eq!(clip.len(), 15_000 * 2);
    assert!(clip[clip.len() - 2..].iter().all(|s| s.abs() < 1e-3));
}

#[test]
fn a_silent_track_makes_no_clip_and_leaves_no_file() {
    let mut rig = setup("clip-silent");
    let (groups, inputs, _) = two_tracks(&mut rig);
    rig.set(inputs[1], "mute", 1.0);
    let request = rig.clip_request(groups[1], "silent", (0, 30_000), 1_000);
    let error = render_clip(&request, &Progress::new()).unwrap_err();
    assert!(matches!(error, ExportJobError::Silent(_)), "{error}");
    assert!(!request.path.exists());
    let mut partial = request.path.as_os_str().to_owned();
    partial.push(".partial");
    assert!(!std::path::Path::new(&partial).exists());
}

#[test]
fn a_true_peak_ceiling_limits_the_mixdown_after_the_loudness_gain() {
    let mut rig = setup("ceiling");
    rig.chain(&[]);
    let mut request = rig.request("ceiling", ExportFormat::WavFloat, (0, FRAMES as u64));
    request.normalize = Some(-5.0);
    let plain = export(&request, &Progress::new()).unwrap();
    let before = plain.mixdown.unwrap();
    // Loud enough that the normalised peak is over the ceiling.
    assert!(before.true_peak_db > -1.0, "{before:?}");

    request.true_peak_ceiling = Some(-1.0);
    let report = export(&request, &Progress::new()).unwrap();
    let mix = report.mixdown.unwrap();
    assert!(mix.true_peak_db <= -0.9, "{mix:?}");
    // The report is of the file as written.
    let file = noodle_io::measure_loudness(&read(request.mixdown.as_ref().unwrap()), 2, RATE);
    assert!((file.true_peak_db - mix.true_peak_db).abs() < 1e-6);
    assert!((file.lufs - mix.lufs).abs() < 1e-6);
    // The limiter takes loudness with the peaks, and the gain makes up some
    // of it: the file lands near the target, never above it, and the gain is
    // more than the plain pass's.
    assert!(mix.lufs <= -5.0 + 0.1 && mix.lufs > -9.0, "{mix:?}");
    assert!(report.gain_db > plain.gain_db, "{report:?} {plain:?}");

    // With no loudness target the ceiling still holds the peaks.
    request.normalize = None;
    request.true_peak_ceiling = Some(-20.0);
    let report = export(&request, &Progress::new()).unwrap();
    assert!(report.mixdown.unwrap().true_peak_db <= -19.9);
    assert_eq!(report.frames, FRAMES as u64);
}

#[test]
fn track_files_are_not_limited_so_they_still_sum_to_the_mixdown() {
    let mut rig = setup("ceiling-tracks");
    let (groups, _, _) = two_tracks(&mut rig);
    let mut request = rig.track_request(groups, "ceil");
    request.true_peak_ceiling = Some(-30.0);
    export(&request, &Progress::new()).unwrap();
    let (a, b) = (read(&request.tracks[0].path), read(&request.tracks[1].path));
    let limited = read(request.mixdown.as_ref().unwrap());
    // The mixdown is held far under the tracks.
    assert!(peak(&limited) < 0.05, "{}", peak(&limited));
    assert!(peak(&a).max(peak(&b)) > 0.05);
}

#[test]
fn the_mixdown_carries_its_isrc_and_a_cue_sheet_is_written_beside_it() {
    let mut rig = setup("isrc");
    rig.chain(&[]);
    let mut request = rig.request("album", ExportFormat::Wav16, (0, FRAMES as u64));
    request.metadata = noodle_io::ExportMetadata {
        title: "Night Drive".into(),
        artist: "The Noodles".into(),
        isrc: "USRC17607839".into(),
    };
    request.cue = true;
    export(&request, &Progress::new()).unwrap();
    let wav = request.mixdown.clone().unwrap();
    let bytes = std::fs::read(&wav).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("USRC17607839"));
    assert_eq!(decode_file(&wav).unwrap().channels, 2);
    let cue = std::fs::read_to_string(wav.with_extension("cue")).unwrap();
    assert!(cue.contains("FILE \"album.wav\" WAVE"), "{cue}");
    assert!(cue.contains("ISRC USRC17607839") && cue.contains("TITLE \"Night Drive\""));

    // Without the box, no sheet.
    request.cue = false;
    std::fs::remove_file(wav.with_extension("cue")).unwrap();
    export(&request, &Progress::new()).unwrap();
    assert!(!wav.with_extension("cue").exists());
}

#[test]
fn a_silent_tail_is_cut_to_the_latest_sounding_file_for_all_of_them() {
    let mut rig = setup("silent-tail");
    let (groups, _, _) = two_tracks(&mut rig);
    // The audio ends at FRAMES; end the range early so the rest is tail.
    let cap = 10 * RATE as u64;
    let mut request = rig.track_request(groups, "tail");
    request.end = 30_000;
    request.silent_tail = Some(cap);
    let report = export(&request, &Progress::new()).unwrap();
    assert!(report.is_complete(), "{report:?}");
    let files = [
        read(request.mixdown.as_ref().unwrap()),
        read(&request.tracks[0].path),
        read(&request.tracks[1].path),
    ];
    // Equal lengths, longer than the body, short of the cap.
    let frames = files[0].len() / 2;
    assert!(files.iter().all(|f| f.len() / 2 == frames));
    assert_eq!(report.frames, frames as u64);
    assert!(frames > 30_000 && (frames as u64) < 30_000 + cap);
    // The last frame sounds in at least one file, and the audio stops there.
    assert!(
        files
            .iter()
            .any(|f| f[f.len() - 2..].iter().any(|s| s.abs() > CLIP_SILENCE))
    );
    // The tail matches an export that runs to the same length untrimmed.
    let mut fixed = rig.track_request(groups, "fixed");
    fixed.end = frames as u64;
    export(&fixed, &Progress::new()).unwrap();
    assert_eq!(read(&fixed.tracks[0].path), files[1]);
}

#[test]
fn a_silent_tail_still_sounding_at_the_cap_fades_out() {
    let mut rig = setup("silent-tail-cap");
    let (groups, _, _) = two_tracks(&mut rig);
    let mut request = rig.track_request(groups, "capped");
    request.end = 20_000;
    request.silent_tail = Some(5_000);
    export(&request, &Progress::new()).unwrap();
    for path in [&request.tracks[0].path, &request.tracks[1].path] {
        let file = read(path);
        assert_eq!(file.len(), 25_000 * 2);
        assert!(file[file.len() - 2..].iter().all(|s| s.abs() < 1e-3));
    }
}
