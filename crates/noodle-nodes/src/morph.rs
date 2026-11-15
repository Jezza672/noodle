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
    /// Positions from 0 to 1 along `min` to `max`.
    pub a: f32,
    pub b: f32,
    /// Switch at the midpoint rather than blend.
    pub stepped: bool,
}

impl MorphRow {
    /// The value at `position` (0 to 1).
    pub fn value(&self, position: f32) -> f32 {
        let t = if self.stepped {
            if position < 0.5 { self.a } else { self.b }
        } else {
            self.a + (self.b - self.a) * position
        };
        scale(t.clamp(0.0, 1.0), self.min, self.max, self.log)
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
                "{name}|{}|{}|{}|{}|{}|{}",
                r.min, r.max, r.log as u8, r.a, r.b, r.stepped as u8
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
            if f.len() != 7 {
                return None;
            }
            Some(MorphRow {
                name: f[0].to_owned(),
                min: f[1].parse().ok()?,
                max: f[2].parse().ok()?,
                log: f[3] == "1",
                a: f[4].parse().ok()?,
                b: f[5].parse().ok()?,
                stepped: f[6] == "1",
            })
        })
        .collect()
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
        let mut layout = Layout::realtime().param(
            MORPH_POSITION,
            "Position",
            ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0).featured(),
        );
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
        for (row, out) in self.rows.iter().zip(outputs.iter_mut()) {
            let out = out.lane_mut(0, 0);
            for (o, &p) in out.iter_mut().zip(position) {
                *o = row.value(if p.is_finite() {
                    p.clamp(0.0, 1.0)
                } else {
                    0.0
                });
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
                stepped: false,
            },
            MorphRow {
                name: "Log".into(),
                min: 100.0,
                max: 10_000.0,
                log: true,
                a: 0.0,
                b: 1.0,
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
                stepped: false,
            },
            MorphRow {
                name: "Choice".into(),
                min: 0.0,
                max: 3.0,
                log: false,
                a: 0.0,
                b: 1.0,
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
}
