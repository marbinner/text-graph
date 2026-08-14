//! The egui shell: viewport transform, input, painting. All geometry comes
//! from `layout` (pure); this module owns presentation and interaction only.
//!
//! One world→screen transform (`to_screen`/`to_world`) — every input handler
//! and paint call goes through it. Zoom is toward the cursor, not the
//! viewport center.

use std::path::Path;
use std::process::ExitCode;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use text_graph::graph::{Graph, NodeId, NodeKind};
use text_graph::{graph, layout, vault};

// ---- palette (dark) ----
const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
const EDGE: Color32 = Color32::from_rgb(0x2e, 0x33, 0x3d);
const DIR: Color32 = Color32::from_rgb(0x7a, 0xa2, 0xf7);
const FILE: Color32 = Color32::from_rgb(0xb8, 0xbc, 0xc8);
const HOVER: Color32 = Color32::from_rgb(0xff, 0xb4, 0x54);
const SELECT: Color32 = Color32::from_rgb(0xff, 0x8a, 0x3d);
const WIKI: Color32 = Color32::from_rgb(0xe0, 0xaf, 0x68);
const TEXT: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xac);

pub fn run(path: &Path) -> ExitCode {
    let scan = match vault::scan(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let viewer = Viewer::new(graph::build(scan));
    let title = format!("text-graph — {}", viewer.g.node(viewer.g.root).name);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_title(&title),
        ..Default::default()
    };
    match eframe::run_native(&title, options, Box::new(move |_cc| Ok(Box::new(viewer)))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Viewer {
    g: Graph,
    /// World positions indexed by NodeId; ghosts None (unrendered in B).
    pos: Vec<Option<Pos2>>,
    /// World point currently at the viewport center.
    center: Pos2,
    /// Screen pixels per world unit.
    zoom: f32,
    hover: Option<NodeId>,
    selected: Option<NodeId>,
    fitted: bool,
    n_files: usize,
    n_dirs: usize,
}

impl Viewer {
    fn new(g: Graph) -> Self {
        let pos = layout::radial(&g)
            .into_iter()
            .map(|p| p.map(|p| Pos2::new(p.x, p.y)))
            .collect();
        let n_files = g.nodes.iter().filter(|n| n.kind == NodeKind::File).count();
        let n_dirs = g.nodes.iter().filter(|n| n.kind == NodeKind::Dir).count();
        Self {
            g,
            pos,
            center: Pos2::ZERO,
            zoom: 1.0,
            hover: None,
            selected: None,
            fitted: false,
            n_files,
            n_dirs,
        }
    }

    /// Frame the whole graph (first paint only — rect is unknown before then).
    fn fit(&mut self, rect: Rect) {
        let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
        let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for p in self.pos.iter().flatten() {
            min = min.min(*p);
            max = max.max(*p);
        }
        if !min.x.is_finite() {
            return;
        }
        let size = (max - min).max(Vec2::splat(1.0));
        self.center = Pos2::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5);
        self.zoom = ((rect.width() / size.x).min(rect.height() / size.y) * 0.9).clamp(0.02, 50.0);
    }

    fn to_screen(&self, rect: Rect, w: Pos2) -> Pos2 {
        rect.center() + (w - self.center) * self.zoom
    }

    fn to_world(&self, rect: Rect, s: Pos2) -> Pos2 {
        self.center + (s - rect.center()) / self.zoom
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if !self.fitted {
            self.fit(rect);
            self.fitted = true;
        }

        // ---- input ----
        if response.dragged() {
            self.center -= response.drag_delta() / self.zoom;
        }
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = pinch * (scroll * 0.0025).exp();
        if factor != 1.0
            && let Some(cursor) = response.hover_pos()
        {
            // keep the world point under the cursor fixed while zooming
            let anchor = self.to_world(rect, cursor);
            self.zoom = (self.zoom * factor).clamp(0.02, 50.0);
            self.center = anchor - (cursor - rect.center()) / self.zoom;
        }

        // ---- cull to viewport (everything downstream iterates `visible`) ----
        let view = rect.expand(60.0);
        let mut visible: Vec<(NodeId, Pos2, f32)> = Vec::new();
        for (i, p) in self.pos.iter().enumerate() {
            let Some(w) = p else { continue };
            let s = self.to_screen(rect, *w);
            if !view.contains(s) {
                continue;
            }
            let id = NodeId(i as u32);
            let world_r: f32 = match self.g.node(id).kind {
                NodeKind::Dir => 9.0,
                NodeKind::File => 5.0,
                NodeKind::Ghost => continue,
            };
            let r = (world_r * self.zoom).clamp(1.5, 14.0);
            visible.push((id, s, r));
        }

        // ---- hover / select (linear scan over the culled set) ----
        self.hover = None;
        if let Some(cursor) = response.hover_pos() {
            let mut best = f32::INFINITY;
            for &(id, s, r) in &visible {
                let d = s.distance(cursor);
                if d < r + 4.0 && d < best {
                    best = d;
                    self.hover = Some(id);
                }
            }
        }
        if response.clicked() {
            self.selected = self.hover;
        }
        let active = self.hover.or(self.selected);

        // ---- paint ----
        let painter = ui.painter_at(rect);

        // contains edges (under everything)
        for (i, node) in self.g.nodes.iter().enumerate() {
            let Some(parent) = node.parent else { continue };
            let (Some(a), Some(b)) = (self.pos[i], self.pos[parent.0 as usize]) else {
                continue;
            };
            let (sa, sb) = (self.to_screen(rect, a), self.to_screen(rect, b));
            if !view.contains(sa) && !view.contains(sb) {
                continue;
            }
            painter.line_segment([sa, sb], Stroke::new(1.0, EDGE));
        }

        // wikilink overlay: only the active node's cross-links are drawn
        let mut adjacent: Vec<NodeId> = Vec::new();
        if let Some(a) = active {
            for l in &self.g.links {
                if l.from != a && l.to != a {
                    continue;
                }
                let other = if l.from == a { l.to } else { l.from };
                let (Some(wa), Some(wb)) =
                    (self.pos[a.0 as usize], self.pos[other.0 as usize])
                else {
                    continue; // ghost endpoints are unplaced until milestone C
                };
                let (sa, sb) = (self.to_screen(rect, wa), self.to_screen(rect, wb));
                let mid = sa.lerp(sb, 0.5);
                let d = sb - sa;
                let ctrl = mid + Vec2::new(-d.y, d.x) * 0.18;
                painter.add(egui::epaint::QuadraticBezierShape::from_points_stroke(
                    [sa, ctrl, sb],
                    false,
                    Color32::TRANSPARENT,
                    Stroke::new(1.6, WIKI),
                ));
                adjacent.push(other);
            }
        }

        // nodes
        for &(id, s, r) in &visible {
            let node = self.g.node(id);
            let fill = match node.kind {
                NodeKind::Dir => DIR,
                _ => FILE,
            };
            painter.circle_filled(s, r, fill);
            if active == Some(id) {
                let color = if self.selected == Some(id) { SELECT } else { HOVER };
                painter.circle_stroke(s, r + 3.0, Stroke::new(2.0, color));
            } else if adjacent.contains(&id) {
                painter.circle_stroke(s, r + 3.0, Stroke::new(1.5, WIKI));
            }
        }

        // labels — LOD: only when zoomed in enough, plus active + neighbours
        for &(id, s, r) in &visible {
            let node = self.g.node(id);
            let show = active == Some(id)
                || adjacent.contains(&id)
                || (node.kind == NodeKind::Dir && r >= 3.0)
                || r >= 5.0;
            if !show {
                continue;
            }
            let label = node.display_name();
            let color = if active == Some(id) { HOVER } else { TEXT };
            painter.text(
                s + Vec2::new(r + 5.0, 0.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.5),
                color,
            );
        }

        // status line
        let status = match active.map(|id| self.g.node(id)) {
            Some(n) => {
                let what = if n.path.is_empty() { &n.name } else { &n.path };
                what.clone()
            }
            None => format!(
                "{} files · {} dirs · {} links   |   drag pan · wheel zoom · click select",
                self.n_files,
                self.n_dirs,
                self.g.links.len()
            ),
        };
        painter.text(
            rect.left_top() + Vec2::new(10.0, 8.0),
            Align2::LEFT_TOP,
            status,
            FontId::proportional(12.0),
            TEXT,
        );
    }
}

impl eframe::App for Viewer {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| self.canvas(ui));
    }
}
