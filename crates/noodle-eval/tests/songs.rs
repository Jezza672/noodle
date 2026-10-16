//! The generated songs: their shape, their files, and that a native render of
//! one at another tempo is a fair answer key for the harness.
//!
//! Bounds are loose and catch broken wiring, not how good a warp sounds: no
//! metric gates CI until it has been checked against listeners (ADR 0003).

use noodle_core::Project;
use noodle_eval::harness::{Harness, Method, View};
use noodle_eval::song::{Range, Song, all};
use noodle_eval::tempo::{TempoCase, steady};
use noodle_eval::{Clip, TimeMap, evaluate};
use std::path::{Path, PathBuf};

const RATE: u32 = 48_000;

fn songs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("songs")
}

/// Warping reads a clip from disk through a streaming thread, and a starved
/// thread shows up as underruns. So the test that warps runs alone, taking this
/// lock to write; every test that only renders takes it to read, and they
/// run together.
static LOAD: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn shared() -> std::sync::RwLockReadGuard<'static, ()> {
    LOAD.read().unwrap_or_else(|e| e.into_inner())
}

/// Runs `check` on every song, one after another, with nothing else rendering.
fn each_song_alone(check: impl Fn(&Song)) {
    let _alone = LOAD.write().unwrap_or_else(|e| e.into_inner());
    for song in all() {
        check(&song);
    }
}

/// Runs `check` on every song at once; the renders are slow enough to want
/// the cores.
fn each_song(check: impl Fn(&Song) + Sync) {
    let _load = shared();
    let songs = all();
    std::thread::scope(|scope| {
        let handles: Vec<_> = songs
            .iter()
            .map(|song| {
                let check = &check;
                scope.spawn(move || check(song))
            })
            .collect();
        for (handle, song) in handles.into_iter().zip(&songs) {
            if let Err(e) = handle.join() {
                panic!("{}: {}", song.name, panic_text(&e));
            }
        }
    });
}

fn panic_text(e: &Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "panicked".into())
}

#[test]
fn every_song_has_the_five_sections_a_sensible_tempo_and_a_gate_sized_excerpt() {
    let songs = all();
    assert_eq!(songs.len(), 5);
    let mut names = std::collections::BTreeSet::new();
    for song in &songs {
        assert!(names.insert(song.name), "{} twice", song.name);
        assert!(
            (60.0..=90.0).contains(&song.seconds()),
            "{}: {:.1} s",
            song.name,
            song.seconds()
        );
        // Inside the tempo detector's range, and not the app's default.
        assert!((80.0..=160.0).contains(&song.bpm) && song.bpm != 120.0);
        let sections: Vec<&str> = song.sections.iter().map(|s| s.name).collect();
        assert_eq!(
            sections,
            ["intro", "verse", "chorus", "breakdown", "drop"],
            "{}",
            song.name
        );
        let excerpt = song.excerpt_range();
        let seconds = excerpt.beats() * 60.0 / song.bpm;
        assert!(
            (15.0..=20.0).contains(&seconds) && excerpt.end <= song.beats(),
            "{}: excerpt {seconds:.1} s",
            song.name
        );
        assert!(song.parts.len() <= 12, "{}", song.name);
        for part in &song.parts {
            assert!(
                !song.notes(part).is_empty(),
                "{}: {} never plays",
                song.name,
                part.name
            );
        }
        // The excerpt is busy: most parts play in it.
        let busy = song
            .parts
            .iter()
            .filter(|p| {
                song.notes(p)
                    .iter()
                    .any(|n| n.beat >= excerpt.start && n.beat < excerpt.end)
            })
            .count();
        // The drums are one part, the rack.
        assert!(busy >= 4, "{}: only {busy} parts in the excerpt", song.name);
    }
}

#[test]
fn the_committed_projects_are_the_songs_and_they_play() {
    let update = std::env::var_os("UPDATE_SONGS").is_some();
    if update {
        noodle_eval::song::samples::write_all().unwrap();
    }
    for song in all() {
        let path = songs_dir().join(format!("{}.ron", song.name));
        let text = song.source_project().to_ron();
        if update {
            std::fs::write(&path, &text).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == text,
            "{} is out of date: UPDATE_SONGS=1 cargo test -p noodle-eval --test songs",
            path.display()
        );
        // And the file loads as a project, with a track per part and a clip
        // on each that plays.
        let project = Project::from_ron(&committed).unwrap();
        assert_eq!(project.clips().count(), song.parts.len(), "{}", song.name);
    }
}

