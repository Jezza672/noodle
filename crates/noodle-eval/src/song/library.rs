//! The five songs.
//!
//! Each has an intro, a verse, a chorus, a breakdown and a drop, at 60 to 90
//! seconds, and between them they cover the material a warp finds hard:
//! four-on-the-floor drums with a steady pulse, sparse and swung drums, dense
//! breakbeats, long pads with no transient to hold on to, and syncopated
//! plucked bass.
//!
//! The tempos are not 120 (the app's default, so a detector that gave up
//! could pass for right) and sit inside the detector's range of 80 to 160.

use super::kit::{Drum, Filter, Fx, Instrument, Osc, Rack, Slot, Tone, Wave, env};
use super::music::{
    ADD9, DOM7, MAJ, MAJ7, MAJOR_PENTATONIC, MIN7, MINOR_PENTATONIC, Note, Pattern, SUS, Scale,
    chord, drum, melody, pad, stabs, tones,
};
use super::{Part, Section, Song};

/// Every song, in the order they are listed in the docs.
pub fn all() -> Vec<Song> {
    vec![
        neon_drive(),
        rainy_lofi(),
        breakbeat_run(),
        glass_ambient(),
        funk_engine(),
    ]
    .into_iter()
    .map(|song| mixed(racked(song)))
    .collect()
}

