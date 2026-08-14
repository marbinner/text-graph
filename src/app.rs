//! The egui shell: viewport transform, input, painting. Geometry comes from
//! `sim` (force-directed, seeded by the pure radial layout); this module owns
//! presentation and interaction only.
//!
//! One world→screen transform (`to_screen`/`to_world`) — every input handler
//! and paint call goes through it. Zoom is toward the cursor. Dragging a node
//! pins it and reheats the simulation; dragging empty space pans.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use text_graph::graph::{Graph, NodeId, NodeKind};
use text_graph::sim::Sim;
use text_graph::{graph, vault};

// ---- palette (dark) ----
const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
const EDGE: Color32 = Color32::from_rgb(0x3a, 0x40, 0x4d);
const DIR: Color32 = Color32::from_rgb(0x7a, 0xa2, 0xf7);
const FILE: Color32 = Color32::from_rgb(0xb8, 0xbc, 0xc8);
const GHOST: Color32 = Color32::from_rgb(0x6b, 0x72, 0x82);
const HOVER: Color32 = Color32::from_rgb(0xff, 0xb4, 0x54);
const SELECT: Color32 = Color32::from_rgb(0xff, 0x8a, 0x3d);
const WIKI: Color32 = Color32::from_rgb(0xe0, 0xaf, 0x68);
const TEXT: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xac);

/// Dim factor applied to everything outside the active node's neighborhood.
const DIM: f32 = 0.18;

