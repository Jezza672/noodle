//! The Tuning node: retunes notes from a Scala file.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Expression, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, NoteId, ParamInfo, Ports, Setup, Unit,
};

/// Retunes the notes that pass through it to a Scala scale (`.scl`), with an
/// optional keyboard mapping (`.kbm`). Each note-on is forwarded unchanged,
/// followed at the same frame by a [`Expression::Pitch`] that moves it by the
/// difference between its tuned pitch and 12-tone equal temperament. Voices,
/// Mono Note, the Sampler and plugins that honour note tuning follow it, so
/// the scale belongs to the signal path, not to any one instrument.
///
/// A pitch expression that arrives for a note (a clip's per-note bend, MPE)
/// is passed on with the note's tuning added, so it still works on top.
///
/// With no scale loaded it is 12-tone equal temperament, so it changes
/// nothing until `reference_hz` moves off 440. `reference_hz` is the
/// frequency of the mapping's reference note (A4 by default); it replaces the
/// `.kbm` file's own frequency. Keys a `.kbm` leaves unmapped don't sound: the
/// note, its note-off and its expression are dropped.
/// `reference_hz` is read when each note starts, so a held note keeps its
/// tuning.
///
/// At most [`HELD`] notes are tuned at once; a note beyond that passes
/// untuned.
pub struct Tuning;

pub const TUNING_ID: &str = "noodle.event.tuning";
pub const SCL_KEY: &str = "scl";
pub const KBM_KEY: &str = "kbm";

/// The most notes held and tuned at once.
pub const HELD: usize = 128;

static CONFIG: [ConfigInfo; 2] = [
    ConfigInfo::text(SCL_KEY, "Scale (.scl)"),
    ConfigInfo::text(KBM_KEY, "Keyboard mapping (.kbm)"),
];

#[derive(Ports)]
struct TuningPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "reference_hz",
        "Reference",
        ParamInfo::new(8.0, 2000.0, 440.0).unit(Unit::Hertz).smoothing(0.0)
    )]
    reference_hz: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: TUNING_ID,
    version: 1,
    name: "Tuning",
    category: "Events",
};

/// A parsed Scala scale: the cents of each degree above the first, the last
/// being the period (usually the octave, 1200).
#[derive(Clone, Debug, PartialEq)]
pub struct Scale {
    pub description: String,
    pub degrees: Vec<f64>,
}

impl Scale {
    /// 12-tone equal temperament.
    pub fn equal() -> Self {
        Self {
            description: "12-tone equal temperament".into(),
            degrees: (1..=12).map(|d| f64::from(d) * 100.0).collect(),
        }
    }

    pub fn period(&self) -> f64 {
        self.degrees.last().copied().unwrap_or(1200.0)
    }

    /// Cents above degree 0 of scale step `step` (which may be negative or
    /// past the period).
    fn cents(&self, step: i64) -> f64 {
        let n = self.degrees.len() as i64;
        let (octave, degree) = (step.div_euclid(n), step.rem_euclid(n));
        let within = if degree == 0 {
            0.0
        } else {
            self.degrees[degree as usize - 1]
        };
        octave as f64 * self.period() + within
    }
}

/// Lines of a Scala file that carry content: comments (`!`) are gone, and
/// anything after the first word is ignored by the callers that need it.
fn content_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.starts_with('!'))
}

/// Parses a `.scl` file. Errors name the line.
pub fn parse_scl(text: &str) -> Result<Scale, String> {
    let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    // The first non-comment line is the description, which may be empty.
    let description = loop {
        match lines.next() {
            Some((_, l)) if l.starts_with('!') => continue,
            Some((_, l)) => break l.to_string(),
            None => return Err("the file is empty".into()),
        }
    };
    let rest: Vec<(usize, &str)> = lines.filter(|(_, l)| !l.starts_with('!')).collect();
    let Some(&(count_line, count)) = rest.first() else {
        return Err("no note count".into());
    };
    let count: usize = count
        .split_whitespace()
        .next()
        .and_then(|w| w.parse().ok())
        .ok_or_else(|| format!("line {count_line}: expected the number of notes"))?;
    if !(1..=1024).contains(&count) {
        return Err(format!("line {count_line}: {count} notes is out of range"));
    }
    let mut degrees = Vec::with_capacity(count);
    for &(line, text) in rest[1..].iter().take(count) {
        let word = text.split_whitespace().next().unwrap_or("");
        let cents = if word.contains('.') {
            word.parse::<f64>()
                .map_err(|_| format!("line {line}: '{word}' is not a pitch"))?
        } else {
            let (num, den) = word.split_once('/').unwrap_or((word, "1"));
            let (num, den): (f64, f64) = (
                num.parse()
                    .map_err(|_| format!("line {line}: '{word}' is not a pitch"))?,
                den.parse()
                    .map_err(|_| format!("line {line}: '{word}' is not a pitch"))?,
            );
            if num <= 0.0 || den <= 0.0 {
                return Err(format!("line {line}: a ratio must be positive"));
            }
            1200.0 * (num / den).log2()
        };
        degrees.push(cents);
    }
    if degrees.len() < count {
        return Err(format!("expected {count} pitches, found {}", degrees.len()));
    }
    Ok(Scale {
        description,
        degrees,
    })
}

