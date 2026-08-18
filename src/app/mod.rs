//! The egui shell: viewport transform, input, painting. Geometry comes from
//! `sim` (force-directed, seeded by the pure radial layout); this module owns
//! presentation and interaction only.
//!
//! One world→screen transform (`to_screen`/`to_world`) — every input handler
//! and paint call goes through it. Zoom is toward the cursor. Dragging a node
//! pins it and reheats the simulation; dragging empty space pans.

mod actions;
mod diag;
mod images;
mod keymap;
mod navigator;
mod picker;
mod previews;
mod reload;
mod settings;
mod terminals;

#[cfg(test)]
mod kb_tests;

use actions::CreateDialog;
use picker::Picker;
use reload::ReloadMsg;
use terminals::{ResizeDrag, TERM_BG, resize_handle};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
// `Config` in this tree means the user's settings — the matcher's is
// aliased so the two can never be confused at a call site.
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use text_graph::agents::{self, AgentPane};
use text_graph::config::Config;
use text_graph::graph::{Graph, LinkKind, NodeId, NodeKind};
use text_graph::keys::{self, Mods, Special};
use text_graph::mirror::{SessionMirror, TermGrid};
use text_graph::sim::Sim;
use text_graph::{config, create, filetype, graph, highlight, mdview, state, vault};

// ---- palette (dark) ----
const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
const EDGE: Color32 = Color32::from_rgb(0x3a, 0x40, 0x4d);
/// Contains (tree) edges — brighter and blue-leaning so the folder
/// skeleton reads at a glance.
const EDGE_TREE: Color32 = Color32::from_rgb(0x50, 0x5c, 0x7a);
const DIR: Color32 = Color32::from_rgb(0x7a, 0xa2, 0xf7);
const FILE: Color32 = Color32::from_rgb(0xb8, 0xbc, 0xc8);
/// Non-markdown, non-image leaves (code, config, data) — dimmer than
/// notes, which stay the brightest thing on the canvas.
const ASSET: Color32 = Color32::from_rgb(0x8b, 0x92, 0x9f);
const GHOST: Color32 = Color32::from_rgb(0x6b, 0x72, 0x82);
/// External web nodes and their edges — cyan says "leaves the vault".
const WEB: Color32 = Color32::from_rgb(0x56, 0xb6, 0xc2);
const IMG: Color32 = Color32::from_rgb(0x9e, 0xce, 0x6a);
const HOVER: Color32 = Color32::from_rgb(0xff, 0xb4, 0x54);
const SELECT: Color32 = Color32::from_rgb(0xff, 0x8a, 0x3d);
const WIKI: Color32 = Color32::from_rgb(0xe0, 0xaf, 0x68);
const TEXT: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xac);
/// Incoming links in the navigator's connections strip.
const LINK_IN: Color32 = Color32::from_rgb(0xbb, 0x9a, 0xf7);

/// Canvas + panel palette, switchable at runtime (⚙ settings). The module
/// consts above are the DARK values and stay the fixed palette for
/// terminal-card internals — terminals are dark in either theme.
#[derive(Clone, Copy)]
pub(super) struct Theme {
    pub(super) light: bool,
    pub(super) bg: Color32,
    pub(super) edge: Color32,
    pub(super) edge_tree: Color32,
    pub(super) dir: Color32,
    pub(super) file: Color32,
    pub(super) asset: Color32,
    pub(super) ghost: Color32,
    pub(super) img: Color32,
    pub(super) hover: Color32,
    pub(super) select: Color32,
    pub(super) wiki: Color32,
    pub(super) text: Color32,
    pub(super) link_in: Color32,
    pub(super) web: Color32,
    /// Card-like surfaces drawn on the canvas (text preview cards, image
    /// placeholder frames).
    pub(super) panel: Color32,
}

impl Theme {
    pub(super) fn dark() -> Self {
        Theme {
            light: false,
            bg: BG,
            edge: EDGE,
            edge_tree: EDGE_TREE,
            dir: DIR,
            file: FILE,
            asset: ASSET,
            ghost: GHOST,
            img: IMG,
            hover: HOVER,
            select: SELECT,
            wiki: WIKI,
            text: TEXT,
            link_in: LINK_IN,
            web: WEB,
            panel: TERM_BG,
        }
    }
    pub(super) fn light() -> Self {
        Theme {
            light: true,
            bg: Color32::from_rgb(0xf5, 0xf4, 0xef),
            edge: Color32::from_rgb(0xc6, 0xc9, 0xd2),
            edge_tree: Color32::from_rgb(0x9d, 0xa8, 0xc6),
            dir: Color32::from_rgb(0x33, 0x5e, 0xcc),
            file: Color32::from_rgb(0x45, 0x49, 0x54),
            asset: Color32::from_rgb(0x70, 0x76, 0x82),
            ghost: Color32::from_rgb(0x9a, 0xa0, 0xac),
            img: Color32::from_rgb(0x43, 0x7d, 0x24),
            hover: Color32::from_rgb(0xc2, 0x69, 0x06),
            select: Color32::from_rgb(0xb3, 0x50, 0x0a),
            wiki: Color32::from_rgb(0xa1, 0x6a, 0x0e),
            text: Color32::from_rgb(0x50, 0x56, 0x62),
            link_in: Color32::from_rgb(0x6f, 0x42, 0xc8),
            web: Color32::from_rgb(0x0b, 0x7f, 0x92),
            panel: Color32::WHITE,
        }
    }
    pub(super) fn get(light: bool) -> Self {
        if light { Self::light() } else { Self::dark() }
    }
}

/// Screen radius `G` frames a node at when it has no neighbours to show.
const LONE_NODE_R: f32 = 60.0;
/// Ceiling for `G` — a tight neighbourhood must not rocket the camera in.
const NEIGHBORHOOD_MAX_ZOOM: f32 = 6.0;

/// Screen radius above which a ghost shows its hollow-page silhouette.
const ICON_MIN_R: f32 = 6.5;

/// Screen radius above which a node paints as its file-type glyph instead
/// of a disc — glyphs are unreadable smaller than this.
const GLYPH_MIN_R: f32 = 4.0;

/// The bundled Nerd Font subset (assets/icons.ttf), registered as the
/// "icons" family at startup — file-type glyphs render with it.
fn install_icon_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "tg-icons".into(),
        egui::FontData::from_static(include_bytes!("../../assets/icons.ttf")).into(),
    );
    fonts.families.insert(
        egui::FontFamily::Name("icons".into()),
        vec!["tg-icons".into()],
    );
    ctx.set_fonts(fonts);
}

fn icon_font(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("icons".into()))
}

/// A file-type glyph followed by text as one galley — for list rows
/// (selectable labels, links) where the icon must ride along with the text
/// through egui's normal widgets.
fn icon_label(
    glyph: char,
    glyph_color: Color32,
    text: &str,
    text_color: Color32,
    size: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &glyph.to_string(),
        0.0,
        egui::TextFormat {
            font_id: icon_font(size),
            color: glyph_color,
            ..Default::default()
        },
    );
    job.append(
        text,
        6.0,
        egui::TextFormat {
            font_id: FontId::proportional(size),
            color: text_color,
            ..Default::default()
        },
    );
    job
}

/// A file-type glyph centered on the node, over a canvas-colored backing
/// disc — edges crossing behind a thin glyph would shred it, and the old
/// solid discs occluded them the same way.
fn paint_glyph_node(
    p: &egui::Painter,
    c: Pos2,
    r: f32,
    glyph: char,
    color: Color32,
    backing: Color32,
) {
    p.circle_filled(c, r, backing);
    p.text(c, Align2::CENTER_CENTER, glyph, icon_font(r * 2.3), color);
}

/// Label LOD ramp: labels ease in between these screen radii instead of
/// popping at a hard cutoff. Dirs surface earlier than leaves.
const LABEL_RAMP_DIR: (f32, f32) = (2.0, 3.2);
const LABEL_RAMP: (f32, f32) = (2.6, 4.2);