/// The song's drum parts as one drum rack: a Sampler per drum behind a Note
/// Split, played by one MIDI track on General MIDI keys, the way a drum
/// machine in a DAW is set up. The patterns are the same; only the key and
/// the instrument they play change.
fn racked(mut song: Song) -> Song {
    let first = song
        .parts
        .iter()
        .position(|p| matches!(p.instrument, Instrument::Drum(_)));
    let Some(first) = first else {
        return song;
    };
    let (drums, mut rest): (Vec<Part>, Vec<Part>) = std::mem::take(&mut song.parts)
        .into_iter()
        .partition(|p| matches!(p.instrument, Instrument::Drum(_)));

    let mut slots = Vec::new();
    let mut lines: Vec<(u8, Vec<(&'static str, Pattern)>)> = Vec::new();
    let mut taken: Vec<u8> = Vec::new();
    for part in drums {
        let Instrument::Drum(drum) = part.instrument else {
            unreachable!("partitioned by drums")
        };
        // General MIDI: kick 36, snare 38, clap 39, closed hat 42, open 46.
        let mut key = match drum {
            Drum::Kick { .. } => 36,
            Drum::Snare { .. } => 38,
            Drum::Clap { .. } => 39,
            Drum::Hat { decay, .. } if decay > 0.1 => 46,
            Drum::Hat { .. } => 42,
        };
        while taken.contains(&key) {
            key += 1;
        }
        taken.push(key);
        slots.push(Slot {
            sample: format!(
                "{}-{}",
                song.name,
                part.name.to_lowercase().replace(' ', "-")
            ),
            drum,
            key,
            db: part.db,
            pan: part.pan,
        });
        lines.push((key, part.plays));
    }

    // One pattern per section: every drum's pattern for it, on its key.
    let mut plays: Vec<(&'static str, Pattern)> = Vec::new();
    for section in &song.sections {
        let mut here: Vec<(u8, Pattern)> = Vec::new();
        for (key, line) in &mut lines {
            if let Some(i) = line.iter().position(|(name, _)| *name == section.name) {
                here.push((*key, line.swap_remove(i).1));
            }
        }
        if here.is_empty() {
            continue;
        }
        let pattern: Pattern = Box::new(move |bar| {
            here.iter()
                .flat_map(|(key, pattern)| {
                    pattern(bar)
                        .into_iter()
                        .map(move |n| Note { key: *key, ..n })
                })
                .collect()
        });
        plays.push((section.name, pattern));
    }
    rest.insert(
        first.min(rest.len()),
        Part {
            name: "Drums",
            instrument: Instrument::Rack(Rack { slots }),
            db: 0.0,
            pan: 0.0,
            plays,
            fx: Vec::new(),
        },
    );
    song.parts = rest;
    song
}

/// Gives each song its mix: a compressor on the drum bus, an EQ and a reverb
/// on the master, chosen to suit it. A compressor over the whole mix would
/// pump the bass the tempo detector listens to, so it sits on the drums.
fn mixed(mut song: Song) -> Song {
    // (type, frequency, gain, Q): 3 is a low cut, 4 a high cut, 2 a high shelf.
    let tidy = Fx::Eq(vec![(3, 30.0, 0.0, 0.7), (2, 9000.0, 1.0, 0.7)]);
    let drums = |song: &mut Song, fx: Fx| {
        for part in &mut song.parts {
            if part.name == "Drums" {
                part.fx = vec![fx.clone()];
            }
        }
    };
    match song.name {
        "neon-drive" => {
            song.master_fx = vec![tidy, Fx::Dynamics("Limiter")];
            drums(&mut song, Fx::Dynamics("Compressor"));
        }
        "rainy-lofi" => {
            song.master_fx = vec![
                Fx::Eq(vec![(3, 40.0, 0.0, 0.7), (4, 9000.0, 0.0, 0.7)]),
                Fx::Reverb {
                    decay: 0.55,
                    damping: 0.6,
                    mix: 0.18,
                },
            ];
        }
        "breakbeat-run" => {
            // Its sub bass is what the tempo detector hears, so no low cut.
            // Bus Glue adds make-up gain, so the master comes down to keep the
            // loudest moment under full scale.
            song.master_db -= 2.0;
            song.master_fx = vec![Fx::Eq(vec![(2, 9000.0, 1.0, 0.7)])];
            drums(&mut song, Fx::Dynamics("Bus Glue"));
        }
        "glass-ambient" => {
            song.master_fx = vec![
                Fx::Reverb {
                    decay: 0.75,
                    damping: 0.35,
                    mix: 0.3,
                },
                Fx::Eq(vec![(3, 50.0, 0.0, 0.7)]),
            ];
        }
        "funk-engine" => {
            // Like breakbeat-run, its mix is tempo-sensitive: no low cut.
            song.master_fx = vec![Fx::Eq(vec![(2, 9000.0, 1.0, 0.7)])];
            drums(&mut song, Fx::Dynamics("Compressor"));
        }
        other => panic!("no mix chosen for {other}"),
    }
    song
}

fn sections(list: &[(&'static str, u32)]) -> Vec<Section> {
    list.iter()
        .map(|&(name, bars)| Section { name, bars })
        .collect()
}

fn part(
    name: &'static str,
    instrument: Instrument,
    db: f32,
    pan: f32,
    plays: Vec<(&'static str, Pattern)>,
) -> Part {
    Part {
        name,
        instrument,
        db,
        pan,
        plays,
        fx: Vec::new(),
    }
}

/// The same pattern in several sections.
fn in_sections(names: &[&'static str], make: impl Fn() -> Pattern) -> Vec<(&'static str, Pattern)> {
    names.iter().map(|&name| (name, make())).collect()
}

fn tone(t: Tone) -> Instrument {
    Instrument::Tone(t)
}

fn kick(base: f32, sweep: f32, decay: f32) -> Instrument {
    Instrument::Drum(Drum::Kick { base, sweep, decay })
}

fn snare(tone: f32, noise: f32, decay: f32) -> Instrument {
    Instrument::Drum(Drum::Snare { tone, noise, decay })
}

fn hat(cutoff: f32, decay: f32) -> Instrument {
    Instrument::Drum(Drum::Hat { cutoff, decay })
}

fn clap(cutoff: f32, decay: f32) -> Instrument {
    Instrument::Drum(Drum::Clap { cutoff, decay })
}

/// A saw with a sine an octave below: a bass with weight and edge.
fn saw_bass(cutoff: f32, amount: f32) -> Tone {
    Tone::new(vec![
        Osc::new(Wave::Saw).level(-3.0),
        Osc::new(Wave::Sine).octave(0.5).level(-1.0),
    ])
    .filter(Filter::Ladder, cutoff, 0.25, amount)
    .filter_env(env(0.002, 0.12, 0.2, 0.1))
    .amp_env(env(0.003, 0.12, 0.8, 0.08))
}

/// Two detuned oscillators through a quickly closing filter: a pluck.
fn pluck(wave: Wave, cutoff: f32, amount: f32, decay: f32) -> Tone {
    Tone::new(vec![
        Osc::new(wave).detuned(-1.0).panned(-0.3).level(-3.0),
        Osc::new(wave).detuned(1.0).panned(0.3).level(-3.0),
    ])
    .filter(Filter::Svf, cutoff, 0.2, amount)
    .filter_env(env(0.002, 0.15, 0.0, 0.1))
    .amp_env(env(0.002, decay, 0.0, 0.15))
    .detune(7.0)
    .voices(4)
}

/// Three detuned saws, spread across the stereo field, with a slow envelope.
fn saw_pad(cutoff: f32, attack: f32, release: f32) -> Tone {
    Tone::new(vec![
        Osc::new(Wave::Saw).detuned(-1.0).panned(-0.6).level(-5.0),
        Osc::new(Wave::Saw).panned(0.0).level(-5.0),
        Osc::new(Wave::Saw).detuned(1.0).panned(0.6).level(-5.0),
    ])
    .filter(Filter::Ladder, cutoff, 0.15, 0.1)
    .filter_env(env(attack, 0.5, 0.7, release))
    .amp_env(env(attack, 0.3, 0.85, release))
    .detune(11.0)
    .voices(6)
}

fn arp_voice(wave: Wave, cutoff: f32) -> Tone {
    Tone::new(vec![Osc::new(wave)])
        .filter(Filter::Svf, cutoff, 0.25, 0.2)
        .filter_env(env(0.002, 0.08, 0.1, 0.05))
        .amp_env(env(0.002, 0.08, 0.35, 0.05))
}

fn lead_voice(cutoff: f32) -> Tone {
    Tone::new(vec![
        Osc::new(Wave::Saw).detuned(-1.0).panned(-0.2).level(-4.0),
        Osc::new(Wave::Square).detuned(1.0).panned(0.2).level(-6.0),
    ])
    .filter(Filter::Ladder, cutoff, 0.2, 0.15)
    .filter_env(env(0.01, 0.3, 0.6, 0.2))
    .amp_env(env(0.01, 0.2, 0.8, 0.15))
    .detune(6.0)
}

const A_MINOR: Scale = Scale {
    root: 69,
    steps: MINOR_PENTATONIC,
};

/// Four on the floor at 126 bpm: a pulsing kick, offbeat hats, a rolling
/// bass, plucks and an arpeggio, with a long pad and a saw lead. A minor.
fn neon_drive() -> Song {
    let am7 = chord(57, MIN7);
    let fmaj7 = chord(53, MAJ7);
    let c = chord(60, MAJ);
    let g = chord(55, MAJ);
    let progression = vec![am7, fmaj7, c, g];
    let all_sections = ["intro", "verse", "chorus", "breakdown", "drop"];
    Song {
        name: "neon-drive",
        master_db: -2.0,
        master_fx: Vec::new(),
        bpm: 126.0,
        sections: sections(&[
            ("intro", 4),
            ("verse", 8),
            ("chorus", 8),
            ("breakdown", 8),
            ("drop", 8),
        ]),
        chords: all_sections
            .iter()
            .map(|&s| (s, progression.clone()))
            .collect(),
        excerpt: (24, 8),
        parts: vec![
            part(
                "Kick",
                kick(48.0, 120.0, 0.32),
                -3.0,
                0.0,
                in_sections(&["verse", "chorus", "drop"], || {
                    drum(&["X...X...X...X..."], Some("X...X...X..xX.xX"), 0.3)
                }),
            ),
            part(
                "Clap",
                clap(1500.0, 0.16),
                -9.0,
                0.1,
                in_sections(&["verse", "chorus", "drop"], || {
                    drum(&["....X.......X..."], None, 0.15)
                }),
            ),
            part(
                "Hat",
                hat(7500.0, 0.045),
                -16.0,
                -0.15,
                in_sections(&["intro", "verse", "chorus", "drop"], || {
                    drum(&["..x...x...x...x."], None, 0.1)
                }),
            ),
            part(
                "Bass",
                tone(saw_bass(180.0, 0.28)),
                -8.0,
                0.0,
                in_sections(&["verse", "chorus", "drop"], || {
                    tones(
                        &["1..1..1.1..1..1.", "1..1..1.1..1.51."],
                        None,
                        0.25,
                        0.2,
                        -1,
                        0.9,
                    )
                }),
            ),
            part(
                "Pluck",
                tone(pluck(Wave::Saw, 700.0, 0.35, 0.28)),
                -11.0,
                -0.2,
                in_sections(&["verse", "chorus", "drop"], || {
                    tones(
                        &["..3...5...7...5.", "..3...5...8...5."],
                        None,
                        0.25,
                        0.2,
                        0,
                        0.8,
                    )
                }),
            ),
            part(
                "Pad",
                tone(saw_pad(700.0, 0.35, 0.7)),
                -15.0,
                0.0,
                in_sections(&["intro", "chorus", "breakdown", "drop"], || pad(0, 0.7)),
            ),
            part(
                "Arp",
                tone(arp_voice(Wave::Square, 2200.0)),
                -14.0,
                0.25,
                in_sections(&["chorus", "breakdown", "drop"], || {
                    tones(
                        &["1235 3215 1236 7632", "1235 3215 2367 8763"],
                        None,
                        0.25,
                        0.18,
                        1,
                        0.75,
                    )
                }),
            ),
            part(
                "Lead",
                tone(lead_voice(2600.0)),
                -11.0,
                0.0,
                in_sections(&["chorus", "drop"], || {
                    melody(
                        &[
                            "4---2---0---2---",
                            "4---2---0---2-4-",
                            "7---5---4---2---",
                            "5-7-5-4-2---0---",
                        ],
                        0.5,
                        0.45,
                        A_MINOR,
                        0.85,
                    )
                }),
            ),
        ],
    }
}

/// Slow and soft at 88 bpm: a sparse kit, ghost notes on the snare, a round
/// bass, a sine-and-triangle keyboard and a mellow pad. D minor.
fn rainy_lofi() -> Song {
    let dm9 = chord(50, [0, 3, 7, 14]);
    let gm7 = chord(55, MIN7);
    let bbmaj7 = chord(58, MAJ7);
    let a7 = chord(57, DOM7);
    let progression = vec![dm9, gm7, bbmaj7, a7];
    let names = ["intro", "verse", "chorus", "breakdown", "drop"];
    let keys = |cutoff: f32| {
        Tone::new(vec![
            Osc::new(Wave::Sine).detuned(-1.0).panned(-0.3).level(-2.0),
            Osc::new(Wave::Triangle)
                .detuned(1.0)
                .panned(0.3)
                .level(-5.0),
            Osc::new(Wave::Sine).octave(2.0).level(-14.0),
        ])
        .filter(Filter::Svf, cutoff, 0.1, 0.1)
        .filter_env(env(0.002, 0.3, 0.2, 0.2))
        .amp_env(env(0.004, 0.9, 0.1, 0.4))
        .detune(6.0)
        .voices(6)
    };
    Song {
        name: "rainy-lofi",
        master_db: -2.5,
        master_fx: Vec::new(),
        bpm: 88.0,
        sections: sections(&[
            ("intro", 4),
            ("verse", 8),
            ("chorus", 8),
            ("breakdown", 4),
            ("drop", 8),
        ]),
        chords: names.iter().map(|&s| (s, progression.clone())).collect(),
        excerpt: (12, 6),
        parts: vec![
            part(
                "Kick",
                kick(42.0, 70.0, 0.4),
                -4.0,
                0.0,
                in_sections(&["verse", "chorus", "drop"], || {
                    drum(&["X.....x...x.....", "X.....x....x...."], None, 0.35)
                }),
            ),
            part(
                "Snare",
                snare(190.0, 2200.0, 0.2),
                -9.0,
                0.05,
                in_sections(&["verse", "chorus", "drop"], || {
                    drum(&["....X.o.....X..o"], None, 0.2)
                }),
            ),
            part(
                "Hat",
                hat(6500.0, 0.06),
                -16.0,
                -0.2,
                in_sections(&["intro", "verse", "chorus", "drop"], || {
                    drum(&["x.o.x.o.x.o.x.oo"], None, 0.12)
                }),
            ),
            part(
                "Bass",
                tone(
                    Tone::new(vec![
                        Osc::new(Wave::Triangle).level(-2.0),
                        Osc::new(Wave::Sine).octave(0.5).level(-1.0),
                    ])
                    .filter(Filter::Ladder, 260.0, 0.1, 0.1)
                    .filter_env(env(0.004, 0.2, 0.3, 0.2))
                    .amp_env(env(0.005, 0.2, 0.8, 0.2)),
                ),
                -6.0,
                0.0,
                in_sections(&["verse", "chorus", "drop"], || {
                    tones(&["1-----..1.5-1..."], None, 0.25, 0.22, -1, 0.85)
                }),
            ),
            part(
                "Keys",
                tone(keys(1800.0)),
                -16.0,
                -0.1,
                in_sections(&["intro", "verse", "chorus", "breakdown", "drop"], || {
                    stabs(&[(0.0, 1.4), (1.5, 0.5), (2.5, 1.2)], 0, 0.7)
                }),
            ),
            part(
                "Pad",
                tone(
                    Tone::new(vec![
                        Osc::new(Wave::Triangle)
                            .detuned(-1.0)
                            .panned(-0.5)
                            .level(-4.0),
                        Osc::new(Wave::Triangle)
                            .detuned(1.0)
                            .panned(0.5)
                            .level(-4.0),
                    ])
                    .filter(Filter::Ladder, 900.0, 0.1, 0.05)
                    .filter_env(env(0.8, 0.5, 0.8, 1.2))
                    .amp_env(env(0.8, 0.3, 0.9, 1.2))
                    .detune(9.0)
                    .voices(6),
                ),
                -16.0,
                0.0,
                in_sections(&["chorus", "breakdown", "drop"], || pad(0, 0.6)),
            ),
            part(
                "Lead",
                tone(
                    Tone::new(vec![
                        Osc::new(Wave::Triangle).level(-2.0),
                        Osc::new(Wave::Sine).octave(2.0).level(-12.0),
                    ])
                    .filter(Filter::Svf, 2400.0, 0.1, 0.1)
                    .filter_env(env(0.01, 0.3, 0.5, 0.3))
                    .amp_env(env(0.015, 0.3, 0.7, 0.35)),
                ),
                -10.0,
                0.2,
                in_sections(&["chorus", "drop"], || {
                    melody(
                        &[
                            "4-----2-0---2---",
                            "4---2-0---a-----",
                            "2-----4-5---4---",
                            "7---5-4---2-----",
                        ],
                        0.5,
                        0.6,
                        Scale {
                            root: 74,
                            steps: MINOR_PENTATONIC,
                        },
                        0.75,
                    )
                }),
            ),
        ],
    }
}

/// A dense breakbeat at 138 bpm: syncopated kick and snare, sixteenth hats, a
/// growling bass, chord stabs and a fast arpeggio. E minor.
fn breakbeat_run() -> Song {
    let em7 = chord(52, MIN7);
    let c = chord(48, MAJ7);
    let d = chord(50, MAJ);
    let b = chord(47, SUS);
    let progression = vec![em7, c, d, b];
    let names = ["intro", "verse", "chorus", "breakdown", "drop"];
    Song {
        name: "breakbeat-run",
        master_db: -3.0,
        master_fx: Vec::new(),
        bpm: 138.0,
        sections: sections(&[
            ("intro", 4),
            ("verse", 8),
            ("chorus", 8),
            ("breakdown", 8),
            ("drop", 12),
        ]),
        chords: names.iter().map(|&s| (s, progression.clone())).collect(),
        excerpt: (28, 10),
        parts: vec![
            part(
                "Kick",
                kick(52.0, 140.0, 0.22),
                -3.0,
                0.0,
                in_sections(&["intro", "verse", "chorus", "drop"], || {
                    drum(
                        &["X.....X..X..X...", "X...X...X.X...X."],
                        Some("X..X..X.X.X.XXXX"),
                        0.2,
                    )
                }),
            ),
            part(
                "Snare",
                snare(210.0, 2600.0, 0.16),
                -7.0,
                0.05,
                in_sections(&["verse", "chorus", "drop"], || {
                    drum(
                        &["....X..o.o..X..o", "....X.o.....X..o"],
                        Some("....X..oX.xoXxXx"),
                        0.15,
                    )
                }),
            ),
            part(
                "Hat",
                hat(8000.0, 0.03),
                -16.0,
                -0.2,
                in_sections(&["intro", "verse", "chorus", "drop"], || {
                    drum(&["xoxoxoxoxoxoxoxo", "xoxxxoxoxoxxxoxo"], None, 0.08)
                }),
            ),
            part(
                "Open hat",
                hat(6000.0, 0.22),
                -18.0,
                0.2,
                in_sections(&["chorus", "drop"], || {
                    drum(&["..x.....x......."], None, 0.3)
                }),
            ),
            part(
                "Bass",
                tone(
                    Tone::new(vec![
                        Osc::new(Wave::Saw).level(-3.0),
                        Osc::new(Wave::Square).octave(0.5).level(-6.0),
                    ])
                    .filter(Filter::Ladder, 150.0, 0.45, 0.4)
                    .filter_env(env(0.002, 0.18, 0.15, 0.1))
                    .amp_env(env(0.003, 0.1, 0.85, 0.06)),
                ),
                -8.0,
                0.0,
                in_sections(&["verse", "chorus", "drop"], || {
                    tones(
                        &["1--1--1-1--1-51-", "1--1--1-1--5-1-4"],
                        None,
                        0.25,
                        0.2,
                        -1,
                        0.9,
                    )
                }),
            ),
            part(
                "Stab",
                tone(pluck(Wave::Saw, 1200.0, 0.3, 0.2)),
                -17.0,
                -0.15,
                in_sections(&["chorus", "drop"], || {
                    stabs(
                        &[(0.0, 0.25), (0.75, 0.25), (2.0, 0.25), (3.0, 0.4)],
                        0,
                        0.8,
                    )
                }),
            ),
            part(
                "Arp",
                tone(arp_voice(Wave::Saw, 3000.0)),
                -15.0,
                0.25,
                in_sections(&["breakdown", "drop"], || {
                    tones(&["1235 6532 3567 8653"], None, 0.25, 0.14, 1, 0.8)
                }),
            ),
            part(
                "Pad",
                tone(saw_pad(600.0, 0.5, 0.8)),
                -16.0,
                0.0,
                in_sections(&["intro", "breakdown"], || pad(0, 0.7)),
            ),
        ],
    }
}

/// Almost no drums at 100 bpm: bell plucks, long pads and a deep sine bass
/// with a light pulse underneath. C major.
fn glass_ambient() -> Song {
    let cadd9 = chord(48, ADD9);
    let am7 = chord(57, MIN7);
    let fmaj7 = chord(53, MAJ7);
    let gsus = chord(55, SUS);
    let progression = vec![cadd9, am7, fmaj7, gsus];
    let names = ["intro", "verse", "chorus", "breakdown", "drop"];
    let bell = |decay: f32| {
        Tone::new(vec![
            Osc::new(Wave::Sine).level(-3.0),
            Osc::new(Wave::Sine).octave(3.0).level(-12.0),
            Osc::new(Wave::Sine).octave(5.04).level(-20.0),
        ])
        .filter(Filter::Svf, 5000.0, 0.05, 0.0)
        .amp_env(env(0.002, decay, 0.0, 0.5))
        .voices(6)
    };
    Song {
        name: "glass-ambient",
        master_db: -1.0,
        master_fx: Vec::new(),
        bpm: 100.0,
        sections: sections(&[
            ("intro", 4),
            ("verse", 8),
            ("chorus", 8),
            ("breakdown", 8),
            ("drop", 8),
        ]),
        chords: names.iter().map(|&s| (s, progression.clone())).collect(),
        excerpt: (12, 7),
        parts: vec![
            part(
                "Pad",
                tone(saw_pad(500.0, 1.2, 1.6)),
                -13.0,
                0.0,
                in_sections(&names, || pad(0, 0.7)),
            ),
            part(
                "Sub",
                tone(
                    Tone::new(vec![Osc::new(Wave::Sine).level(-1.0)])
                        .filter(Filter::Svf, 400.0, 0.0, 0.0)
                        .amp_env(env(0.02, 0.2, 0.9, 0.5)),
                ),
                -13.0,
                0.0,
                in_sections(&["verse", "chorus", "breakdown", "drop"], || {
                    tones(&["1---------------"], None, 0.25, 3.8, -1, 0.9)
                }),
            ),
            part(
                "Bells",
                tone(bell(1.6)),
                -12.0,
                0.2,
                in_sections(&["verse", "chorus", "drop"], || {
                    tones(
                        &["5...7...6...8...", "8...6...7...5..."],
                        None,
                        0.5,
                        0.5,
                        1,
                        0.7,
                    )
                }),
            ),
            part(
                "Glass",
                tone(bell(0.9)),
                -14.0,
                -0.25,
                in_sections(&["chorus", "breakdown", "drop"], || {
                    melody(
                        &[
                            "....4.......2...",
                            "0.......2...4...",
                            "....7...4.......",
                            "2.......0.......",
                        ],
                        0.5,
                        0.5,
                        Scale {
                            root: 72,
                            steps: MAJOR_PENTATONIC,
                        },
                        0.65,
                    )
                }),
            ),
            part(
                "Pulse",
                tone(arp_voice(Wave::Triangle, 1500.0)),
                -18.0,
                0.0,
                in_sections(&["chorus", "drop"], || {
                    tones(&["1.3.5.3.1.3.5.3."], None, 0.25, 0.15, 0, 0.6)
                }),
            ),
            part(
                "Hat",
                hat(9000.0, 0.05),
                -22.0,
                0.15,
                in_sections(&["chorus", "drop"], || {
                    drum(&["....x.......x..."], None, 0.1)
                }),
            ),
            part(
                "Kick",
                kick(40.0, 60.0, 0.5),
                -8.0,
                0.0,
                in_sections(&["drop"], || drum(&["X.......X......."], None, 0.4)),
            ),
        ],
    }
}

/// A syncopated groove at 110 bpm: ghosted snares, sixteenth hats, a
/// plucked square bass, clav stabs and a brass-like lead. G minor.
fn funk_engine() -> Song {
    let gm7 = chord(55, MIN7);
    let c7 = chord(60, DOM7);
    let bbmaj7 = chord(58, MAJ7);
    let d7 = chord(50, DOM7);
    let progression = vec![gm7, c7, bbmaj7, d7];
    let names = ["intro", "verse", "chorus", "breakdown", "drop"];
    Song {
        name: "funk-engine",
        master_db: -2.5,
        master_fx: Vec::new(),
        bpm: 110.0,
        sections: sections(&[
            ("intro", 4),
            ("verse", 8),
            ("chorus", 8),
            ("breakdown", 4),
            ("drop", 8),
        ]),
        chords: names.iter().map(|&s| (s, progression.clone())).collect(),
        excerpt: (16, 8),
        parts: vec![
            part(
                "Kick",
                kick(50.0, 110.0, 0.26),
                -3.0,
                0.0,
                in_sections(&["intro", "verse", "chorus", "drop"], || {
                    drum(
                        &["X..x..X...x.X...", "X..x...x..xX...."],
                        Some("X..x..X.X.x.X.xX"),
                        0.25,
                    )
                }),
            ),
            part(
                "Snare",
                snare(200.0, 2400.0, 0.17),
                -8.0,
                0.05,
                in_sections(&["verse", "chorus", "drop"], || {
                    drum(&["....X..o.o..X.o.", "....X.o...o.X..o"], None, 0.15)
                }),
            ),
            part(
                "Hat",
                hat(7000.0, 0.04),
                -16.0,
                -0.2,
                in_sections(&["intro", "verse", "chorus", "drop"], || {
                    drum(&["xoxoxoxXxoxoxoxo"], None, 0.09)
                }),
            ),
            part(
                "Bass",
                tone(
                    Tone::new(vec![
                        Osc::new(Wave::Square).level(-4.0),
                        Osc::new(Wave::Sine).octave(0.5).level(-2.0),
                    ])
                    .filter(Filter::Ladder, 200.0, 0.35, 0.3)
                    .filter_env(env(0.002, 0.1, 0.3, 0.08))
                    .amp_env(env(0.002, 0.1, 0.7, 0.06)),
                ),
                -8.0,
                0.0,
                in_sections(&["verse", "chorus", "drop"], || {
                    tones(
                        &["1.1.1.5.1..1.51.", "1.1.1.5.1.5.4.3."],
                        None,
                        0.25,
                        0.18,
                        -1,
                        0.9,
                    )
                }),
            ),
            part(
                "Clav",
                tone(pluck(Wave::Saw, 1400.0, 0.4, 0.14)),
                -16.0,
                -0.2,
                in_sections(&["verse", "chorus", "breakdown", "drop"], || {
                    stabs(&[(0.5, 0.2), (1.25, 0.2), (2.5, 0.2), (3.25, 0.2)], 0, 0.8)
                }),
            ),
            part(
                "Pad",
                tone(saw_pad(800.0, 0.3, 0.6)),
                -17.0,
                0.0,
                in_sections(&["intro", "chorus", "breakdown"], || pad(0, 0.6)),
            ),
            part(
                "Brass",
                tone(lead_voice(2200.0)),
                -11.0,
                0.15,
                in_sections(&["chorus", "drop"], || {
                    melody(
                        &[
                            "4-.4-.2-0-.2-.4-",
                            "4-.4-.7-5-.4-.2-",
                            "5-.5-.4-2-.0-.2-",
                            "7---5---4-2-0---",
                        ],
                        0.25,
                        0.22,
                        Scale {
                            root: 67,
                            steps: MINOR_PENTATONIC,
                        },
                        0.85,
                    )
                }),
            ),
        ],
    }
}
