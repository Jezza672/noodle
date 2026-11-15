//! The Morph node.

use noodle_core::Value;
use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Setup,
};

/// Blends a set of parameters between two settings, A and B, with one
/// `position` (0 to 1). "Morph between presets..." on a group adds one, with
/// an output wired into every parameter whose value differs between two
/// presets; the wires are real, so nothing is modulated unseen, and
/// `position` can be automated, modulated or learned from MIDI.
///
/// **Rows.** The `targets` config is a list of rows, one per output (the list
/// decides the ports, so it is config). Each holds the parameter's `min`,
/// `max` and taper, and `a` and `b` as positions from 0 to 1 along that range
/// (as a Version Value slot is stored; a parameter a wire offsets uses -1 to 1
/// with 0.5 as no move). The output is the row's value at `position`,
/// interpolated in position space, so a log parameter morphs along its taper.
/// A stepped row (a choice) switches at the midpoint instead.
///
/// Stateless, so editing a row swaps in without a fade.
pub struct Morph;

pub const MORPH_ID: &str = "noodle.mod.morph";

/// The config key that holds the rows (see [`rows_to_text`]).
pub const MORPH_TARGETS: &str = "targets";
pub const MORPH_POSITION: &str = "position";
/// The second axis, for four presets (see [`MorphRow::c`]).
pub const MORPH_Y: &str = "y";

const TARGETS: ConfigInfo = ConfigInfo::text(MORPH_TARGETS, "Targets");
static CONFIG: [ConfigInfo; 1] = [TARGETS];

static INFO: NodeInfo = NodeInfo {
    id: MORPH_ID,
    version: 1,
    name: "Morph",
    category: "Modulation",
};

/// One output of the node.
#[derive(Clone, Debug, PartialEq)]
pub struct MorphRow {
    /// What the output is called: the node and parameter it was made for.
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub log: bool,
    /// Positions from 0 to 1 along `min` to `max`, at the four corners of the
    /// pad: `a` where `position` and `y` are 0, `b` at `position` 1, `c` at
    /// `y` 1, `d` at both. A two-preset morph has `c` = `a` and `d` = `b`, so
    /// `y` does nothing.
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    /// Switch at the midpoint rather than blend.
    pub stepped: bool,
}

impl MorphRow {
    /// A row for a two-preset morph: `c` and `d` repeat `a` and `b`.
    pub fn two(name: String, min: f32, max: f32, log: bool, a: f32, b: f32, stepped: bool) -> Self {
        Self {
            name,
            min,
            max,
            log,
            a,
            b,
            c: a,
            d: b,
            stepped,
        }
    }

    /// Whether the row's `y` axis does anything.
    pub fn uses_y(&self) -> bool {
        self.c != self.a || self.d != self.b
    }

    /// The corner's position along the range: 0 is `a`, 1 is `b`, 2 is `c`, 3
    /// is `d`.
    pub fn corner(&self, corner: usize) -> f32 {
        [self.a, self.b, self.c, self.d][corner.min(3)]
    }

    pub fn set_corner(&mut self, corner: usize, at: f32) {
        let at = at.clamp(0.0, 1.0);
        match corner {
            0 => self.a = at,
            1 => self.b = at,
            2 => self.c = at,
            _ => self.d = at,
        }
    }

    /// Where along the range (0 to 1) the row is at `position` along the top
    /// edge and `y` down the pad, blended bilinearly between the corners.
    pub fn blend(&self, position: f32, y: f32) -> f32 {
        let t = if self.stepped {
            // The nearest corner.
            match (position < 0.5, y < 0.5) {
                (true, true) => self.a,
                (false, true) => self.b,
                (true, false) => self.c,
                (false, false) => self.d,
            }
        } else {
            let top = self.a + (self.b - self.a) * position;
            let bottom = self.c + (self.d - self.c) * position;
            top + (bottom - top) * y
        };
        t.clamp(0.0, 1.0)
    }

    /// The value at a position along the range.
    pub fn at_position(&self, t: f32) -> f32 {
        scale(t.clamp(0.0, 1.0), self.min, self.max, self.log)
    }

    /// The value at `position` along the top edge and `y` down the pad.
    pub fn value(&self, position: f32, y: f32) -> f32 {
        self.at_position(self.blend(position, y))
    }
}

/// Makes `corner` of every row what the rows blend to at `position` and `y`
/// now: capturing the sound found by moving the pad.
pub fn set_corner_from_current(rows: &mut [MorphRow], corner: usize, position: f32, y: f32) {
    for row in rows {
        let at = row.blend(position, y);
        row.set_corner(corner, at);
    }
}

/// Fills `corner` of every row with a random place in its range, from `seed`
/// (the same seed gives the same values). A stepped row lands on one of its
/// choices.
pub fn randomise_corner(rows: &mut [MorphRow], corner: usize, seed: u64) {
    // SplitMix64: small, and good enough for picking sounds.
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    };
    for row in rows {
        let r = next();
        let at = if row.stepped {
            let steps = (row.max - row.min).round().max(1.0);
            (r * steps).round() / steps
        } else {
            r
        };
        row.set_corner(corner, at);
    }
}