/// A keyboard mapping from a `.kbm` file.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    /// One entry per key of the repeating pattern: the scale degree, or
    /// `None` for an unmapped key. Empty means a linear mapping.
    pub map: Vec<Option<i64>>,
    pub middle: i64,
    pub reference_note: i64,
    pub reference_hz: f64,
    /// Scale steps per repeat of the map.
    pub formal_octave: Option<i64>,
}

impl Default for Mapping {
    fn default() -> Self {
        Self {
            map: Vec::new(),
            middle: 60,
            reference_note: 69,
            reference_hz: 440.0,
            formal_octave: None,
        }
    }
}

/// Parses a `.kbm` file. Errors name the line.
pub fn parse_kbm(text: &str) -> Result<Mapping, String> {
    let mut values: Vec<(usize, &str)> = content_lines(text)
        .filter(|(_, l)| !l.is_empty())
        .map(|(n, l)| (n, l.split_whitespace().next().unwrap_or("")))
        .collect();
    if values.len() < 7 {
        return Err("a keyboard mapping needs seven header lines".into());
    }
    let tail = values.split_off(7);
    let int = |i: usize| -> Result<i64, String> {
        values[i]
            .1
            .parse()
            .map_err(|_| format!("line {}: '{}' is not a number", values[i].0, values[i].1))
    };
    let size = int(0)?;
    if !(0..=127).contains(&size) {
        return Err(format!(
            "line {}: the map size is out of range",
            values[0].0
        ));
    }
    let middle = int(3)?;
    let reference_note = int(4)?;
    let reference_hz: f64 = values[5]
        .1
        .parse()
        .map_err(|_| format!("line {}: '{}' is not a frequency", values[5].0, values[5].1))?;
    if reference_hz <= 0.0 {
        return Err(format!(
            "line {}: the frequency must be positive",
            values[5].0
        ));
    }
    let formal = int(6)?;
    let mut map = Vec::new();
    for (line, word) in tail.into_iter().take(size as usize) {
        map.push(if word.eq_ignore_ascii_case("x") {
            None
        } else {
            Some(
                word.parse()
                    .map_err(|_| format!("line {line}: '{word}' is not a degree"))?,
            )
        });
    }
    if map.len() < size as usize {
        return Err(format!("expected {size} map entries, found {}", map.len()));
    }
    Ok(Mapping {
        map,
        middle,
        reference_note,
        reference_hz,
        formal_octave: (formal > 0).then_some(formal),
    })
}

/// The semitones each key moves from 12-tone equal temperament (A4 = 440 Hz at
/// key 69), for a reference note at `reference_hz`. Keys the mapping leaves
/// unmapped are NaN: they don't sound.
pub fn offsets(scale: &Scale, mapping: &Mapping, reference_hz: f64) -> [f32; 128] {
    let n = scale.degrees.len() as i64;
    let steps = |key: i64| -> Option<i64> {
        let from_middle = key - mapping.middle;
        if mapping.map.is_empty() {
            return Some(from_middle);
        }
        let size = mapping.map.len() as i64;
        let (repeat, index) = (from_middle.div_euclid(size), from_middle.rem_euclid(size));
        let degree = mapping.map[index as usize]?;
        Some(repeat * mapping.formal_octave.unwrap_or(n) + degree)
    };
    let reference = steps(mapping.reference_note)
        .map(|s| scale.cents(s))
        .unwrap_or(0.0);
    let mut out = [f32::NAN; 128];
    for (key, slot) in out.iter_mut().enumerate() {
        let Some(step) = steps(key as i64) else {
            continue;
        };
        let hz = reference_hz * ((scale.cents(step) - reference) / 1200.0).exp2();
        let equal = 440.0 * ((key as f64 - 69.0) / 12.0).exp2();
        *slot = (12.0 * (hz / equal).log2()) as f32;
    }
    out
}

