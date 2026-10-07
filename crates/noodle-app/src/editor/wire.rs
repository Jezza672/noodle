//! Wire curves, and the geometry for hovering, cutting and rerouting them.

use egui::{Pos2, Vec2};

/// How many straight segments a wire is split into for hit testing.
const SEGMENTS: usize = 24;

/// The control points of the curve from an output at `from` to an input at
/// `to`. Wires leave and enter ports horizontally, like Blender's.
pub fn curve(from: Pos2, to: Pos2) -> [Pos2; 4] {
    let reach = ((to.x - from.x).abs() * 0.5).max(40.0);
    [
        from,
        from + Vec2::new(reach, 0.0),
        to - Vec2::new(reach, 0.0),
        to,
    ]
}

/// Points along a cubic Bézier curve, ends included.
pub fn flatten(points: [Pos2; 4]) -> Vec<Pos2> {
    let [p0, p1, p2, p3] = points.map(Pos2::to_vec2);
    (0..=SEGMENTS)
        .map(|i| {
            let t = i as f32 / SEGMENTS as f32;
            let u = 1.0 - t;
            (p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t))
                .to_pos2()
        })
        .collect()
}

pub fn distance_to_polyline(p: Pos2, line: &[Pos2]) -> f32 {
    line.windows(2)
        .map(|w| distance_to_segment(p, w[0], w[1]))
        .fold(f32::INFINITY, f32::min)
}

fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 {
        ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + ab * t).distance(p)
}

/// Where segment `a`–`b` crosses segment `c`–`d`, if it does.
pub fn segments_cross(a: Pos2, b: Pos2, c: Pos2, d: Pos2) -> Option<Pos2> {
    let r = b - a;
    let s = d - c;
    let denominator = r.x * s.y - r.y * s.x;
    if denominator.abs() < 1e-9 {
        return None; // parallel
    }
    let ac = c - a;
    let t = (ac.x * s.y - ac.y * s.x) / denominator;
    let u = (ac.x * r.y - ac.y * r.x) / denominator;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then(|| a + r * t)
}

/// The first point along `wire` where `stroke` crosses it.
pub fn first_crossing(wire: &[Pos2], stroke: &[Pos2]) -> Option<Pos2> {
    wire.windows(2).find_map(|w| {
        stroke
            .windows(2)
            .find_map(|s| segments_cross(w[0], w[1], s[0], s[1]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curve_starts_and_ends_at_its_ports() {
        let line = flatten(curve(Pos2::new(0.0, 0.0), Pos2::new(200.0, 80.0)));
        assert_eq!(line.first(), Some(&Pos2::new(0.0, 0.0)));
        assert!((*line.last().unwrap() - Pos2::new(200.0, 80.0)).length() < 1e-3);
        // Symmetric control points put the middle halfway.
        assert!((line[SEGMENTS / 2] - Pos2::new(100.0, 40.0)).length() < 1e-3);
    }

    #[test]
    fn distance_to_a_polyline() {
        let line = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
        ];
        assert_eq!(distance_to_polyline(Pos2::new(5.0, 3.0), &line), 3.0);
        assert_eq!(distance_to_polyline(Pos2::new(14.0, 5.0), &line), 4.0);
        assert_eq!(distance_to_polyline(Pos2::new(-3.0, -4.0), &line), 5.0);
    }

    #[test]
    fn crossing_segments() {
        let hit = segments_cross(
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
            Pos2::new(10.0, 0.0),
        );
        assert_eq!(hit, Some(Pos2::new(5.0, 5.0)));
        // Lines that would cross if they were longer.
        let miss = segments_cross(
            Pos2::new(0.0, 0.0),
            Pos2::new(4.0, 4.0),
            Pos2::new(0.0, 10.0),
            Pos2::new(10.0, 0.0),
        );
        assert_eq!(miss, None);
        let parallel = segments_cross(
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(0.0, 1.0),
            Pos2::new(10.0, 1.0),
        );
        assert_eq!(parallel, None);
    }

    #[test]
    fn a_stroke_across_a_wire_crosses_it() {
        let wire = flatten(curve(Pos2::new(0.0, 0.0), Pos2::new(200.0, 0.0)));
        let across = [Pos2::new(100.0, -50.0), Pos2::new(100.0, 50.0)];
        let hit = first_crossing(&wire, &across).unwrap();
        assert!((hit - Pos2::new(100.0, 0.0)).length() < 1e-3);
        let beside = [Pos2::new(250.0, -50.0), Pos2::new(250.0, 50.0)];
        assert_eq!(first_crossing(&wire, &beside), None);
    }
}
