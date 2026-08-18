//! The camera: where the view sits, the world⇄screen transform, canvas
//! rect-change compensation, and the frame glide.
//!
//! Two invariants live here, both regression-tested below:
//!
//! - The world is anchored to the canvas-rect CENTER. [`Camera::compensate`]
//!   shifts `center` whenever the rect changes (side panel open/close/
//!   resize, window resize) so every world point keeps its screen position
//!   and only the visible clip changes — without it the scene slides out
//!   from under the cursor: the node just clicked escapes, and the second
//!   click of a double-click misses.
//! - A glide moves only the CENTER — zoom stays exactly what it was (no
//!   snap, no zoom floor) — and manual camera input wins: every pan/zoom
//!   site cancels it through [`Camera::cancel_glide`]. The glide target is
//!   a NODE, not a point, so a settling sim can't make it land beside the
//!   node; the easing itself runs in `canvas()`, which is the one place
//!   that knows the node's current world position and the finder lift.

use std::time::Instant;

use eframe::egui::{Pos2, Rect};
use text_graph::graph::NodeId;

pub(super) struct Camera {
    /// World point currently at the viewport center.
    pub(super) center: Pos2,
    /// Screen pixels per world unit.
    pub(super) zoom: f32,
    /// In-flight glide: (start center, target node, start time).
    pub(super) anim: Option<(Pos2, NodeId, Instant)>,
    /// The first whole-graph fit has happened. Cleared by `gg` to ask for
    /// a re-fit on the next frame (the fit needs the canvas rect, which
    /// only `canvas()` knows).
    pub(super) fitted: bool,
    /// Canvas rect of the previous frame — what `compensate` diffs against.
    pub(super) last_rect: Option<Rect>,
}

impl Camera {
    pub(super) fn new() -> Self {
        Camera {
            center: Pos2::ZERO,
            zoom: 1.0,
            anim: None,
            fitted: false,
            last_rect: None,
        }
    }

    pub(super) fn to_screen(&self, rect: Rect, w: Pos2) -> Pos2 {
        rect.center() + (w - self.center) * self.zoom
    }

    pub(super) fn to_world(&self, rect: Rect, s: Pos2) -> Pos2 {
        self.center + (s - rect.center()) / self.zoom
    }

    /// The canvas rect moved (panel toggled, window resized): shift the
    /// camera by the same amount so every world point keeps its screen
    /// position. Call once per frame, before anything reads the transform.
    pub(super) fn compensate(&mut self, rect: Rect) {
        if let Some(last) = self.last_rect
            && last.center() != rect.center()
        {
            self.center += (rect.center() - last.center()) / self.zoom;
        }
        self.last_rect = Some(rect);
    }

    /// Start gliding the center onto `id` — zoom stays exactly as it is,
    /// and the quick movement (instead of a snap) shows where the jump
    /// came from and where it lands.
    pub(super) fn start_glide(&mut self, id: NodeId) {
        self.anim = Some((self.center, id, Instant::now()));
    }

    /// Manual camera input wins over a glide — every site that pans or
    /// zooms by hand goes through this, so the rule is greppable.
    pub(super) fn cancel_glide(&mut self) {
        self.anim = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Vec2, pos2};

    fn cam(center: Pos2, zoom: f32) -> Camera {
        Camera {
            center,
            zoom,
            ..Camera::new()
        }
    }

    #[test]
    fn world_and_screen_are_inverses() {
        let c = cam(pos2(13.0, -4.0), 2.5);
        let rect = Rect::from_min_size(pos2(100.0, 50.0), Vec2::new(640.0, 480.0));
        let w = pos2(-7.25, 19.5);
        let s = c.to_screen(rect, w);
        let back = c.to_world(rect, s);
        assert!((back - w).length() < 1e-4, "{back:?} != {w:?}");
    }

    /// The no-slide rule: after the rect moves (panel opened, window
    /// resized), every world point must keep its exact screen position —
    /// only the visible clip may change.
    #[test]
    fn compensation_keeps_world_points_on_screen() {
        let mut c = cam(pos2(3.0, 8.0), 1.7);
        let before = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(800.0, 600.0));
        c.compensate(before); // primes last_rect, like the first frame
        let w = pos2(42.0, -3.0);
        let s_before = c.to_screen(before, w);
        // side pane opens: the canvas keeps its left edge and loses width
        let after = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(500.0, 600.0));
        c.compensate(after);
        let s_after = c.to_screen(after, w);
        assert!(
            (s_after - s_before).length() < 1e-3,
            "world point slid on screen: {s_before:?} -> {s_after:?}"
        );
    }

    #[test]
    fn a_glide_is_cancelled_by_manual_input_and_never_touches_zoom() {
        let mut c = cam(pos2(0.0, 0.0), 3.3);
        c.start_glide(NodeId(7));
        assert!(c.anim.is_some());
        assert_eq!(c.zoom, 3.3, "starting a glide leaves zoom alone");
        c.cancel_glide();
        assert!(c.anim.is_none(), "manual camera input wins");
    }
}
