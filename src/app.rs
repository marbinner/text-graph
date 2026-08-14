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

/// Zoom level a double-clicked card flies to — full styled screen, readable.
const CARD_ZOOM: f32 = 2.2;

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

/// Bottom-right corner zone of a card that resizes instead of moving.
fn resize_handle(card: Rect) -> Rect {
    Rect::from_min_max(card.max - Vec2::splat(16.0), card.max)
}

/// Two diagonal grip lines marking the resize corner.
fn paint_resize_grip(p: &egui::Painter, card: Rect, color: Color32) {
    let m = card.max;
    p.line_segment(
        [
            Pos2::new(m.x - 11.0, m.y - 3.0),
            Pos2::new(m.x - 3.0, m.y - 11.0),
        ],
        Stroke::new(1.5, color),
    );
    p.line_segment(
        [
            Pos2::new(m.x - 6.0, m.y - 3.0),
            Pos2::new(m.x - 3.0, m.y - 6.0),
        ],
        Stroke::new(1.5, color),
    );
}

/// In-flight native resize of a graph-launched session via its corner grip:
/// the drag maps to `resize-window` on the real tmux pane (debounced), and
/// the card follows through the normal mirror resize path.
struct ResizeDrag {
    key: (String, String),
    cols0: f32,
    rows0: f32,
    start: Pos2,
    adv: f32,
    line_h: f32,
    want: (u16, u16),
    sent: (u16, u16),
    last_sent: Instant,
}

// ---- terminal cards ----
const AGENT: Color32 = Color32::from_rgb(0x4e, 0xc9, 0x8b);
const TERM_BG: Color32 = Color32::from_rgb(0x10, 0x13, 0x19);
const TERM_BORDER: Color32 = Color32::from_rgb(0x3a, 0x40, 0x4d);
const TERM_FG_T: (u8, u8, u8) = (0xc9, 0xd1, 0xd9);
const TERM_BG_T: (u8, u8, u8) = (0x10, 0x13, 0x19);

/// One style-run of a terminal row, precomputed at cache time so painting is
/// a straight pass.
struct Run {
    start_col: u16,
    text: String,
    fg: Color32,
    bg: Option<Color32>,
    italic: bool,
    underline: bool,
}

/// A pane's screen, converted once per mirror generation.
struct CachedPane {
    cols: u16,
    /// Real pane height (rows) — the resize gesture's baseline.
    total_rows: u16,
    rows: Vec<Vec<Run>>, // trailing blank rows trimmed
    cursor: Option<(u16, u16)>,
    /// Pane app requested bracketed paste — pastes get ESC[200~ markers.
    bracketed_paste: bool,
}

fn brighten(c: Color32) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(45),
        c.g().saturating_add(45),
        c.b().saturating_add(45),
    )
}

fn build_cached(grid: &TermGrid) -> CachedPane {
    let cols = grid.cols as usize;
    let blank_row = |r: usize| {
        grid.cells[r * cols..(r + 1) * cols]
            .iter()
            .all(|c| c.ch == ' ' && c.bg.is_none() && !c.inverse)
    };
    let mut shown = 0;
    for r in 0..grid.rows as usize {
        if !blank_row(r) {
            shown = r + 1;
        }
    }
    if let Some((cr, _)) = grid.cursor {
        shown = shown.max(cr as usize + 1);
    }
    // never exceed the real row count — a 1-row pane must not slice past the
    // cells vec (was a reachable panic)
    let max_rows = grid.rows.max(1) as usize;
    let shown = shown.max(2).min(max_rows);

    let mut rows = Vec::with_capacity(shown);
    for r in 0..shown {
        let mut runs: Vec<Run> = Vec::new();
        for (ci, cell) in grid.cells[r * cols..(r + 1) * cols].iter().enumerate() {
            let (mut fg_t, mut bg_t) = (cell.fg.unwrap_or(TERM_FG_T), cell.bg);
            if cell.inverse {
                let old = fg_t;
                fg_t = bg_t.unwrap_or(TERM_BG_T);
                bg_t = Some(old);
            }
            let mut fg = Color32::from_rgb(fg_t.0, fg_t.1, fg_t.2);
            if cell.bold {
                fg = brighten(fg);
            }
            let bg = bg_t.map(|(r, g, b)| Color32::from_rgb(r, g, b));
            match runs.last_mut() {
                Some(run)
                    if run.fg == fg
                        && run.bg == bg
                        && run.italic == cell.italic
                        && run.underline == cell.underline =>
                {
                    run.text.push(cell.ch);
                }
                _ => runs.push(Run {
                    start_col: ci as u16,
                    text: cell.ch.to_string(),
                    fg,
                    bg,
                    italic: cell.italic,
                    underline: cell.underline,
                }),
            }
        }
        rows.push(runs);
    }
    CachedPane {
        cols: grid.cols,
        total_rows: grid.rows,
        rows,
        cursor: grid.cursor,
        bracketed_paste: grid.bracketed_paste,
    }
}

