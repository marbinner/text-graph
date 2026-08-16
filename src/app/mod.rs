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
use nucleo_matcher::{Config, Matcher};
use text_graph::agents::{self, AgentPane};
use text_graph::graph::{Graph, LinkKind, NodeId, NodeKind};
use text_graph::keys::{self, Mods, Special};
use text_graph::mirror::{SessionMirror, TermGrid};
use text_graph::sim::Sim;
use text_graph::{config, create, filetype, graph, mdview, state, vault};

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

/// Dim factor applied to everything outside the active node's neighborhood.
const DIM: f32 = 0.18;

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

/// Screen radius above which a ghost shows its hollow-page silhouette.
const ICON_MIN_R: f32 = 6.5;

/// Camera glide duration for frame_node jumps — long enough to read the
/// direction of travel, short enough to never feel laggy.
const CAM_ANIM_SECS: f32 = 0.18;

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
fn pressed_fresh(ui: &egui::Ui, key: Key) -> bool {
    ui.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key {
                    key: k,
                    pressed: true,
                    repeat: false,
                    ..
                } if *k == key
            )
        })
    })
}

/// 0 below the ramp, 1 above it — how strongly a node's label shows at
/// this screen radius.
fn label_lod(kind: NodeKind, r: f32) -> f32 {
    let (lo, hi) = if kind == NodeKind::Dir {
        LABEL_RAMP_DIR
    } else {
        LABEL_RAMP
    };
    ((r - lo) / (hi - lo)).clamp(0.0, 1.0)
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
    let cfg = config::load_or_migrate(&root);
    let viewer = Viewer::new(graph::build(scan), root, cfg);
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
    settings_open: bool,
    /// User preferences (per user, not per vault) — see `config.rs`. The
    /// canvas reads it live, so every change shows on the next frame.
    cfg: config::Config,
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
    // ---- search ----
    matcher: Matcher,
    /// The picker: prompt, ranked results, preview, content-scan worker.
    picker: Picker,
    // ---- detail pane ----
    root: PathBuf,
    md_cache: CommonMarkCache,
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
                (base + (*d as f32).sqrt() * 1.3f32).min(18.0)
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
                dir_by_path.insert(n.path.clone(), NodeId(i as u32));
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

    fn new(g: Graph, root: PathBuf, cfg: config::Config) -> Self {
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
        } = Self::derived(&g);
        let (reload_tx, reload_rx) = std::sync::mpsc::channel();
        let vs = state::load(&root);
        let cam = vs.camera;
        let show_web = !vs.hide_web;
        let theme = Theme::get(cfg.light);
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
            settings_open: false,
            cfg,
            cam_anim: None,
            n_files,
            n_dirs,
            n_images,
            n_assets,
            n_webs,
            show_web,
            matcher: Matcher::new(Config::DEFAULT),
            picker: Picker::new(),
            root,
            md_cache: CommonMarkCache::default(),
            detail: None,
            detail_stamp: None,
            _watcher: None,
            reload_at: Arc::new(Mutex::new(None)),
            reload_gen: 0,
            scan_inflight: false,
            rescan_queued: false,
            reload_tx,
            reload_rx,
            last_reload: None,
            reload_error: None,
            diag_open: false,
            dir_by_path,
            terms: terminals::Terminals::new(restore_offsets, restore_pins),
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

    /// Glide the camera to center on `id` — zoom stays exactly as it is,
    /// and the quick movement (instead of a snap) shows where the jump
    /// came from and where it lands.
    fn frame_node(&mut self, id: NodeId) {
        self.cam_anim = Some((self.center, id, Instant::now()));
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        if self.create.is_some() {
            return; // the create dialog owns the keyboard
        }
        let (open_key, esc, enter, frame_key, reset) = ui.input(|i| {
            (
                (i.modifiers.is_none() && (i.key_pressed(Key::F) || i.key_pressed(Key::Slash)))
                    || (i.modifiers.command && i.key_pressed(Key::F)),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.modifiers.is_none() && i.key_pressed(Key::Z),
                i.key_pressed(Key::Num0) || i.key_pressed(Key::Home),
            )
        });
        let no_mods = ui.input(|i| i.modifiers.is_none());
        let (term_key, web_key, edit_key, agent_key) = (
            no_mods && pressed_fresh(ui, Key::T),
            no_mods && pressed_fresh(ui, Key::W),
            no_mods && pressed_fresh(ui, Key::E),
            no_mods && pressed_fresh(ui, Key::A),
        );
        // EVERY graph action below must hold this: egui widgets read input
        // without consuming it, so keys typed into a focused text field
        // (the search prompt, the create dialog) reach here too. Unguarded branches
        // re-fit the camera on '0', opened search on '/', and deselected on
        // Esc — all mid-typing. (The picker's own branch stays unguarded on
        // purpose: its Enter/Esc/arrows act WHILE its field is focused.)
        let widget_free = ui.memory(|m| m.focused().is_none());
        if self.picker.open {
            self.picker_keys(ui);
        } else if open_key && widget_free {
            self.picker.open();
        } else if esc && widget_free {
            // dismiss order: link cursor, terminal cursor, then selection.
            // (The search prompt is handled a branch above, while it still
            // has focus — egui surrenders widget focus at frame START on
            // Escape, so a focus guard can never see a live text field.)
            if self.conn_cursor.take().is_none() && self.terms.cursor.take().is_none() {
                self.selected = None;
            }
        } else if enter
            && widget_free
            && let Some(sel) = self.selected
            && let Some(ci) = self.conn_cursor
        {
            // Enter on a highlighted connection = follow it
            if let Some(t) = self.connections(sel).get(ci).copied() {
                self.selected = Some(t);
                self.frame_node(t);
                self.nav_scroll = true;
            }
            self.conn_cursor = None;
        } else if enter
            && widget_free
            && let Some(k) = self.terms.cursor.clone()
        {
            // Enter on the terminal cursor = start typing into it
            self.terms.focused = Some(k);
        } else if enter
            // if an egui widget (e.g. the detail pane's button, tab-focused)
            // has focus, Enter already activates it — don't also fire here,
            // or the editor opens twice. Repeat-guarded like e/t/a/l: a
            // held Enter would spawn an editor/browser per repeat tick.
            && widget_free
            && pressed_fresh(ui, Key::Enter)
            && let Some(sel) = self.selected
        {
            self.open_in_editor(sel);
        } else if frame_key
            && widget_free
            && let Some(sel) = self.selected
        {
            self.frame_node(sel);
        } else if reset && widget_free {
            self.fitted = false; // canvas re-fits on the next frame
        } else if edit_key
            && widget_free
            && let Some(sel) = self.selected
            && self.editable(sel)
            && self.terms.tmux_ok
        {
            // e = edit the selected text file in a graph terminal card
            let ctx = ui.ctx().clone();
            self.edit_in_graph_terminal(&ctx, sel);
        } else if web_key && widget_free {
            // toggle web (cited-URL) nodes — the sim keeps simulating them,
            // so this never reflows the layout
            self.show_web = !self.show_web;
            self.set_flash(
                if self.show_web {
                    "web links shown — w hides them"
                } else {
                    "web links hidden — w brings them back"
                }
                .into(),
            );
        } else if term_key
            && widget_free
            && self.terms.tmux_ok
            && let Some(sel) = self.selected
        {
            // t = new terminal at the selected node's folder
            let dir = self.node_dir(sel);
            self.new_terminal(&dir);
        } else if agent_key
            && widget_free
            && self.terms.tmux_ok
            && let Some(sel) = self.selected
        {
            // a = the DEFAULT agent (⚙ settings) at the selected node's folder
            let dir = self.node_dir(sel);
            let ctx = ui.ctx().clone();
            let agent = self.cfg.agent();
            self.launch_agent(&ctx, &dir, &agent);
        }

        // Ranger-style tree walk — SELECTION IS THE MODE: with a node
        // selected, hjkl walks the Contains tree (discrete steps, key
        // repeat); with nothing selected they pan. Esc switches back.
        let tree_nav = self.selected.is_some() && widget_free;
        if let Some(sel) = self.selected.filter(|_| tree_nav) {
            let (h, j, k, l, g, sg, out_jump, back_jump) = ui.input(|i| {
                let m = i.modifiers.is_none();
                (
                    m && i.key_pressed(Key::H),
                    m && i.key_pressed(Key::J),
                    m && i.key_pressed(Key::K),
                    m && i.key_pressed(Key::L),
                    m && i.key_pressed(Key::G),
                    i.modifiers.shift_only() && i.key_pressed(Key::G),
                    m && i.key_pressed(Key::CloseBracket),
                    m && i.key_pressed(Key::OpenBracket),
                )
            });
            let mut to: Option<NodeId> = None;
            if h {
                to = self.g.node(sel).parent;
            } else if j {
                to = self.g.nav_sibling(sel, 1);
            } else if k {
                to = self.g.nav_sibling(sel, -1);
            } else if l {
                if let Some(ci) = self.conn_cursor {
                    // l on a highlighted connection = follow it
                    to = self.connections(sel).get(ci).copied();
                    self.conn_cursor = None;
                } else {
                    match self.g.node(sel).kind {
                        NodeKind::Dir => to = self.g.nav_enter(sel),
                        // every leaf kind opens — the navigator's Asset pane
                        // advertises "open (Enter / l)", so l must match
                        // Enter's open_in_editor behavior (text Assets edit,
                        // binary ones xdg-open). key repeat must not spawn
                        // an editor per repeat tick
                        NodeKind::File | NodeKind::Image | NodeKind::Web | NodeKind::Asset
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
                }
            } else if out_jump || back_jump {
                // ] / [ walk a highlight through the connections strip
                // (children, then outgoing, then incoming); Enter or l
                // follows the highlighted one
                let len = self.connections(sel).len() as isize;
                if len > 0 {
                    let cur = self.conn_cursor.map(|i| i as isize);
                    let next = if out_jump {
                        cur.map_or(0, |i| (i + 1).min(len - 1))
                    } else {
                        cur.map_or(len - 1, |i| (i - 1).max(0))
                    };
                    self.conn_cursor = Some(next as usize);
                    self.nav_scroll = true;
                }
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
            if h || j || k || sg {
                self.conn_cursor = None; // tree moves dismiss the link cursor
            }
            if let Some(t) = to {
                self.selected = Some(t);
                self.frame_node(t); // the camera follows the walk
                self.nav_scroll = true; // and the sibling list follows too
                self.conn_cursor = None;
            }
        }

        // vim-style camera: hjkl pans (when no node is selected), d/u zooms
        // — continuous while held. Suppressed under the picker: its prompt
        // can lose focus (a click into the preview), and a bare 'd' must
        // not start zooming the canvas behind an open finder.
        if widget_free && !self.picker.open {
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
                self.cam_anim = None; // manual camera input wins
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

    fn world_pos(&self, i: usize) -> Pos2 {
        Pos2::new(self.sim.x[i], self.sim.y[i])
    }

    /// Where the camera has to sit for node `i` to land somewhere VISIBLE:
    /// dead center normally, lifted into the band above the floating
    /// finder while that covers the middle of the canvas.
    fn frame_target(&self, i: usize, rect: Rect) -> Pos2 {
        let mut p = self.world_pos(i);
        if self.picker.open {
            p.y += rect.height() * picker::FRAME_LIFT_FRAC / self.zoom;
        }
        p
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

    /// Screen radius above which an Image node paints as a thumbnail box
    /// rather than a disc.
    const IMG_BOX_MIN_R: f32 = 5.0;

    /// The rect an Image node's thumbnail occupies: aspect from the decoded
    /// texture (4:3 until it arrives), fit into half-extents 1.5r × r.
    fn image_box(&self, id: NodeId, s: Pos2, r: f32) -> Rect {
        let aspect = self
            .thumbs
            .aspect(&self.g.node(id).path)
            .unwrap_or(4.0 / 3.0);
        let half_h = (1.5 * r / aspect).min(r);
        Rect::from_center_size(s, Vec2::new(aspect * half_h, half_h) * 2.0)
    }

    /// UNCLAMPED screen radius above which a File node shows its text
    /// preview card. (The clamped radius caps at 16, so zoom depth would be
    /// invisible through it.)
    const PREVIEW_MIN_R: f32 = 13.0;

    /// The card rect a File node's text preview occupies at this zoom.
    fn preview_box(&self, id: NodeId, s: Pos2) -> Rect {
        let ur = self.radius[id.0 as usize] * self.zoom;
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
        match self.g.node(id).kind {
            NodeKind::Image if r >= Self::IMG_BOX_MIN_R => Some(self.image_box(id, s, r)),
            NodeKind::File | NodeKind::Asset
                if self.radius[id.0 as usize] * self.zoom >= Self::PREVIEW_MIN_R
                    && self.previewable(id) =>
            {
                Some(self.preview_box(id, s))
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
                let t = (t0.elapsed().as_secs_f32() / CAM_ANIM_SECS).min(1.0);
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
        let factor = pinch * (scroll * 0.0025).exp();
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
                self.theme.edge_tree.gamma_multiply(DIM)
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
                (hue.gamma_multiply(DIM), 1.0)
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

        // nodes
        for &(id, s, r) in &visible {
            let node = self.g.node(id);
            let on = lit[id.0 as usize];
            let dimmed = |c: Color32| if on { c } else { c.gamma_multiply(DIM) };
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
                    let want = ur >= Self::PREVIEW_MIN_R && self.previewable(id);
                    let presence = ui.ctx().animate_value_with_time(
                        egui::Id::new(("preview", &node.path)),
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
                            egui::Id::new(("preview-dim", &node.path)),
                            if on { 1.0 } else { DIM },
                            0.15,
                        );
                        let a = presence * dim_a;
                        let bx = self.preview_box(id, s);
                        painter.rect_filled(bx, 3.0, self.theme.panel.gamma_multiply(a));
                        painter.rect_stroke(
                            bx,
                            3.0,
                            Stroke::new(1.0, self.theme.edge.gamma_multiply(a)),
                            egui::StrokeKind::Outside,
                        );
                        let fs = (ur * 0.22).clamp(6.5, 12.0);
                        // notes read as prose, assets read as code
                        let font = if node.kind == NodeKind::File {
                            FontId::proportional(fs)
                        } else {
                            FontId::monospace(fs * 0.92)
                        };
                        let text = self
                            .previews
                            .get_or_load(&self.root, &node.path)
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
                    if r < Self::IMG_BOX_MIN_R {
                        painter.circle_filled(s, r, dimmed(self.theme.img));
                    } else {
                        // decode on demand, draw the thumbnail once it lands;
                        // a framed placeholder with the photo glyph meanwhile
                        self.thumbs
                            .request(ui.ctx(), &node.path, self.root.join(&node.path));
                        let bx = self.image_box(id, s, r);
                        // dim/undim FADES for pictures: the neighborhood
                        // dim snapping a photo between bright and near-black
                        // on every hover change read as flicker
                        let target = if on { 1.0 } else { DIM };
                        let t = ui.ctx().animate_value_with_time(
                            egui::Id::new(("img-tint", &node.path)),
                            target,
                            0.15,
                        );
                        let tint = Color32::WHITE.gamma_multiply(t);
                        match self.thumbs.cache.get(&node.path) {
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
                label_lod(node.kind, r)
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
        self.pump_picker(ui.ctx());
        // ONE side pane: the ranger, or the search that filters it. The
        // canvas keeps the rest of the window either way, and `canvas()`
        // compensates the camera as the pane widens, so the highlighted
        // result glides into what stays visible.
        if self.selected.is_some() || self.picker.open {
            egui::Panel::right("detail")
                .resizable(true)
                .show(ui, |ui| self.side_pane(ui));
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
        self.persist_state(false);
        // egui repaints on demand; without a heartbeat the debounced save
        // would never run once the sim settles and input stops
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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