impl Tuning {
    fn tables(config: &Config) -> Result<(Scale, Mapping), NodeError> {
        let scl = CONFIG[0].get_text(config);
        let kbm = CONFIG[1].get_text(config);
        let scale = if scl.trim().is_empty() {
            Scale::equal()
        } else {
            parse_scl(&scl).map_err(|e| NodeError::config(format!("Tuning scale: {e}")))?
        };
        let mapping = if kbm.trim().is_empty() {
            Mapping::default()
        } else {
            parse_kbm(&kbm).map_err(|e| NodeError::config(format!("Tuning mapping: {e}")))?
        };
        Ok((scale, mapping))
    }
}

impl NodeType for Tuning {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        Self::tables(config)?;
        Ok(TuningPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let (scale, mapping) = Self::tables(setup.config)?;
        Ok(Instance::realtime(TuningNode {
            scale,
            mapping,
            table: Box::new([0.0; 128]),
            table_for: f32::NAN,
            held: Vec::with_capacity(HELD),
            untuned: 0,
        }))
    }
}

struct TuningNode {
    scale: Scale,
    mapping: Mapping,
    /// Semitone offset per key for `table_for` Hz.
    table: Box<[f32; 128]>,
    table_for: f32,
    /// Notes sounding and the offset each was started with. Never grows past
    /// [`HELD`], so it never allocates.
    held: Vec<(NoteId, f32)>,
    /// Notes that passed untuned because too many were held.
    untuned: u64,
}

impl TuningNode {
    fn refresh(&mut self, hz: f32) {
        if hz != self.table_for {
            self.table_for = hz;
            // 128 logs into an array that is already there: no allocation.
            *self.table = offsets(&self.scale, &self.mapping, f64::from(hz));
        }
    }
}

impl Node for TuningNode {
    fn reset(&mut self) {
        self.held.clear();
    }

    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let hz = inputs[TuningPorts::REFERENCE_HZ]
            .lane(0, 0)
            .first()
            .copied()
            .unwrap_or(440.0)
            .clamp(8.0, 2000.0);
        let out = &mut event_outputs[TuningPorts::OUT];
        for event in event_inputs[TuningPorts::NOTES] {
            match event.kind {
                EventKind::NoteOn { note, key, .. } => {
                    let offset = self.table_offset(hz, key);
                    if offset.is_nan() && self.held.len() < HELD {
                        // Unmapped: remembered so its off and expression go too.
                        self.held.push((note, offset));
                        continue;
                    }
                    let _ = out.push(*event);
                    if self.held.len() < HELD {
                        self.held.push((note, offset));
                        if offset != 0.0 {
                            let _ = out.push(Event {
                                time: event.time,
                                kind: EventKind::Expression {
                                    note,
                                    expression: Expression::Pitch,
                                    value: offset,
                                },
                            });
                        }
                    } else {
                        self.untuned += 1;
                    }
                }
                EventKind::NoteOff { note, .. } => {
                    let dropped = self.held.iter().any(|(n, o)| *n == note && o.is_nan());
                    self.held.retain(|(n, _)| *n != note);
                    if !dropped {
                        let _ = out.push(*event);
                    }
                }
                EventKind::Expression {
                    note,
                    expression: Expression::Pitch,
                    value,
                } => {
                    let base = self
                        .held
                        .iter()
                        .find(|(n, _)| *n == note)
                        .map_or(0.0, |(_, b)| *b);
                    if base.is_nan() {
                        continue;
                    }
                    let _ = out.push(Event {
                        time: event.time,
                        kind: EventKind::Expression {
                            note,
                            expression: Expression::Pitch,
                            value: value + base,
                        },
                    });
                }
                _ => {
                    let _ = out.push(*event);
                }
            }
        }
    }
}