#[inline]
fn scale(t: f32, min: f32, max: f32, log: bool) -> f32 {
    if log && min > 0.0 && max > min {
        min * (max / min).powf(t)
    } else {
        min + t * (max - min)
    }
}

/// The rows as the `targets` config holds them: one line each, fields
/// separated by `|`. A name's own `|` and line breaks are replaced.
pub fn rows_to_text(rows: &[MorphRow]) -> String {
    rows.iter()
        .map(|r| {
            let name: String = r
                .name
                .chars()
                .map(|c| if c == '|' || c == '\n' { ' ' } else { c })
                .collect();
            format!(
                "{name}|{}|{}|{}|{}|{}|{}|{}|{}",
                r.min, r.max, r.log as u8, r.a, r.b, r.c, r.d, r.stepped as u8
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads the rows back. A line that doesn't parse is skipped.
pub fn rows_from_text(text: &str) -> Vec<MorphRow> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('|').collect();
            if f.len() != 9 {
                return None;
            }
            Some(MorphRow {
                name: f[0].to_owned(),
                min: f[1].parse().ok()?,
                max: f[2].parse().ok()?,
                log: f[3] == "1",
                a: f[4].parse().ok()?,
                b: f[5].parse().ok()?,
                c: f[6].parse().ok()?,
                d: f[7].parse().ok()?,
                stepped: f[8] == "1",
            })
        })
        .collect()
}

/// The rows a node's config holds (none for a config without any).
pub fn rows_of(config: &Config) -> Vec<MorphRow> {
    rows_from_text(&TARGETS.get_text(config))
}

/// The node's config for `rows`.
pub fn morph_config(rows: &[MorphRow]) -> Config {
    Config::new().with(MORPH_TARGETS, Value::Text(rows_to_text(rows)))
}

/// The most rows (outputs) a Morph has.
pub const MAX_ROWS: usize = 256;

/// The key of row `index`'s output.
pub fn morph_output_key(index: usize) -> String {
    format!("t{}", index + 1)
}

fn rows(config: &Config) -> Result<Vec<MorphRow>, NodeError> {
    let rows = rows_from_text(&TARGETS.get_text(config));
    if rows.len() > MAX_ROWS {
        return Err(NodeError::config(format!(
            "Morph has at most {MAX_ROWS} rows, not {}",
            rows.len()
        )));
    }
    Ok(rows)
}

impl NodeType for Morph {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn stateless(&self) -> bool {
        true
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let mut layout = Layout::realtime()
            .param(
                MORPH_POSITION,
                "Position",
                ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0).featured(),
            )
            .param(MORPH_Y, "Y", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0));
        for (i, row) in rows(config)?.iter().enumerate() {
            layout = layout.control_output(morph_output_key(i), row.name.clone());
        }
        Ok(layout.memoryless())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(MorphNode {
            rows: rows(setup.config)?,
        }))
    }
}

struct MorphNode {
    rows: Vec<MorphRow>,
}

