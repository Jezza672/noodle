//! Panning and zooming: the mapping between graph coordinates, where node
//! positions live, and screen points.

use egui::{Pos2, Rect, Vec2};

pub const MIN_ZOOM: f32 = 0.2;
pub const MAX_ZOOM: f32 = 3.0;

/// The part of the graph the editor shows. A graph point `p` is drawn at
/// `canvas.min + (p + offset) * zoom`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub offset: Vec2,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            offset: Vec2::splat(40.0),
            zoom: 1.0,
        }
    }
}

/// A [`View`] placed on a canvas, for converting points during one frame.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    origin: Pos2,
    offset: Vec2,
    pub zoom: f32,
}

impl View {
    pub fn on(&self, canvas: Rect) -> Transform {
        Transform {
            origin: canvas.min,
            offset: self.offset,
            zoom: self.zoom,
        }
    }

    /// Moves the graph by `delta` screen points.
    pub fn pan(&mut self, delta: Vec2) {
        self.offset += delta / self.zoom;
    }

    /// Zooms by `factor`, keeping the graph point under `anchor` (a screen
    /// point) where it is.
    pub fn zoom_around(&mut self, canvas: Rect, anchor: Pos2, factor: f32) {
        let before = self.on(canvas).to_graph(anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.on(canvas).to_graph(anchor);
        self.offset += after - before;
    }

    /// Centres `bounds` (in graph coordinates) on the canvas, zoomed to fit
    /// but never past 1.
    pub fn fit(&mut self, canvas: Rect, bounds: Rect) {
        let margin = 40.0;
        let available = (canvas.size() - Vec2::splat(2.0 * margin)).max(Vec2::splat(1.0));
        let size = bounds.size().max(Vec2::splat(1.0));
        self.zoom = (available.x / size.x)
            .min(available.y / size.y)
            .clamp(MIN_ZOOM, 1.0);
        self.offset = canvas.size() / (2.0 * self.zoom) - bounds.center().to_vec2();
    }
}

impl Transform {
    pub fn to_screen(self, p: Pos2) -> Pos2 {
        self.origin + (p.to_vec2() + self.offset) * self.zoom
    }

    pub fn to_graph(self, p: Pos2) -> Pos2 {
        ((p - self.origin) / self.zoom - self.offset).to_pos2()
    }

    pub fn rect_to_screen(self, r: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(r.min), self.to_screen(r.max))
    }

    /// A length in graph units, in screen points.
    pub fn scale(self, length: f32) -> f32 {
        length * self.zoom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0))
    }

    #[test]
    fn screen_and_graph_points_round_trip() {
        let view = View {
            offset: Vec2::new(-30.0, 12.0),
            zoom: 1.7,
        };
        let t = view.on(canvas());
        let p = Pos2::new(250.0, -75.0);
        assert!((t.to_graph(t.to_screen(p)) - p).length() < 1e-4);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let mut view = View::default();
        let anchor = Pos2::new(400.0, 300.0);
        let under = view.on(canvas()).to_graph(anchor);
        view.zoom_around(canvas(), anchor, 1.5);
        assert_eq!(view.zoom, 1.5);
        assert!((view.on(canvas()).to_screen(under) - anchor).length() < 1e-3);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut view = View::default();
        view.zoom_around(canvas(), Pos2::ZERO, 100.0);
        assert_eq!(view.zoom, MAX_ZOOM);
        view.zoom_around(canvas(), Pos2::ZERO, 1e-6);
        assert_eq!(view.zoom, MIN_ZOOM);
    }

    #[test]
    fn fitting_centres_the_bounds() {
        let mut view = View::default();
        let bounds = Rect::from_min_size(Pos2::new(1000.0, 1000.0), Vec2::new(2000.0, 100.0));
        view.fit(canvas(), bounds);
        assert!(view.zoom < 1.0);
        let t = view.on(canvas());
        assert!((t.to_screen(bounds.center()) - canvas().center()).length() < 1e-3);
        assert!(canvas().contains_rect(t.rect_to_screen(bounds)));
    }
}
