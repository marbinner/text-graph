//! The egui shell: viewport transform, input, painting. Geometry comes from
//! `sim` (force-directed, seeded by the pure radial layout); this module owns
//! presentation and interaction only.
//!
//! One world→screen transform (`to_screen`/`to_world`) — every input handler
//! and paint call goes through it. Zoom is toward the cursor. Dragging a node
//! pins it and reheats the simulation; dragging empty space pans.

mod actions;
mod diag;
mod navigator;
mod reload;
mod terminals;

use actions::CreateDialog;
use reload::ReloadMsg;
use terminals::{ResizeDrag, TERM_BG, resize_handle};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use text_graph::agents::{self, AgentPane};
use text_graph::graph::{Graph, NodeId, NodeKind};
use text_graph::keys::{self, Mods, Special};
use text_graph::mirror::{SessionMirror, TermGrid};
use text_graph::sim::Sim;
use text_graph::{create, graph, state, vault};

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

/// Screen radius above which a node shows its type glyph.
const ICON_MIN_R: f32 = 6.5;

/// Folder silhouette: tab + rounded body, sized relative to the node disc.
fn paint_folder_icon(p: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    let w = r * 1.02;
    let h = r * 0.72;
    let body = Rect::from_center_size(c + Vec2::new(0.0, r * 0.10), Vec2::new(w, h));
    let tab = Rect::from_min_size(
        Pos2::new(body.min.x, body.min.y - r * 0.20),
        Vec2::new(w * 0.45, r * 0.24),
    );
    p.rect_filled(tab, r * 0.08, color);
    p.rect_filled(body, r * 0.10, color);
}

/// Dog-eared page. `fill` paints it solid (punch-out on filled discs);
/// `outline` strokes it instead (hollow ghosts).
fn paint_doc_icon(
    p: &egui::Painter,
    c: Pos2,
    r: f32,
    fill: Option<Color32>,
    outline: Option<Color32>,
) {
    let w = r * 0.78;
    let h = r * 1.02;
    let page = Rect::from_center_size(c, Vec2::new(w, h));
    let ear = w * 0.40;
    let pts = vec![
        page.min,
        Pos2::new(page.max.x - ear, page.min.y),
        Pos2::new(page.max.x, page.min.y + ear),
        Pos2::new(page.max.x, page.max.y),
        Pos2::new(page.min.x, page.max.y),
    ];
    if let Some(color) = fill {
        p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else if let Some(color) = outline {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.0, color)));
    }
}

/// Everything `derived()` recomputes from a fresh graph.
struct Derived {
    radius: Vec<f32>,
    haystacks: Vec<String>,
    n_files: usize,
    n_dirs: usize,
    dir_by_path: HashMap<String, NodeId>,
}

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
        // egui quits on Ctrl+Q by default; that key releases terminal focus
        // here. Closing the window still quits.
        cc.egui_ctx.options_mut(|o| o.quit_shortcuts.clear());
        let mut viewer = viewer;
        viewer.start_watcher(cc.egui_ctx.clone());
        viewer.start_agent_scan(cc.egui_ctx.clone());
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
    /// Monotonic reload request counter — results from superseded requests
    /// are discarded on arrival.
    reload_gen: u64,
    reload_tx: std::sync::mpsc::Sender<ReloadMsg>,
    reload_rx: std::sync::mpsc::Receiver<ReloadMsg>,
    /// Health state, surfaced by the diagnostics badge.
    last_reload: Option<Instant>,
    reload_error: Option<String>,
    diag_open: bool,
    // ---- terminals in the graph ----
    /// Dir path → node, for anchoring agent panes at their cwd.
    dir_by_path: HashMap<String, NodeId>,
    /// Everything terminal-card related — see terminals::Terminals.
    terms: terminals::Terminals,
    // ---- view-state persistence ----
    /// View state as last written to `.text-graph/view` (skip no-op saves).
    saved_state: Option<state::ViewState>,
    last_save: Instant,
    save_warned: bool,
    // ---- tree navigation ----
    /// First `g` of a `gg` chord, with its press time.
    pending_g: Option<Instant>,
    /// Find-in-directory prompt (`f` in tree-nav mode): the query, live.
    nav_find: Option<String>,
    nav_find_focus: bool,
    /// Last applied find query, to jump only when the text changes.
    nav_find_last: String,
    /// Scroll the navigator's sibling list to the cursor next frame.
    nav_scroll: bool,
    // ---- creation (right-click menu) ----
    /// Node captured at right-click time — the context menu's subject.
    ctx_node: Option<NodeId>,
    /// Card captured at right-click time (lifecycle actions lead the menu).
    ctx_card: Option<(String, String)>,
    /// Open "new note/folder" dialog, if any.
    create: Option<CreateDialog>,
    /// Transient status-bar message and its birth time.
    flash: Option<(String, Instant)>,
    /// Select and frame this rel path once a reload turns it into a node.
    pending_select: Option<String>,
}

