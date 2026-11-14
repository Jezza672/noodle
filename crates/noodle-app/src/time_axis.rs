//! One horizontal time axis for every view that draws time left to right: the
//! arrangement (seconds), the piano roll (quarter notes) and the clip editor
//! (seconds of the file, or beats of the warp).
//!
//! The views differ in the unit they count in and in how a tick or a beat
//! becomes that unit (a tempo map, a fixed 960 ticks to the quarter, a warp).
//! What they share is the line from unit to screen: a point where the unit's
//! zero is, and a number of points per unit. That, and zooming about the
//! pointer, is here so that the views cannot drift apart.

/// Where a unit of time is on the screen: the screen x of unit 0 and the
/// screen points per unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeAxis {
    pub origin: f32,
    pub scale: f32,
}

impl TimeAxis {
    pub fn new(origin: f32, scale: f32) -> Self {
        Self { origin, scale }
    }

    /// The screen x of `unit`.
    pub fn x(self, unit: f64) -> f32 {
        self.origin + (unit * f64::from(self.scale)) as f32
    }

    /// The unit at screen x.
    pub fn at(self, x: f32) -> f64 {
        f64::from(x - self.origin) / f64::from(self.scale)
    }
}

/// Zooms by `factor` keeping the unit at `anchor` (points from the pane's
/// left edge) where it is: `scale` is clamped to `limits` and `scroll_x`
/// follows. This is how a pinch or Ctrl-scroll zooms every time axis.
pub fn zoom_about(
    scale: &mut f32,
    scroll_x: &mut f32,
    factor: f32,
    anchor: f32,
    limits: (f32, f32),
) {
    let unit = (anchor + *scroll_x) / *scale;
    *scale = (*scale * factor).clamp(limits.0, limits.1);
    *scroll_x = unit * *scale - anchor;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_and_at_are_inverses() {
        let axis = TimeAxis::new(60.0, 50.0);
        assert_eq!(axis.x(0.0), 60.0);
        assert_eq!(axis.x(2.0), 160.0);
        assert!((axis.at(axis.x(3.25)) - 3.25).abs() < 1e-5);
    }

    #[test]
    fn zooming_keeps_the_unit_under_the_pointer_still() {
        let (mut scale, mut scroll) = (60.0, 30.0);
        let before = TimeAxis::new(-scroll, scale);
        let unit = before.at(200.0);
        zoom_about(&mut scale, &mut scroll, 2.0, 200.0, (1.0, 1000.0));
        assert_eq!(scale, 120.0);
        let after = TimeAxis::new(-scroll, scale);
        assert!((after.x(unit) - 200.0).abs() < 1e-3);
    }

    #[test]
    fn zoom_stops_at_its_limits() {
        let (mut scale, mut scroll) = (60.0, 0.0);
        zoom_about(&mut scale, &mut scroll, 100.0, 10.0, (1.0, 100.0));
        assert_eq!(scale, 100.0);
    }
}