/// Was `key` pressed this frame as a FRESH press (key repeat excluded)?
/// Launch-style keybinds must not fire per repeat tick — holding e/t/a
/// would otherwise spawn a session per tick.
/// 0 below the ramp, 1 above it — how strongly a node's label shows at
/// this screen radius.
/// `density` (a setting) SHIFTS the ramp rather than reshaping it: at 2×
/// labels reach readable size at half the zoom, and the dir/leaf offset
/// survives.
fn label_lod(kind: NodeKind, r: f32, density: f32) -> f32 {
    let (lo, hi) = if kind == NodeKind::Dir {
        LABEL_RAMP_DIR
    } else {
        LABEL_RAMP
    };
    let d = density.max(0.05);
    let (lo, hi) = (lo / d, hi / d);
    ((r - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Side pane: the share of the window it opens at when the user has never
/// resized it, and the floor it can be dragged to. A preview wants ~70
/// characters of prose before it stops being a preview and becomes a
/// column of syllables.
const PANE_FRAC: f32 = 0.3;
const PANE_MIN: f32 = 340.0;

/// What the side pane should be this frame: the width the user last set,
/// else its share of the window. Clamped so a window resize can never
/// leave the pane wider than the canvas — the ONE case where it moves on
/// its own.
fn pane_width(win_w: f32, stored: Option<f32>) -> f32 {
    // the 60% ceiling outranks the floor: on a window too small for both,
    // the canvas keeps its share rather than the pane keeping its minimum
    let max = (win_w * 0.6).max(1.0);
    stored
        .unwrap_or(win_w * PANE_FRAC)
        .clamp(PANE_MIN.min(max), max)
}

/// Cursor flashlight: labels within this screen distance of the pointer are
/// revealed even when the zoom LOD would hide them.
const REVEAL_R: f32 = 130.0;
/// Cap on flashlight labels — a dense zoomed-out cluster would otherwise
/// dissolve into text soup.
const REVEAL_MAX: usize = 12;

/// The nearest lit nodes within `REVEAL_R` of the cursor, each with a 0..=1
/// proximity weight (1 at the cursor). Capped at `REVEAL_MAX`, nearest
/// first, ties broken by node index so the pick is deterministic.
fn reveal_near_cursor(
    cursor: Pos2,
    visible: &[(NodeId, Pos2, f32)],
    lit: &[bool],
) -> Vec<(NodeId, f32)> {
    let mut near: Vec<(f32, NodeId)> = visible
        .iter()
        .filter(|(id, _, _)| lit[id.0 as usize])
        .filter_map(|&(id, s, _)| {
            let d = s.distance(cursor);
            (d < REVEAL_R).then_some((d, id))
        })
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    near.truncate(REVEAL_MAX);
    near.into_iter()
        .map(|(d, id)| (id, 1.0 - d / REVEAL_R))
        .collect()
}

/// Darken RGB by `f` (alpha untouched) — depth shading for folders.
fn shade(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

/// The folder color at a given tree depth: the root's folders are the
/// brightest blue on the canvas, each level a shade darker.
fn dir_depth_color(base: Color32, depth: u8) -> Color32 {
    shade(base, 1.0 - depth.min(4) as f32 * 0.10)
}

/// Tapered tree edge: thick at the parent, thin at the child — hierarchy
/// and direction readable without an arrowhead per edge.
fn paint_tree_edge(p: &egui::Painter, parent: Pos2, child: Pos2, color: Color32) {
    let d = child - parent;
    let len = d.length();
    if len < 1.0 {
        return;
    }
    let perp = Vec2::new(-d.y, d.x) / len;
    let (wp, wc) = (2.1, 0.35);
    p.add(egui::Shape::convex_polygon(
        vec![
            parent + perp * wp,
            parent - perp * wp,
            child - perp * wc,
            child + perp * wc,
        ],
        color,
        Stroke::NONE,
    ));
}

/// Filled arrowhead with its TIP at `tip`, pointing along `dir`.
fn paint_arrowhead(p: &egui::Painter, tip: Pos2, dir: Vec2, size: f32, color: Color32) {
    let len = dir.length();
    if len < 0.5 {
        return;
    }
    let d = dir / len;
    let perp = Vec2::new(-d.y, d.x);
    p.add(egui::Shape::convex_polygon(
        vec![
            tip,
            tip - d * size + perp * (size * 0.45),
            tip - d * size - perp * (size * 0.45),
        ],
        color,
        Stroke::NONE,
    ));
}

/// Photo silhouette: a punched-out landscape frame with a sun dot and a
/// mountain wedge back in the disc color.
fn paint_img_icon(p: &egui::Painter, c: Pos2, r: f32, punch: Color32, disc: Color32) {
    let w = r * 1.05;
    let h = r * 0.85;
    let frame = Rect::from_center_size(c, Vec2::new(w, h));
    p.rect_filled(frame, r * 0.10, punch);
    p.circle_filled(frame.min + Vec2::new(w * 0.30, h * 0.32), r * 0.11, disc);
    let base = frame.max.y - h * 0.16;
    let pts = vec![
        Pos2::new(frame.min.x + w * 0.14, base),
        Pos2::new(frame.min.x + w * 0.55, frame.min.y + h * 0.40),
        Pos2::new(frame.min.x + w * 0.88, base),
    ];
    p.add(egui::Shape::convex_polygon(pts, disc, Stroke::NONE));
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
    depths: Vec<u8>,
    n_files: usize,
    n_dirs: usize,
    n_images: usize,
    n_assets: usize,
    n_webs: usize,
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
    let loaded = config::load_or_migrate(&root);
    let mut viewer = Viewer::new(graph::build(scan), root, loaded.config);
    viewer.config_error = loaded.read_error;
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
        install_icon_font(&cc.egui_ctx);
        // file:// + image-decode loaders — markdown previews render
        // embedded pictures through these
        egui_extras::install_image_loaders(&cc.egui_ctx);
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
    /// Tree depth per node (0 = root; ghosts/web nodes are 0).
    depths: Vec<u8>,
    /// World point currently at the viewport center.
    center: Pos2,
    /// Screen pixels per world unit.
    zoom: f32,
    hover: Option<NodeId>,
    /// (node, dwell start, screen anchor) — drives the full hover preview.
    hover_since: Option<(NodeId, Instant, Pos2)>,
    /// Body of the hovered file, read on demand (one at a time, like
    /// `detail`).
    hover_body: Option<(NodeId, String)>,
    selected: Option<NodeId>,
    drag_node: Option<NodeId>,
    fitted: bool,
    /// Canvas rect of the previous frame — camera compensation on change.
    last_canvas_rect: Option<Rect>,
    /// Active palette (dark/light) — swapped from the ⚙ settings window.
    theme: Theme,
    /// egui visuals need (re)applying (startup, theme toggle).
    apply_visuals: bool,
    /// The ⚙ window's own state (open, section, filter, pending edit).
    settings: settings::SettingsUi,
    /// User preferences (per user, not per vault) — see `config.rs`. The
    /// canvas reads it live, so every change shows on the next frame.
    cfg: Config,
    /// In-flight camera glide: (start center, target node, start time).
    /// The target is a NODE so a settling sim can't make the glide land
    /// beside it. Manual pan/zoom input cancels it.
    cam_anim: Option<(Pos2, NodeId, Instant)>,
    n_files: usize,
    n_dirs: usize,
    n_images: usize,
    n_assets: usize,
    n_webs: usize,
    /// Web nodes visible (the `w` toggle; persisted inverted as hide_web).
    show_web: bool,
    /// Side-pane width the user dragged to, persisted per vault. `None`
    /// until they DRAG it — a default must never be written back, or the
    /// first frame's window size (eframe opens at 1280 before the WM has
    /// its say) freezes the pane at a fraction of a window that no longer
    /// exists.
    pane_width: Option<f32>,
    // ---- search ----
    matcher: Matcher,
    /// The picker: prompt, ranked results, preview, content-scan worker.
    picker: Picker,
    // ---- detail pane ----
    root: PathBuf,
    md_cache: CommonMarkCache,
    /// Width the pane's body actually laid out at, measured each frame.
    /// A body that lays out wider than the pane is a body that will be
    /// CLIPPED at the window edge — the wrap-width regression test reads
    /// this, and a Cell so measuring can happen inside the paint closure.
    pane_content_w: std::cell::Cell<f32>,
    /// What the side pane is previewing — the finder's highlighted row
    /// while it is open, else the selection. Built by `sync_pane_preview`,
    /// drawn by `preview_pane`: one subject, one previewer.
    pane_preview: Option<picker::Preview>,
    /// Body of the previewed file, read on demand and cached per node…
    detail: Option<(NodeId, String)>,
    /// …with the (mtime, len) it was read at, so a reload re-reads only
    /// when that file actually changed.
    detail_stamp: Option<images::Stamp>,
    // ---- live reload ----
    /// Kept alive for the watcher thread; None if watching failed.
    _watcher: Option<notify::RecommendedWatcher>,
    /// Timestamp of the last relevant filesystem event (debounce state).
    reload_at: Arc<Mutex<Option<Instant>>>,
    /// Startup or callback failure from notify, separate from scan/build
    /// failures so a successful recovery scan cannot erase the warning.
    watch_error: Arc<Mutex<Option<String>>>,
    /// Monotonic reload request counter — results from superseded requests
    /// are discarded on arrival.
    reload_gen: u64,
    /// A scan+build worker is running — at most ONE at a time (a slow scan
    /// under a fast save cadence must not stack concurrent full walks).
    scan_inflight: bool,
    /// A debounce expired while a scan was in flight; run one trailing
    /// rescan when it lands.
    rescan_queued: bool,
    reload_tx: std::sync::mpsc::Sender<ReloadMsg>,
    reload_rx: std::sync::mpsc::Receiver<ReloadMsg>,
    /// Health state, surfaced by the diagnostics badge.
    last_reload: Option<Instant>,
    reload_error: Option<String>,
    /// A startup config read failure blocks saves: defaults keep the viewer
    /// usable, but must never overwrite a file we did not successfully load.
    config_error: Option<String>,
    diag_open: bool,
    // ---- terminals in the graph ----
    /// Dir path → node, for anchoring agent panes at their cwd.
    dir_by_path: HashMap<String, NodeId>,
    /// Everything terminal-card related — see terminals::Terminals.
    terms: terminals::Terminals,
    /// Thumbnail decode worker + texture cache for Image nodes.
    thumbs: images::Thumbs,
    /// Excerpt cache for zoomed-in File previews.
    previews: previews::Previews,
    // ---- view-state persistence ----
    /// View state as last written to `.text-graph/view` (skip no-op saves).
    saved_state: Option<state::ViewState>,
    /// Line kinds the loaded view file had that this version doesn't know —
    /// written back verbatim on every save (forward compatibility).
    view_unknown: Vec<String>,
    last_save: Instant,
    save_warned: bool,
    // ---- tree navigation ----
    /// First `g` of a `gg` chord, with its press time.
    pending_g: Option<Instant>,
    /// Scroll the navigator's sibling list to the cursor next frame.
    nav_scroll: bool,
    /// Cursor into the connections strip (] / [ step it, Enter/l jumps).
    conn_cursor: Option<usize>,
    // ---- creation (right-click menu) ----
    /// Node captured at right-click time — the context menu's subject.
    ctx_node: Option<NodeId>,
    /// Card captured at right-click time (lifecycle actions lead the menu).
    ctx_card: Option<(String, String)>,
    /// Open "new note/folder" dialog, if any.
    create: Option<CreateDialog>,
    /// Transient status-bar message and its birth time.
    flash: Option<(String, Instant)>,
    /// Tab was ours this frame — take widget focus back at the end of it.
    /// egui decides its Tab focus move in `Memory::begin_pass`, BEFORE any
    /// of our code runs, so consuming the event can't stop it; the gear and
    /// health badges would take the keyboard and swallow the next Tab.
    tab_taken: bool,
    /// Select and frame this rel path once a reload turns it into a node.
    pending_select: Option<String>,
}

impl Viewer {
    /// Everything derivable from the graph alone — shared by `new` and the
    /// live-reload `rebuild`.
    fn derived(g: &Graph, node_scale: f32) -> Derived {
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
        let depths: Vec<u8> = (0..g.nodes.len())
            .map(|i| g.depth(NodeId(i as u32)).min(255) as u8)
            .collect();
        // Folders SHRINK with depth — the root is unmistakably the biggest
        // thing on the canvas, each level visibly smaller, files below all
        // of them. Hierarchy readable from size alone.
        let radius = g
            .nodes
            .iter()
            .enumerate()
            .zip(&degree)
            .map(|((i, n), d)| {
                let base = match n.kind {
                    NodeKind::Dir => (10.0 - depths[i] as f32 * 1.5).max(5.5),
                    NodeKind::Image => 4.5,
                    NodeKind::File => 3.5,
                    NodeKind::Asset => 3.0,
                    NodeKind::Ghost => 3.0,
                    NodeKind::Web => 2.6,
                };
                (base + (*d as f32).sqrt() * 1.3f32).min(18.0) * node_scale
            })
            .collect();
        let n_files = g.nodes.iter().filter(|n| n.kind == NodeKind::File).count();
        let n_dirs = g.nodes.iter().filter(|n| n.kind == NodeKind::Dir).count();
        let n_images = g.nodes.iter().filter(|n| n.kind == NodeKind::Image).count();
        let n_assets = g.nodes.iter().filter(|n| n.kind == NodeKind::Asset).count();
        let n_webs = g.nodes.iter().filter(|n| n.kind == NodeKind::Web).count();
        let mut dir_by_path = HashMap::new();
        for (i, n) in g.nodes.iter().enumerate() {
            if n.kind == NodeKind::Dir {
                dir_by_path.insert(n.path_key(), NodeId(i as u32));
            }
        }
        Derived {
            radius,
            depths,
            n_files,
            n_dirs,
            n_images,
            n_assets,
            n_webs,
            dir_by_path,
        }
    }

    fn new(g: Graph, root: PathBuf, cfg: Config) -> Self {
        let sim = Sim::new(&g);
        let Derived {
            radius,
            depths,
            n_files,
            n_dirs,
            n_images,
            n_assets,
            n_webs,
            dir_by_path,
        } = Self::derived(&g, cfg.node_scale);
        let (reload_tx, reload_rx) = std::sync::mpsc::channel();
        let vs = state::load(&root);
        let cam = vs.camera;
        let show_web = !vs.hide_web;
        let theme = Theme::get(cfg.light);
        let agent_allowlist = cfg.agent_choices();
        let mut restore_offsets: HashMap<String, Vec<(String, Vec2)>> = HashMap::new();
        for c in vs.cards {
            // clamp like the camera below: a corrupt offset would park the
            // card outside every possible view forever (it fails the
            // visibility cull but keeps being re-saved)
            restore_offsets.entry(c.session).or_default().push((
                c.pane,
                Vec2::new(c.dx.clamp(-1e5, 1e5), c.dy.clamp(-1e5, 1e5)),
            ));
        }
        let mut restore_pins: HashMap<String, Vec<(String, ())>> = HashMap::new();
        for (session, pane) in vs.pins {
            restore_pins.entry(session).or_default().push((pane, ()));
        }
        Self {
            g,
            sim,
            radius,
            depths,
            // center is clamped like zoom: a huge-but-finite restored value
            // (corrupt/hand-edited view file) overflows world_to_screen to
            // ±inf — nothing paints, nothing hit-tests, and fitted=true
            // suppresses the auto-fit that would recover
            center: cam.map_or(Pos2::ZERO, |(x, y, _)| {
                Pos2::new(x.clamp(-1e6, 1e6), y.clamp(-1e6, 1e6))
            }),
            zoom: cam.map_or(1.0, |(_, _, z)| z.clamp(0.02, 50.0)),
            hover: None,
            hover_since: None,
            hover_body: None,
            selected: None,
            drag_node: None,
            fitted: cam.is_some(), // a restored camera must not be re-fit away
            last_canvas_rect: None,
            theme,
            apply_visuals: true,
            settings: settings::SettingsUi::default(),
            cfg,
            cam_anim: None,
            n_files,
            n_dirs,
            n_images,
            n_assets,
            n_webs,
            show_web,
            pane_width: vs.pane_width,
            matcher: Matcher::new(MatcherConfig::DEFAULT),
            picker: Picker::new(),
            root,
            md_cache: CommonMarkCache::default(),
            pane_content_w: std::cell::Cell::new(0.0),
            pane_preview: None,
            detail: None,
            detail_stamp: None,
            _watcher: None,
            reload_at: Arc::new(Mutex::new(None)),
            watch_error: Arc::new(Mutex::new(None)),
            reload_gen: 0,
            scan_inflight: false,
            rescan_queued: false,
            reload_tx,
            reload_rx,
            last_reload: None,
            reload_error: None,
            config_error: None,
            diag_open: false,
            dir_by_path,
            terms: terminals::Terminals::new(restore_offsets, restore_pins, agent_allowlist),
            thumbs: images::Thumbs::new(),
            previews: previews::Previews::default(),
            saved_state: None,
            view_unknown: vs.unknown,
            last_save: Instant::now(),
            save_warned: false,
            pending_g: None,
            nav_scroll: false,
            conn_cursor: None,
            ctx_node: None,
            ctx_card: None,
            create: None,
            flash: None,
            tab_taken: false,
            pending_select: None,
        }
    }

    /// Nearest Dir node at or above `cwd`.
    fn anchor_for(&self, cwd: &Path) -> NodeId {
        let mut rel = cwd
            .strip_prefix(&self.root)
            .unwrap_or(Path::new(""))
            .to_path_buf();
        loop {
            if let Some(&id) = self.dir_by_path.get(&vault::path_key(&rel)) {
                return id;
            }
            if !rel.pop() {
                return self.g.root;
            }
        }
    }

    /// Tab / Shift+Tab: step the terminal cursor to the next card in a
    /// stable order, centering on it and leaving the zoom alone — the card
    /// expands because it is the cursor, so it is readable wherever you
    /// were. Enter then takes you INTO it.
    fn step_card_cursor(&mut self, delta: isize) {
        let keys = self.terms.cards_in_order();
        if keys.is_empty() {
            self.set_flash("no agent or terminal cards yet — t starts one".into());
            return;
        }
        let at = self
            .terms
            .cursor
            .as_ref()
            .and_then(|c| keys.iter().position(|k| k == c));
        let next = match at {
            // first Tab lands on the first card, not on the second
            None if delta > 0 => 0,
            None => keys.len() - 1,
            Some(i) => (i as isize + delta).rem_euclid(keys.len() as isize) as usize,
        };
        let key = keys[next].clone();
        self.terms.cursor = Some(key.clone());
        self.fly_to_card_at(key, false);
    }

    /// The node the finder is highlighting, if it is highlighting a node —
    /// the canvas opens it up (preview card, thumbnail) while it is what
    /// the reader is looking at.
    pub(super) fn highlighted_node(&self) -> Option<NodeId> {
        if !self.picker.open {
            return None;
        }
        match self.picker.cursor_row().map(|r| &r.target) {
            Some(text_graph::search::Target::Node(i)) if (*i as usize) < self.g.nodes.len() => {
                Some(NodeId(*i))
            }
            _ => None,
        }
    }

    /// Glide the camera to center on `id` — zoom stays exactly as it is,
    /// and the quick movement (instead of a snap) shows where the jump
    /// came from and where it lands.
    fn frame_node(&mut self, id: NodeId) {
        self.cam_anim = Some((self.center, id, Instant::now()));
    }

    /// Ctrl+Q is the way BACK: whatever has the keyboard gives it up, and
    /// whatever is selected is deselected, in one press. From a terminal
    /// you are typing into, that has to land you somewhere `f` works —
    /// which means dropping egui's widget focus too, or the next keystroke
    /// goes into a text field instead of the graph.
    pub(super) fn release_everything(&mut self, ctx: &egui::Context) {
        self.terms.focused = None;
        self.terms.cursor = None;
        self.selected = None;
        self.conn_cursor = None;
        if self.picker.open {
            self.picker.close();
        }
        if self.settings.open {
            self.close_settings();
        }
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }

    /// A zoom that shows `id` together with its neighbourhood — parent,
    /// children and links — rather than a fixed step: "show me around
    /// this" means something different for a leaf note and for a folder
    /// with forty children.
    fn neighborhood_zoom(&self, id: NodeId, rect: Rect) -> f32 {
        let here = self.world_pos(id.0 as usize);
        let node = self.g.node(id);
        let around = node
            .parent
            .into_iter()
            .chain(node.children.iter().copied())
            .chain(self.g.outlinks(id).map(|l| l.to))
            .chain(self.g.backlinks(id).map(|l| l.from));
        // the node stays centered, so what matters is how far the
        // furthest neighbour reaches from it in each direction
        let (mut dx, mut dy) = (0.0f32, 0.0f32);
        for n in around {
            let p = self.world_pos(n.0 as usize);
            dx = dx.max((p.x - here.x).abs());
            dy = dy.max((p.y - here.y).abs());
        }
        let r = self.radius[id.0 as usize].max(0.5);
        // nothing around it: frame the node itself at a readable size
        let zoom = if dx <= 1.0 && dy <= 1.0 {
            LONE_NODE_R / r
        } else {
            let fit = (rect.width() * 0.5 / dx.max(1.0)).min(rect.height() * 0.5 / dy.max(1.0));
            fit * 0.85
        };
        zoom.clamp(0.02, NEIGHBORHOOD_MAX_ZOOM)
    }

    /// Called once the frame's widgets have drawn, so egui's Tab focus
    /// move has landed — and undone. Focus on the gear or the health badge
    /// means the NEXT Tab goes to egui's navigation instead of the cards,
    /// which is how Tab "stopped working" after tabbing once. egui decides
    /// this in `Memory::begin_pass`, before any of our code runs, so
    /// consuming the event cannot prevent it; taking the focus back after
    /// the fact can.
    pub(super) fn release_tab_focus(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.tab_taken)
            && let Some(id) = ctx.memory(|m| m.focused())
        {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }

    fn set_flash(&mut self, msg: String) {
        self.flash = Some((msg, Instant::now()));
    }

    fn world_pos(&self, i: usize) -> Pos2 {
        Pos2::new(self.sim.x[i], self.sim.y[i])
    }

    /// Where the camera has to sit for node `i` to land somewhere VISIBLE:
    /// dead center normally, lifted into the band above the floating
    /// finder while that covers the middle of the canvas.
    fn frame_target(&self, i: usize, rect: Rect) -> Pos2 {
        let mut p = self.world_pos(i);
        if self.picker.open {
            p.y += rect.height() * picker::frame_lift_frac(self.cfg.finder_y) / self.zoom;
        }
        p
    }

    /// Frame the whole graph (first paint only — rect is unknown before then).
    /// Where the camera would sit to show the whole graph: center and
    /// zoom, computed together but applied separately — `gg` takes both,
    /// `0` takes only the zoom and stays where you are.
    fn whole_graph_view(&self, rect: Rect) -> Option<(Pos2, f32)> {
        let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
        let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for i in 0..self.g.nodes.len() {
            let p = self.world_pos(i);
            min = min.min(p);
            max = max.max(p);
        }
        if !min.x.is_finite() {
            return None;
        }
        let size = (max - min).max(Vec2::splat(1.0));
        Some((
            Pos2::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5),
            ((rect.width() / size.x).min(rect.height() / size.y) * 0.85).clamp(0.02, 50.0),
        ))
    }

    fn fit(&mut self, rect: Rect) {
        if let Some((center, zoom)) = self.whole_graph_view(rect) {
            self.center = center;
            self.zoom = zoom;
        }
    }

    fn to_screen(&self, rect: Rect, w: Pos2) -> Pos2 {
        rect.center() + (w - self.center) * self.zoom
    }

    fn to_world(&self, rect: Rect, s: Pos2) -> Pos2 {
        self.center + (s - rect.center()) / self.zoom
    }

    /// Screen radius above which an Image node paints as a thumbnail box
    /// rather than a disc.
    const IMG_BOX_MIN_R: f32 = 5.0;

    /// The rect an Image node's thumbnail occupies: aspect from the decoded
    /// texture (4:3 until it arrives), fit into half-extents 1.5r × r.
    fn image_box(&self, id: NodeId, s: Pos2, r: f32) -> Rect {
        let aspect = self
            .thumbs
            .aspect(&self.g.node(id).path_key())
            .unwrap_or(4.0 / 3.0);
        let half_h = (1.5 * r / aspect).min(r);
        Rect::from_center_size(s, Vec2::new(aspect * half_h, half_h) * 2.0)
    }

    /// Screen radius the finder's highlighted node is drawn at when it
    /// would otherwise be too small to read.
    const OPENED_MIN_R: f32 = 24.0;

    /// UNCLAMPED screen radius above which a File node shows its text
    /// preview card. (The clamped radius caps at 16, so zoom depth would be
    /// invisible through it.)
    const PREVIEW_MIN_R: f32 = 13.0;

    /// The card rect a File node's text preview occupies at this zoom.
    /// `opened` (the finder's highlight) gets a readable floor: opening a
    /// node the reader is looking at into a card too small to read would
    /// be a worse answer than the dot it replaced.
    fn preview_box(&self, id: NodeId, s: Pos2, opened: bool) -> Rect {
        let mut ur = self.radius[id.0 as usize] * self.zoom;
        if opened {
            ur = ur.max(Self::OPENED_MIN_R);
        }
        let size = Vec2::new((ur * 5.2).min(280.0), (ur * 6.0).min(320.0));
        Rect::from_center_size(s, size)
    }

    /// A file-type icon's color under the active theme — the per-language
    /// palette is dark-tuned, so light mode darkens it a shade to keep
    /// yellows and cyans readable on paper.
    fn icon_color(&self, icon: filetype::FileIcon) -> Color32 {
        let c = Color32::from_rgb(icon.color.0, icon.color.1, icon.color.2);
        if self.theme.light { shade(c, 0.72) } else { c }
    }

    /// The glyph + color a node shows in lists (navigator, popups).
    fn node_icon(&self, id: NodeId) -> (char, Color32) {
        let node = self.g.node(id);
        let icon = match node.kind {
            NodeKind::Dir => filetype::ICON_FOLDER,
            NodeKind::Image => filetype::ICON_IMAGE,
            NodeKind::Ghost => {
                return ('\u{f016}', self.theme.ghost); // an unwritten page
            }
            NodeKind::Web => filetype::ICON_WEB,
            _ => filetype::icon_of(&node.path),
        };
        (icon.glyph, self.icon_color(icon))
    }

    /// Can this node open into a text-preview card? Markdown always;
    /// assets only when their extension is textual (a binary excerpt would
    /// be mojibake).
    fn previewable(&self, id: NodeId) -> bool {
        let node = self.g.node(id);
        match node.kind {
            NodeKind::File => true,
            NodeKind::Asset => filetype::is_text(&node.path),
            _ => false,
        }
    }

    /// The rect a node's expanded form occupies — image thumbnail or text
    /// preview card — if it is expanded at the current zoom. Hover targets,
    /// selection rings, and label anchors all follow this shape.
    fn node_box(&self, id: NodeId, s: Pos2, r: f32) -> Option<Rect> {
        // the finder's highlighted node is drawn opened, so it has to be
        // HIT like that too — rings, labels and clicks follow the shape
        // the reader can see
        let open = self.highlighted_node() == Some(id);
        let r = if open { r.max(Self::OPENED_MIN_R) } else { r };
        match self.g.node(id).kind {
            NodeKind::Image if r >= Self::IMG_BOX_MIN_R => Some(self.image_box(id, s, r)),
            NodeKind::File | NodeKind::Asset
                if (self.radius[id.0 as usize] * self.zoom >= Self::PREVIEW_MIN_R || open)
                    && self.previewable(id) =>
            {
                Some(self.preview_box(id, s, open))
            }
            _ => None,
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        // The world is anchored to the rect CENTER, so the side panel
        // opening/closing (or resizing, or a window resize) would slide the
        // whole scene sideways — the node just clicked escapes the cursor
        // and the second click of a double-click misses. Shift the camera by
        // the same amount so every world point keeps its screen position;
        // only the visible clip changes.
        if let Some(last) = self.last_canvas_rect
            && last.center() != rect.center()
        {
            self.center += (rect.center() - last.center()) / self.zoom;
        }
        self.last_canvas_rect = Some(rect);
        if let Some((from, id, t0)) = self.cam_anim {
            if (id.0 as usize) < self.g.nodes.len() {
                // glide duration is a setting; 0 means jump
                let t = if self.cfg.glide <= 0.0 {
                    1.0
                } else {
                    (t0.elapsed().as_secs_f32() / self.cfg.glide).min(1.0)
                };
                let e = 1.0 - (1.0 - t) * (1.0 - t); // ease-out
                self.center = from.lerp(self.frame_target(id.0 as usize, rect), e);
                if t >= 1.0 {
                    self.cam_anim = None;
                }
                ui.ctx().request_repaint();
            } else {
                self.cam_anim = None; // graph swapped underneath
            }
        }
        if !self.fitted {
            self.fit(rect);
            self.fitted = true;
        }

        // ---- simulation ----
        self.sim.configure(self.cfg.spread, self.cfg.freeze);
        if self.sim.active() {
            self.sim.tick(3);
            ui.ctx().request_repaint();
        }

        // ---- agent terminals: discovery snapshot, mirrors, screen caches ----
        let ctx = ui.ctx().clone();
        self.sync_terminals(&ctx);
        self.thumbs.pump(&ctx);

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
                // corner grip on explicitly owned sessions = native resize; the
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
                // the real session by dozens of columns. Cursor/focused cards
                // render expanded at any zoom, so they're never compact.
                let expanded = self.terms.is_expanded(&t);
                let compact = (6.0 * self.zoom).clamp(2.5, 16.0) < 5.0 && !expanded;
                if on_handle
                    && ours
                    && !compact
                    && let Some(c) = self.terms.cache.get(&t)
                    && let Some(pos) = response.hover_pos()
                {
                    // cell metrics must match what the card is rendered at
                    let mut f = (6.0 * self.zoom).clamp(2.5, 16.0);
                    if expanded {
                        f = f.max(terminals::EXPAND_FONT);
                    }
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
                    let cur_center = self
                        .terms
                        .rects
                        .iter()
                        .find(|(s, p, _)| (s, p) == (&t.0, &t.1))
                        .map(|(_, _, r)| r.center());
                    let anchor_s = self
                        .terms
                        .panes
                        .iter()
                        .find(|a| a.session == t.0 && a.pane == t.1)
                        .map(|a| {
                            let id = self.card_anchor(a);
                            self.to_screen(rect, self.world_pos(id.0 as usize))
                        });
                    if let (Some(center), Some(anchor_s)) = (cur_center, anchor_s) {
                        self.terms
                            .offsets
                            .insert(t.clone(), (center - anchor_s) / self.zoom);
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
                self.cam_anim = None; // manual pan wins over a glide
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
        let factor = pinch * (scroll * 0.0025 * self.cfg.zoom_speed).exp();
        if factor != 1.0
            && let Some(cursor) = response.hover_pos()
        {
            // keep the world point under the cursor fixed while zooming
            self.cam_anim = None;
            let anchor = self.to_world(rect, cursor);
            self.zoom = (self.zoom * factor).clamp(0.02, 50.0);
            self.center = anchor - (cursor - rect.center()) / self.zoom;
        }

        // ---- cull to viewport ----
        let view = rect.expand(60.0);
        let mut visible: Vec<(NodeId, Pos2, f32)> = Vec::new();
        for i in 0..self.g.nodes.len() {
            // hidden web nodes are skipped at render/hit-test only — the
            // sim keeps simulating them, so toggling never reflows
            if !self.show_web && self.g.nodes[i].kind == NodeKind::Web {
                continue;
            }
            let s = self.to_screen(rect, self.world_pos(i));
            // images render as thumbnails, so their "radius" is the box
            // half-height with a much larger cap — hover targets, rings, and
            // label anchors all follow it. Their cull test must cover the
            // box extent (half-width ≤ 1.5r), or a picture straddling the
            // viewport edge pops in and out while panning.
            let r = if self.g.nodes[i].kind == NodeKind::Image {
                let r = (self.radius[i] * 2.0 * self.zoom).clamp(1.5, 110.0);
                if !view.expand2(Vec2::new(r * 1.5, r)).contains(s) {
                    continue;
                }
                r
            } else {
                if !view.contains(s) {
                    continue;
                }
                (self.radius[i] * self.zoom).clamp(1.5, 16.0)
            };
            visible.push((NodeId(i as u32), s, r));
        }

        // ---- hover / select ----
        if let Some(id) = self.drag_node {
            self.hover = Some(id);
        } else {
            let prev = self.hover;
            self.hover = None;
            if over_card.is_none()
                && let Some(cursor) = response.hover_pos()
            {
                let mut best = f32::INFINITY;
                for &(id, s, r) in &visible {
                    let d = s.distance(cursor);
                    // an expanded node (thumbnail / preview card) is wider
                    // than its nominal radius — hover anywhere on it, not a
                    // circle around its center (the mismatch made hover
                    // flicker at the corners of wide boxes). The previously
                    // hovered node gets hysteresis slack: while the sim
                    // drifts (settling, reload ripples) a node must not
                    // escape the stationary cursor mid-dwell.
                    let hit = if let Some(bx) = self.node_box(id, s, r) {
                        bx.expand(4.0).contains(cursor)
                    } else {
                        d < r + 4.0 || (prev == Some(id) && d < r + 16.0)
                    };
                    if hit && d < best {
                        best = d;
                        self.hover = Some(id);
                    }
                }
            }
        }
        // dwell tracking for the full hover preview: the anchor freezes at
        // dwell start so the popup doesn't chase the pointer; any drag
        // (pan, node, card) resets it
        if response.dragged() {
            self.hover_since = None;
            self.terms.hover_since = None;
        } else {
            match (self.hover, self.hover_since) {
                (Some(h), Some((id, ..))) if id == h => {}
                (Some(h), _) => {
                    let a = response.hover_pos().unwrap_or_else(|| rect.center());
                    self.hover_since = Some((h, Instant::now(), a));
                }
                (None, _) => self.hover_since = None,
            }
            // same dwell tracking for terminal cards (drives the peek popup)
            match (&over_card, &self.terms.hover_since) {
                (Some(k), Some((hk, ..))) if hk == k => {}
                (Some(k), _) => {
                    let a = response.hover_pos().unwrap_or_else(|| rect.center());
                    self.terms.hover_since = Some((k.clone(), Instant::now(), a));
                }
                (None, _) => self.terms.hover_since = None,
            }
        }
        if response.clicked() {
            if let Some(t) = over_card.clone() {
                if ui.input(|i| i.modifiers.command) {
                    // Ctrl+click pins the card open (expanded at any zoom,
                    // several at once) without touching keyboard focus;
                    // Ctrl+click again unpins
                    self.terms.toggle_pin(t);
                } else {
                    self.terms.cursor = Some(t.clone());
                    self.terms.focused = Some(t);
                    // the pane owns the keyboard now — the picker's prompt
                    // would swallow every keystroke meant for it
                    self.picker.close();
                }
            } else {
                // click-away releases terminal focus AND lands as a normal
                // graph click in the same gesture — selecting a node must
                // never cost a second click. But a release-click on EMPTY
                // space only releases: it must not also close the navigator
                // pane the selection holds open.
                let was_focused = self.terms.focused.take().is_some();
                self.terms.cursor = None;
                if self.hover.is_some() || !was_focused {
                    self.selected = self.hover;
                }
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
                    .map(|a| self.card_anchor(a))
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
        let searching = self.picker.searching();
        let cursor_node = self.picker.cursor_node();
        let n_nodes = self.g.nodes.len();
        let lit: Vec<bool> = if searching {
            self.picker
                .node_scores
                .iter()
                .map(Option::is_some)
                .collect()
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
            let color = if on {
                self.theme.edge_tree
            } else {
                self.theme.edge_tree.gamma_multiply(self.cfg.focus_fade)
            };
            // sa = child, sb = parent — the wedge thins toward the child
            paint_tree_edge(&painter, sb, sa, color);
        }

        // wikilink edges — always visible as faint curves, bright when they
        // touch the active node
        for l in &self.g.links {
            if l.kind == LinkKind::External && !self.show_web {
                continue;
            }
            let sa = self.to_screen(rect, self.world_pos(l.from.0 as usize));
            let sb = self.to_screen(rect, self.world_pos(l.to.0 as usize));
            if !view.intersects(Rect::from_two_pos(sa, sb)) {
                continue;
            }
            let bright = active == Some(l.from) || active == Some(l.to);
            let on = lit[l.from.0 as usize] && lit[l.to.0 as usize];
            // external (citation) edges: cyan and fainter — context, not
            // structure
            let (hue, rest_a) = match l.kind {
                LinkKind::WikiLink => (self.theme.wiki, 0.35),
                LinkKind::External => (self.theme.web, 0.22),
            };
            let (color, width) = if bright {
                (hue, 1.8)
            } else if on {
                (hue.gamma_multiply(rest_a), 1.0)
            } else {
                (hue.gamma_multiply(self.cfg.focus_fade), 1.0)
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
            // arrowhead at the target end, pulled back to the node's rim so
            // the icon painted on top doesn't swallow it
            let tr = (self.radius[l.to.0 as usize] * self.zoom).clamp(1.5, 16.0) + 4.0;
            let tangent = sb - ctrl;
            if (sb - sa).length() > tr + 14.0 && tangent.length() > 0.5 {
                let tip = sb - tangent.normalized() * tr;
                paint_arrowhead(&painter, tip, tangent, width * 2.2 + 3.0, color);
            }
        }

        // Card tethers belong with the edges — UNDER node icons — but their
        // endpoints are only known once paint_terminals lays the cards out.
        // Reserve a slot here; paint_terminals fills it.
        let tether_slot = painter.add(egui::Shape::Noop);

        // What the finder is highlighting opens up wherever it sits: the
        // preview card for a note, the picture for an image. You are
        // looking at it in the list; the graph should show you the thing,
        // not a dot you have to zoom into.
        let opened = self.highlighted_node();

        // nodes
        for &(id, s, r) in &visible {
            let node = self.g.node(id);
            let on = lit[id.0 as usize];
            let fade = self.cfg.focus_fade;
            let dimmed = |c: Color32| if on { c } else { c.gamma_multiply(fade) };
            // Big enough to read, a node paints as its file-type glyph
            // (Nerd Font icons — python is the python logo, css the css
            // shield); below that, a colored disc. Ghosts stay hollow.
            let glyph = r >= GLYPH_MIN_R;
            match node.kind {
                NodeKind::Web => {
                    // a cited URL — cyan globe once readable, dot below
                    if glyph {
                        paint_glyph_node(
                            &painter,
                            s,
                            r,
                            filetype::ICON_WEB.glyph,
                            dimmed(self.theme.web),
                            dimmed(self.theme.bg),
                        );
                    } else {
                        painter.circle_filled(s, r, dimmed(self.theme.web));
                    }
                }
                NodeKind::Ghost => {
                    painter.circle_stroke(s, r, Stroke::new(1.2, dimmed(self.theme.ghost)));
                    if r >= ICON_MIN_R {
                        // an unwritten page, hollow like its node
                        paint_doc_icon(&painter, s, r, None, Some(dimmed(self.theme.ghost)));
                    }
                }
                NodeKind::Dir => {
                    let col = dir_depth_color(self.theme.dir, self.depths[id.0 as usize]);
                    if glyph {
                        paint_glyph_node(
                            &painter,
                            s,
                            r,
                            filetype::ICON_FOLDER.glyph,
                            dimmed(col),
                            dimmed(self.theme.bg),
                        );
                    } else {
                        painter.circle_filled(s, r, dimmed(col));
                    }
                }
                NodeKind::File | NodeKind::Asset => {
                    let disc = if node.kind == NodeKind::File {
                        self.theme.file
                    } else {
                        self.theme.asset
                    };
                    // zoomed in far enough, a textual leaf opens into a
                    // preview card (the canvas sibling of the detail pane);
                    // presence fades so the disc↔card flip never pops.
                    // Binary assets stay discs at every zoom.
                    let ur = self.radius[id.0 as usize] * self.zoom;
                    let open_here = opened == Some(id);
                    let want = self.cfg.canvas_previews
                        && (ur >= Self::PREVIEW_MIN_R || open_here)
                        && self.previewable(id);
                    let key = node.path_key();
                    let presence = ui.ctx().animate_value_with_time(
                        egui::Id::new(("preview", &key)),
                        if want { 1.0 } else { 0.0 },
                        0.12,
                    );
                    if presence < 0.95 {
                        if glyph {
                            let icon = filetype::icon_of(&node.path);
                            let color = self.icon_color(icon);
                            paint_glyph_node(
                                &painter,
                                s,
                                r,
                                icon.glyph,
                                dimmed(color),
                                dimmed(self.theme.bg),
                            );
                        } else {
                            painter.circle_filled(s, r, dimmed(disc));
                        }
                    }
                    if presence >= 0.05 {
                        // dim fades like image tint — a big card snapping
                        // between bright and near-black reads as flicker
                        let dim_a = ui.ctx().animate_value_with_time(
                            egui::Id::new(("preview-dim", &key)),
                            if on { 1.0 } else { fade },
                            0.15,
                        );
                        let a = presence * dim_a;
                        let bx = self.preview_box(id, s, open_here);
                        painter.rect_filled(bx, 3.0, self.theme.panel.gamma_multiply(a));
                        painter.rect_stroke(
                            bx,
                            3.0,
                            Stroke::new(1.0, self.theme.edge.gamma_multiply(a)),
                            egui::StrokeKind::Outside,
                        );
                        let fs = (ur.max(if open_here { Self::OPENED_MIN_R } else { 0.0 }) * 0.22)
                            .clamp(6.5, 12.0);
                        // notes read as prose, assets read as code
                        let font = if node.kind == NodeKind::File {
                            FontId::proportional(fs)
                        } else {
                            FontId::monospace(fs * 0.92)
                        };
                        let Some(path) = node.absolute_path(&self.root) else {
                            continue;
                        };
                        let text = self
                            .previews
                            .get_or_load(&key, &path, node.kind == NodeKind::File)
                            .to_string();
                        let galley = painter.layout(
                            text,
                            font,
                            self.theme.text.gamma_multiply(a),
                            bx.width() - 12.0,
                        );
                        painter.with_clip_rect(bx.shrink(2.0)).galley(
                            bx.min + Vec2::new(6.0, 5.0),
                            galley,
                            self.theme.text,
                        );
                    }
                }
                NodeKind::Image => {
                    let open_here = opened == Some(id);
                    if (r < Self::IMG_BOX_MIN_R && !open_here) || !self.cfg.thumbnails {
                        painter.circle_filled(s, r, dimmed(self.theme.img));
                    } else {
                        // decode on demand, draw the thumbnail once it lands;
                        // a framed placeholder with the photo glyph meanwhile
                        let key = node.path_key();
                        let Some(path) = node.absolute_path(&self.root) else {
                            continue;
                        };
                        self.thumbs.request(ui.ctx(), &key, path);
                        let bx = self.image_box(
                            id,
                            s,
                            if open_here {
                                r.max(Self::OPENED_MIN_R)
                            } else {
                                r
                            },
                        );
                        // dim/undim FADES for pictures: the neighborhood
                        // dim snapping a photo between bright and near-black
                        // on every hover change read as flicker
                        let target = if on { 1.0 } else { fade };
                        let t = ui.ctx().animate_value_with_time(
                            egui::Id::new(("img-tint", &key)),
                            target,
                            0.15,
                        );
                        let tint = Color32::WHITE.gamma_multiply(t);
                        match self.thumbs.cache.get(&key) {
                            Some(images::ThumbState::Ready { tex, .. }) => {
                                painter.image(
                                    tex.id(),
                                    bx,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    tint,
                                );
                            }
                            _ => {
                                painter.rect_filled(bx, 2.0, dimmed(self.theme.panel));
                                paint_img_icon(
                                    &painter,
                                    s,
                                    r.min(14.0),
                                    dimmed(self.theme.img),
                                    dimmed(self.theme.panel),
                                );
                            }
                        }
                        painter.rect_stroke(
                            bx,
                            2.0,
                            Stroke::new(1.0, dimmed(self.theme.edge)),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }
            // rings follow the painted shape: expanded boxes (thumbnails,
            // preview cards) get a rect ring, everything else the circle
            let boxed = self.node_box(id, s, r);
            let ring = |color: Color32, width: f32| {
                if let Some(b) = boxed {
                    painter.rect_stroke(
                        b.expand(3.0),
                        2.0,
                        Stroke::new(width, color),
                        egui::StrokeKind::Outside,
                    );
                } else {
                    painter.circle_stroke(s, r + 3.0, Stroke::new(width, color));
                }
            };
            if active == Some(id) {
                let color = if self.selected == Some(id) {
                    self.theme.select
                } else {
                    self.theme.hover
                };
                ring(color, 2.0);
            } else if cursor_node == Some(id) {
                ring(self.theme.hover, 2.0);
            } else if partners.contains(&id) {
                ring(self.theme.wiki, 1.5);
            }
        }

        // labels — LOD by screen radius; always for the active neighborhood;
        // plus the cursor flashlight: nodes near the pointer reveal their
        // names (distance-faded) even when zoomed out below the LOD cutoff
        let reveal: HashMap<NodeId, f32> = match response.hover_pos() {
            Some(c) if over_card.is_none() => {
                reveal_near_cursor(c, &visible, &lit).into_iter().collect()
            }
            _ => HashMap::new(),
        };
        for &(id, s, r) in &visible {
            if active == Some(id) {
                continue; // painted LAST, on a backdrop — see below
            }
            let node = self.g.node(id);
            // full strength for the active/partner/search cases; otherwise
            // the LOD ramp and the cursor flashlight compete — whichever
            // reveals harder wins
            let full = if searching {
                lit[id.0 as usize] && (r >= 3.0 || cursor_node == Some(id))
            } else {
                active == Some(id) || partners.contains(&id)
            };
            let lod = if !searching && lit[id.0 as usize] {
                label_lod(node.kind, r, self.cfg.label_density)
            } else {
                0.0
            };
            let fade = reveal.get(&id).copied().unwrap_or(0.0);
            let strength = if full { 1.0 } else { lod.max(fade) };
            if strength <= 0.0 {
                continue;
            }
            // folder labels are blue and scale with the node — parent
            // folders read as large bright anchors in the label soup
            let is_dir = node.kind == NodeKind::Dir;
            let color = if active == Some(id) {
                self.theme.hover
            } else if is_dir {
                dir_depth_color(self.theme.dir, self.depths[id.0 as usize])
                    .gamma_multiply(0.45 + 0.55 * strength)
            } else {
                self.theme.text.gamma_multiply(0.35 + 0.65 * strength)
            };
            let font = if is_dir {
                FontId::proportional((r * 1.15).clamp(11.5, 16.5))
            } else {
                FontId::proportional(11.5)
            };
            // an expanded box is wider than r — hang the label off its
            // right edge, not the nominal radius
            let anchor = if let Some(b) = self.node_box(id, s, r) {
                Pos2::new(b.max.x + 5.0, s.y)
            } else {
                s + Vec2::new(r + 5.0, 0.0)
            };
            painter.text(
                anchor,
                Align2::LEFT_CENTER,
                node.display_name(),
                font,
                color,
            );
        }

        // The hovered/selected node's label paints last of all, over a
        // dark backdrop — whatever the cursor is on must be readable, not
        // buried in the label soup or under a neighbor's preview box.
        if let Some(aid) = active
            && let Some(&(_, s, r)) = visible.iter().find(|(id, ..)| *id == aid)
        {
            let node = self.g.node(aid);
            let font = if node.kind == NodeKind::Dir {
                FontId::proportional((r * 1.15).clamp(11.5, 16.5))
            } else {
                FontId::proportional(11.5)
            };
            let anchor = if let Some(b) = self.node_box(aid, s, r) {
                Pos2::new(b.max.x + 5.0, s.y)
            } else {
                s + Vec2::new(r + 5.0, 0.0)
            };
            let galley = painter.layout_no_wrap(node.display_name().into(), font, self.theme.hover);
            let pos = Pos2::new(anchor.x, anchor.y - galley.size().y * 0.5);
            let back = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(4.0, 2.0));
            painter.rect_filled(back, 4.0, self.theme.bg.gamma_multiply(0.88));
            painter.galley(pos, galley, self.theme.hover);
        }

        // terminal cards, on top of the graph (their tethers fill the
        // reserved under-node slot)
        self.paint_terminals(&painter, rect, view, tether_slot);

        // full-content hover preview (dwell to open; tooltip layer)
        self.hover_preview_ui(ui);
        // terminal peek: dwell on a compact card shows its full screen
        self.hover_peek_ui(ui);

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
            let count = self.picker.node_scores.iter().flatten().count();
            let tcount = self.terms.scores.iter().flatten().count();
            let terms = if tcount > 0 {
                format!(" + {tcount} terminals")
            } else {
                String::new()
            };
            format!(
                "{count} node{}{terms} lit — the highlighted result is framed here",
                if count == 1 { "" } else { "s" }
            )
        } else if active.is_none()
            && let Some((s, p)) = &self.terms.cursor
        {
            format!("{s} {p} — Enter types into it · Ctrl+click pins open · Esc dismisses")
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
                    "{} files · {} dirs{}{}{} · {} links{}   |   / find anything · hjkl move · d/u zoom · f find here · z center · w web · 0 reset  |  on selection: e edit · t terminal · a agent",
                    self.n_files,
                    self.n_dirs,
                    if self.n_images > 0 {
                        format!(" · {} images", self.n_images)
                    } else {
                        String::new()
                    },
                    if self.n_assets > 0 {
                        format!(" · {} assets", self.n_assets)
                    } else {
                        String::new()
                    },
                    if self.n_webs > 0 {
                        format!(" · {} web", self.n_webs)
                    } else {
                        String::new()
                    },
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
            self.theme.text,
        );
    }
}

impl Viewer {
    /// The side pane, and the width it actually took (the return value is
    /// what the regression test watches: content used to be able to widen
    /// it, and egui STORES a panel's content-driven rect, so the pane
    /// ratcheted further open with every wide note walked onto).
    fn side_panel(&mut self, ui: &mut egui::Ui) -> f32 {
        let win_w = ui.available_width();
        let want = pane_width(win_w, self.pane_width);
        let max = (win_w * 0.6).max(1.0);
        // egui remembers a panel's width ITSELF, in its own memory, from
        // the previous frame's CONTENT-driven rect — and `default_size`
        // only applies while it has nothing remembered. So any frame that
        // came out narrow (a startup window, a moment of wide content, a
        // window resize clamping against the 60% ceiling) became the
        // width from then on, and our recomputed value never got a say.
        //
        // The width is OURS: pinned to an exact range every frame, so the
        // only thing egui can remember is what we already decided. The
        // range opens up for exactly as long as the resize handle is
        // being dragged — that is the one moment the user is the author,
        // and what they land on is what we store.
        let resize_id = egui::Id::new("detail").with("__resize");
        let dragging = ui.ctx().is_being_dragged(resize_id);
        let range = if dragging {
            PANE_MIN.min(max)..=max
        } else {
            want..=want
        };
        let resp = egui::Panel::right("detail")
            .resizable(true)
            .size_range(range)
            .show(ui, |ui| self.side_pane(ui));
        // The reported rect is content-driven, so clamp what we keep:
        // nothing that overflows the pane may become the width the user
        // thinks they chose.
        let got = resp.response.rect.width();
        if dragging {
            self.pane_width = Some(got.clamp(PANE_MIN.min(max), max));
        }
        got
    }
}

impl eframe::App for Viewer {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.apply_visuals {
            self.apply_visuals = false;
            ui.ctx().set_visuals(if self.theme.light {
                egui::Visuals::light()
            } else {
                egui::Visuals::dark()
            });
        }
        self.pump_reload(ui.ctx());
        let release = ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Q));
        if release {
            self.release_everything(ui.ctx());
        }
        if self.terms.focused.is_some() && self.create.is_none() {
            // keyboard belongs to the terminal; graph keybinds are suspended.
            // The create dialog outranks it — otherwise clicking a card with
            // the dialog open would drain its keystrokes into the pane.
            self.forward_input(ui);
        } else {
            self.handle_keys(ui);
        }
        self.pump_picker(ui.ctx());
        // ONE side pane, previewing whatever is current: the finder's
        // highlighted row, else the selection. The canvas keeps the rest
        // of the window either way, and `canvas()` compensates the camera
        // as the pane widens, so the highlighted result glides into what
        // stays visible.
        if self.selected.is_some() || self.picker.open {
            self.side_panel(ui);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(self.theme.bg))
            .show(ui, |ui| self.canvas(ui));
        let ctx = ui.ctx().clone();
        // over the canvas, after it: the finder floats, the preview of
        // whatever it highlights stays in the side pane
        self.picker_overlay_ui(&ctx);
        self.create_dialog_ui(&ctx);
        self.diag_ui(&ctx);
        self.settings_ui(&ctx);
        self.release_tab_focus(&ctx);
        self.persist_state(false);
        // egui repaints on demand; without a heartbeat the debounced save
        // would never run once the sim settles and input stops
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.terms.stop_agent_scan();
        self.persist_state(true);
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::*;

    fn vis(pts: &[(f32, f32)]) -> Vec<(NodeId, Pos2, f32)> {
        pts.iter()
            .enumerate()
            .map(|(i, &(x, y))| (NodeId(i as u32), Pos2::new(x, y), 3.0))
            .collect()
    }

    #[test]
    fn nearer_nodes_weigh_more_and_far_ones_drop() {
        let v = vis(&[(0.0, 0.0), (60.0, 0.0), (500.0, 0.0)]);
        let r = reveal_near_cursor(Pos2::ZERO, &v, &[true; 3]);
        let ids: Vec<u32> = r.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, vec![0, 1]); // 500px is outside the flashlight
        assert!((r[0].1 - 1.0).abs() < 1e-5);
        assert!(r[0].1 > r[1].1);
    }

    #[test]
    fn dimmed_nodes_stay_dark() {
        let v = vis(&[(0.0, 0.0), (10.0, 0.0)]);
        let r = reveal_near_cursor(Pos2::ZERO, &v, &[true, false]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, NodeId(0));
    }

    #[test]
    fn crowd_is_capped_deterministically() {
        // all at the same distance — the cap must keep the lowest indexes
        let pts: Vec<(f32, f32)> = (0..20).map(|_| (50.0, 0.0)).collect();
        let v = vis(&pts);
        let r = reveal_near_cursor(Pos2::ZERO, &v, &[true; 20]);
        assert_eq!(r.len(), REVEAL_MAX);
        let ids: Vec<u32> = r.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, (0..REVEAL_MAX as u32).collect::<Vec<_>>());
    }
}