impl Viewer {
    /// Everything derivable from the graph alone — shared by `new` and the
    /// live-reload `rebuild`.
    fn derived(g: &Graph) -> Derived {
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
        let mut dir_by_path = HashMap::new();
        for (i, n) in g.nodes.iter().enumerate() {
            if n.kind == NodeKind::Dir {
                dir_by_path.insert(n.path.clone(), NodeId(i as u32));
            }
        }
        Derived {
            radius,
            haystacks,
            n_files,
            n_dirs,
            dir_by_path,
        }
    }

    fn new(g: Graph, root: PathBuf) -> Self {
        let sim = Sim::new(&g);
        let Derived {
            radius,
            haystacks,
            n_files,
            n_dirs,
            dir_by_path,
        } = Self::derived(&g);
        let n = haystacks.len();
        let (reload_tx, reload_rx) = std::sync::mpsc::channel();
        let vs = state::load(&root);
        let cam = vs.camera;
        let mut restore_offsets: HashMap<String, Vec<(String, Vec2)>> = HashMap::new();
        for c in vs.cards {
            restore_offsets
                .entry(c.session)
                .or_default()
                .push((c.pane, Vec2::new(c.dx, c.dy)));
        }
        Self {
            g,
            sim,
            radius,
            center: cam.map_or(Pos2::ZERO, |(x, y, _)| Pos2::new(x, y)),
            zoom: cam.map_or(1.0, |(_, _, z)| z.clamp(0.02, 50.0)),
            hover: None,
            selected: None,
            drag_node: None,
            fitted: cam.is_some(), // a restored camera must not be re-fit away
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
            reload_gen: 0,
            reload_tx,
            reload_rx,
            last_reload: None,
            reload_error: None,
            diag_open: false,
            dir_by_path,
            terms: terminals::Terminals::new(restore_offsets),
            saved_state: None,
            last_save: Instant::now(),
            save_warned: false,
            pending_g: None,
            nav_find: None,
            nav_find_focus: false,
            nav_find_last: String::new(),
            nav_scroll: false,
            ctx_node: None,
            ctx_card: None,
            create: None,
            flash: None,
            pending_select: None,
        }
    }

