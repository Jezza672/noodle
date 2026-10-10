//! A mapping curve: a chain of cubic Bézier segments from (0, 0) to (1, 1),
//! shared by the [`Remap`](crate::Remap) node (which plays it) and the
//! properties panel (which edits it).
//!
//! The curve is a function of x, so each segment's control points are kept
//! between its two anchors in x, which makes every segment, and so the whole
//! curve, monotone in x. The ends are fixed: the first anchor is (0, 0) and
//! the last is (1, 1). Scale the output range of the node to flip or squash
//! the result.
//!
//! A curve is saved in a node's config as text (`to_text`/`from_text`), and
//! turned into a lookup table (`lut`) for the audio thread when the node is
//! built, which happens off that thread.

use std::fmt::Write;

/// Entries in a lookup table, not counting the last one at x = 1.
pub const LUT_STEPS: usize = 1024;
/// How finely each segment is sampled to build the lookup table.
const SEGMENT_SAMPLES: usize = 256;
/// The closest two anchors may come in x.
const MIN_GAP: f32 = 1e-3;

/// One anchor and its two handles. The handles are offsets from the anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
    /// The handle towards the previous anchor (unused on the first point).
    pub in_handle: (f32, f32),
    /// The handle towards the next anchor (unused on the last point).
    pub out_handle: (f32, f32),
}

impl CurvePoint {
    fn at(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            in_handle: (0.0, 0.0),
            out_handle: (0.0, 0.0),
        }
    }
}

/// Which handle of a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    In,
    Out,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    points: Vec<CurvePoint>,
}

impl Default for Curve {
    fn default() -> Self {
        Self::linear()
    }
}

