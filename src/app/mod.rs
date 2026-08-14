//! The egui shell: viewport transform, input, painting. Geometry comes from
//! `sim` (force-directed, seeded by the pure radial layout); this module owns
//! presentation and interaction only.
//!
//! One world→screen transform (`to_screen`/`to_world`) — every input handler
//! and paint call goes through it. Zoom is toward the cursor. Dragging a node
//! pins it and reheats the simulation; dragging empty space pans.

mod diag;
mod terminals;

use terminals::{CachedPane, ResizeDrag, TERM_BG, resize_handle};

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

/// State of the "New note / New folder" dialog (opened via right-click).
struct CreateDialog {
    folder: bool,
    /// Vault-relative target directory ("" = root) and its display label.
    dir: String,
    label: String,
    buf: String,
    /// Focus the text field on the next frame (open / after an error).
    focus: bool,
    err: Option<String>,
}

type ReloadMsg = (u64, anyhow::Result<Graph>);

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
    /// Written by the scanner thread, snapshotted each frame.
    agents_seen: Arc<Mutex<Vec<AgentPane>>>,
    agent_panes: Vec<AgentPane>,
    mirrors: HashMap<String, SessionMirror>,
    /// Sessions whose last mirror attach failed, with the failure time —
    /// retried after a cooldown instead of at frame rate.
    attach_backoff: HashMap<String, Instant>,
    /// (session, pane) → converted screen; refreshed per mirror generation.
    term_cache: HashMap<(String, String), CachedPane>,
    term_gen: HashMap<String, u64>,
    term_activity: HashMap<String, Instant>,
    /// Keyboard goes to this pane instead of the graph.
    focused_term: Option<(String, String)>,
    /// Card hit-boxes from the last paint (screen space).
    term_rects: Vec<(String, String, Rect)>,
    /// User-arranged card positions: world-space offset of the card's min
    /// corner from its anchor node. Absent = automatic outward placement.
    term_offsets: HashMap<(String, String), Vec2>,
    /// Parked arrangements (from disk, or from sessions that went away),
    /// keyed by session name: reclaimed when a matching session reappears.
    restore_offsets: HashMap<String, Vec<(String, Vec2)>>,
    /// View state as last written to `.text-graph/view` (skip no-op saves).
    saved_state: Option<state::ViewState>,
    last_save: Instant,
    save_warned: bool,
    /// Card currently being dragged.
    drag_card: Option<(String, String)>,
    /// Corner-grip resize in progress (tg_ sessions only — native resize).
    resize_term: Option<ResizeDrag>,
    /// Set on double-click: next paint recenters the view on this card.
    zoom_to_card: Option<(String, String)>,
    /// Position of the `t` (cycle terminals) key, modulo the pane count.
    term_cycle: usize,
    /// First `g` of a `gg` chord (tree navigation), with its press time.
    pending_g: Option<Instant>,
    /// Find-in-directory prompt (`f` in tree-nav mode): the query, live.
    nav_find: Option<String>,
    nav_find_focus: bool,
    /// Last applied find query, to jump only when the text changes.
    nav_find_last: String,
    /// Scroll the navigator's sibling list to the cursor on the next frame
    /// (set by keyboard navigation, not by clicks).
    nav_scroll: bool,
    /// The card the terminal cursor is on: highlighted, Enter focuses it.
    /// Unlike `focused_term`, the keyboard still belongs to the graph.
    term_selected: Option<(String, String)>,
    /// Fuzzy-search scores for agent panes (aligned with `agent_panes`,
    /// re-scored every frame while searching — the pane list is live).
    term_scores: Vec<Option<u32>>,
    /// Best terminal hit: index at scoring time PLUS its key, so consumers
    /// can survive the pane list shifting underneath the index.
    term_best: Option<(usize, (String, String))>,
    // ---- creation (right-click menu) ----
    /// Node captured at right-click time — the context menu's subject.
    ctx_node: Option<NodeId>,
    /// Card captured at right-click time (its lifecycle actions lead the menu).
    ctx_card: Option<(String, String)>,
    /// Open "new note/folder" dialog, if any.
    create: Option<CreateDialog>,
    /// Transient status-bar message and its birth time.
    flash: Option<(String, Instant)>,
    /// Select and frame this rel path once a reload turns it into a node.
    pending_select: Option<String>,
    /// tmux presence, probed once at startup — gates "Launch agent".
    tmux_ok: bool,
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
            agents_seen: Arc::new(Mutex::new(Vec::new())),
            agent_panes: Vec::new(),
            mirrors: HashMap::new(),
            attach_backoff: HashMap::new(),
            term_cache: HashMap::new(),
            term_gen: HashMap::new(),
            term_activity: HashMap::new(),
            focused_term: None,
            term_rects: Vec::new(),
            term_offsets: HashMap::new(),
            restore_offsets,
            saved_state: None,
            last_save: Instant::now(),
            save_warned: false,
            drag_card: None,
            resize_term: None,
            zoom_to_card: None,
            term_cycle: 0,
            pending_g: None,
            nav_find: None,
            nav_find_focus: false,
            nav_find_last: String::new(),
            nav_scroll: false,
            term_selected: None,
            term_scores: Vec::new(),
            term_best: None,
            ctx_node: None,
            ctx_card: None,
            create: None,
            flash: None,
            pending_select: None,
            tmux_ok: std::process::Command::new("tmux")
                .arg("-V")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
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
            && w.watch(&self.root, notify::RecursiveMode::Recursive)
                .is_ok()
        {
            self._watcher = Some(w);
        }
    }

    /// Swap a freshly built graph in, carrying over sim positions,
    /// selection, and search identity by path so an edit ripples the layout
    /// instead of re-settling it. Identity is `Node::ident` (ghosts
    /// namespaced — see graph.rs). The expensive scan+build happens on a
    /// worker thread (see `ui`); only this cheap carry-over runs here.
    fn apply_graph(&mut self, g: Graph) {
        self.last_reload = Some(Instant::now());
        self.reload_error = None;

        let old_pos: HashMap<String, (f32, f32)> = self
            .g
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.ident(), (self.sim.x[i], self.sim.y[i])))
            .collect();
        let mut sim = Sim::new(&g);
        for (i, node) in g.nodes.iter().enumerate() {
            if let Some(&(x, y)) = old_pos.get(&node.ident()) {
                sim.x[i] = x;
                sim.y[i] = y;
            }
        }
        sim.calm();

        let by_ident: HashMap<String, NodeId> = g
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.ident(), NodeId(i as u32)))
            .collect();
        self.selected = self
            .selected
            .and_then(|id| by_ident.get(&self.g.node(id).ident()).copied());
        // remap rather than clear: a reload landing mid-drag (agents save
        // files constantly) must not silently turn the gesture into a pan
        self.drag_node = self
            .drag_node
            .and_then(|id| by_ident.get(&self.g.node(id).ident()).copied());
        self.ctx_node = self
            .ctx_node
            .and_then(|id| by_ident.get(&self.g.node(id).ident()).copied());
        self.hover = None;
        self.best = None;

        let Derived {
            radius,
            haystacks,
            n_files,
            n_dirs,
            dir_by_path,
        } = Self::derived(&g);
        self.radius = radius;
        self.haystacks = haystacks;
        self.n_files = n_files;
        self.n_dirs = n_dirs;
        self.dir_by_path = dir_by_path;
        self.scores = vec![None; g.nodes.len()];
        self.last_query.clear(); // force a re-score against the new nodes
        self.detail = None; // re-read the body — the pane shows fresh edits
        self.g = g;
        self.sim = sim;

        // a note we just created: select and frame it the moment it lands
        if let Some(p) = self.pending_select.clone()
            && let Some(i) = self
                .g
                .nodes
                .iter()
                .position(|n| n.kind != NodeKind::Ghost && n.path == p)
        {
            self.pending_select = None;
            self.selected = Some(NodeId(i as u32));
            self.frame_node(NodeId(i as u32));
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
                    .term_best
                    .as_ref()
                    .and_then(|(i, _)| self.term_scores.get(*i).copied().flatten());
                if term_score > node_score
                    && let Some((_, bk)) = self.term_best.clone()
                    // resolve by key, not index: the pane list may have
                    // shifted since the scores were computed
                    && let Some(i) = self
                        .agent_panes
                        .iter()
                        .position(|a| a.session == bk.0 && a.pane == bk.1)
                {
                    self.term_cycle = i + 1;
                    self.term_selected = Some(bk.clone());
                    self.focused_term = Some(bk.clone());
                    self.fly_to_card(bk);
                } else if let Some(best) = self.best {
                    self.selected = Some(best);
                    self.frame_node(best);
                    self.term_selected = None; // Enter must not later hijack
                }
                self.close_search();
            }
        } else if open_key {
            self.search_open = true;
            self.search_focus_pending = true;
        } else if esc {
            // the terminal cursor dismisses first, then node selection
            if self.term_selected.take().is_none() {
                self.selected = None;
            }
        } else if enter
            && ui.memory(|m| m.focused().is_none())
            && let Some(k) = self.term_selected.clone()
        {
            // Enter on the terminal cursor = start typing into it
            self.focused_term = Some(k);
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
        } else if term_key && ui.memory(|m| m.focused().is_none()) && !self.agent_panes.is_empty() {
            // hop the terminal cursor to the next card — keyboard stays on
            // the graph so repeated t keeps hopping; Enter dives in
            let i = self.term_cycle % self.agent_panes.len();
            self.term_cycle += 1;
            let a = &self.agent_panes[i];
            let key = (a.session.clone(), a.pane.clone());
            self.term_selected = Some(key.clone());
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

    /// Camera + every card arrangement, live or parked, sorted for a
    /// deterministic file.
    fn snapshot_state(&self) -> state::ViewState {
        let mut cards: Vec<state::CardPos> = self
            .term_offsets
            .iter()
            .map(|((s, p), off)| state::CardPos {
                session: s.clone(),
                pane: p.clone(),
                dx: off.x,
                dy: off.y,
            })
            .collect();
        for (s, list) in &self.restore_offsets {
            for (p, off) in list {
                cards.push(state::CardPos {
                    session: s.clone(),
                    pane: p.clone(),
                    dx: off.x,
                    dy: off.y,
                });
            }
        }
        cards.sort_by(|a, b| (&a.session, &a.pane).cmp(&(&b.session, &b.pane)));
        state::ViewState {
            camera: Some((self.center.x, self.center.y, self.zoom)),
            cards,
        }
    }

    /// Debounced view-state save; `force` (exit) skips the debounce. Errors
    /// warn once and go quiet (read-only vaults stay usable).
    fn persist_state(&mut self, force: bool) {
        if !force && self.last_save.elapsed() < Duration::from_secs(3) {
            return;
        }
        let s = self.snapshot_state();
        if self.saved_state.as_ref() == Some(&s) {
            return;
        }
        self.last_save = Instant::now();
        match state::save(&self.root, &s) {
            Ok(()) => self.saved_state = Some(s),
            Err(e) => {
                if !self.save_warned {
                    eprintln!("couldn't save view state: {e}");
                    self.save_warned = true;
                }
            }
        }
    }

    /// The directory the context menu's actions apply to (vault-relative,
    /// "" = root) and a human label for it.
    fn ctx_dir(&self) -> (String, String) {
        let dir = self
            .ctx_node
            .map(|id| {
                let n = self.g.node(id);
                match n.kind {
                    NodeKind::Dir => n.path.clone(),
                    _ => n
                        .parent
                        .map(|p| self.g.node(p).path.clone())
                        .unwrap_or_default(),
                }
            })
            .unwrap_or_default();
        let label = if dir.is_empty() {
            self.root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("vault")
                .to_string()
        } else {
            dir.clone()
        };
        (dir, label)
    }

    /// Right-click menu: card lifecycle first (when a card was clicked),
    /// then creation anchored at the clicked node's directory.
    fn context_menu_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(170.0);
        if let Some((s, p)) = self.ctx_card.clone()
            // only while the pane is still alive
            && self.agent_panes.iter().any(|a| a.session == s && a.pane == p)
        {
            if ui.button("Attach in terminal…").clicked() {
                self.attach_external(&s, &p);
            }
            ui.menu_button("Kill terminal", |ui| {
                ui.label(
                    egui::RichText::new("ends whatever is running there")
                        .weak()
                        .small(),
                );
                if ui.button(format!("Kill {s} {p}")).clicked() {
                    self.kill_pane(&s, &p);
                }
            });
            ui.separator();
        }
        // a ghost is a referenced-but-unwritten note: offer to make it real
        if let Some(id) = self.ctx_node
            && self.g.node(id).kind == NodeKind::Ghost
        {
            let target = self.g.node(id).path.clone();
            if ui.button(format!("Write \"{target}\"")).clicked() {
                let res = create::ghost_rel_path(&target)
                    .and_then(|rel| create::write_note(&self.root, &rel).map(|_| rel));
                match res {
                    Ok(rel) => {
                        self.pending_select = Some(rel.clone());
                        self.set_flash(format!("created {rel}"));
                        *self.reload_at.lock().unwrap() = Some(Instant::now());
                    }
                    Err(e) => self.set_flash(format!("can't create: {e}")),
                }
            }
            return;
        }

        let (dir, label) = self.ctx_dir();
        ui.label(egui::RichText::new(format!("in {label}/")).weak().small());
        if ui.button("New note…").clicked() {
            self.open_create(false, dir.clone(), label.clone());
        }
        if ui.button("New folder…").clicked() {
            self.open_create(true, dir.clone(), label.clone());
        }
        if self.tmux_ok {
            ui.separator();
            if ui.button("New terminal").clicked() {
                let path = self.ctx_path(&dir);
                match agents::launch_shell(None, &path) {
                    Ok(name) => self.set_flash(format!("opened terminal — session {name}")),
                    Err(e) => self.set_flash(format!("terminal failed: {e}")),
                }
            }
            ui.menu_button("Launch agent", |ui| {
                for agent in agents::default_allowlist() {
                    if ui.button(&agent).clicked() {
                        self.launch_agent(&dir, &agent);
                    }
                }
            });
        }
    }

    /// Absolute path for a vault-relative dir ("" = root).
    fn ctx_path(&self, dir: &str) -> PathBuf {
        if dir.is_empty() {
            self.root.clone()
        } else {
            self.root.join(dir)
        }
    }

    fn open_create(&mut self, folder: bool, dir: String, label: String) {
        self.focused_term = None; // the dialog owns the keyboard now
        self.close_search();
        self.create = Some(CreateDialog {
            folder,
            dir,
            label,
            buf: String::new(),
            focus: true,
            err: None,
        });
    }

    /// The centered "New note / New folder" window, while `self.create` is on.
    fn create_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.create.take() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(if dlg.folder { "New folder" } else { "New note" })
            .id(egui::Id::new("tg-create"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("in {}/", dlg.label)).weak());
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut dlg.buf)
                        .hint_text(if dlg.folder {
                            "folder or sub/folder"
                        } else {
                            "name or sub/name"
                        })
                        .desired_width(260.0),
                );
                if dlg.focus {
                    resp.request_focus();
                    dlg.focus = false;
                }
                if let Some(e) = &dlg.err {
                    ui.colored_label(SELECT, e);
                }
                submit = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.horizontal(|ui| {
                    submit |= ui.button("Create").clicked();
                    cancel =
                        ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
                });
            });
        if submit {
            let res = if dlg.folder {
                create::folder_rel_path(&dlg.dir, &dlg.buf)
                    .and_then(|rel| create::make_folder(&self.root, &rel).map(|_| rel))
            } else {
                create::note_rel_path(&dlg.dir, &dlg.buf)
                    .and_then(|rel| create::write_note(&self.root, &rel).map(|_| rel))
            };
            match res {
                Ok(rel) if dlg.folder => {
                    // empty dirs are pruned from the graph, deliberately
                    self.set_flash(format!(
                        "created {rel}/ — appears once it holds a note (\"sub/name\" in New note also creates folders)"
                    ));
                }
                Ok(rel) => {
                    self.pending_select = Some(rel.clone());
                    self.set_flash(format!("created {rel}"));
                    *self.reload_at.lock().unwrap() = Some(Instant::now());
                }
                Err(e) => {
                    dlg.err = Some(e.to_string());
                    dlg.focus = true;
                    self.create = Some(dlg); // stay open for a correction
                }
            }
        } else if !cancel {
            self.create = Some(dlg);
        }
    }

    /// Open the selection externally — always in a NEW window. Files go to
    /// the editor: terminal editors ($EDITOR=nvim etc.) are wrapped in a
    /// fresh terminal emulator instead of hijacking whatever terminal
    /// launched the viewer; GUI editors open their own windows anyway. Dirs
    /// open in the file manager; ghosts have nothing to open.
    fn open_in_editor(&self, id: NodeId) {
        let node = self.g.node(id);
        let path = self.root.join(&node.path);
        let result = match node.kind {
            NodeKind::File => spawn_editor(&path),
            NodeKind::Dir => detached(std::process::Command::new("xdg-open").arg(&path)),
            NodeKind::Ghost => return,
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
            self.term_scores.clear();
            self.term_best = None;
            return;
        }
        // Terminals re-score every frame (cheap: a handful of panes) — the
        // pane list changes underneath the node-score cache.
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut tbest: Option<(u32, usize)> = None;
        self.term_scores = self
            .agent_panes
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
        self.term_best = tbest.map(|(_, i)| {
            let a = &self.agent_panes[i];
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

    fn load_body(&self, id: NodeId) -> String {
        let node = self.g.node(id);
        match node.kind {
            NodeKind::File => vault::read_body(&self.root.join(&node.path))
                .unwrap_or_else(|e| format!("*error reading file:* {e}")),
            _ => String::new(),
        }
    }

    /// Live find-in-directory (`f`): when the query changed, jump the
    /// cursor to the best fuzzy match among the current listing (the
    /// selection's siblings; the root searches its children).
    fn nav_find_apply(&mut self) {
        let Some(q) = self.nav_find.clone() else {
            return;
        };
        if q == self.nav_find_last {
            return;
        }
        self.nav_find_last = q.clone();
        let Some(sel) = self.selected else { return };
        if q.is_empty() {
            return;
        }
        let candidates = match self.g.node(sel).parent {
            Some(p) => self.g.node(p).children.clone(),
            None => self.g.node(sel).children.clone(),
        };
        let pattern = Pattern::parse(&q, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut best: Option<(u32, NodeId)> = None;
        for c in candidates {
            let n = self.g.node(c);
            let hay = format!("{} {}", n.display_name(), n.name);
            if let Some(s) = pattern.score(Utf32Str::new(&hay, &mut buf), &mut self.matcher)
                && best.is_none_or(|(bs, _)| s > bs)
            {
                best = Some((s, c));
            }
        }
        if let Some((_, id)) = best
            && Some(id) != self.selected
        {
            self.selected = Some(id);
            self.frame_node(id);
            self.nav_scroll = true;
        }
    }

    /// The ranger-style navigator: breadcrumb, sibling column with the
    /// cursor, preview column. Keyboard walking happens in `handle_keys`
    /// (hjkl / gg / G while a node is selected); this renders the state and
    /// accepts clicks.
    fn detail_pane(&mut self, ui: &mut egui::Ui) {
        self.nav_find_apply();
        let Some(sel) = self.selected else { return };
        if self.detail.as_ref().map(|(id, _)| *id) != Some(sel) {
            self.detail = Some((sel, self.load_body(sel)));
        }
        // Owned copies so the panel closures below can borrow self freely.
        let (kind, display, sub, parent) = {
            let node = self.g.node(sel);
            let sub = if node.path.is_empty() {
                node.name.clone()
            } else {
                node.path.clone()
            };
            (node.kind, node.display_name().to_string(), sub, node.parent)
        };

        ui.set_min_width(430.0);
        ui.add_space(6.0);
        let mut jump: Option<NodeId> = None;

        // breadcrumb: clickable ancestors, root first
        let mut chain: Vec<NodeId> = Vec::new();
        let mut cur = parent;
        while let Some(p) = cur {
            chain.push(p);
            cur = self.g.node(p).parent;
        }
        chain.reverse();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for a in &chain {
                let name = self.g.node(*a).display_name().to_string();
                if ui.link(egui::RichText::new(name).color(DIR)).clicked() {
                    jump = Some(*a);
                }
                ui.label(egui::RichText::new("/").weak());
            }
            ui.label(egui::RichText::new(&display).strong());
        });
        ui.label(egui::RichText::new(sub).small().color(TEXT));
        ui.separator();

        // ranger columns: siblings (cursor) | preview of the selection
        let sibs: Vec<NodeId> = match parent {
            Some(p) => self.g.node(p).children.clone(),
            None => vec![sel], // root (and ghosts): a list of one
        };
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.vertical(|ui| {
                ui.set_width(150.0);
                // find-in-directory prompt (f): lives while it has focus
                let mut close_find = false;
                if let Some(q) = &mut self.nav_find {
                    let resp = ui.add(
                        egui::TextEdit::singleline(q)
                            .hint_text("find…")
                            .desired_width(140.0),
                    );
                    if self.nav_find_focus {
                        resp.request_focus();
                        self.nav_find_focus = false;
                    } else if resp.lost_focus() {
                        close_find = true; // Enter, Esc, or a click elsewhere
                    }
                }
                if close_find {
                    self.nav_find = None;
                    self.nav_find_last.clear();
                }
                egui::ScrollArea::vertical()
                    .id_salt("nav-sibs")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for c in &sibs {
                            let n = self.g.node(*c);
                            let is_dir = n.kind == NodeKind::Dir;
                            let label = if is_dir {
                                format!("{}/", n.display_name())
                            } else {
                                n.display_name().to_string()
                            };
                            let mut text = egui::RichText::new(label);
                            if is_dir {
                                text = text.color(DIR);
                            }
                            let resp = ui.selectable_label(*c == sel, text);
                            if *c == sel && self.nav_scroll {
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                            if resp.clicked() {
                                jump = Some(*c);
                            }
                        }
                    });
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                match kind {
                    NodeKind::File => {
                        if ui.button("open in editor  (Enter / l)").clicked() {
                            self.open_in_editor(sel);
                        }
                        // wikilink neighborhood: ] follows, [ backtracks
                        let outs: Vec<NodeId> = self.g.outlinks(sel).map(|l| l.to).collect();
                        let backs: Vec<NodeId> = self.g.backlinks(sel).map(|l| l.from).collect();
                        for (arrow, ids) in [("→", outs), ("←", backs)] {
                            if ids.is_empty() {
                                continue;
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(egui::RichText::new(arrow).color(WIKI));
                                for id in ids {
                                    let name = self.g.node(id).display_name().to_string();
                                    if ui
                                        .link(egui::RichText::new(name).color(WIKI).small())
                                        .clicked()
                                    {
                                        jump = Some(id);
                                    }
                                }
                            });
                        }
                        ui.add_space(4.0);
                        // take/put-back so the markdown cache and the body can
                        // be borrowed simultaneously without a per-frame clone
                        let detail = self.detail.take();
                        if let Some((_, body)) = &detail {
                            egui::ScrollArea::vertical()
                                .id_salt("nav-preview")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    CommonMarkViewer::new().show(ui, &mut self.md_cache, body);
                                });
                        }
                        self.detail = detail;
                    }
                    NodeKind::Dir => {
                        let children = self.g.node(sel).children.clone();
                        egui::ScrollArea::vertical()
                            .id_salt("nav-preview")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.label(format!("{} entries — l enters", children.len()));
                                ui.add_space(4.0);
                                for c in children {
                                    let child = self.g.node(c);
                                    let icon = if child.kind == NodeKind::Dir {
                                        "▸ "
                                    } else {
                                        "· "
                                    };
                                    if ui.link(format!("{icon}{}", child.display_name())).clicked()
                                    {
                                        jump = Some(c);
                                    }
                                }
                            });
                    }
                    NodeKind::Ghost => {
                        ui.label("Not written yet. Referenced from:");
                        ui.add_space(4.0);
                        let refs: Vec<NodeId> = self.g.backlinks(sel).map(|l| l.from).collect();
                        for r in refs {
                            if ui.link(self.g.node(r).path.clone()).clicked() {
                                jump = Some(r);
                            }
                        }
                    }
                }
            });
        });
        self.nav_scroll = false;
        if let Some(j) = jump {
            self.selected = Some(j);
            self.frame_node(j);
            self.nav_scroll = true;
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
            self.term_rects
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
                        self.term_rects
                            .iter()
                            .rev()
                            .find(|(s, p, _)| (s, p) == (&t.0, &t.1)),
                    )
                    .is_some_and(|(pos, (_, _, r))| resize_handle(*r).contains(pos));
                let ours = self
                    .agent_panes
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
                    && let Some(c) = self.term_cache.get(&t)
                    && let Some(pos) = response.hover_pos()
                {
                    let f = (6.0 * self.zoom).clamp(2.5, 16.0);
                    let probe = ui.painter().layout_no_wrap(
                        "M".into(),
                        FontId::monospace(f),
                        Color32::WHITE,
                    );
                    let cur = (c.cols, c.total_rows);
                    self.resize_term = Some(ResizeDrag {
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
                        .term_rects
                        .iter()
                        .find(|(s, p, _)| (s, p) == (&t.0, &t.1))
                        .map(|(_, _, r)| r.min);
                    let anchor_s = self
                        .agent_panes
                        .iter()
                        .find(|a| a.session == t.0 && a.pane == t.1)
                        .map(|a| {
                            let id = self.anchor_for(&a.cwd);
                            self.to_screen(rect, self.world_pos(id.0 as usize))
                        });
                    if let (Some(min), Some(anchor_s)) = (cur_min, anchor_s) {
                        self.term_offsets
                            .insert(t.clone(), (min - anchor_s) / self.zoom);
                    }
                    self.drag_card = Some(t);
                }
            } else {
                self.drag_node = self.hover;
            }
        }
        if response.dragged() {
            if self.resize_term.is_some() {
                if let (Some(rz), Some(cur)) =
                    (self.resize_term.as_mut(), response.interact_pointer_pos())
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
                        if let Some(m) = self.mirrors.get_mut(&sess) {
                            m.command(&cmd);
                        }
                    }
                }
                ui.ctx().request_repaint();
            } else if let Some(t) = self.drag_card.clone() {
                if let Some(off) = self.term_offsets.get_mut(&t) {
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
            self.drag_card = None;
            if let Some(rz) = self.resize_term.take()
                && rz.want != rz.sent
                && let Some(m) = self.mirrors.get_mut(&rz.key.0)
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
                    .agent_panes
                    .iter()
                    .position(|a| a.session == t.0 && a.pane == t.1)
                {
                    self.term_cycle = i + 1;
                }
                self.term_selected = Some(t.clone());
                self.focused_term = Some(t);
                self.close_search();
            } else if self.focused_term.is_some() {
                self.focused_term = None; // click-away releases; click again to select
            } else {
                self.term_selected = None;
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
                self.agent_panes
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
        } else if let Some((s, p)) = &self.focused_term {
            format!("typing into {s} {p} — Ctrl+Q or click away releases")
        } else if searching {
            let count = self.scores.iter().filter(|s| s.is_some()).count();
            let tcount = self.term_scores.iter().flatten().count();
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
            && let Some((s, p)) = &self.term_selected
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

/// Editors that run inside a terminal and therefore need one opened for them.
const TERMINAL_EDITORS: &[&str] = &[
    "vim", "nvim", "vi", "nano", "micro", "hx", "helix", "kak", "vis", "ne",
];

fn spawn_editor(file: &Path) -> std::io::Result<()> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    let Some(editor) = editor else {
        return detached(std::process::Command::new("xdg-open").arg(file));
    };
    // $EDITOR may carry args ("code --wait") — split on whitespace
    let mut parts = editor.split_whitespace();
    let prog = parts.next().unwrap_or("xdg-open").to_string();
    let args: Vec<String> = parts.map(str::to_string).collect();
    let base = Path::new(&prog)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&prog);
    if TERMINAL_EDITORS.contains(&base)
        && let Some(mut term) = new_terminal_window()
    {
        term.arg(&prog).args(&args).arg(file);
        return detached(&mut term);
    }
    detached(std::process::Command::new(&prog).args(&args).arg(file))
}