impl Node for MorphNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs, outputs, ..
        } = io;
        let position = inputs[0].lane(0, 0);
        let y = inputs[1].lane(0, 0);
        let unit = |p: f32| {
            if p.is_finite() {
                p.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        for (row, out) in self.rows.iter().zip(outputs.iter_mut()) {
            let out = out.lane_mut(0, 0);
            for ((o, &p), &y) in out.iter_mut().zip(position).zip(y) {
                *o = row.value(unit(p), unit(y));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn rows() -> Vec<MorphRow> {
        vec![
            MorphRow {
                name: "Linear".into(),
                min: 0.0,
                max: 10.0,
                log: false,
                a: 0.0,
                b: 1.0,
                c: 0.0,
                d: 1.0,
                stepped: false,
            },
            MorphRow {
                name: "Log".into(),
                min: 100.0,
                max: 10_000.0,
                log: true,
                a: 0.0,
                b: 1.0,
                c: 0.0,
                d: 1.0,
                stepped: false,
            },
            // An offset parameter: -1 to 1, 0.5 is no move.
            MorphRow {
                name: "Offset".into(),
                min: -1.0,
                max: 1.0,
                log: false,
                a: 0.5,
                b: 1.0,
                c: 0.5,
                d: 1.0,
                stepped: false,
            },
            MorphRow {
                name: "Choice".into(),
                min: 0.0,
                max: 3.0,
                log: false,
                a: 0.0,
                b: 1.0,
                c: 0.0,
                d: 1.0,
                stepped: true,
            },
        ]
    }

    fn at(position: f32) -> Vec<f32> {
        let mut h = Harness::new(&Morph, &morph_config(&rows()), &[], 48_000.0, 4).unwrap();
        h.set(0, position);
        h.run(4).unwrap();
        (0..4).map(|i| h.output(i).lane(0, 0)[3]).collect()
    }

    #[test]
    fn it_outputs_each_row_at_a_b_and_between() {
        let a = at(0.0);
        let b = at(1.0);
        let mid = at(0.5);
        assert_eq!(a, vec![0.0, 100.0, 0.0, 0.0]);
        assert_eq!(b, vec![10.0, 10_000.0, 1.0, 3.0]);
        assert!((mid[0] - 5.0).abs() < 1e-5);
        // A log parameter is halfway along its taper: the geometric mean.
        assert!((mid[1] - 1000.0).abs() < 0.5, "{}", mid[1]);
        // The offset goes from no move to full.
        assert!((mid[2] - 0.5).abs() < 1e-5);
        // A choice switches at the midpoint.
        assert_eq!(at(0.49)[3], 0.0);
        assert_eq!(mid[3], 3.0);
    }

    #[test]
    fn position_is_read_per_sample() {
        let mut h = Harness::new(&Morph, &morph_config(&rows()), &[], 48_000.0, 4).unwrap();
        h.set(0, 0.0);
        h.run(4).unwrap();
        assert_eq!(h.output(0).lane(0, 0)[0], 0.0);
        h.set(0, 1.0);
        h.run(4).unwrap();
        assert_eq!(h.output(0).lane(0, 0)[0], 10.0);
    }

    #[test]
    fn rows_survive_the_text_form() {
        let r = rows();
        assert_eq!(rows_from_text(&rows_to_text(&r)), r);
        // Bad lines are skipped.
        assert!(rows_from_text("junk\nonly|3").is_empty());
    }

    #[test]
    fn one_output_per_row() {
        let layout = Morph.layout(&morph_config(&rows())).unwrap();
        assert_eq!(layout.outputs.len(), 4);
        assert_eq!(layout.outputs[1].name, "Log");
        assert_eq!(layout.outputs[1].key, "t2");
    }

    #[test]
    fn four_corners_blend_bilinearly() {
        let rows = vec![
            MorphRow {
                name: "Pad".into(),
                min: 0.0,
                max: 100.0,
                log: false,
                a: 0.0,
                b: 0.2,
                c: 0.6,
                d: 1.0,
                stepped: false,
            },
            MorphRow {
                name: "Choice".into(),
                min: 0.0,
                max: 3.0,
                log: false,
                a: 0.0,
                b: 1.0 / 3.0,
                c: 2.0 / 3.0,
                d: 1.0,
                stepped: true,
            },
        ];
        let at = |x: f32, y: f32| -> Vec<f32> {
            let mut h = Harness::new(&Morph, &morph_config(&rows), &[], 48_000.0, 4).unwrap();
            h.set(0, x);
            h.set(1, y);
            h.run(4).unwrap();
            (0..2).map(|i| h.output(i).lane(0, 0)[3]).collect()
        };
        let close = |got: f32, want: f32| assert!((got - want).abs() < 1e-3, "{got} vs {want}");
        close(at(0.0, 0.0)[0], 0.0);
        close(at(1.0, 0.0)[0], 20.0);
        close(at(0.0, 1.0)[0], 60.0);
        close(at(1.0, 1.0)[0], 100.0);
        // The centre is the mean of the four corners.
        close(at(0.5, 0.5)[0], 45.0);
        close(at(0.5, 0.0)[0], 10.0);
        // A choice takes the nearest corner's.
        assert_eq!(
            [
                at(0.0, 0.0)[1],
                at(1.0, 0.0)[1],
                at(0.0, 1.0)[1],
                at(1.0, 1.0)[1]
            ],
            [0.0, 1.0, 2.0, 3.0]
        );
    }

    #[test]
    fn randomise_is_repeatable_and_in_range() {
        let mut one = rows();
        let mut two = rows();
        randomise_corner(&mut one, 1, 7);
        randomise_corner(&mut two, 1, 7);
        assert_eq!(one, two);
        let mut other = rows();
        randomise_corner(&mut other, 1, 8);
        assert_ne!(one, other);
        for row in &one {
            assert!((0.0..=1.0).contains(&row.b), "{row:?}");
        }
        // Only the corner asked for changes; a choice lands on a choice.
        assert_eq!(one[0].a, 0.0);
        let steps = one[3].b * 3.0;
        assert!((steps - steps.round()).abs() < 1e-4, "{steps}");
    }

    #[test]
    fn set_from_current_captures_the_blend() {
        let mut rows = rows();
        let before = rows[0].value(0.25, 0.0);
        set_corner_from_current(&mut rows, 3, 0.25, 0.0);
        assert!((rows[0].value(1.0, 1.0) - before).abs() < 1e-4);
        assert!((rows[1].value(1.0, 1.0) - 100.0 * 100f32.powf(0.25)).abs() < 0.1);
    }

    #[test]
    fn a_two_preset_row_ignores_y() {
        let row = MorphRow::two("x".into(), 0.0, 10.0, false, 0.2, 0.8, false);
        assert!(!row.uses_y());
        assert_eq!(row.value(0.5, 0.0), row.value(0.5, 1.0));
    }
}