impl TuningNode {
    fn table_offset(&mut self, hz: f32, key: u8) -> f32 {
        self.refresh(hz);
        self.table[usize::from(key.min(127))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Value;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;

    fn config(scl: &str, kbm: &str) -> Config {
        let mut config = Config::new();
        config.set(SCL_KEY, Value::Text(scl.into()));
        config.set(KBM_KEY, Value::Text(kbm.into()));
        config
    }

    /// Key 61 a quarter tone above key 60: 24 steps to the octave.
    fn quarter_tone() -> String {
        let mut text = String::from("! 24edo\nQuarter tones\n24\n");
        for d in 1..=24 {
            text.push_str(&format!("{:.5}\n", f64::from(d) * 50.0));
        }
        text
    }

    fn on(time: u32, id: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.8,
            },
        }
    }

    fn off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    fn bend(time: u32, id: u32, value: f32) -> Event {
        Event {
            time,
            kind: EventKind::Expression {
                note: NoteId(id),
                expression: Expression::Pitch,
                value,
            },
        }
    }

    fn harness(config: &Config) -> Harness {
        Harness::new(&Tuning, config, &[], RATE, 128).unwrap()
    }

    fn pitches(h: &Harness) -> Vec<(u32, f32)> {
        h.events(TuningPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::Expression {
                    expression: Expression::Pitch,
                    value,
                    ..
                } => Some((e.time, value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_12_tet_scale_has_no_offsets() {
        let mut text = String::from("12 tet\n12\n");
        for d in 1..=12 {
            text.push_str(&format!("{}.0\n", d * 100));
        }
        let table = offsets(&parse_scl(&text).unwrap(), &Mapping::default(), 440.0);
        assert!(table.iter().all(|o| o.abs() < 1e-4), "{table:?}");
    }

    #[test]
    fn a_just_scale_with_ratios_and_comments_parses() {
        let text = "! just.scl\n!\nJust major\n 7 \n! a comment\n9/8\n5/4\n4/3 fourth\n3/2\n5/3\n15/8\n2/1\n";
        let scale = parse_scl(text).unwrap();
        assert_eq!(scale.description, "Just major");
        assert_eq!(scale.degrees.len(), 7);
        assert!((scale.degrees[1] - 386.3137).abs() < 1e-3);
        assert!((scale.period() - 1200.0).abs() < 1e-9);
        // Under a linear mapping from key 60, key 64 (four steps) is 5/4 over
        // the tonic: 13.7 cents flat of the equal-tempered key 64 above
        // key 60's own tuning.
        let mapping = Mapping {
            middle: 60,
            reference_note: 60,
            reference_hz: 261.6256,
            formal_octave: None,
            map: Vec::new(),
        };
        let table = offsets(&scale, &mapping, 261.6256);
        assert!(table[60].abs() < 1e-3);
        // Step 2 of the 7 is 5/4 = 386.3 cents; key 62 is 200 cents in 12-TET.
        assert!(
            (table[62] - (386.3137 - 200.0) / 100.0).abs() < 1e-3,
            "{}",
            table[62]
        );
    }

    #[test]
    fn bad_files_say_which_line() {
        assert!(parse_scl("").is_err());
        let error = parse_scl("d\n2\n100.0\nbanana\n").unwrap_err();
        assert!(error.contains("line 4"), "{error}");
        assert!(
            parse_scl("d\n3\n100.0\n")
                .unwrap_err()
                .contains("expected 3")
        );
        let error = parse_kbm("0\n0\n127\n60\n69\nabc\n12\n").unwrap_err();
        assert!(error.contains("line 6"), "{error}");
    }

    #[test]
    fn a_keyboard_mapping_sets_the_reference_and_leaves_unmapped_keys() {
        // Two-key map: key 60 is degree 0, key 61 is unmapped.
        let kbm = "2\n0\n127\n60\n60\n300.0\n12\n0\nx\n";
        let mapping = parse_kbm(kbm).unwrap();
        assert_eq!(mapping.map, [Some(0), None]);
        let table = offsets(&Scale::equal(), &mapping, 300.0);
        // Key 60 is at 300 Hz now, not 261.6: a little over two semitones up.
        let expected = 12.0 * (300.0f64 / 261.6256).log2();
        assert!((f64::from(table[60]) - expected).abs() < 1e-3);
        assert!(table[61].is_nan());
    }

    /// The Hz a key sounds at once its pitch expression is applied.
    fn hz(key: u8, offset: f32) -> f64 {
        440.0 * ((f64::from(key) - 69.0 + f64::from(offset)) / 12.0).exp2()
    }

    #[test]
    fn an_unmapped_key_does_not_sound() {
        // Key 60 mapped, key 61 unmapped, repeating.
        let kbm = "2\n0\n127\n60\n60\n261.6256\n12\n0\nx\n";
        let mut h = harness(&config("", kbm));
        h.send_events(
            TuningPorts::NOTES,
            &[
                on(0, 1, 60),
                on(0, 2, 61),
                bend(5, 2, 1.0),
                off(50, 2),
                off(60, 1),
            ],
        );
        h.run(128).unwrap();
        let out = h.events(TuningPorts::OUT);
        let note_of = |k: &EventKind| match k {
            EventKind::NoteOn { note, .. }
            | EventKind::NoteOff { note, .. }
            | EventKind::Expression { note, .. } => note.0,
            _ => 0,
        };
        assert!(out.iter().all(|e| note_of(&e.kind) == 1), "{out:?}");
        // Note 1: on, its tuning, off.
        assert_eq!(out.len(), 3, "{out:?}");
    }

    #[test]
    fn a_note_gets_a_pitch_expression_in_the_same_frame() {
        let mut h = harness(&config(&quarter_tone(), ""));
        h.send_events(
            TuningPorts::NOTES,
            &[on(10, 1, 60), on(10, 2, 61), off(90, 1)],
        );
        h.run(128).unwrap();
        let p = pitches(&h);
        assert_eq!(p.len(), 2);
        assert!(p.iter().all(|(t, _)| *t == 10));
        // Key 61 sounds a quarter tone (50 cents) above key 60.
        let ratio = hz(61, p[1].1) / hz(60, p[0].1);
        assert!((ratio - 2f64.powf(50.0 / 1200.0)).abs() < 1e-5, "{ratio}");
        let kinds: Vec<_> = h.events(TuningPorts::OUT).iter().map(|e| e.kind).collect();
        assert!(matches!(kinds[0], EventKind::NoteOn { .. }));
        assert!(matches!(kinds.last(), Some(EventKind::NoteOff { .. })));
    }

    #[test]
    fn a_bend_is_added_on_top_of_the_tuning() {
        let mut h = harness(&config(&quarter_tone(), ""));
        h.send_events(TuningPorts::NOTES, &[on(0, 1, 61), bend(20, 1, 1.0)]);
        h.run(128).unwrap();
        let p = pitches(&h);
        assert_eq!(p.len(), 2);
        assert!((p[1].1 - p[0].1 - 1.0).abs() < 1e-5, "{p:?}");
    }

    #[test]
    fn the_129th_held_note_passes_untuned() {
        let mut h = harness(&config(&quarter_tone(), ""));
        let notes: Vec<Event> = (0..=HELD as u32).map(|i| on(0, i + 1, 61)).collect();
        let mut tuned = 0;
        for chunk in notes[..HELD].chunks(64) {
            h.send_events(TuningPorts::NOTES, chunk);
            h.run(128).unwrap();
            tuned += pitches(&h).len();
        }
        assert_eq!(tuned, HELD);
        h.send_events(TuningPorts::NOTES, &notes[HELD..]);
        h.run(128).unwrap();
        assert!(pitches(&h).is_empty());
    }

    #[test]
    fn a_held_note_keeps_its_tuning_when_the_reference_moves() {
        let mut h = harness(&config("", ""));
        h.send_events(TuningPorts::NOTES, &[on(0, 1, 69)]);
        h.run(128).unwrap();
        assert!(pitches(&h).is_empty());
        h.set(TuningPorts::REFERENCE_HZ, 466.1638);
        h.send_events(TuningPorts::NOTES, &[bend(0, 1, 0.25), on(1, 2, 69)]);
        h.run(128).unwrap();
        let p = pitches(&h);
        // The first note's bend passes with its own (zero) base; the second
        // note starts a semitone up.
        assert!((p[0].1 - 0.25).abs() < 1e-4, "{p:?}");
        assert!((p[1].1 - 1.0).abs() < 1e-3, "{p:?}");
    }

    #[test]
    fn a_bad_file_fails_the_node() {
        assert!(Tuning.layout(&config("x\n1\nnope\n", "")).is_err());
    }
}