#[test]
fn a_song_renders_the_same_every_time_and_stays_under_full_scale() {
    each_song(|song| {
        let a = song.render(&steady(song.bpm), song.whole(), RATE).unwrap();
        let expected = song.seconds() * f64::from(RATE) * 2.0;
        assert!((a.len() as f64 - expected).abs() <= 2.0, "{}", song.name);
        assert!(
            a.iter().all(|x| x.is_finite() && x.abs() < 1.0),
            "{}",
            song.name
        );
        // Not near silent anywhere: every second has something in it.
        for (i, second) in a.chunks(2 * RATE as usize).enumerate() {
            if second.len() < RATE as usize {
                continue;
            }
            assert!(
                second.iter().any(|x| x.abs() > 0.02),
                "{}: second {i} is silent",
                song.name
            );
        }
        let range = song.excerpt_range();
        let b = song.render(&steady(song.bpm), range, RATE).unwrap();
        assert_eq!(
            b,
            song.render(&steady(song.bpm), range, RATE).unwrap(),
            "{}",
            song.name
        );
        // A range is the same sound as that stretch of a longer render, once
        // what began before it has died away: from its second bar on. (Parts
        // with noise are left out: a noise node runs on through the song, so
        // its sequence at a given beat depends on where the render began.)
        let steady_parts = |p: &noodle_eval::song::Part| {
            matches!(p.instrument, noodle_eval::song::kit::Instrument::Tone(_))
        };
        let lead_in = Range {
            start: range.start - 8.0,
            ..range
        };
        let long = song
            .render_of(&steady(song.bpm), lead_in, RATE, steady_parts)
            .unwrap();
        let short = song
            .render_of(&steady(song.bpm), range, RATE, steady_parts)
            .unwrap();
        let frame = |beats: f64| (beats * 60.0 / song.bpm * f64::from(RATE)).round() as usize;
        let (skip, lead) = (frame(4.0), frame(8.0));
        // Oscillators run free from the start of a render, so the waveforms
        // differ in phase: compare the loudness, a beat at a time, instead.
        let envelope = |x: &[f32]| -> Vec<f64> {
            x.chunks(2 * frame(1.0))
                .map(|w| (w.iter().map(|v| f64::from(v * v)).sum::<f64>() / w.len() as f64).sqrt())
                .collect()
        };
        let (a, b) = (
            envelope(&long[2 * (lead + skip)..]),
            envelope(&short[2 * skip..]),
        );
        // Not the last, partial beat, whose few samples are not a loudness.
        let n = a.len().min(b.len()) - 1;
        let peak = a.iter().cloned().fold(0.0, f64::max);
        let worst = (0..n).map(|i| (a[i] - b[i]).abs()).fold(0.0, f64::max);
        assert!(
            worst < 0.1 * peak,
            "{}: excerpt loudness differs by {worst} of {peak}",
            song.name
        );
    });
}

#[test]
fn the_native_render_puts_the_notes_where_the_tempo_map_says() {
    let _load = shared();
    // The answer key must line up with the source through the time map, or the
    // scores of every warp are off. Kicks and hats are the timing the metric
    // reads, and they sit on the song's grid.
    let song = all().into_iter().find(|s| s.name == "neon-drive").unwrap();
    let range = song.excerpt_range();
    let h = Harness::for_song(&song, range, RATE).unwrap();
    for case in [
        TempoCase::Ratio(1.5),
        TempoCase::Ratio(0.75),
        TempoCase::Ramp,
    ] {
        let target = h.target(case);
        let source = song.render(&steady(song.bpm), range, RATE).unwrap();
        let native = song.render(&target, range, RATE).unwrap();
        let report = evaluate(
            Clip::new(&source, 2, RATE),
            Clip::new(&native, 2, RATE),
            &h.source_map(case, &target),
        );
        let t = report.timing;
        assert!(
            t.expected > 80 && t.matched * 10 >= t.expected * 9,
            "{}: {t:?}",
            case.name()
        );
        assert!(t.median_abs_offset_ms < 6.0, "{}: {t:?}", case.name());
    }
    // A wrong map is caught, though less sharply than on a probe: a song has
    // eight onsets a second, so a late onset often finds another to pair with.
    let target = h.target(TempoCase::Ratio(1.5));
    let source = song.render(&steady(song.bpm), range, RATE).unwrap();
    let native = song.render(&target, range, RATE).unwrap();
    let wrong = evaluate(
        Clip::new(&source, 2, RATE),
        Clip::new(&native, 2, RATE),
        &TimeMap::Speed(1.2),
    );
    assert!(
        wrong.timing.median_abs_offset_ms > 10.0,
        "{:?}",
        wrong.timing
    );
}