fn hash_angle(session: &str, pane: &str, index: usize) -> f32 {
    let mut h: u32 = 17;
    for b in session.bytes().chain(pane.bytes()) {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    h = h.wrapping_add(index as u32 * 97);
    (h % 628) as f32 / 100.0
}

fn map_key(key: Key) -> Option<Special> {
    Some(match key {
        Key::Enter => Special::Enter,
        Key::Tab => Special::Tab,
        Key::Backspace => Special::Backspace,
        Key::Escape => Special::Escape,
        Key::ArrowUp => Special::Up,
        Key::ArrowDown => Special::Down,
        Key::ArrowLeft => Special::Left,
        Key::ArrowRight => Special::Right,
        Key::Home => Special::Home,
        Key::End => Special::End,
        Key::PageUp => Special::PageUp,
        Key::PageDown => Special::PageDown,
        Key::Delete => Special::Delete,
        Key::Insert => Special::Insert,
        Key::F1 => Special::F(1),
        Key::F2 => Special::F(2),
        Key::F3 => Special::F(3),
        Key::F4 => Special::F(4),
        Key::F5 => Special::F(5),
        Key::F6 => Special::F(6),
        Key::F7 => Special::F(7),
        Key::F8 => Special::F(8),
        Key::F9 => Special::F(9),
        Key::F10 => Special::F(10),
        Key::F11 => Special::F(11),
        Key::F12 => Special::F(12),
        _ => return None,
    })
}

/// Characters reachable for Ctrl chords (plain characters arrive as text).
fn key_char(key: Key) -> Option<char> {
    use Key::*;
    Some(match key {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        Num0 => '0',
        Num1 => '1',
        Num2 => '2',
        Num3 => '3',
        Num4 => '4',
        Num5 => '5',
        Num6 => '6',
        Num7 => '7',
        Num8 => '8',
        Num9 => '9',
        Space => ' ',
        OpenBracket => '[',
        CloseBracket => ']',
        Backslash => '\\',
        Slash => '/',
        Minus => '-',
        _ => return None,
    })
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

    /// Forward this frame's keyboard input to the focused pane, DRAINING the
    /// keyboard events so egui's own focus/shortcut machinery never sees
    /// them — otherwise Tab moves widget focus to a detail-pane button and
    /// Enter fake-clicks it (spawning an editor) while the user is just
    /// typing into a terminal. Pointer/window events are put back untouched.
    ///
    /// Text and Ctrl-chords go as raw hex; special keys go as tmux key names
    /// so tmux applies the pane's terminal modes. Ctrl+C/X arrive from
    /// egui-winit as clipboard events with NO Key event, so they're handled
    /// as Copy/Cut — without that, an agent can't be interrupted.
    fn forward_input(&mut self, ui: &egui::Ui) {
        let Some((session, pane)) = self.focused_term.clone() else {
            return;
        };
        if !self.mirrors.contains_key(&session) {
            self.focused_term = None;
            return;
        }
        let frame_mods = ui.ctx().input(|i| i.modifiers);
        let events = ui.ctx().input_mut(|i| std::mem::take(&mut i.events));
        let mut keep: Vec<egui::Event> = Vec::with_capacity(events.len());
        let mut cmds: Vec<String> = Vec::new();
        for ev in events {
            match ev {
                egui::Event::Text(ref t) if !t.is_empty() => {
                    // Alt+letter chords (readline word motion) arrive as BOTH
                    // a Text event and a Key event on X11; the Key arm sends
                    // the ESC-prefixed chord, so drop the bare text — but
                    // only for ASCII alphanumerics, so AltGr-composed chars
                    // (€, ñ — non-ASCII) still type normally.
                    let alt_chord = frame_mods.alt
                        && !frame_mods.ctrl
                        && t.chars().all(|c| c.is_ascii_alphanumeric());
                    if !alt_chord {
                        cmds.push(keys::hex_cmd(&pane, t.as_bytes()));
                    }
                }
                egui::Event::Paste(ref t) if !t.is_empty() => {
                    // Bracketed paste when the pane app asked for it: without
                    // the markers, every newline in a multiline paste reads
                    // as Enter — a REPL or agent prompt submits mid-paste.
                    let bracketed = self
                        .term_cache
                        .get(&(session.clone(), pane.clone()))
                        .is_some_and(|c| c.bracketed_paste);
                    if bracketed {
                        let mut bytes = b"\x1b[200~".to_vec();
                        bytes.extend_from_slice(t.as_bytes());
                        bytes.extend_from_slice(b"\x1b[201~");
                        cmds.push(keys::hex_cmd(&pane, &bytes));
                    } else {
                        cmds.push(keys::hex_cmd(&pane, t.as_bytes()));
                    }
                }
                egui::Event::Copy => cmds.push(keys::hex_cmd(&pane, &[0x03])), // Ctrl+C
                egui::Event::Cut => cmds.push(keys::hex_cmd(&pane, &[0x18])),  // Ctrl+X
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let mods = Mods {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                    };
                    if let Some(sp) = map_key(key) {
                        cmds.push(keys::special_cmd(&pane, sp, mods));
                    } else if (mods.ctrl || mods.alt)
                        && let Some(c) = key_char(key)
                        && let Some(cmd) = keys::chord_cmd(&pane, c, mods)
                    {
                        cmds.push(cmd);
                    }
                }
                egui::Event::Key { .. } => {} // key releases: swallow
                other => keep.push(other),
            }
        }
        ui.ctx().input_mut(|i| {
            let mut late = std::mem::take(&mut i.events);
            keep.append(&mut late);
            i.events = keep;
        });
        if !cmds.is_empty()
            && let Some(m) = self.mirrors.get_mut(&session)
        {
            for c in &cmds {
                m.command(c);
            }
        }
    }

    /// Poll tmux for agent panes anchored in this vault (default server),
    /// publish diffs, wake the UI.
    fn start_agent_scan(&self, ctx: egui::Context) {
        let shared = self.agents_seen.clone();
        let root = self.root.clone();
        std::thread::spawn(move || {
            let allow = agents::default_allowlist();
            let mut tracker = agents::Tracker::new();
            loop {
                let panes = agents::scan(&root);
                let active = tracker.update(&panes, &allow, Instant::now());
                {
                    let mut s = shared.lock().unwrap();
                    if *s != active {
                        *s = active;
                        ctx.request_repaint();
                    }
                }
                std::thread::sleep(Duration::from_millis(1500));
            }
        });
    }

    /// Snapshot discovery, keep mirrors in sync with it, pump events, and
    /// refresh converted screens per mirror generation.
    fn sync_terminals(&mut self, ctx: &egui::Context) {
        self.agent_panes = self.agents_seen.lock().unwrap().clone();

        let sessions: HashSet<String> =
            self.agent_panes.iter().map(|a| a.session.clone()).collect();
        for s in &sessions {
            if !self.mirrors.contains_key(s) {
                // failed attaches back off — retrying at frame rate would
                // spawn tmux processes 60 times a second
                if self
                    .attach_backoff
                    .get(s)
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
                {
                    continue;
                }
                let c = ctx.clone();
                // Never pass a size for discovered sessions — the user may be
                // viewing them in a real terminal.
                match SessionMirror::attach(s, None, None, move || c.request_repaint()) {
                    Ok(m) => {
                        self.mirrors.insert(s.clone(), m);
                        self.attach_backoff.remove(s);
                    }
                    Err(_) => {
                        self.attach_backoff.insert(s.clone(), Instant::now());
                    }
                }
            }
        }
        self.attach_backoff.retain(|s, _| sessions.contains(s));
        self.mirrors
            .retain(|s, m| !m.exited && sessions.contains(s));
        self.term_cache.retain(|(s, _), _| sessions.contains(s));
        self.term_gen.retain(|s, _| sessions.contains(s));
        // Arrangements are never dropped, only parked by session name and
        // reclaimed when a session with that name reappears (exact pane
        // first, then any spot) — including across viewer restarts via
        // .text-graph/view. Logic lives in state.rs, where it's unit-tested.
        state::park_absent(&mut self.term_offsets, &mut self.restore_offsets, &sessions);
        let pane_keys: Vec<(String, String)> = self
            .agent_panes
            .iter()
            .map(|a| (a.session.clone(), a.pane.clone()))
            .collect();
        state::claim(
            &mut self.term_offsets,
            &mut self.restore_offsets,
            &pane_keys,
        );
        let focus_dead = self
            .focused_term
            .as_ref()
            .is_some_and(|(s, _)| !self.mirrors.contains_key(s));
        if focus_dead {
            self.focused_term = None;
        }
        if self.term_selected.as_ref().is_some_and(|(s, p)| {
            !self
                .agent_panes
                .iter()
                .any(|a| &a.session == s && &a.pane == p)
        }) {
            self.term_selected = None;
        }
        // A focused pane killed externally (session survives) must release
        // the keyboard — otherwise every keystroke drains into a dead
        // target while all graph keybinds stay suspended.
        if self.focused_term.as_ref().is_some_and(|(s, p)| {
            !self
                .agent_panes
                .iter()
                .any(|a| &a.session == s && &a.pane == p)
        }) {
            self.focused_term = None;
        }

        for (s, m) in &mut self.mirrors {
            if m.pump() {
                self.term_activity.insert(s.clone(), Instant::now());
            }
            let mgen = m.generation();
            if self.term_gen.get(s).copied() != Some(mgen) {
                self.term_gen.insert(s.clone(), mgen);
                let grids = m.grids();
                self.term_cache
                    .retain(|(cs, cp), _| cs != s || grids.iter().any(|(p, _)| p == cp));
                for (pane, grid) in grids {
                    self.term_cache
                        .insert((s.clone(), pane), build_cached(&grid));
                }
            }
        }

        // glow fades need a few follow-up frames
        if self
            .term_activity
            .values()
            .any(|t| t.elapsed() < Duration::from_secs(2))
        {
            ctx.request_repaint_after(Duration::from_millis(150));
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

    fn paint_terminals(&mut self, painter: &egui::Painter, rect: Rect, view: Rect) {
        self.term_rects.clear();
        // one shot per double-click, whether or not the card still exists
        let recenter = self.zoom_to_card.take();
        if self.agent_panes.is_empty() {
            return;
        }
        let f = (6.0 * self.zoom).clamp(2.5, 16.0);
        let compact = f < 5.0;
        let font = FontId::monospace(f);
        // measured monospace advance keeps columns honest at any zoom
        let probe = painter.layout_no_wrap("M".into(), font.clone(), Color32::WHITE);
        let (adv, line_h) = (probe.size().x, probe.size().y);
        let title_h = 16.0;
        let pad = 6.0;

        for (i, a) in self.agent_panes.iter().enumerate() {
            let key = (a.session.clone(), a.pane.clone());
            let Some(c) = self.term_cache.get(&key) else {
                continue;
            };
            let focused = self.focused_term.as_ref() == Some(&key);
            let anchor = self.anchor_for(&a.cwd);
            let anchor_w = self.world_pos(anchor.0 as usize);
            let anchor_s = self.to_screen(rect, anchor_w);
            let size = if compact {
                Vec2::new(230.0, 40.0)
            } else {
                Vec2::new(
                    c.cols as f32 * adv + pad * 2.0,
                    c.rows.len() as f32 * line_h + title_h + pad * 2.0,
                )
            };
            // User-arranged cards keep their world-space offset from the
            // anchor; otherwise place outward from the graph center relative
            // to the anchor (jittered so several cards fan out), past the
            // node's radius in screen space — the tether points inward and
            // the card never sits on top of the cluster it's attached to.
            let (card, tether_to) = if let Some(off) = self.term_offsets.get(&key) {
                let card = Rect::from_min_size(anchor_s + *off * self.zoom, size);
                (card, card.center())
            } else {
                let jitter = (hash_angle(&a.session, &a.pane, i) - std::f32::consts::PI) * 0.25;
                let base = if anchor_w.to_vec2().length() > 1.0 {
                    anchor_w.to_vec2().angle()
                } else {
                    hash_angle(&a.session, &a.pane, i)
                };
                let dir = Vec2::angled(base + jitter);
                let anchor_r = (self.radius[anchor.0 as usize] * self.zoom).clamp(1.5, 16.0);
                let p = anchor_s + dir * (anchor_r + 22.0);
                let min = Pos2::new(
                    if dir.x >= 0.0 { p.x } else { p.x - size.x },
                    if dir.y >= 0.0 { p.y } else { p.y - size.y },
                );
                (Rect::from_min_size(min, size), p)
            };
            // Double-click fly-in: the zoom jump already happened in the
            // input pass; now that the card's rect is known at the new zoom,
            // shift the view so the card sits centered.
            if recenter.as_ref() == Some(&key) {
                self.center += (card.center() - rect.center()) / self.zoom;
                painter.ctx().request_repaint();
            }
            if !view.intersects(card) {
                continue;
            }
            self.term_rects
                .push((a.session.clone(), a.pane.clone(), card));

            // drawn before the card, so it vanishes cleanly behind its edge
            painter.extend(egui::Shape::dashed_line(
                &[anchor_s, tether_to],
                Stroke::new(1.0, EDGE),
                6.0,
                4.0,
            ));

            let hot = self
                .term_activity
                .get(&a.session)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
            let searching = self.search_open && !self.query.is_empty();
            let smatch = searching && self.term_scores.get(i).copied().flatten().is_some();
            let sbest = searching && self.term_best.as_ref().is_some_and(|(_, bk)| bk == &key);
            let cursor = self.term_selected.as_ref() == Some(&key);
            let (border, bw) = if focused {
                (SELECT, 2.5)
            } else if sbest {
                (HOVER, 2.5)
            } else if smatch {
                (WIKI, 2.0)
            } else if cursor {
                (SELECT, 2.0)
            } else if hot {
                (AGENT, 1.5)
            } else {
                (TERM_BORDER, 1.5)
            };
            painter.rect_filled(card, 4.0, TERM_BG);
            painter.rect_stroke(card, 4.0, Stroke::new(bw, border), egui::StrokeKind::Inside);
            if sbest {
                // same ring the best node hit gets — Enter jumps here
                painter.rect_stroke(
                    card.expand(3.0),
                    6.0,
                    Stroke::new(1.5, HOVER),
                    egui::StrokeKind::Outside,
                );
            }
            // everything inside the card is clipped to it — no overflow, ever
            let cp = painter.with_clip_rect(card);
            let title = format!("{} · {} {}", a.agent, a.session, a.pane);
            cp.text(
                card.left_top() + Vec2::new(pad, 2.0),
                Align2::LEFT_TOP,
                title,
                FontId::proportional(11.0),
                if focused {
                    SELECT
                } else if hot {
                    AGENT
                } else {
                    TEXT
                },
            );

            if compact {
                // metadata beats a TUI's bottom status bar for glanceability
                let state = match self.term_activity.get(&a.session) {
                    Some(t) if t.elapsed() < Duration::from_secs(3) => "active".to_string(),
                    Some(t) => {
                        let s = t.elapsed().as_secs();
                        if s < 60 {
                            format!("idle {s}s")
                        } else {
                            format!("idle {}m", s / 60)
                        }
                    }
                    None => "quiet".to_string(),
                };
                let meta = format!("in {}/ · {}", self.g.node(anchor).display_name(), state);
                cp.text(
                    card.left_top() + Vec2::new(pad, 21.0),
                    Align2::LEFT_TOP,
                    meta,
                    FontId::proportional(10.5),
                    Color32::from_rgb(TERM_FG_T.0, TERM_FG_T.1, TERM_FG_T.2),
                );
                continue; // compact cards: no grip — resize is gated off too
            }

            let origin = card.left_top() + Vec2::new(pad, title_h + pad);
            for (r, row) in c.rows.iter().enumerate() {
                let y = origin.y + r as f32 * line_h;
                let mut job = egui::text::LayoutJob::default();
                for run in row {
                    if let Some(bg) = run.bg {
                        let x0 = origin.x + run.start_col as f32 * adv;
                        let w = run.text.chars().count() as f32 * adv;
                        cp.rect_filled(
                            Rect::from_min_size(Pos2::new(x0, y), Vec2::new(w, line_h)),
                            0.0,
                            bg,
                        );
                    }
                    let mut fmt = egui::TextFormat::simple(font.clone(), run.fg);
                    fmt.italics = run.italic;
                    if run.underline {
                        fmt.underline = Stroke::new(1.0, run.fg);
                    }
                    job.append(&run.text, 0.0, fmt);
                }
                let galley = cp.layout_job(job);
                cp.galley(Pos2::new(origin.x, y), galley, Color32::WHITE);
            }
            if let Some((cr, cc)) = c.cursor
                && (cr as usize) < c.rows.len()
            {
                let p0 = Pos2::new(origin.x + cc as f32 * adv, origin.y + cr as f32 * line_h);
                cp.rect_filled(
                    Rect::from_min_size(p0, Vec2::new(adv.max(2.0), line_h)),
                    0.0,
                    HOVER.gamma_multiply(0.55),
                );
            }
            if a.ours {
                paint_resize_grip(&cp, card, border);
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

    /// Re-scan the vault and swap the graph in, carrying over sim positions,
    /// selection, and search identity by path so an edit ripples the layout
    /// instead of re-settling it. Identity is `Node::ident` (ghosts
    /// namespaced — see graph.rs).
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
                !i.modifiers.command && i.key_pressed(Key::F),
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

        // vim-style navigation: hjkl pans, d/u zooms — continuous while held
        if ui.memory(|m| m.focused().is_none()) {
            let (dt, h, j, k, l, d, u) = ui.input(|i| {
                let m = i.modifiers.is_none();
                (
                    i.stable_dt.min(0.1),
                    m && i.key_down(Key::H),
                    m && i.key_down(Key::J),
                    m && i.key_down(Key::K),
                    m && i.key_down(Key::L),
                    m && i.key_down(Key::D),
                    m && i.key_down(Key::U),
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

    /// Jump the view into a card: readable zoom now, exact centering on the
    /// next paint (which knows the card's rect at the new zoom).
    fn fly_to_card(&mut self, t: (String, String)) {
        if let Some(id) = self
            .agent_panes
            .iter()
            .find(|a| a.session == t.0 && a.pane == t.1)
            .map(|a| self.anchor_for(&a.cwd))
        {
            self.center = self.world_pos(id.0 as usize);
        }
        self.zoom = CARD_ZOOM;
        self.zoom_to_card = Some(t);
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

    /// Open a real terminal window attached to the card's session, landed on
    /// its pane. `;` is tmux's command separator — it goes through argv
    /// unshelled, so no quoting games.
    fn attach_external(&mut self, session: &str, pane: &str) {
        let Some(mut cmd) = new_terminal_window() else {
            self.set_flash("no terminal emulator found — set $TERMINAL".into());
            return;
        };
        cmd.args(["tmux", "attach-session", "-t", &format!("={session}")]);
        cmd.args([
            ";",
            "select-window",
            "-t",
            pane,
            ";",
            "select-pane",
            "-t",
            pane,
        ]);
        match detached(&mut cmd) {
            Ok(()) => self.set_flash(format!("attaching {session} in a new terminal")),
            Err(e) => self.set_flash(format!("attach failed: {e}")),
        }
    }

    /// Kill just this pane (pane ids are server-global). tmux ends the
    /// session with its last pane, which removes the card via discovery.
    fn kill_pane(&mut self, session: &str, pane: &str) {
        let ok = std::process::Command::new("tmux")
            .args(["kill-pane", "-t", pane])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            if self
                .focused_term
                .as_ref()
                .is_some_and(|(fs, fp)| fs == session && fp == pane)
            {
                self.focused_term = None;
            }
            self.set_flash(format!("killed {session} {pane}"));
        } else {
            self.set_flash(format!("kill failed for {session} {pane}"));
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

    fn launch_agent(&mut self, dir: &str, agent: &str) {
        let path = self.ctx_path(dir);
        match agents::launch(None, &path, agent) {
            Ok(name) => self.set_flash(format!("launched {agent} — session {name}")),
            Err(e) => self.set_flash(format!("launch failed: {e}")),
        }
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

    fn detail_pane(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.selected else { return };
        if self.detail.as_ref().map(|(id, _)| *id) != Some(sel) {
            self.detail = Some((sel, self.load_body(sel)));
        }
        // Owned copies so the panel closures below can borrow self freely.
        let (kind, display, sub) = {
            let node = self.g.node(sel);
            let sub = if node.path.is_empty() {
                node.name.clone()
            } else {
                node.path.clone()
            };
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
                        let icon = if child.kind == NodeKind::Dir {
                            "▸ "
                        } else {
                            "· "
                        };
                        if ui.link(format!("{icon}{}", child.display_name())).clicked() {
                            jump = Some(c);
                        }
                    }
                });
            }
            NodeKind::Ghost => {
                ui.label("Not written yet. Referenced from:");
                ui.add_space(4.0);
                let refs: Vec<NodeId> = self
                    .g
                    .links
                    .iter()
                    .filter(|l| l.to == sel)
                    .map(|l| l.from)
                    .collect();
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
                    "{} files · {} dirs · {} links{}   |   / search · f frame · 0 reset · hjkl pan · d/u zoom · t terminals",
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
            self.rebuild();
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
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| self.canvas(ui));
        let ctx = ui.ctx().clone();
        self.create_dialog_ui(&ctx);
        self.persist_state(false);
        // egui repaints on demand; without a heartbeat the debounced save
        // would never run once the sim settles and input stops
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_state(true);
    }
}
