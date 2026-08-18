//! The egui shell. Geometry comes from `sim` (force-directed, seeded by
//! the pure radial layout); the `app` tree owns presentation and
//! interaction only, split by concern into the child modules (see
//! CLAUDE.md's layout map): `canvas` paints the frame, `camera` owns the
//! one world⇄screen transform every input handler and paint call goes
//! through, `keymap` dispatches the keys.
//!
//! This file holds what the children share: the `Viewer` struct and its
//! construction, the theme/palette, node geometry (`node_box` and
//! friends), icon plumbing, the side panel, and the eframe `App` impl.

mod actions;
mod camera;
mod canvas;
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

use picker::Picker;
use terminals::TERM_BG;

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

/// Label LOD ramp: labels ease in between these screen radii instead of
/// popping at a hard cutoff. Dirs surface earlier than leaves.
const LABEL_RAMP_DIR: (f32, f32) = (2.0, 3.2);
const LABEL_RAMP: (f32, f32) = (2.6, 4.2);

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

/// Darken RGB by `f` (alpha untouched) — depth shading for folders.
fn shade(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

/// Everything `derived()` recomputes from a fresh graph. Held whole on
/// the Viewer (`self.derived`) so build and reload refresh it in one
/// assignment.
struct Derived {
    /// World-space radius per node (degree-scaled, Obsidian-style).
    radius: Vec<f32>,
    /// Tree depth per node (0 = root; ghosts/web nodes are 0).
    depths: Vec<u8>,
    n_files: usize,
    n_dirs: usize,
    n_images: usize,
    n_assets: usize,
    n_webs: usize,
    /// Dir path → node, for anchoring agent panes at their cwd.
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
    /// Everything recomputed from the graph on build/reload — radii,
    /// depths, kind counts, dir index. One field so a reload can't
    /// refresh half of it.
    derived: Derived,
    /// The view: center/zoom, glide, rect compensation — see `camera.rs`.
    cam: camera::Camera,
    hover: Option<NodeId>,
    /// (node, dwell start, screen anchor) — drives the full hover preview.
    hover_since: Option<(NodeId, Instant, Pos2)>,
    /// Body of the hovered file, read on demand (one at a time, like
    /// `detail`).
    hover_body: Option<(NodeId, String)>,
    selected: Option<NodeId>,
    drag_node: Option<NodeId>,
    /// Active palette (dark/light) — swapped from the ⚙ settings window.
    theme: Theme,
    /// egui visuals need (re)applying (startup, theme toggle).
    apply_visuals: bool,
    /// The ⚙ window's own state (open, section, filter, pending edit).
    settings: settings::SettingsUi,
    /// User preferences (per user, not per vault) — see `config.rs`. The
    /// canvas reads it live, so every change shows on the next frame.
    cfg: Config,
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
    /// Live reload: watcher, debounce, scan worker, health — see
    /// `reload::Reload`.
    reload: reload::Reload,
    /// A startup config read failure blocks saves: defaults keep the viewer
    /// usable, but must never overwrite a file we did not successfully load.
    config_error: Option<String>,
    diag_open: bool,
    /// Per-stage frame timing, shown by the ⚙ frame-statistics overlay.
    frames: diag::FrameStats,
    /// Everything terminal-card related — see terminals::Terminals.
    terms: terminals::Terminals,
    /// Thumbnail decode worker + texture cache for Image nodes.
    thumbs: images::Thumbs,
    /// Excerpt cache for zoomed-in File previews.
    previews: previews::Previews,
    /// View-state persistence bookkeeping — private to `reload.rs`, where
    /// the one save path lives.
    persist: reload::Persist,
    // ---- tree navigation ----
    /// First `g` of a `gg` chord, with its press time.
    pending_g: Option<Instant>,
    /// Scroll the navigator's sibling list to the cursor next frame.
    nav_scroll: bool,
    /// Cursor into the connections strip (] / [ step it, Enter/l jumps).
    conn_cursor: Option<usize>,
    /// Right-click subject + create dialog + post-create selection — see
    /// `actions::Menu`.
    menu: actions::Menu,
    /// Transient status-bar message and its birth time.
    flash: Option<(String, Instant)>,
    /// Tab was ours this frame — take widget focus back at the end of it.
    /// egui decides its Tab focus move in `Memory::begin_pass`, BEFORE any
    /// of our code runs, so consuming the event can't stop it; the gear and
    /// health badges would take the keyboard and swallow the next Tab.
    tab_taken: bool,
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
        let derived = Self::derived(&g, cfg.node_scale);
        let vs = state::load(&root);
        let saved_cam = vs.camera;
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
            derived,
            cam: camera::Camera {
                // center is clamped like zoom: a huge-but-finite restored
                // value (corrupt/hand-edited view file) overflows
                // world_to_screen to ±inf — nothing paints, nothing
                // hit-tests, and fitted=true suppresses the auto-fit that
                // would recover
                center: saved_cam.map_or(Pos2::ZERO, |(x, y, _)| {
                    Pos2::new(x.clamp(-1e6, 1e6), y.clamp(-1e6, 1e6))
                }),
                zoom: saved_cam.map_or(1.0, |(_, _, z)| z.clamp(0.02, 50.0)),
                // a restored camera must not be re-fit away
                fitted: saved_cam.is_some(),
                ..camera::Camera::new()
            },
            hover: None,
            hover_since: None,
            hover_body: None,
            selected: None,
            drag_node: None,
            theme,
            apply_visuals: true,
            settings: settings::SettingsUi::default(),
            cfg,
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
            reload: reload::Reload::new(),
            config_error: None,
            diag_open: false,
            frames: diag::FrameStats::new(),
            terms: terminals::Terminals::new(restore_offsets, restore_pins, agent_allowlist),
            thumbs: images::Thumbs::new(),
            previews: previews::Previews::default(),
            persist: reload::Persist::new(vs.unknown),
            pending_g: None,
            nav_scroll: false,
            conn_cursor: None,
            menu: actions::Menu::default(),
            flash: None,
            tab_taken: false,
        }
    }

    /// Nearest Dir node at or above `cwd`.
    fn anchor_for(&self, cwd: &Path) -> NodeId {
        let mut rel = cwd
            .strip_prefix(&self.root)
            .unwrap_or(Path::new(""))
            .to_path_buf();
        loop {
            if let Some(&id) = self.derived.dir_by_path.get(&vault::path_key(&rel)) {
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

    /// Glide the camera to center on `id` — see `Camera::start_glide`.
    fn frame_node(&mut self, id: NodeId) {
        self.cam.start_glide(id);
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
        let r = self.derived.radius[id.0 as usize].max(0.5);
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
            p.y += rect.height() * picker::frame_lift_frac(self.cfg.finder_y) / self.cam.zoom;
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
            self.cam.center = center;
            self.cam.zoom = zoom;
        }
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
        let mut ur = self.derived.radius[id.0 as usize] * self.cam.zoom;
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
                if (self.derived.radius[id.0 as usize] * self.cam.zoom >= Self::PREVIEW_MIN_R
                    || open)
                    && self.previewable(id) =>
            {
                Some(self.preview_box(id, s, open))
            }
            _ => None,
        }
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
        let frame_start = Instant::now();
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
        if self.terms.focused.is_some() && self.menu.dialog.is_none() {
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
        self.frame_stats_ui(&ctx);
        self.release_tab_focus(&ctx);
        self.persist_state(false);
        self.frames.end_frame(frame_start.elapsed());
        // egui repaints on demand; without a heartbeat the debounced save
        // would never run once the sim settles and input stops
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.terms.stop_agent_scan();
        self.persist_state(true);
    }
}