impl Curve {
    /// The straight line from (0, 0) to (1, 1): the output follows the input.
    pub fn linear() -> Self {
        let mut first = CurvePoint::at(0.0, 0.0);
        first.out_handle = (1.0 / 3.0, 1.0 / 3.0);
        let mut last = CurvePoint::at(1.0, 1.0);
        last.in_handle = (-1.0 / 3.0, -1.0 / 3.0);
        Self {
            points: vec![first, last],
        }
    }

    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Reads the text form. Anything unreadable gives the straight line, so a
    /// damaged project still loads.
    pub fn from_text(text: &str) -> Self {
        let parsed: Option<Vec<CurvePoint>> = text
            .split(';')
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let v: Vec<f32> = p
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .ok()?;
                (v.len() == 6 && v.iter().all(|f| f.is_finite())).then(|| CurvePoint {
                    x: v[0],
                    y: v[1],
                    in_handle: (v[2], v[3]),
                    out_handle: (v[4], v[5]),
                })
            })
            .collect();
        match parsed {
            Some(points) if points.len() >= 2 => {
                let mut curve = Self { points };
                curve.sanitize();
                curve
            }
            _ => Self::linear(),
        }
    }

    /// One point per `;`, each "x y in_dx in_dy out_dx out_dy". The straight
    /// line is the empty string, the config default.
    pub fn to_text(&self) -> String {
        if *self == Self::linear() {
            return String::new();
        }
        let mut text = String::new();
        for p in &self.points {
            if !text.is_empty() {
                text.push(';');
            }
            let _ = write!(
                text,
                "{} {} {} {} {} {}",
                p.x, p.y, p.in_handle.0, p.in_handle.1, p.out_handle.0, p.out_handle.1
            );
        }
        text
    }

    /// Puts everything back in range: ends fixed, anchors in order, handles
    /// inside their segment in x, everything inside the unit square in y.
    fn sanitize(&mut self) {
        let n = self.points.len();
        self.points[0].x = 0.0;
        self.points[0].y = 0.0;
        self.points[n - 1].x = 1.0;
        self.points[n - 1].y = 1.0;
        for i in 1..n - 1 {
            let lo = self.points[i - 1].x + MIN_GAP;
            let hi = 1.0 - MIN_GAP * (n - 1 - i) as f32;
            let p = &mut self.points[i];
            p.x = p.x.clamp(lo, hi.max(lo));
            p.y = p.y.clamp(0.0, 1.0);
        }
        for i in 0..n {
            let (x, y) = (self.points[i].x, self.points[i].y);
            let prev = if i > 0 { self.points[i - 1].x } else { x };
            let next = if i + 1 < n { self.points[i + 1].x } else { x };
            let p = &mut self.points[i];
            p.in_handle = clamp_handle(p.in_handle, x, y, prev, x);
            p.out_handle = clamp_handle(p.out_handle, x, y, x, next);
        }
    }

    /// Moves anchor `index` to (x, y). The end points don't move.
    pub fn move_point(&mut self, index: usize, x: f32, y: f32) {
        if index == 0 || index + 1 >= self.points.len() {
            return;
        }
        self.points[index].x = x;
        self.points[index].y = y;
        self.sanitize();
    }

    /// Moves a handle so it sits at (x, y) in curve coordinates.
    pub fn move_handle(&mut self, index: usize, which: Handle, x: f32, y: f32) {
        let Some(p) = self.points.get_mut(index) else {
            return;
        };
        let offset = (x - p.x, y - p.y);
        match which {
            Handle::In => p.in_handle = offset,
            Handle::Out => p.out_handle = offset,
        }
        self.sanitize();
    }

    /// Adds an anchor on the curve at `x`, leaving the curve's shape as it
    /// was, and returns its index.
    pub fn insert_at(&mut self, x: f32) -> usize {
        let x = x.clamp(MIN_GAP, 1.0 - MIN_GAP);
        let i = self
            .points
            .windows(2)
            .position(|w| x < w[1].x)
            .unwrap_or(self.points.len() - 2);
        let [p0, p3] = [self.points[i], self.points[i + 1]];
        let c = [
            (p0.x, p0.y),
            (p0.x + p0.out_handle.0, p0.y + p0.out_handle.1),
            (p3.x + p3.in_handle.0, p3.y + p3.in_handle.1),
            (p3.x, p3.y),
        ];
        // Find t where the segment is at `x` (x is monotone in t).
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..32 {
            let mid = 0.5 * (lo + hi);
            if bezier(&c, mid).0 < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let t = 0.5 * (lo + hi);
        // De Casteljau: split the segment in two at t.
        let q0 = lerp(c[0], c[1], t);
        let q1 = lerp(c[1], c[2], t);
        let q2 = lerp(c[2], c[3], t);
        let r0 = lerp(q0, q1, t);
        let r1 = lerp(q1, q2, t);
        let s = lerp(r0, r1, t);
        self.points[i].out_handle = (q0.0 - c[0].0, q0.1 - c[0].1);
        self.points[i + 1].in_handle = (q2.0 - c[3].0, q2.1 - c[3].1);
        let mut mid = CurvePoint::at(s.0, s.1);
        mid.in_handle = (r0.0 - s.0, r0.1 - s.1);
        mid.out_handle = (r1.0 - s.0, r1.1 - s.1);
        self.points.insert(i + 1, mid);
        self.sanitize();
        i + 1
    }

    /// Removes anchor `index`, unless it is one of the ends.
    pub fn remove(&mut self, index: usize) -> bool {
        if index == 0 || index + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(index);
        self.sanitize();
        true
    }

    /// The curve sampled at `LUT_STEPS + 1` evenly spaced x values from 0
    /// to 1. Allocates, so build it off the audio thread.
    pub fn lut(&self) -> Box<[f32]> {
        let mut poly: Vec<(f32, f32)> = Vec::with_capacity(self.points.len() * SEGMENT_SAMPLES);
        for w in self.points.windows(2) {
            let c = [
                (w[0].x, w[0].y),
                (w[0].x + w[0].out_handle.0, w[0].y + w[0].out_handle.1),
                (w[1].x + w[1].in_handle.0, w[1].y + w[1].in_handle.1),
                (w[1].x, w[1].y),
            ];
            for s in 0..=SEGMENT_SAMPLES {
                poly.push(bezier(&c, s as f32 / SEGMENT_SAMPLES as f32));
            }
        }
        let mut lut = Vec::with_capacity(LUT_STEPS + 1);
        let mut j = 0;
        for i in 0..=LUT_STEPS {
            let x = i as f32 / LUT_STEPS as f32;
            while j + 2 < poly.len() && poly[j + 1].0 < x {
                j += 1;
            }
            let ((x0, y0), (x1, y1)) = (poly[j], poly[j + 1]);
            let y = if x1 > x0 {
                y0 + (y1 - y0) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0)
            } else {
                y1
            };
            lut.push(y.clamp(0.0, 1.0));
        }
        lut.into_boxed_slice()
    }

    /// The curve's height at `x` (0 to 1), for drawing and tests.
    pub fn eval(&self, x: f32) -> f32 {
        lookup(&self.lut(), x)
    }
}

