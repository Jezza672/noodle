//! What the Wavetable and Grains nodes show in the properties panel: the cycle
//! at the node's `position`, or the sample with the grains' position, spray
//! and live grains drawn on it.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, vec2};
use noodle_core::{Node, NodeId};
use noodle_nodes::{
    CYCLE_KEY, DEFAULT_CYCLE, GRAINS_ID, GRAINS_SAMPLE_KEY, Sample, SampleRef, TABLE_KEY,
    WAVETABLE_ID,
};

use crate::sample_view::{self, Region};
use crate::session::Session;
use crate::theme::{self, timeline as colors};

/// The config key that names the sample of a node of this type, if the type is
/// one of the two these displays are for.
pub fn sample_key(type_id: &str) -> Option<&'static str> {
    match type_id {
        GRAINS_ID => Some(GRAINS_SAMPLE_KEY),
        WAVETABLE_ID => Some(TABLE_KEY),
        _ => None,
    }
}

fn param(node: &Node, key: &str) -> f32 {
    node.params.get(key).copied().unwrap_or(0.0)
}

/// The samples of the cycle at `position` (0 to 1) of a table cut every `n`
/// samples, `points` of them evenly spaced across the cycle. Neighbouring
/// cycles are blended as the node blends them; a file shorter than `n` is one
/// cycle, stretched.
pub fn cycle_points(sample: &Sample, n: usize, position: f32, points: usize) -> Vec<f32> {
    let mono: Vec<f32> = sample
        .left
        .iter()
        .zip(sample.right.iter())
        .map(|(l, r)| (l + r) * 0.5)
        .collect();
    if mono.is_empty() || n == 0 || points == 0 {
        return vec![0.0; points];
    }
    let (cycles, length) = if mono.len() >= n {
        (mono.len() / n, n)
    } else {
        (1, mono.len())
    };
    let travel = position.clamp(0.0, 1.0) * (cycles - 1) as f32;
    let first = (travel as usize).min(cycles - 1);
    let next = (first + 1).min(cycles - 1);
    let across = travel - first as f32;
    let at = |cycle: usize, x: f32| {
        let i = (x * length as f32) as usize % length;
        mono[cycle * length + i.min(length - 1)]
    };
    (0..points)
        .map(|p| {
            let x = p as f32 / points as f32;
            let a = at(first, x);
            a + (at(next, x) - a) * across
        })
        .collect()
}

/// The display for the Wavetable or Grains node `id`, if it has a sample.
pub fn preview(ui: &mut Ui, session: &Session, id: NodeId) {
    let Some(node) = session.project().graph().node(id) else {
        return;
    };
    let Some(key) = sample_key(&node.type_id) else {
        return;
    };
    let text = match node.config.get(key) {
        Some(noodle_core::Value::Text(text)) => text.clone(),
        _ => String::new(),
    };
    let reference = SampleRef::parse(&text);
    let Some(sample) = session
        .samples()
        .get(&reference.hash)
        .map(|s| s.window(reference.start, reference.end))
    else {
        return;
    };
    let (rect, response) = ui.allocate_exact_size(
        vec2(
            ui.available_width(),
            if node.type_id == GRAINS_ID {
                64.0
            } else {
                80.0
            },
        ),
        Sense::hover(),
    );
    if node.type_id == GRAINS_ID {
        let whole = Region {
            start: 0.0,
            end: 1.0,
            looping: None,
        };
        sample_view::draw_preview(ui, rect, &reference.to_text(), &sample, whole);
        let painter = ui.painter_at(rect);
        let x = |fraction: f32| rect.left() + fraction.clamp(0.0, 1.0) * rect.width();
        let (position, spray) = (param(node, "position"), param(node, "spray"));
        // The spray: where grains may start, as a band around the position.
        painter.rect_filled(
            Rect::from_x_y_ranges(
                x(position - spray * 0.5)..=x(position + spray * 0.5),
                rect.y_range(),
            ),
            0.0,
            theme::ACCENT.gamma_multiply(0.25),
        );
        painter.line_segment(
            [
                Pos2::new(x(position), rect.top()),
                Pos2::new(x(position), rect.bottom()),
            ],
            Stroke::new(1.5, Color32::WHITE),
        );
        // The grains playing now.
        for at in session.telemetry().read_positions(id) {
            painter.line_segment(
                [
                    Pos2::new(x(at), rect.bottom() - 10.0),
                    Pos2::new(x(at), rect.bottom()),
                ],
                Stroke::new(2.0, colors::PLAYHEAD),
            );
        }
        response.on_hover_text(
            "The sample, the position (line), the spray (band) and the grains playing",
        );
    } else {
        let n = match node.config.get(CYCLE_KEY) {
            Some(noodle_core::Value::Int(n)) if *n > 0 => *n as usize,
            _ => DEFAULT_CYCLE,
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, colors::BACKGROUND);
        let points = (rect.width() as usize).clamp(16, 512);
        let (middle, half) = (rect.center().y, rect.height() / 2.0 - 3.0);
        let line = |values: &[f32], stroke: Stroke| {
            let shape: Vec<Pos2> = values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    Pos2::new(
                        rect.left() + i as f32 / (values.len() - 1).max(1) as f32 * rect.width(),
                        middle - v.clamp(-1.0, 1.0) * half,
                    )
                })
                .collect();
            painter.add(egui::Shape::line(shape, stroke));
        };
        // The stack faintly, the cycle in force brightly.
        for step in 0..=8 {
            let faint = cycle_points(&sample, n, step as f32 / 8.0, points);
            line(&faint, Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.15)));
        }
        let now = cycle_points(&sample, n, param(node, "position"), points);
        line(&now, Stroke::new(1.5, theme::ACCENT));
        response.on_hover_text("The cycle at the position");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(data: Vec<f32>) -> Sample {
        Sample {
            left: data.clone().into(),
            right: data.into(),
            sample_rate: 48_000,
        }
    }

    #[test]
    fn the_cycle_blends_neighbours_and_ends_on_the_last() {
        let mut data = vec![0.0; 8];
        data.extend(vec![1.0; 8]);
        data.extend(vec![-1.0; 8]);
        let s = sample(data);
        assert_eq!(cycle_points(&s, 8, 0.0, 4), [0.0; 4]);
        assert_eq!(cycle_points(&s, 8, 0.25, 4), [0.5; 4]);
        assert_eq!(cycle_points(&s, 8, 0.5, 4), [1.0; 4]);
        assert_eq!(cycle_points(&s, 8, 1.0, 4), [-1.0; 4]);
    }

    #[test]
    fn a_short_file_is_one_cycle_and_an_empty_one_is_flat() {
        let s = sample(vec![0.0, 1.0, 0.0, -1.0]);
        let points = cycle_points(&s, 2048, 0.7, 4);
        assert_eq!(points, [0.0, 1.0, 0.0, -1.0]);
        assert_eq!(cycle_points(&sample(Vec::new()), 2048, 0.0, 3), [0.0; 3]);
    }
}