#[test]
fn every_excerpt_goes_through_the_harness() {
    each_song_alone(|song| {
        let range = song.excerpt_range();
        let h = Harness::for_song(song, range, RATE).unwrap();
        let rows = h
            .run_song(
                song.name,
                song,
                range,
                TempoCase::Ratio(0.75),
                &[Method::Grain],
            )
            .unwrap();
        assert_eq!(rows.len(), View::ALL.len(), "{}", song.name);
        for row in &rows {
            assert_eq!(row.probe, song.name);
            assert!(
                row.metrics
                    .iter()
                    .all(|(n, v)| v.is_finite() || n.contains("pitch")),
                "{} {}: {:?}",
                song.name,
                row.key(),
                row.metrics
                    .iter()
                    .filter(|(_, v)| !v.is_finite())
                    .collect::<Vec<_>>()
            );
        }
        // Grain keeps the timing: the round trip lands on the source.
        let trip = rows.iter().find(|r| r.view == View::RoundTrip).unwrap();
        assert!(
            trip.metric("median_abs_offset_ms").unwrap() < 40.0,
            "{}: {:?}",
            song.name,
            trip.metrics
        );
    });
}

/// Tempo detection on full mixes (#141): the kick and bass line carry the beat
/// under hats, plucks and arpeggios that bury it in the full-band onsets. Both
/// the excerpt and the whole song, since the app analyses up to two minutes.
#[test]
fn the_tempo_of_a_full_mix_is_found_from_its_bass() {
    let _shared = shared();
    for song in all() {
        for (what, range) in [("excerpt", song.excerpt_range()), ("whole", song.whole())] {
            let audio = song.render(&steady(song.bpm), range, RATE).unwrap();
            let analysis = noodle_eval::harness::analyse(&audio, RATE);
            let label = format!("{} {what}", song.name);
            let tempo = analysis
                .tempo
                .unwrap_or_else(|| panic!("{label}: no tempo"));
            assert!(
                (tempo - song.bpm).abs() / song.bpm < 0.002,
                "{label}: {tempo}"
            );
            // The renders start on a bar, so the beat falls on the start. A
            // bass line that is on the off-beat as often as on it (funk) can
            // put it half a beat off: the onsets themselves favour the
            // off-beat there (more of them, and louder), so no rule over
            // onsets can tell, and no more than half a beat is asked.
            // Glass-ambient's beat is its bells, which come a little late
            // on purpose and fade in over a pad, so its onsets sit further off.
            let limit = if song.name == "glass-ambient" {
                0.1
            } else {
                0.05
            };
            let phase = analysis.grid.unwrap().phase;
            let off = phase.abs().min((phase.abs() - 0.5).abs());
            assert!(off < limit, "{label}: phase {phase}");
            if song.name != "funk-engine" {
                assert!(phase.abs() < limit, "{label}: phase {phase}");
            }
        }
    }
}