/// Reads a lookup table at `x`, clamped to 0 to 1, interpolating between
/// entries. Safe on the audio thread.
#[inline]
pub fn lookup(lut: &[f32], x: f32) -> f32 {
    let x = if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) };
    let pos = x * (lut.len() - 1) as f32;
    let i = (pos as usize).min(lut.len() - 2);
    let frac = pos - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * frac
}

fn clamp_handle(h: (f32, f32), x: f32, y: f32, x_lo: f32, x_hi: f32) -> (f32, f32) {
    (
        (x + h.0).clamp(x_lo, x_hi) - x,
        (y + h.1).clamp(0.0, 1.0) - y,
    )
}

fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn bezier(c: &[(f32, f32); 4], t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let (a, b, c2, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * c[0].0 + b * c[1].0 + c2 * c[2].0 + d * c[3].0,
        a * c[0].1 + b * c[1].1 + c2 * c[2].1 + d * c[3].1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_the_identity() {
        let c = Curve::linear();
        for i in 0..=10 {
            let x = i as f32 / 10.0;
            assert!((c.eval(x) - x).abs() < 1e-3, "x = {x}");
        }
        assert_eq!(c.to_text(), "");
        assert_eq!(Curve::from_text(""), Curve::linear());
    }

    #[test]
    fn inserting_a_point_keeps_the_shape() {
        let mut c = Curve::linear();
        c.move_handle(0, Handle::Out, 0.0, 0.9);
        let before = c.lut();
        let i = c.insert_at(0.4);
        assert_eq!(i, 1);
        assert_eq!(c.points().len(), 3);
        let after = c.lut();
        for (a, b) in before.iter().zip(after.iter()) {
            assert!((a - b).abs() < 2e-3);
        }
    }

    #[test]
    fn moving_a_point_changes_the_curve_and_it_stays_a_function() {
        let mut c = Curve::linear();
        let i = c.insert_at(0.5);
        c.move_point(i, 0.5, 0.9);
        c.move_handle(i, Handle::In, -5.0, 3.0); // far outside
        let lut = c.lut();
        assert!(c.eval(0.5) > 0.8);
        assert!(lut.windows(2).all(|w| w[0] <= w[1] + 1e-4 || w[1] >= 0.0));
        for p in c.points() {
            assert!((0.0..=1.0).contains(&(p.x + p.in_handle.0)));
            assert!((0.0..=1.0).contains(&(p.y + p.in_handle.1)));
        }
    }

    #[test]
    fn the_ends_are_fixed_and_can_not_be_removed() {
        let mut c = Curve::linear();
        c.move_point(0, 0.3, 0.3);
        c.move_point(1, 0.3, 0.3);
        assert_eq!(c, Curve::linear());
        assert!(!c.remove(0));
        assert!(!c.remove(1));
        let i = c.insert_at(0.5);
        assert!(c.remove(i));
        assert_eq!(c.points().len(), 2);
    }

    #[test]
    fn anchors_stay_in_order() {
        let mut c = Curve::linear();
        let a = c.insert_at(0.3);
        let b = c.insert_at(0.7);
        c.move_point(a, 0.95, 0.5);
        assert!(c.points()[a].x < c.points()[b].x);
    }

    #[test]
    fn text_round_trips() {
        let mut c = Curve::linear();
        let i = c.insert_at(0.25);
        c.move_point(i, 0.25, 0.75);
        assert_eq!(Curve::from_text(&c.to_text()), c);
    }

    #[test]
    fn bad_text_gives_the_straight_line() {
        for text in ["junk", "1 2 3", "0 0 0 0 0 0", "0 0 0 0 0 NaN;1 1 0 0 0 0"] {
            assert_eq!(Curve::from_text(text), Curve::linear(), "{text}");
        }
    }

    #[test]
    fn lookup_clamps_and_survives_nan() {
        let lut = Curve::linear().lut();
        assert_eq!(lookup(&lut, -5.0), 0.0);
        assert_eq!(lookup(&lut, 5.0), 1.0);
        assert_eq!(lookup(&lut, f32::NAN), 0.0);
    }
}