    /// Nearest Dir node at or above `cwd`.
    fn anchor_for(&self, cwd: &Path) -> NodeId {
        let rel = cwd.strip_prefix(&self.root).unwrap_or(Path::new(""));
        let mut key = rel.to_string_lossy().replace('\\', "/");
        loop {
            if let Some(&id) = self.dir_by_path.get(&key) {
                return id;
            }
            match key.rfind('/') {
                Some(i) => key.truncate(i),
                None if !key.is_empty() => key.clear(),
                None => return self.g.root,
            }
        }
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
        if self.create.is_some() {
            return; // the create dialog owns the keyboard
        }
        let (open_key, esc, enter, frame_key, reset, term_key) = ui.input(|i| {
            (
                i.key_pressed(Key::Slash) || (i.modifiers.command && i.key_pressed(Key::F)),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.modifiers.is_none() && i.key_pressed(Key::Z),
                i.key_pressed(Key::Num0) || i.key_pressed(Key::Home),
                i.modifiers.is_none() && i.key_pressed(Key::T),
            )
        });
        if self.search_open {
            if esc {
                self.close_search();
            } else if enter {
                // a terminal hit that outscores every node wins the jump —
                // and lands focused, ready to type
                let node_score = self.best.and_then(|id| self.scores[id.0 as usize]);
                let term_score = self
                    .terms
                    .best
                    .as_ref()
                    .and_then(|(i, _)| self.terms.scores.get(*i).copied().flatten());
                if term_score > node_score
                    && let Some((_, bk)) = self.terms.best.clone()
                    // resolve by key, not index: the pane list may have
                    // shifted since the scores were computed
                    && let Some(i) = self.terms.panes
                        .iter()
                        .position(|a| a.session == bk.0 && a.pane == bk.1)
                {
                    self.terms.cycle = i + 1;
                    self.terms.cursor = Some(bk.clone());
                    self.terms.focused = Some(bk.clone());
                    self.fly_to_card(bk);
                } else if let Some(best) = self.best {
                    self.selected = Some(best);
                    self.frame_node(best);
                    self.terms.cursor = None; // Enter must not later hijack
                }
                self.close_search();
            }
        } else if open_key {
            self.search_open = true;
            self.search_focus_pending = true;
        } else if esc {
            // the terminal cursor dismisses first, then node selection
            if self.terms.cursor.take().is_none() {
                self.selected = None;
            }
        } else if enter
            && ui.memory(|m| m.focused().is_none())
            && let Some(k) = self.terms.cursor.clone()
        {
            // Enter on the terminal cursor = start typing into it
            self.terms.focused = Some(k);
        } else if enter
            // if an egui widget (e.g. the detail pane's button, tab-focused)
            // has focus, Enter already activates it — don't also fire here,
            // or the editor opens twice
            && ui.memory(|m| m.focused().is_none())
            && let Some(sel) = self.selected
        {
            self.open_in_editor(sel);
        } else if frame_key && let Some(sel) = self.selected {
            self.frame_node(sel);
        } else if reset {
            self.fitted = false; // canvas re-fits on the next frame
        } else if term_key && ui.memory(|m| m.focused().is_none()) && !self.terms.panes.is_empty() {
            // hop the terminal cursor to the next card — keyboard stays on
            // the graph so repeated t keeps hopping; Enter dives in
            let i = self.terms.cycle % self.terms.panes.len();
            self.terms.cycle += 1;
            let a = &self.terms.panes[i];
            let key = (a.session.clone(), a.pane.clone());
            self.terms.cursor = Some(key.clone());
            self.fly_to_card(key);
        }

        // Ranger-style tree walk — SELECTION IS THE MODE: with a node
        // selected, hjkl walks the Contains tree (discrete steps, key
        // repeat); with nothing selected they pan. Esc switches back.
        let tree_nav = self.selected.is_some() && ui.memory(|m| m.focused().is_none());
        if let Some(sel) = self.selected.filter(|_| tree_nav) {
            let (h, j, k, l, g, sg, find, out_jump, back_jump) = ui.input(|i| {
                let m = i.modifiers.is_none();
                (
                    m && i.key_pressed(Key::H),
                    m && i.key_pressed(Key::J),
                    m && i.key_pressed(Key::K),
                    m && i.key_pressed(Key::L),
                    m && i.key_pressed(Key::G),
                    i.modifiers.shift_only() && i.key_pressed(Key::G),
                    m && i.key_pressed(Key::F),
                    m && i.key_pressed(Key::CloseBracket),
                    m && i.key_pressed(Key::OpenBracket),
                )
            });
            if find {
                // ranger f: find within the current directory's listing
                self.nav_find = Some(String::new());
                self.nav_find_last.clear();
                self.nav_find_focus = true;
            }
            let mut to: Option<NodeId> = None;
            if h {
                to = self.g.node(sel).parent;
            } else if j {
                to = self.g.nav_sibling(sel, 1);
            } else if k {
                to = self.g.nav_sibling(sel, -1);
            } else if l {
                match self.g.node(sel).kind {
                    NodeKind::Dir => to = self.g.nav_enter(sel),
                    // key repeat must not spawn an editor per repeat tick
                    NodeKind::File
                        if !ui.input(|i| {
                            i.events.iter().any(|e| {
                                matches!(
                                    e,
                                    egui::Event::Key {
                                        key: Key::L,
                                        pressed: true,
                                        repeat: true,
                                        ..
                                    }
                                )
                            })
                        }) =>
                    {
                        self.open_in_editor(sel);
                    }
                    _ => {}
                }
            } else if out_jump {
                // ] follows the note's first outgoing wikilink
                to = self.g.outlinks(sel).next().map(|l| l.to);
            } else if back_jump {
                // [ jumps to the first note linking here
                to = self.g.backlinks(sel).next().map(|l| l.from);
            } else if sg {
                to = self.g.nav_sibling_end(sel, true);
            } else if g {
                // vim gg: two bare g presses in quick succession
                if self
                    .pending_g
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(600))
                {
                    to = self.g.nav_sibling_end(sel, false);
                    self.pending_g = None;
                } else {
                    self.pending_g = Some(Instant::now());
                }
            }
            if (h || j || k || l || sg || out_jump || back_jump) && !g {
                self.pending_g = None;
            }
            if let Some(t) = to {
                self.selected = Some(t);
                self.frame_node(t); // the camera follows the walk
                self.nav_scroll = true; // and the sibling list follows too
            }
        }