#[test]
fn stems_are_scored_one_part_at_a_time() {
    let _alone = LOAD.write().unwrap_or_else(|e| e.into_inner());
    let song = all().into_iter().find(|s| s.name == "neon-drive").unwrap();
    let range = song.excerpt_range();
    let h = Harness::for_song(&song, range, RATE).unwrap();
    let rows = h
        .run_stems(
            "neon-drive",
            &song,
            range,
            TempoCase::Ratio(0.75),
            &[Method::Grain],
        )
        .unwrap();
    // Parts that play in the excerpt, each once per view, named for the part.
    let mut names: Vec<&str> = rows.iter().map(|r| r.probe.as_str()).collect();
    names.dedup();
    assert!(names.len() >= 3, "{names:?}");
    for name in &names {
        let part = name.strip_prefix("neon-drive/").unwrap();
        assert!(song.parts.iter().any(|p| p.name == part), "{name}");
        assert_eq!(
            rows.iter().filter(|r| r.probe == *name).count(),
            View::ALL.len(),
            "{name}"
        );
    }
    assert_eq!(rows.len(), names.len() * View::ALL.len());
}

#[test]
fn the_native_render_is_the_source_retimed() {
    let _load = shared();
    // At 0.75 times the tempo the same notes take a third longer; at 1.5 times
    // they take two thirds as long. The excerpt is not silent either way.
    let song = all().into_iter().find(|s| s.name == "funk-engine").unwrap();
    let range = song.excerpt_range();
    let h = Harness::for_song(&song, range, RATE).unwrap();
    for ratio in [0.75, 1.5] {
        let (source, native) = h
            .song_renders(&song, range, TempoCase::Ratio(ratio))
            .unwrap();
        let expected = source.len() as f64 / ratio;
        assert!(
            (native.len() as f64 - expected).abs() <= 4.0,
            "x{ratio}: {} frames, wanted {expected}",
            native.len()
        );
        for audio in [&source, &native] {
            assert!(audio.iter().any(|x| x.abs() > 0.2), "x{ratio}: silent");
        }
    }
}

#[test]
fn a_range_is_the_notes_inside_it_moved_to_the_start() {
    let song = all().into_iter().next().unwrap();
    let range = Range {
        start: 16.0,
        end: 32.0,
    };
    let project = song.project(&steady(song.bpm), range);
    for clip in project.clips().map(|(_, clip)| clip) {
        let midi = clip.as_midi().unwrap();
        assert_eq!(midi.length, noodle_core::Tick::from_quarters(16.0));
        assert!(midi.notes.iter().all(|n| n.start < midi.length));
    }
}

#[test]
fn every_instrument_is_a_group_of_primitive_nodes_with_its_controls_exposed() {
    use noodle_core::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT, TRACK_INPUT};
    // No node of a song does a synth's job on its own: these are all the types
    // that appear, and all of them are building blocks.
    const PRIMITIVES: [&str; 26] = [
        "noodle.osc.sine",
        "noodle.osc.saw",
        "noodle.osc.square",
        "noodle.osc.triangle",
        "noodle.noise.white",
        "noodle.filter.svf",
        "noodle.filter.ladder",
        "noodle.mod.adsr",
        "noodle.util.vca",
        "noodle.util.math",
        "noodle.util.mix",
        "noodle.util.pan",
        "noodle.util.gain",
        "noodle.event.mono",
        "noodle.event.note_split",
        "noodle.instrument.sampler",
        "noodle.effect.dynamics",
        "noodle.effect.eq",
        "noodle.effect.reverb",
        "noodle.poly.voices",
        "noodle.poly.voice_mix",
        "noodle.io.output",
        "noodle.group.input",
        "noodle.group.output",
        GROUP,
        TRACK_INPUT,
    ];
    for song in all() {
        let project = song.source_project();
        let graph = project.graph();
        for (id, node) in graph.nodes() {
            assert!(
                PRIMITIVES.contains(&node.type_id.as_str()),
                "{}: node {id:?} is a {}",
                song.name,
                node.type_id
            );
        }
        // Each part's synth is a group inside its track, and a tonal one
        // exposes its pitch, gate and the controls a player reaches for.
        for part in &song.parts {
            let track = graph
                .nodes()
                .find(|(_, n)| {
                    n.type_id == GROUP
                        && n.parent.is_none()
                        && n.config.get("name") == Some(&noodle_core::Value::Text(part.name.into()))
                })
                .unwrap_or_else(|| panic!("{}: no track {}", song.name, part.name))
                .0;
            let synth = graph
                .children(Some(track))
                .find(|(_, n)| n.type_id == GROUP)
                .unwrap_or_else(|| panic!("{}: {} has no instrument", song.name, part.name))
                .0;
            let ports = graph.group_ports(synth);
            let rack = matches!(part.instrument, noodle_eval::song::kit::Instrument::Rack(_));
            for name in if rack {
                vec!["notes"]
            } else {
                vec!["pitch", "gate", "velocity"]
            } {
                assert!(
                    ports.input(name).is_some(),
                    "{} {}: {name}",
                    song.name,
                    part.name
                );
            }
            if let noodle_eval::song::kit::Instrument::Rack(rack) = &part.instrument {
                // Every slot's level, tune and pan are controls of the rack.
                for (i, slot) in rack.slots.iter().enumerate() {
                    let label = format!(
                        "{} {}",
                        noodle_eval::song::kit::drum_label(slot.drum),
                        i + 1
                    );
                    for control in ["level", "tune", "pan"] {
                        assert!(
                            ports.input(&format!("{label} {control}")).is_some(),
                            "{} {}: {label} {control}",
                            song.name,
                            part.name
                        );
                    }
                }
            }
            assert!(ports.output("out").is_some());
            if matches!(part.instrument, noodle_eval::song::kit::Instrument::Tone(_)) {
                for name in [
                    "cutoff",
                    "resonance",
                    "amount",
                    "detune",
                    "attack",
                    "release",
                ] {
                    assert!(
                        ports.input(name).is_some(),
                        "{} {}: {name}",
                        song.name,
                        part.name
                    );
                }
            }
            // And it holds more than one node: it is a patch.
            assert!(
                graph
                    .children(Some(synth))
                    .filter(|(_, n)| !matches!(n.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT))
                    .count()
                    >= 5
            );
        }
    }
}