pub fn run(path: &Path) -> ExitCode {
    let scan = match vault::scan(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let root = scan.root.clone();
    let viewer = Viewer::new(graph::build(scan), root);
    let title = format!("text-graph — {}", viewer.g.node(viewer.g.root).name);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_title(&title),
        ..Default::default()
    };
    let app = Box::new(move |cc: &eframe::CreationContext<'_>| {
        let mut viewer = viewer;
        viewer.start_watcher(cc.egui_ctx.clone());
        Ok(Box::new(viewer) as Box<dyn eframe::App>)
    });
    match eframe::run_native(&title, options, app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Viewer {
    g: Graph,
    sim: Sim,
    /// World-space radius per node (degree-scaled, Obsidian-style).
    radius: Vec<f32>,
    /// World point currently at the viewport center.
    center: Pos2,
    /// Screen pixels per world unit.
    zoom: f32,
    hover: Option<NodeId>,
    selected: Option<NodeId>,
    drag_node: Option<NodeId>,
    fitted: bool,
    n_files: usize,
    n_dirs: usize,
    // ---- search ----
    matcher: Matcher,
    /// Per-node "name aliases path" string the fuzzy pattern scores against.
    haystacks: Vec<String>,
    search_open: bool,
    search_focus_pending: bool,
    query: String,
    last_query: String,
    scores: Vec<Option<u32>>,
    best: Option<NodeId>,
    // ---- detail pane ----
    root: PathBuf,
    md_cache: CommonMarkCache,
    /// Body of the selected file, read on demand and cached per selection.
    detail: Option<(NodeId, String)>,
    // ---- live reload ----
    /// Kept alive for the watcher thread; None if watching failed.
    _watcher: Option<notify::RecommendedWatcher>,
    /// Timestamp of the last relevant filesystem event (debounce state).
    reload_at: Arc<Mutex<Option<Instant>>>,
}

impl Viewer {
    /// Everything derivable from the graph alone — shared by `new` and the
    /// live-reload `rebuild`.
    fn derived(g: &Graph) -> (Vec<f32>, Vec<String>, usize, usize) {
        let mut degree = vec![0usize; g.nodes.len()];
        for (i, node) in g.nodes.iter().enumerate() {
            degree[i] += node.children.len();
            if node.parent.is_some() {
                degree[i] += 1;
            }
        }
        for l in &g.links {
            degree[l.from.0 as usize] += 1;
            degree[l.to.0 as usize] += 1;
        }
        let radius = g
            .nodes
            .iter()
            .zip(&degree)
            .map(|(n, d)| {
                let base = match n.kind {
                    NodeKind::Dir => 6.0,
                    NodeKind::File => 3.5,
                    NodeKind::Ghost => 3.0,
                };
                (base + (*d as f32).sqrt() * 1.3f32).min(18.0)
            })
            .collect();
        let haystacks: Vec<String> = g
            .nodes
            .iter()
            .map(|n| format!("{} {} {}", n.display_name(), n.aliases.join(" "), n.path))
            .collect();
        let n_files = g.nodes.iter().filter(|n| n.kind == NodeKind::File).count();
        let n_dirs = g.nodes.iter().filter(|n| n.kind == NodeKind::Dir).count();
        (radius, haystacks, n_files, n_dirs)
    }

    fn new(g: Graph, root: PathBuf) -> Self {
        let sim = Sim::new(&g);
        let (radius, haystacks, n_files, n_dirs) = Self::derived(&g);
        let n = haystacks.len();
        Self {
            g,
            sim,
            radius,
            center: Pos2::ZERO,
            zoom: 1.0,
            hover: None,
            selected: None,
            drag_node: None,
            fitted: false,
            n_files,
            n_dirs,
            matcher: Matcher::new(Config::DEFAULT),
            haystacks,
            search_open: false,
            search_focus_pending: false,
            query: String::new(),
            last_query: String::new(),
            scores: vec![None; n],
            best: None,
            root,
            md_cache: CommonMarkCache::default(),
            detail: None,
            _watcher: None,
            reload_at: Arc::new(Mutex::new(None)),
        }
    }

    /// Watch the vault; on a relevant event, stamp the debounce clock and
    /// wake the UI thread. Failure to watch just means no live reload.
    fn start_watcher(&mut self, ctx: egui::Context) {
        use notify::Watcher as _;
        let state = self.reload_at.clone();
        let root = self.root.clone();
        let handler = move |res: Result<notify::Event, notify::Error>| {
            let Ok(event) = res else { return };
            let relevant = event.paths.iter().any(|p| {
                let rel = p.strip_prefix(&root).unwrap_or(p);
                let hidden = rel
                    .components()
                    .any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with('.')));
                if hidden {
                    return false; // .obsidian/.git churn must not trigger reloads
                }
                match rel.extension().and_then(|e| e.to_str()) {
                    Some(ext) => ext.eq_ignore_ascii_case("md"),
                    None => true, // directory events (creates, renames)
                }
            });
            if relevant {
                *state.lock().unwrap() = Some(Instant::now());
                ctx.request_repaint();
            }
        };
        if let Ok(mut w) = notify::recommended_watcher(handler)
            && w.watch(&self.root, notify::RecursiveMode::Recursive).is_ok()
        {
            self._watcher = Some(w);
        }
    }

    /// Re-scan the vault and swap the graph in, carrying over sim positions,
    /// selection, and search identity by path so an edit ripples the layout
    /// instead of re-settling it.
    fn rebuild(&mut self) {
        let Ok(scan) = vault::scan(&self.root) else {
            return; // vault temporarily unreadable — keep showing the old graph
        };
        let g = graph::build(scan);

        let old_pos: HashMap<String, (f32, f32)> = self
            .g
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.path.clone(), (self.sim.x[i], self.sim.y[i])))
            .collect();
        let mut sim = Sim::new(&g);
        for (i, node) in g.nodes.iter().enumerate() {
            if let Some(&(x, y)) = old_pos.get(&node.path) {
                sim.x[i] = x;
                sim.y[i] = y;
            }
        }
        sim.calm();

        let by_path: HashMap<&str, NodeId> = g
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.path.as_str(), NodeId(i as u32)))
            .collect();
        self.selected = self
            .selected
            .and_then(|id| by_path.get(self.g.node(id).path.as_str()).copied());
        self.hover = None;
        self.drag_node = None;
        self.best = None;

        let (radius, haystacks, n_files, n_dirs) = Self::derived(&g);
        self.radius = radius;
        self.haystacks = haystacks;
        self.n_files = n_files;
        self.n_dirs = n_dirs;
        self.scores = vec![None; g.nodes.len()];
        self.last_query.clear(); // force a re-score against the new nodes
        self.detail = None; // re-read the body — the pane shows fresh edits
        self.g = g;
        self.sim = sim;
    }

    fn frame_node(&mut self, id: NodeId) {
        self.center = self.world_pos(id.0 as usize);
        if self.zoom < 0.9 {
            self.zoom = 0.9;
        }
    }

    fn close_search(&mut self) {
        self.search_open = false;
        self.query.clear();
        self.last_query.clear();
        self.scores.fill(None);
        self.best = None;
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        let (open_key, esc, enter) = ui.input(|i| {
            (
                i.key_pressed(Key::Slash) || (i.modifiers.command && i.key_pressed(Key::F)),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
            )
        });
        if self.search_open {
            if esc {
                self.close_search();
            } else if enter {
                if let Some(best) = self.best {
                    self.selected = Some(best);
                    self.frame_node(best);
                }
                self.close_search();
            }
        } else if open_key {
            self.search_open = true;
            self.search_focus_pending = true;
        } else if esc {
            self.selected = None;
        } else if enter
            && let Some(sel) = self.selected
        {
            self.open_in_editor(sel);
        }
    }

    /// Spawn $VISUAL / $EDITOR (split on whitespace so "code --wait" works),
    /// falling back to xdg-open. Detached; the viewer stays read-only.
    fn open_in_editor(&self, id: NodeId) {
        let node = self.g.node(id);
        if node.kind != NodeKind::File {
            return;
        }
        let path = self.root.join(&node.path);
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.trim().is_empty()));
        let result = match &editor {
            Some(ed) => {
                let mut parts = ed.split_whitespace();
                let prog = parts.next().unwrap_or("xdg-open");
                std::process::Command::new(prog).args(parts).arg(&path).spawn()
            }
            None => std::process::Command::new("xdg-open").arg(&path).spawn(),
        };
        if let Err(e) = result {
            eprintln!("failed to open {}: {e}", path.display());
        }
    }

    /// Re-score all nodes when the query changed (cheap: one fuzzy match per
    /// node per keystroke).
    fn update_search(&mut self) {
        if !self.search_open || self.query.is_empty() {
            if !self.last_query.is_empty() || self.best.is_some() {
                self.last_query.clear();
                self.scores.fill(None);
                self.best = None;
            }
            return;
        }
        if self.query == self.last_query {
            return;
        }
        self.last_query = self.query.clone();
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut best: Option<(u32, NodeId)> = None;
        for (i, hay) in self.haystacks.iter().enumerate() {
            let score = pattern.score(Utf32Str::new(hay, &mut buf), &mut self.matcher);
            self.scores[i] = score;
            if let Some(s) = score
                && best.is_none_or(|(bs, _)| s > bs)
            {
                best = Some((s, NodeId(i as u32)));
            }
        }
        self.best = best.map(|(_, id)| id);
    }

    fn search_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("search:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("name, alias, or path — Enter jumps to best, Esc closes")
                    .desired_width(f32::INFINITY),
            );
            if self.search_focus_pending {
                resp.request_focus();
                self.search_focus_pending = false;
            }
        });
    }

    fn load_body(&self, id: NodeId) -> String {
        let node = self.g.node(id);
        match node.kind {
            NodeKind::File => vault::read_body(&self.root.join(&node.path))
                .unwrap_or_else(|e| format!("*error reading file:* {e}")),
            _ => String::new(),
        }
    }

    fn detail_pane(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.selected else { return };
        if self.detail.as_ref().map(|(id, _)| *id) != Some(sel) {
            self.detail = Some((sel, self.load_body(sel)));
        }
        // Owned copies so the panel closures below can borrow self freely.
        let (kind, display, sub) = {
            let node = self.g.node(sel);
            let sub = if node.path.is_empty() { node.name.clone() } else { node.path.clone() };
            (node.kind, node.display_name().to_string(), sub)
        };

        ui.set_min_width(320.0);
        ui.add_space(6.0);
        ui.heading(display);
        ui.label(egui::RichText::new(sub).small().color(TEXT));
        ui.separator();

        let mut jump: Option<NodeId> = None;
        match kind {
            NodeKind::File => {
                if ui.button("open in editor  (Enter)").clicked() {
                    self.open_in_editor(sel);
                }
                ui.add_space(4.0);
                // take/put-back so the markdown cache and the body can be
                // borrowed simultaneously without cloning the body per frame
                let detail = self.detail.take();
                if let Some((_, body)) = &detail {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        CommonMarkViewer::new().show(ui, &mut self.md_cache, body);
                    });
                }
                self.detail = detail;
            }
            NodeKind::Dir => {
                let children = self.g.node(sel).children.clone();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(format!("{} entries", children.len()));
                    ui.add_space(4.0);
                    for c in children {
                        let child = self.g.node(c);
                        let icon = if child.kind == NodeKind::Dir { "▸ " } else { "· " };
                        if ui.link(format!("{icon}{}", child.display_name())).clicked() {
                            jump = Some(c);
                        }
                    }
                });
            }
            NodeKind::Ghost => {
                ui.label("Not written yet. Referenced from:");
                ui.add_space(4.0);
                let refs: Vec<NodeId> =
                    self.g.links.iter().filter(|l| l.to == sel).map(|l| l.from).collect();
                for r in refs {
                    if ui.link(self.g.node(r).path.clone()).clicked() {
                        jump = Some(r);
                    }
                }
            }
        }
        if let Some(j) = jump {
            self.selected = Some(j);
            self.frame_node(j);
        }
    }

    fn world_pos(&self, i: usize) -> Pos2 {
        Pos2::new(self.sim.x[i], self.sim.y[i])
    }

    /// Frame the whole graph (first paint only — rect is unknown before then).
    fn fit(&mut self, rect: Rect) {
        let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
        let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for i in 0..self.g.nodes.len() {
            let p = self.world_pos(i);
            min = min.min(p);
            max = max.max(p);
        }
        if !min.x.is_finite() {
            return;
        }
        let size = (max - min).max(Vec2::splat(1.0));
        self.center = Pos2::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5);
        self.zoom = ((rect.width() / size.x).min(rect.height() / size.y) * 0.85).clamp(0.02, 50.0);
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

        // ---- simulation ----
        if self.sim.active() {
            self.sim.tick(3);
            ui.ctx().request_repaint();
        }

        // ---- input ----
        // drag_started uses last frame's hover — standard immediate-mode lag,
        // imperceptible at interactive frame rates.
        if response.drag_started() {
            self.drag_node = self.hover;
        }
        if response.dragged() {
            if let (Some(id), Some(cur)) = (self.drag_node, response.interact_pointer_pos()) {
                let w = self.to_world(rect, cur);
                self.sim.pin(id.0, w.x, w.y);
                ui.ctx().request_repaint();
            } else {
                self.center -= response.drag_delta() / self.zoom;
            }
        }
        if response.drag_stopped() {
            self.sim.unpin();
            self.drag_node = None;
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

        // ---- cull to viewport ----
        let view = rect.expand(60.0);
        let mut visible: Vec<(NodeId, Pos2, f32)> = Vec::new();
        for i in 0..self.g.nodes.len() {
            let s = self.to_screen(rect, self.world_pos(i));
            if !view.contains(s) {
                continue;
            }
            let r = (self.radius[i] * self.zoom).clamp(1.5, 16.0);
            visible.push((NodeId(i as u32), s, r));
        }

        // ---- hover / select ----
        if let Some(id) = self.drag_node {
            self.hover = Some(id);
        } else {
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
        }
        if response.clicked() {
            self.selected = self.hover;
        }
        if response.double_clicked()
            && let Some(h) = self.hover
        {
            self.open_in_editor(h);
        }
        let active = self.hover.or(self.selected);

        // Neighborhood of the active node: tree parent + children + wikilink
        // partners. Everything else dims (the Obsidian hover effect).
        let mut neighbors: HashSet<NodeId> = HashSet::new();
        let mut partners: HashSet<NodeId> = HashSet::new();
        if let Some(a) = active {
            neighbors.insert(a);
            let node = self.g.node(a);
            if let Some(p) = node.parent {
                neighbors.insert(p);
            }
            for c in &node.children {
                neighbors.insert(*c);
            }
            for l in &self.g.links {
                if l.from == a {
                    partners.insert(l.to);
                    neighbors.insert(l.to);
                } else if l.to == a {
                    partners.insert(l.from);
                    neighbors.insert(l.from);
                }
            }
        }

        // ---- lit mask: search matches win; else the active neighborhood ----
        let searching = self.search_open && !self.query.is_empty();
        let n_nodes = self.g.nodes.len();
        let lit: Vec<bool> = if searching {
            self.scores.iter().map(Option::is_some).collect()
        } else if active.is_some() {
            (0..n_nodes)
                .map(|i| neighbors.contains(&NodeId(i as u32)))
                .collect()
        } else {
            vec![true; n_nodes]
        };

        // ---- paint ----
        let painter = ui.painter_at(rect);

        // contains edges (under everything)
        for (i, node) in self.g.nodes.iter().enumerate() {
            let Some(parent) = node.parent else { continue };
            let sa = self.to_screen(rect, self.world_pos(i));
            let sb = self.to_screen(rect, self.world_pos(parent.0 as usize));
            if !view.contains(sa) && !view.contains(sb) {
                continue;
            }
            let on = lit[i] && lit[parent.0 as usize];
            let color = if on { EDGE } else { EDGE.gamma_multiply(DIM) };
            painter.line_segment([sa, sb], Stroke::new(1.0, color));
        }

        // wikilink edges — always visible as faint curves, bright when they
        // touch the active node
        for l in &self.g.links {
            let sa = self.to_screen(rect, self.world_pos(l.from.0 as usize));
            let sb = self.to_screen(rect, self.world_pos(l.to.0 as usize));
            if !view.contains(sa) && !view.contains(sb) {
                continue;
            }
            let bright = active == Some(l.from) || active == Some(l.to);
            let on = lit[l.from.0 as usize] && lit[l.to.0 as usize];
            let (color, width) = if bright {
                (WIKI, 1.8)
            } else if on {
                (WIKI.gamma_multiply(0.35), 1.0)
            } else {
                (WIKI.gamma_multiply(DIM), 1.0)
            };
            let mid = sa.lerp(sb, 0.5);
            let d = sb - sa;
            let ctrl = mid + Vec2::new(-d.y, d.x) * 0.18;
            painter.add(egui::epaint::QuadraticBezierShape::from_points_stroke(
                [sa, ctrl, sb],
                false,
                Color32::TRANSPARENT,
                Stroke::new(width, color),
            ));
        }

        // nodes
        for &(id, s, r) in &visible {
            let node = self.g.node(id);
            let on = lit[id.0 as usize];
            let dimmed = |c: Color32| if on { c } else { c.gamma_multiply(DIM) };
            match node.kind {
                NodeKind::Ghost => {
                    painter.circle_stroke(s, r, Stroke::new(1.2, dimmed(GHOST)));
                }
                NodeKind::Dir => {
                    painter.circle_filled(s, r, dimmed(DIR));
                }
                NodeKind::File => {
                    painter.circle_filled(s, r, dimmed(FILE));
                }
            }
            if active == Some(id) {
                let color = if self.selected == Some(id) { SELECT } else { HOVER };
                painter.circle_stroke(s, r + 3.0, Stroke::new(2.0, color));
            } else if searching && self.best == Some(id) {
                painter.circle_stroke(s, r + 3.0, Stroke::new(2.0, HOVER));
            } else if partners.contains(&id) {
                painter.circle_stroke(s, r + 3.0, Stroke::new(1.5, WIKI));
            }
        }

        // labels — LOD by screen radius; always for the active neighborhood
        for &(id, s, r) in &visible {
            let node = self.g.node(id);
            let show = if searching {
                lit[id.0 as usize] && (r >= 3.0 || self.best == Some(id))
            } else {
                active == Some(id)
                    || partners.contains(&id)
                    || (lit[id.0 as usize]
                        && ((node.kind == NodeKind::Dir && r >= 3.0) || r >= 5.0))
            };
            if !show {
                continue;
            }
            let color = if active == Some(id) { HOVER } else { TEXT };
            painter.text(
                s + Vec2::new(r + 5.0, 0.0),
                Align2::LEFT_CENTER,
                node.display_name(),
                FontId::proportional(11.5),
                color,
            );
        }

        // status line
        let status = if searching {
            let count = self.scores.iter().filter(|s| s.is_some()).count();
            format!(
                "{count} match{} — Enter jumps to best · Esc closes",
                if count == 1 { "" } else { "es" }
            )
        } else {
            match active.map(|id| self.g.node(id)) {
                Some(n) => {
                let what = if n.path.is_empty() { &n.name } else { &n.path };
                if n.kind == NodeKind::Ghost {
                    format!("[[{what}]] — not written yet")
                } else {
                    what.clone()
                }
            }
                None => format!(
                    "{} files · {} dirs · {} links{}   |   drag node: move · drag space: pan · wheel: zoom · / search",
                    self.n_files,
                    self.n_dirs,
                    self.g.links.len(),
                    if self.sim.active() { " · settling…" } else { "" },
                ),
            }
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
        // debounced live reload: rebuild once the vault has been quiet
        let due = {
            let mut at = self.reload_at.lock().unwrap();
            match *at {
                Some(t) if t.elapsed() >= Duration::from_millis(300) => {
                    *at = None;
                    true
                }
                Some(_) => {
                    ui.ctx().request_repaint_after(Duration::from_millis(120));
                    false
                }
                None => false,
            }
        };
        if due {
            self.rebuild();
        }
        self.handle_keys(ui);
        self.update_search();
        if self.search_open {
            egui::Panel::top("search_bar").show(ui, |ui| self.search_bar(ui));
        }
        if self.selected.is_some() {
            egui::Panel::right("detail")
                .resizable(true)
                .show(ui, |ui| self.detail_pane(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| self.canvas(ui));
    }
}