        // vim-style camera: hjkl pans (when no node is selected), d/u zooms
        // — continuous while held
        if ui.memory(|m| m.focused().is_none()) {
            let (dt, h, j, k, l, d, u) = ui.input(|i| {
                let m = i.modifiers.is_none() && !tree_nav;
                (
                    i.stable_dt.min(0.1),
                    m && i.key_down(Key::H),
                    m && i.key_down(Key::J),
                    m && i.key_down(Key::K),
                    m && i.key_down(Key::L),
                    i.modifiers.is_none() && i.key_down(Key::D),
                    i.modifiers.is_none() && i.key_down(Key::U),
                )
            });
            if h || j || k || l || d || u {
                let axis = |pos: bool, neg: bool| (pos as i8 - neg as i8) as f32;
                let pan = 2200.0 * dt / self.zoom; // constant screen-space speed
                self.center.x += pan * axis(l, h);
                self.center.y += pan * axis(j, k);
                // u dives in, d pulls out (vim half-page up/down, roughly)
                let zf = 6.0f32.powf(dt * axis(u, d));
                self.zoom = (self.zoom * zf).clamp(0.02, 50.0);
                ui.ctx().request_repaint();
            }
        }
    }

    fn set_flash(&mut self, msg: String) {
        self.flash = Some((msg, Instant::now()));
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
            self.terms.scores.clear();
            self.terms.best = None;
            return;
        }
        // Terminals re-score every frame (cheap: a handful of panes) — the
        // pane list changes underneath the node-score cache.
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut tbest: Option<(u32, usize)> = None;
        self.terms.scores = self
            .terms
            .panes
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let dir = a
                    .cwd
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                let hay = format!("{} {} {} {}", a.agent, a.session, a.pane, dir);
                let score = pattern.score(Utf32Str::new(&hay, &mut buf), &mut self.matcher);
                if let Some(s) = score
                    && tbest.is_none_or(|(bs, _)| s > bs)
                {
                    tbest = Some((s, i));
                }
                score
            })
            .collect();
        self.terms.best = tbest.map(|(_, i)| {
            let a = &self.terms.panes[i];
            (i, (a.session.clone(), a.pane.clone()))
        });
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
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if !self.fitted {
            self.fit(rect);
            self.fitted = true;
        }

        // ---- simulation ----
        if self.sim.active() {
            self.sim.tick(3);
            ui.ctx().request_repaint();
        }

        // ---- agent terminals: discovery snapshot, mirrors, screen caches ----
        let ctx = ui.ctx().clone();
        self.sync_terminals(&ctx);

        // ---- input ----
        // Cards sit on top and win pointer contention. over_card and hover
        // use last frame's geometry — standard immediate-mode lag,
        // imperceptible at interactive frame rates.
        let over_card: Option<(String, String)> = response.hover_pos().and_then(|c| {
            // reverse: cards are painted in order, so the LAST rect containing
            // the cursor is the one visibly on top
            self.terms
                .rects
                .iter()
                .rev()
                .find(|(_, _, r)| r.contains(c))
                .map(|(s, p, _)| (s.clone(), p.clone()))
        });
        if response.drag_started() {
            if let Some(t) = over_card.clone() {
                // corner grip on our own (tg_) sessions = native resize; the
                // rest of the card moves it. Foreign sessions never get
                // resized from here — that would reflow someone's real
                // terminal view.
                let on_handle = response
                    .hover_pos()
                    .zip(
                        self.terms
                            .rects
                            .iter()
                            .rev()
                            .find(|(s, p, _)| (s, p) == (&t.0, &t.1)),
                    )
                    .is_some_and(|(pos, (_, _, r))| resize_handle(*r).contains(pos));
                let ours = self
                    .terms
                    .panes
                    .iter()
                    .find(|a| a.session == t.0 && a.pane == t.1)
                    .is_some_and(|a| a.ours);
                // no resize from compact LOD: the fixed summary box gives no
                // feedback and its ~1.5px cell advance makes a twitch reflow
                // the real session by dozens of columns
                let compact = (6.0 * self.zoom).clamp(2.5, 16.0) < 5.0;
                if on_handle
                    && ours
                    && !compact
                    && let Some(c) = self.terms.cache.get(&t)
                    && let Some(pos) = response.hover_pos()
                {
                    let f = (6.0 * self.zoom).clamp(2.5, 16.0);
                    let probe = ui.painter().layout_no_wrap(
                        "M".into(),
                        FontId::monospace(f),
                        Color32::WHITE,
                    );
                    let cur = (c.cols, c.total_rows);
                    self.terms.resize = Some(ResizeDrag {
                        key: t,
                        cols0: cur.0 as f32,
                        rows0: cur.1 as f32,
                        start: pos,
                        adv: probe.size().x.max(1.0),
                        line_h: probe.size().y.max(1.0),
                        want: cur,
                        sent: cur,
                        last_sent: Instant::now(),
                    });
                } else {
                    // seed the override from where the card currently is, so
                    // the first dragged frame doesn't jump
                    let cur_min = self
                        .terms
                        .rects
                        .iter()
                        .find(|(s, p, _)| (s, p) == (&t.0, &t.1))
                        .map(|(_, _, r)| r.min);
                    let anchor_s = self
                        .terms
                        .panes
                        .iter()
                        .find(|a| a.session == t.0 && a.pane == t.1)
                        .map(|a| {
                            let id = self.anchor_for(&a.cwd);
                            self.to_screen(rect, self.world_pos(id.0 as usize))
                        });
                    if let (Some(min), Some(anchor_s)) = (cur_min, anchor_s) {
                        self.terms
                            .offsets
                            .insert(t.clone(), (min - anchor_s) / self.zoom);
                    }
                    self.terms.drag_card = Some(t);
                }
            } else {
                self.drag_node = self.hover;
            }
        }
        if response.dragged() {
            if self.terms.resize.is_some() {
                if let (Some(rz), Some(cur)) =
                    (self.terms.resize.as_mut(), response.interact_pointer_pos())
                {
                    let cols = (rz.cols0 + (cur.x - rz.start.x) / rz.adv).round();
                    let rows = (rz.rows0 + (cur.y - rz.start.y) / rz.line_h).round();
                    rz.want = (cols.clamp(20.0, 220.0) as u16, rows.clamp(5.0, 80.0) as u16);
                    if rz.want != rz.sent && rz.last_sent.elapsed() > Duration::from_millis(90) {
                        let (cols, rows) = rz.want;
                        let cmd = format!("resize-window -t {} -x {cols} -y {rows}", rz.key.1);
                        let sess = rz.key.0.clone();
                        rz.sent = rz.want;
                        rz.last_sent = Instant::now();
                        if let Some(m) = self.terms.mirrors.get_mut(&sess) {
                            m.command(&cmd);
                        }
                    }
                }
                ui.ctx().request_repaint();
            } else if let Some(t) = self.terms.drag_card.clone() {
                if let Some(off) = self.terms.offsets.get_mut(&t) {
                    *off += response.drag_delta() / self.zoom;
                }
                ui.ctx().request_repaint();
            } else if let (Some(id), Some(cur)) = (self.drag_node, response.interact_pointer_pos())
            {
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
            self.terms.drag_card = None;
            if let Some(rz) = self.terms.resize.take()
                && rz.want != rz.sent
                && let Some(m) = self.terms.mirrors.get_mut(&rz.key.0)
            {
                // flush the debounced tail so the final size always lands
                m.command(&format!(
                    "resize-window -t {} -x {} -y {}",
                    rz.key.1, rz.want.0, rz.want.1
                ));
            }
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
            if over_card.is_none()
                && let Some(cursor) = response.hover_pos()
            {
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
            if let Some(t) = over_card.clone() {
                // clicking also parks the t-cursor here, so cycling resumes
                // from this card
                if let Some(i) = self
                    .terms
                    .panes
                    .iter()
                    .position(|a| a.session == t.0 && a.pane == t.1)
                {
                    self.terms.cycle = i + 1;
                }
                self.terms.cursor = Some(t.clone());
                self.terms.focused = Some(t);
                self.close_search();
            } else if self.terms.focused.is_some() {
                self.terms.focused = None; // click-away releases; click again to select
            } else {
                self.terms.cursor = None;
                self.selected = self.hover;
            }
        }
        if response.double_clicked() {
            if let Some(t) = over_card.clone() {
                self.fly_to_card(t);
            } else if let Some(h) = self.hover {
                self.open_in_editor(h);
            }
        }
        if response.secondary_clicked() {
            // right-click on a card targets its anchor dir; on a node, that
            // node; on empty space, the vault root (ctx_node = None)
            self.ctx_card = over_card.clone();
            self.ctx_node = if let Some(t) = &over_card {
                self.terms
                    .panes
                    .iter()
                    .find(|a| a.session == t.0 && a.pane == t.1)
                    .map(|a| self.anchor_for(&a.cwd))
            } else {
                self.hover
            };
        }
        response.context_menu(|ui| self.context_menu_ui(ui));
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
            // bbox test, not endpoint containment: a long edge crossing the
            // viewport must not vanish when both ends are off-screen
            if !view.intersects(Rect::from_two_pos(sa, sb)) {
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
            if !view.intersects(Rect::from_two_pos(sa, sb)) {
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
            // Type glyph inside the disc once it's big enough to read — a
            // dark punch-out silhouette, painter primitives only (no text
            // layout), so zoomed-out rendering stays a plain circle.
            let glyph = r >= ICON_MIN_R;
            let punch = dimmed(TERM_BG);
            match node.kind {
                NodeKind::Ghost => {
                    painter.circle_stroke(s, r, Stroke::new(1.2, dimmed(GHOST)));
                    if glyph {
                        // an unwritten page, hollow like its node
                        paint_doc_icon(&painter, s, r, None, Some(dimmed(GHOST)));
                    }
                }
                NodeKind::Dir => {
                    painter.circle_filled(s, r, dimmed(DIR));
                    if glyph {
                        paint_folder_icon(&painter, s, r, punch);
                    }
                }
                NodeKind::File => {
                    painter.circle_filled(s, r, dimmed(FILE));
                    if glyph {
                        paint_doc_icon(&painter, s, r, Some(punch), None);
                    }
                }
            }
            if active == Some(id) {
                let color = if self.selected == Some(id) {
                    SELECT
                } else {
                    HOVER
                };
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

        // terminal cards, on top of the graph
        self.paint_terminals(&painter, rect, view);

        // status line
        if self
            .flash
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(6))
        {
            self.flash = None;
        }
        let status = if let Some((msg, _)) = &self.flash {
            msg.clone()
        } else if let Some((s, p)) = &self.terms.focused {
            format!("typing into {s} {p} — Ctrl+Q or click away releases")
        } else if searching {
            let count = self.scores.iter().filter(|s| s.is_some()).count();
            let tcount = self.terms.scores.iter().flatten().count();
            let terms = if tcount > 0 {
                format!(" + {tcount} terminals")
            } else {
                String::new()
            };
            format!(
                "{count} match{}{terms} — Enter jumps to best · Esc closes",
                if count == 1 { "" } else { "es" }
            )
        } else if active.is_none()
            && let Some((s, p)) = &self.terms.cursor
        {
            format!("{s} {p} — Enter types into it · t next · Esc dismisses")
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
                    "{} files · {} dirs · {} links{}   |   / search · hjkl move · d/u zoom · f find · z center · t terminals · 0 reset",
                    self.n_files,
                    self.n_dirs,
                    self.g.links.len(),
                    if self.sim.active() {
                        " · settling…"
                    } else {
                        ""
                    },
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
        self.pump_reload(ui.ctx());
        let release = ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Q));
        if release {
            self.terms.focused = None;
        }
        if self.terms.focused.is_some() && self.create.is_none() {
            // keyboard belongs to the terminal; graph keybinds are suspended.
            // The create dialog outranks it — otherwise clicking a card with
            // the dialog open would drain its keystrokes into the pane.
            self.forward_input(ui);
        } else {
            self.handle_keys(ui);
        }
        self.update_search();
        if self.search_open {
            egui::Panel::top("search_bar").show(ui, |ui| self.search_bar(ui));
        }
        if self.selected.is_some() {
            egui::Panel::right("detail")
                .resizable(true)
                .show(ui, |ui| self.detail_pane(ui));
        } else {
            self.nav_find = None; // no navigator, no find prompt
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| self.canvas(ui));
        let ctx = ui.ctx().clone();
        self.create_dialog_ui(&ctx);
        self.diag_ui(&ctx);
        self.persist_state(false);
        // egui repaints on demand; without a heartbeat the debounced save
        // would never run once the sim settles and input stops
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_state(true);
    }
}