#[test]
fn every_part_makes_sound_on_its_own() {
    // A broken wire inside an instrument would leave its track silent under
    // the rest of the mix, where the other tests would not notice.
    each_song(|song| {
        for part in &song.parts {
            let first = song
                .notes(part)
                .iter()
                .map(|n| n.beat)
                .fold(f64::INFINITY, f64::min);
            let start = (first / 4.0).floor() * 4.0;
            let range = Range {
                start,
                end: start + 8.0,
            };
            let x = song
                .render_of(&steady(song.bpm), range, RATE, |p| p.name == part.name)
                .unwrap();
            let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(
                x.iter().all(|v| v.is_finite()) && peak > 0.02,
                "{}: {} has peak {peak}",
                song.name,
                part.name
            );
            // Stereo: the right side sounds too (no part is panned hard left).
            assert!(
                x.chunks(2).any(|f| f[1].abs() > 0.005),
                "{}: {}",
                song.name,
                part.name
            );
        }
    });
}

#[test]
fn the_committed_samples_are_what_the_synthesis_makes() {
    use noodle_eval::song::samples;
    // Rendering twice can differ in the last bit between machines, so the
    // file is compared by ear-level tolerance, not by hash.
    let store = samples::store();
    for (name, drum) in samples::needed() {
        let made = samples::synthesize(drum);
        let hash = samples::hash(&name);
        let committed = store.get(&hash).unwrap_or_else(|| {
            panic!("{name}: missing: UPDATE_SONGS=1 cargo test -p noodle-eval --test songs")
        });
        assert_eq!(committed.left.len(), made.samples.len(), "{name}");
        let worst = committed
            .left
            .iter()
            .zip(&made.samples)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-3, "{name}: differs by {worst}");
    }
}

#[test]
fn compressor_and_limiter_are_dynamics_nodes_not_groups() {
    use noodle_core::Value;
    let mut modes = std::collections::BTreeSet::new();
    for song in all() {
        let project = song.source_project();
        for (_, node) in project.graph().nodes() {
            if node.type_id == "noodle.effect.dynamics" {
                modes.insert(node.params.get("mode").copied().unwrap_or(0.0) as i32);
            }
            if let Some(Value::Text(name)) = node.config.get("name") {
                assert!(
                    name != "Compressor" && name != "Limiter",
                    "{}: {name} is a Dynamics preset, not a group",
                    song.name
                );
            }
        }
    }
    // Both a compressor and a limiter are used, so both are covered.
    assert_eq!(modes.into_iter().collect::<Vec<_>>(), vec![0, 1]);
}