/// A command that opens a new terminal-emulator window and runs whatever is
/// appended to it. $TERMINAL wins; otherwise the first emulator on PATH.
fn new_terminal_window() -> Option<std::process::Command> {
    let mk = |bin: &str, extra: &[&str]| -> std::process::Command {
        let mut c = std::process::Command::new(bin);
        c.args(extra); // user-supplied flags go before the command separator
        let base = Path::new(bin)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(bin);
        match base {
            "gnome-terminal" => {
                c.arg("--");
            }
            "wezterm" => {
                c.args(["start", "--"]);
            }
            "kitty" | "foot" => {} // these take the command directly
            _ => {
                c.arg("-e"); // the de-facto convention
            }
        }
        c
    };
    if let Ok(term) = std::env::var("TERMINAL") {
        // "$TERMINAL" may carry flags ("foot -a floating"), like $EDITOR
        let mut words = term.split_whitespace();
        if let Some(bin) = words.next() {
            return Some(mk(bin, &words.collect::<Vec<_>>()));
        }
    }
    [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "foot",
        "alacritty",
        "kitty",
        "wezterm",
        "xterm",
    ]
    .into_iter()
    .find(|bin| on_path(bin))
    .map(|bin| mk(bin, &[]))
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Spawn fully detached from our stdio, so even a mis-detected terminal
/// editor can never take over the terminal the viewer was launched from.
fn detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
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
            // scan+build on a worker (45ms at 500 files — a visible hitch
            // if done here, and agents save constantly); only the cheap
            // carry-over runs on the UI thread when the result lands
            self.reload_gen += 1;
            let generation = self.reload_gen;
            let root = self.root.clone();
            let tx = self.reload_tx.clone();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let res = vault::scan(&root).map(graph::build);
                let _ = tx.send((generation, res));
                ctx.request_repaint();
            });
        }
        while let Ok((generation, res)) = self.reload_rx.try_recv() {
            if generation != self.reload_gen {
                continue; // superseded by a newer save — discard stale build
            }
            match res {
                Ok(g) => self.apply_graph(g),
                Err(e) => self.reload_error = Some(format!("{e:#}")),
            }
        }
        let release = ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Q));
        if release {
            self.focused_term = None;
        }
        if self.focused_term.is_some() && self.create.is_none() {
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
