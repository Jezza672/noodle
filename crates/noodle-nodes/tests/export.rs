//! Export, end to end: the file holds what playback plays, offline nodes
//! included, over the range asked for, in the format asked for.

use std::path::PathBuf;

use noodle_core::group::create_track;
use noodle_core::spare::{self, MIXER};
use noodle_core::{Clip, Command, Connection, Endpoint, History, Node, NodeId, Project, Tick};
use noodle_engine::{OUTPUT_ID, Progress, Registry, Settings};
use noodle_io::{CacheStore, ExportFormat, decode_file, read_wav, write_wav};
use noodle_nodes::{
    ExportJobError, ExportRequest, Freezer, RenderRequest, TRACK_INPUT_ID, TrackExport, export,
    render_project_with_clips, spawn_export,
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
            freezer: Some(self.freezer.clone()),
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
