//! The picker: one keyboard-driven finder over everything in the graph —
//! note names, aliases, paths, file CONTENT, and live terminal panes.
//!
//! The prompt and its results FLOAT over the canvas, centered on it with
//! the prompt just below the middle (telescope-style) so the eye stays near
//! the center of the screen; previews stay in the side pane, where a
//! walked-to file previews too. `frame_target` lifts the followed node into
//! the band above the overlay, so the graph stays readable behind it.
//!
//! Per keystroke: names/aliases/paths re-score in memory (instant), while
//! file content is scanned by a worker thread after a short debounce and
//! streams back in batches. Matching itself lives in `search.rs`; this
//! module is state, keys, and paint.
//!
//! Nothing here resets itself just because the vault reloaded: hits are
//! keyed by PATH, the preview re-reads only when its own file changed, and
//! the cursor rides its row's identity. Agents save files every few
//! seconds — a search that blinked on each would be unusable.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use text_graph::search::{self, Class, FileHits, Names, Query, Row, Target};

use super::*;

/// Typing pause before the content scan starts. Long enough that a fast
/// typist never starts a scan per character, short enough to feel live.
const DEBOUNCE: Duration = Duration::from_millis(90);
/// Bytes of a file read for the preview pane.
const PREVIEW_BYTES: u64 = 256 * 1024;
/// Preview context above the hit, and total lines kept.
const PREVIEW_BEFORE: usize = 12;
const PREVIEW_LINES: usize = 500;
/// Chars of a single preview line rendered (minified files are one line).
const PREVIEW_LINE_CAP: usize = 400;
/// Result row height: title line + snippet line, uniform so the list can
/// virtualize (`show_rows`) over a vault-sized result set.
const ROW_H: f32 = 34.0;
/// Files the empty prompt lists, newest first.
const RECENT_MAX: usize = 30;
/// Rows a Ctrl+D / Ctrl+U half-page jump moves.
const HALF_PAGE: isize = 8;
/// The floating finder's width, as a fraction of the window and clamped.
const OVERLAY_W_FRAC: f32 = 0.46;
const OVERLAY_W_MIN: f32 = 380.0;
const OVERLAY_W_MAX: f32 = 760.0;
/// A scan has to run at least this long before the "scanning…" hint
/// appears. Every vault reload (an agent saving anything) and every
/// keystroke restarts one, and those finish in milliseconds — a label
/// that strobed on each was noise, not information.
const SCAN_HINT_DELAY: Duration = Duration::from_millis(250);
/// Gap below which a new scan continues the previous one's clock: a scan
/// that ends and restarts within a frame or two is one search still
/// running, while a rescan kicked off by a reload a moment later is a new
/// (and, on a normal vault, very short) one.
const SCAN_RESUME_GAP: Duration = Duration::from_millis(40);
/// How far ABOVE the canvas center a framed node is placed while the
/// finder floats there — the middle of the band left free above the
/// prompt. Without it, following a result would park it under the
/// overlay, which is the one place you cannot see.
/// How far up the canvas a followed node is lifted so the overlay never
/// covers it: half the distance from the prompt to the middle. Derived
/// from where the prompt actually IS (a setting), or framing would park
/// results behind the one thing you can't see through.
pub(super) fn frame_lift_frac(finder_y: f32) -> f32 {
    0.5 - finder_y * 0.5
}

pub(super) enum ScanMsg {
    Hits(u64, Vec<FileHits>),
    Done(u64, Query, search::ScanOutcome),
}

/// One rendered line of the preview pane.
#[derive(Clone)]
pub(super) struct PreviewLine {
    pub(super) no: usize,
    pub(super) text: String,
    pub(super) ranges: Vec<search::Range>,
    /// Syntax colouring for this line, empty when the file type is one
    /// syntect doesn't know (or the file was too big to colour).
    pub(super) spans: Vec<highlight::Span>,
    /// The line the result row points at.
    pub(super) hit: bool,
}

pub(super) enum PreviewBody {
    /// Raw file lines around a content hit, every match highlighted — the
    /// only view that can justify a content match.
    Text(Vec<PreviewLine>),
    /// A terminal pane's screen.
    Screen(Vec<String>),
    /// No matched line to show: the navigator's own preview column renders
    /// it (markdown, picture, folder listing, referrers) — the pane looks
    /// the same whether you got here by searching or by walking.
    Node(NodeId),
    Note(String),
}

pub(super) struct Preview {
    /// Identity of the subject this was built for — a finder row's key, or
    /// `sel\t<ident>` for the selection. A rebuild happens when this
    /// changes (or the file behind it does), never per frame.
    pub(super) key: String,
    pub(super) title: String,
    pub(super) subtitle: String,
    pub(super) meta: String,
    /// The node the pane is about, when it is about one — the header's
    /// breadcrumb and the connections strip are built from it.
    pub(super) subject: Option<NodeId>,
    pub(super) body: PreviewBody,
    /// Index into `Text` lines that the hit sits on.
    pub(super) focus: Option<usize>,
    /// Scroll the hit into view on the next frame.
    pub(super) scroll: bool,
    /// (vault-relative path, stamp) the raw lines were read from — a
    /// reload re-reads only when THIS file changed, so an agent saving
    /// something else can't blink the preview.
    pub(super) source: Option<(String, Option<super::images::Stamp>)>,
}

/// Where the list comes from. The overlay is ONE surface — browsing is
/// the finder pointed at a folder instead of at the whole vault — so a
/// second chooser (with its own preview, its own keys, its own drift)
/// never has to exist.
#[derive(Clone, PartialEq)]
pub(super) enum Source {
    /// Fuzzy over names, aliases, paths, file contents and live panes.
    Find,
    /// The entries of one folder, in tree order, filtered by the query.
    /// Held as a vault-relative PATH: reloads renumber the arena, and an
    /// open overlay must survive an agent's save.
    Browse(String),
}

pub(super) struct Picker {
    pub(super) open: bool,
    pub(super) source: Source,
    pub(super) query: String,
    /// Query the rows were last built for.
    built: String,
    /// Rebuild rows on the next pump regardless of the query (a reload
    /// renumbered the nodes underneath them).
    dirty: bool,
    focus_pending: bool,
    pub(super) rows: Vec<Row>,
    pub(super) cursor: usize,
    /// The cursor row's identity — rows are rebuilt constantly (streaming
    /// batches, reloads), and an index would slide out from under it.
    pub(super) cursor_key: Option<String>,
    /// Keep the cursor row in view on the next paint.
    scroll: bool,
    list_offset: f32,
    pub(super) list_h: f32,
    /// Per-node match score, for the canvas lit mask (None = no match).
    pub(super) node_scores: Vec<Option<u32>>,
    /// The name tier, cached: fuzzy-scoring every node is the expensive
    /// half of a rebuild, and a streaming scan triggers a rebuild per
    /// batch. Invalidated by a new query (`names_for`) or a reload (None).
    name_rows: Vec<Row>,
    name_scores: Vec<Option<u32>>,
    names_for: Option<String>,
    // ---- content scan ----
    generation: u64,
    /// The generation workers compare themselves against; bumping it
    /// cancels every scan in flight.
    live: Arc<AtomicU64>,
    tx: Sender<ScanMsg>,
    rx: Receiver<ScanMsg>,
    /// Content hits by vault-relative PATH (never by node index — reloads
    /// renumber the arena), tagged with the generation that found them so a
    /// finished scan can evict the previous one's leftovers.
    pub(super) content: HashMap<String, (u64, FileHits)>,
    scanning: bool,
    /// When the current run of scanning began, and when it last went idle
    /// — together they decide whether the hint is worth showing.
    scan_since: Option<Instant>,
    scan_idle_at: Option<Instant>,
    /// A query whose scan finished COMPLETE (not cancelled, not truncated)
    /// and the vault-relative paths that had hits — the candidate set a
    /// longer query may narrow against instead of re-reading the vault.
    done: Option<(Query, HashSet<String>)>,
    pending_scan: Option<Query>,
    pending_at: Instant,
    /// Pane list the current rows were built from (panes come and go).
    pane_keys: Vec<(String, String)>,
    /// (node, mtime) for the vault's files, newest first — what an empty
    /// find prompt lists. Rebuilt after a reload, i.e. after whatever
    /// changed on disk changed.
    recent: Option<Vec<(u32, std::time::SystemTime)>>,
    /// Result the camera is dwelling on, and when it landed there.
    follow: Option<(NodeId, Instant)>,
    followed: Option<NodeId>,
    /// The cursor was moved by hand. Opening the picker on a bare listing
    /// must not yank the camera to the vault's first file — but arrowing
    /// through that listing deliberately should still follow.
    user_moved: bool,
}

impl Picker {
    pub(super) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Picker {
            open: false,
            source: Source::Find,
            query: String::new(),
            built: String::new(),
            dirty: false,
            focus_pending: false,
            rows: Vec::new(),
            cursor: 0,
            cursor_key: None,
            scroll: false,
            list_offset: 0.0,
            list_h: 0.0,
            node_scores: Vec::new(),
            name_rows: Vec::new(),
            name_scores: Vec::new(),
            names_for: None,
            generation: 0,
            live: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
            content: HashMap::new(),
            scanning: false,
            scan_since: None,
            scan_idle_at: None,
            done: None,
            pending_scan: None,
            pending_at: Instant::now(),
            pane_keys: Vec::new(),
            recent: None,
            follow: None,
            followed: None,
            user_moved: false,
        }
    }

    /// Is the canvas showing a result set? Drives the lit mask — a browse
    /// listing lights its folder's entries the way a query lights matches.
    pub(super) fn searching(&self) -> bool {
        self.open && (!self.query.trim().is_empty() || self.browsing().is_some())
    }

    pub(super) fn open(&mut self) {
        self.open = true;
        self.source = Source::Find;
        self.focus_pending = true;
        self.dirty = true;
    }

    /// Open on a folder's entries. The query starts empty — browsing is
    /// "show me what is here", and typing narrows it to this folder
    /// (the scoped search that never needed its own keybind).
    pub(super) fn browse(&mut self, dir: String) {
        self.open = true;
        self.source = Source::Browse(dir);
        self.query.clear();
        self.built.clear();
        self.cursor = 0;
        self.cursor_key = None;
        self.user_moved = false;
        self.focus_pending = true;
        self.dirty = true;
    }

    pub(super) fn browsing(&self) -> Option<&str> {
        match &self.source {
            Source::Browse(dir) => Some(dir),
            Source::Find => None,
        }
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.source = Source::Find;
        self.query.clear();
        self.built.clear();
        self.rows.clear();
        self.name_rows.clear();
        self.names_for = None;
        self.cursor = 0;
        self.cursor_key = None;
        self.content.clear();
        self.done = None;
        self.pending_scan = None;
        self.follow = None;
        self.followed = None;
        self.user_moved = false;
        self.node_scores.fill(None);
        self.cancel();
    }

    /// Retire every scan in flight: workers compare their generation with
    /// `live` between files and stop when it moved.
    fn cancel(&mut self) {
        self.generation += 1;
        self.live.store(self.generation, Ordering::Relaxed);
        self.set_scanning(false);
    }

    /// Mark the scan busy or idle, keeping the "busy since" clock running
    /// across the brief gaps between back-to-back scans (a keystroke ends
    /// one and starts the next within a frame or two).
    fn set_scanning(&mut self, on: bool) {
        if on && !self.scanning {
            let resumes = self
                .scan_idle_at
                .is_some_and(|t| t.elapsed() < SCAN_RESUME_GAP);
            if !resumes || self.scan_since.is_none() {
                self.scan_since = Some(Instant::now());
            }
        } else if !on && self.scanning {
            self.scan_idle_at = Some(Instant::now());
        }
        self.scanning = on;
    }

    /// Is a scan worth telling the user about? Only one that has been
    /// running long enough to explain a wait.
    pub(super) fn scan_hint(&self) -> bool {
        self.scanning
            && self
                .scan_since
                .is_some_and(|t| t.elapsed() >= SCAN_HINT_DELAY)
    }

    /// A vault reload renumbered the nodes, so the name tier re-scores —
    /// but the CONTENT hits are keyed by path and stay, and so does the
    /// preview. Agents save files every few seconds; a search that emptied
    /// its own list and reset its preview that often would be unusable.
    /// The refreshed results replace these in place when the rescan lands.
    pub(super) fn on_reload(&mut self, n_nodes: usize) {
        self.node_scores = vec![None; n_nodes];
        self.name_rows.clear();
        self.names_for = None; // node indices moved: re-score from scratch
        // the reload was triggered by SOME file changing and we don't know
        // which, so the rescan can't narrow — but it must not throw away
        // what is on screen while it runs
        self.done = None;
        self.recent = None; // the reload IS a file changing — re-stat
        self.dirty = true;
        if self.open && !self.query.trim().is_empty() {
            self.pending_scan = Some(Query::parse(&self.query));
            self.pending_at = Instant::now();
        }
        self.cancel();
    }

    pub(super) fn cursor_row(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    pub(super) fn cursor_node(&self) -> Option<NodeId> {
        match self.cursor_row()?.target {
            Target::Node(i) => Some(NodeId(i)),
            Target::Pane { .. } => None,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.user_moved = true;
        self.cursor_key = self.cursor_row().map(|r| r.key.clone());
        self.scroll = true;
    }

    /// Instant feedback while the scan catches up: a query that only grew
    /// can only shrink the previous match set, so filter the hits already
    /// on screen instead of blanking the list for a debounce. Counts stay
    /// provisional until the scan lands and replaces them.
    fn refilter(&mut self, q: &Query, prev: &Query) {
        if q.is_empty() || !q.narrows(prev) {
            self.content.clear();
            return;
        }
        let mut buf = String::new();
        self.content
            .retain(|_, (_, h)| match q.match_line(&h.best.text, &mut buf) {
                Some(m) => {
                    h.best.ranges = m.ranges;
                    h.best.score = m.score;
                    true
                }
                None => false,
            });
    }
}

impl Viewer {
    /// Keys while the picker owns the keyboard. Like the old search bar
    /// this branch is deliberately NOT `widget_free`-guarded: its Enter,
    /// Esc and arrows must act while its own text field has focus.
    pub(super) fn picker_keys(&mut self, ui: &egui::Ui) {
        let (esc, enter, ctrl, alt, shift, tab, backspace) = ui.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.modifiers.command,
                i.modifiers.alt,
                i.modifiers.shift,
                i.key_pressed(Key::Tab),
                i.key_pressed(Key::Backspace),
            )
        });
        let step = ui.input(|i| {
            let ctrl = i.modifiers.command;
            let mut d = 0isize;
            if i.key_pressed(Key::ArrowDown) || (ctrl && i.key_pressed(Key::N)) {
                d += 1;
            }
            if i.key_pressed(Key::ArrowUp) || (ctrl && i.key_pressed(Key::P)) {
                d -= 1;
            }
            if i.key_pressed(Key::PageDown) || (ctrl && i.key_pressed(Key::D)) {
                d += HALF_PAGE;
            }
            if i.key_pressed(Key::PageUp) || (ctrl && i.key_pressed(Key::U)) {
                d -= HALF_PAGE;
            }
            d
        });
        if esc {
            self.picker.close();
            return;
        }
        // Tab swaps the SOURCE, keeping the query: what you typed as a
        // filter inside a folder is usually what you want to search the
        // whole vault for when it wasn't there.
        if tab {
            match self.picker.browsing().map(str::to_string) {
                Some(_) => {
                    let q = self.picker.query.clone();
                    self.picker.open();
                    self.picker.query = q;
                }
                None => {
                    let dir = self.browse_start();
                    let q = self.picker.query.clone();
                    self.picker.browse(dir);
                    self.picker.query = q;
                }
            }
            return;
        }
        let browsing = self.picker.browsing().is_some();
        // Backspace on an EMPTY filter walks up — with text it edits, which
        // is what a text field must always do first.
        if browsing && backspace && self.picker.query.is_empty() {
            self.browse_up();
            return;
        }
        if step != 0 {
            self.picker.move_cursor(step);
        }
        if enter {
            // Enter on a FOLDER means "go in" while browsing — that is what
            // browsing is. Shift+Enter takes the folder itself instead, so
            // a directory can still become the selection (t/a/e act on it).
            let into = browsing
                .then(|| self.picker.cursor_row())
                .flatten()
                .and_then(|r| match r.target {
                    Target::Node(i) if (i as usize) < self.g.nodes.len() => Some(NodeId(i)),
                    _ => None,
                })
                .filter(|id| self.g.node(*id).kind == NodeKind::Dir && !shift);
            match into {
                Some(id) => self.browse_into(id),
                // Ctrl/Alt+Enter opens the file in $EDITOR at the matched line
                None => self.picker_accept(ctrl || alt),
            }
        }
    }

    /// Where `b` starts browsing: the selected folder, the selected file's
    /// folder, or the vault root.
    pub(super) fn browse_start(&self) -> String {
        match self.selected {
            Some(id) if self.g.node(id).kind == NodeKind::Dir => self.g.node(id).path.clone(),
            Some(id) => self
                .g
                .node(id)
                .parent
                .map(|p| self.g.node(p).path.clone())
                .unwrap_or_default(),
            None => self.g.node(self.g.root).path.clone(),
        }
    }

    /// Take the highlighted result: select and frame the node (or focus the
    /// terminal card), optionally opening the file in an editor first.
    pub(super) fn picker_accept(&mut self, editor: bool) {
        let Some(row) = self.picker.cursor_row().cloned() else {
            return;
        };
        match row.target {
            Target::Node(i) if (i as usize) < self.g.nodes.len() => {
                let id = NodeId(i);
                self.selected = Some(id);
                self.frame_node(id);
                // a stale terminal cursor must not hijack the next Enter
                self.terms.cursor = None;
                if editor {
                    self.open_at_line(id, row.snippet.as_ref().map(|s| s.line));
                }
            }
            Target::Pane { session, pane } => {
                let key = (session, pane);
                if self
                    .terms
                    .panes
                    .iter()
                    .any(|a| a.session == key.0 && a.pane == key.1)
                {
                    self.terms.cursor = Some(key.clone());
                    self.terms.focused = Some(key.clone());
                    // focused cards expand to a readable size at ANY zoom,
                    // so the finder only recenters — keeping the zoom keeps
                    // the overview of what is around it
                    self.fly_to_card_at(key, false);
                }
            }
            Target::Node(_) => {} // renumbered away by a reload
        }
        self.picker.close();
    }

    /// One frame of picker work: re-derive rows when something changed,
    /// start/collect the content scan, follow the cursor with the camera,
    /// and load the preview. Called every frame while open.
    pub(super) fn pump_picker(&mut self, ctx: &egui::Context) {
        self.pump_picker_inner(ctx);
        // the pane's subject is STATE, not paint: syncing here (rather than
        // while the panel draws) keeps it correct on frames the pane is
        // closed and keeps the pane a pure renderer
        self.sync_pane_preview();
    }

    fn pump_picker_inner(&mut self, ctx: &egui::Context) {
        if self.picker.node_scores.len() != self.g.nodes.len() {
            self.picker.node_scores = vec![None; self.g.nodes.len()];
        }
        if !self.picker.open {
            // a closed picker lights nothing: drop the card scores the last
            // search left behind
            if !self.terms.scores.is_empty() || self.terms.best.is_some() {
                self.terms.scores.clear();
                self.terms.best = None;
            }
            return;
        }
        let q = Query::parse(&self.picker.query);
        if self.picker.query != self.picker.built {
            let prev = Query::parse(&self.picker.built);
            self.picker.built = self.picker.query.clone();
            self.picker.refilter(&q, &prev);
            self.picker.pending_scan = Some(q.clone());
            self.picker.pending_at = Instant::now();
            self.picker.set_scanning(!q.is_empty());
            self.picker.dirty = true;
        }
        // the pane list changes underneath the rows (agents come and go)
        let panes: Vec<(String, String)> = self
            .terms
            .panes
            .iter()
            .map(|a| (a.session.clone(), a.pane.clone()))
            .collect();
        if panes != self.picker.pane_keys {
            self.picker.pane_keys = panes;
            self.picker.dirty = true;
        }
        if let Some(pq) = self.picker.pending_scan.clone() {
            let waited = self.picker.pending_at.elapsed();
            if waited >= DEBOUNCE {
                self.picker.pending_scan = None;
                self.start_scan(ctx, pq);
            } else {
                ctx.request_repaint_after(DEBOUNCE - waited);
            }
        }
        while let Ok(msg) = self.picker.rx.try_recv() {
            match msg {
                ScanMsg::Hits(generation, hits) if generation == self.picker.generation => {
                    for h in hits {
                        self.picker.content.insert(h.rel.clone(), (generation, h));
                    }
                    self.picker.dirty = true;
                }
                ScanMsg::Done(generation, query, outcome)
                    if generation == self.picker.generation =>
                {
                    self.picker.set_scanning(false);
                    if outcome.cancelled {
                        continue;
                    }
                    // leftovers the refilter kept alive from an older
                    // generation are gone for good now: this scan looked
                    // at every candidate file
                    self.picker.content.retain(|_, (g, _)| *g == generation);
                    self.picker.done = (!outcome.truncated)
                        .then(|| (query, self.picker.content.keys().cloned().collect()));
                    self.picker.dirty = true;
                }
                _ => {} // superseded generation
            }
        }
        if self.picker.dirty {
            self.picker.dirty = false;
            self.rebuild_rows();
        }
        self.picker_follow(ctx);
    }

    /// Kick a content scan for `q` on a worker thread. Candidate files are
    /// every textual leaf in path order — or, when the query merely grew
    /// and the last scan finished whole, only the files that matched it.
    fn start_scan(&mut self, ctx: &egui::Context, q: Query) {
        self.picker.generation += 1;
        let generation = self.picker.generation;
        self.picker.live.store(generation, Ordering::Relaxed);
        // an empty query has nothing to scan for; content search off means
        // names/aliases/paths only; and browsing is structural — typing
        // filters the folder, it does not read every file in the vault
        if q.is_empty() || !self.cfg.content_search || self.picker.browsing().is_some() {
            self.picker.set_scanning(false);
            self.picker.content.clear();
            self.picker.done = None;
            return;
        }
        let narrow = self
            .picker
            .done
            .as_ref()
            .filter(|(prev, _)| q.narrows(prev))
            .map(|(_, files)| files);
        let mut files: Vec<String> = self
            .g
            .nodes
            .iter()
            .filter(|n| match n.kind {
                NodeKind::File => true,
                NodeKind::Asset => filetype::is_text(&n.path),
                _ => false,
            })
            .filter(|n| narrow.is_none_or(|set| set.contains(&n.path)))
            .map(|n| n.path.clone())
            .collect();
        files.sort();
        self.picker.set_scanning(true);
        // wake once when the hint would become due — a long scan with no
        // hits produces no other repaint to ride on
        ctx.request_repaint_after(SCAN_HINT_DELAY);
        let root = self.root.clone();
        let tx = self.picker.tx.clone();
        let live = self.picker.live.clone();
        let max_bytes = self.cfg.search_max_bytes();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let cancelled = || live.load(Ordering::Relaxed) != generation;
            let outcome =
                search::scan_files(&root, &q, &files, max_bytes, &cancelled, &mut |batch| {
                    let _ = tx.send(ScanMsg::Hits(generation, batch));
                    ctx.request_repaint();
                });
            let _ = tx.send(ScanMsg::Done(generation, q, outcome));
            ctx.request_repaint();
        });
    }

    /// Fuzzy-score every node's name fields — the expensive half of a
    /// rebuild, so it is cached until the query (or the graph) changes.
    fn score_names(&mut self) {
        if self.picker.names_for.as_deref() == Some(self.picker.query.as_str()) {
            return;
        }
        self.picker.names_for = Some(self.picker.query.clone());
        let mut scores: Vec<Option<u32>> = vec![None; self.g.nodes.len()];
        let mut rows: Vec<Row> = Vec::new();
        // an empty prompt leaves the ranger in place, so there is nothing
        // to rank and nothing to light on the canvas
        if Query::parse(&self.picker.query).is_empty() {
            self.picker.name_scores = scores;
            self.picker.name_rows = rows;
            return;
        }
        let pat = search::pattern(&self.picker.query);
        for (i, n) in self.g.nodes.iter().enumerate() {
            // hidden web nodes aren't on the canvas to jump to
            if !self.show_web && n.kind == NodeKind::Web {
                continue;
            }
            let names = Names {
                display: n.display_name(),
                aliases: &n.aliases,
                path: &n.path,
            };
            let Some(hit) = search::score_names(&pat, &mut self.matcher, names) else {
                continue;
            };
            // the title is always the node's name; when the match landed on
            // an alias or the path, the SUBTITLE carries the highlight
            let (title_ranges, subtitle, subtitle_ranges) = match hit.class {
                Class::Name => (hit.ranges, n.path.clone(), Vec::new()),
                Class::Alias => {
                    let sub = if n.path.is_empty() {
                        hit.field.clone()
                    } else {
                        format!("{} · {}", hit.field, n.path)
                    };
                    (Vec::new(), sub, hit.ranges)
                }
                _ => (Vec::new(), n.path.clone(), hit.ranges),
            };
            scores[i] = Some(hit.score);
            rows.push(Row {
                target: Target::Node(i as u32),
                class: hit.class,
                score: hit.score,
                title: n.display_name().to_string(),
                title_ranges,
                subtitle,
                subtitle_ranges,
                snippet: None,
                more: 0,
                more_capped: false,
                key: n.ident(),
            });
        }
        self.picker.name_scores = scores;
        self.picker.name_rows = rows;
    }

    /// The most recently modified files, newest first — the empty-prompt
    /// list. Stat'ing the vault is a few milliseconds and happens once per
    /// reload, not per keystroke.
    fn recent_rows(&mut self) {
        if self.picker.recent.is_none() {
            let mut v: Vec<(u32, std::time::SystemTime)> = self
                .g
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    matches!(n.kind, NodeKind::File | NodeKind::Asset | NodeKind::Image)
                })
                .filter_map(|(i, n)| {
                    std::fs::metadata(self.root.join(&n.path))
                        .and_then(|m| m.modified())
                        .ok()
                        .map(|t| (i as u32, t))
                })
                .collect();
            // newest first, ties by node index so the list is deterministic
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v.truncate(RECENT_MAX);
            self.picker.recent = Some(v);
        }
        let recent = self.picker.recent.clone().unwrap_or_default();
        let rows: Vec<Row> = recent
            .iter()
            .filter_map(|(i, t)| {
                let n = self.g.nodes.get(*i as usize)?;
                Some(Row {
                    target: Target::Node(*i),
                    class: Class::Name,
                    score: 0,
                    title: n.display_name().to_string(),
                    title_ranges: Vec::new(),
                    subtitle: format!("{} · {}", n.path, super::previews::ago(*t)),
                    subtitle_ranges: Vec::new(),
                    snippet: None,
                    more: 0,
                    more_capped: false,
                    key: n.ident(),
                })
            })
            .collect();
        self.picker.node_scores = vec![None; self.g.nodes.len()];
        self.picker.rows = rows;
        let was = self.picker.cursor;
        self.picker.cursor = self
            .picker
            .cursor_key
            .as_ref()
            .and_then(|k| self.picker.rows.iter().position(|r| &r.key == k))
            .unwrap_or(0);
        if self.picker.cursor != was {
            self.picker.scroll = true;
        }
    }

    /// The entries of the browsed folder, in TREE order (dirs first, as
    /// the graph stores them) — never ranked by score, because a list you
    /// are walking must not reorder under the cursor. A query filters it
    /// in place, which is scoped search: same surface, same keys.
    fn browse_rows(&mut self, dir: &str) {
        let id = self.g.by_path(dir).unwrap_or(self.g.root);
        let q = Query::parse(&self.picker.query);
        let pat = search::pattern(&self.picker.query);
        let mut scores: Vec<Option<u32>> = vec![None; self.g.nodes.len()];
        let mut rows = Vec::new();
        for c in self.g.node(id).children.clone() {
            let n = self.g.node(c);
            let (kind, name) = (n.kind, n.display_name().to_string());
            let label = if kind == NodeKind::Dir {
                format!("{name}/")
            } else {
                name
            };
            let mut ranges = Vec::new();
            if !q.is_empty() {
                // match on the NAME only: inside a folder, the path is
                // context you already have
                let names = Names {
                    display: &label,
                    aliases: &n.aliases,
                    path: "",
                };
                let Some(hit) = search::score_names(&pat, &mut self.matcher, names) else {
                    continue;
                };
                if hit.class == Class::Name {
                    ranges = hit.ranges;
                }
            }
            // a note titled from its frontmatter reads under a name its
            // file doesn't have — show the filename alongside, or a folder
            // listing can't be matched against what is on disk
            let file = n.path.rsplit('/').next().unwrap_or_default().to_string();
            let subtitle = if kind == NodeKind::Dir || label.trim_end_matches('/') == file {
                String::new()
            } else {
                file
            };
            scores[c.0 as usize] = Some(1);
            rows.push(Row {
                target: Target::Node(c.0),
                class: Class::Name,
                score: 0,
                title: label,
                title_ranges: ranges,
                subtitle,
                subtitle_ranges: Vec::new(),
                snippet: None,
                more: 0,
                more_capped: false,
                key: self.g.node(c).ident(),
            });
        }
        self.picker.node_scores = scores;
        self.picker.rows = rows;
        let was = self.picker.cursor;
        self.picker.cursor = self
            .picker
            .cursor_key
            .as_ref()
            .and_then(|k| self.picker.rows.iter().position(|r| &r.key == k))
            .unwrap_or(0);
        if self.picker.cursor != was {
            self.picker.scroll = true;
        }
        self.picker.cursor_key = self.picker.cursor_row().map(|r| r.key.clone());
    }

    /// Walk into a folder (Enter on a directory row) — the list becomes
    /// its entries and the filter starts over, like `l` in the ranger.
    fn browse_into(&mut self, id: NodeId) {
        let path = self.g.node(id).path.clone();
        self.picker.browse(path);
    }

    /// Up to the parent folder, landing the cursor on the folder we came
    /// from so `Backspace`-then-Enter is a no-op round trip.
    fn browse_up(&mut self) {
        let Some(dir) = self.picker.browsing().map(str::to_string) else {
            return;
        };
        let Some(id) = self.g.by_path(&dir) else {
            return;
        };
        let Some(parent) = self.g.node(id).parent else {
            return; // already at the vault root
        };
        let came_from = self.g.node(id).ident();
        self.picker.browse(self.g.node(parent).path.clone());
        self.picker.cursor_key = Some(came_from);
    }

    /// Merge the three sources — cached fuzzy name hits, streamed content
    /// hits, live terminal panes — into one ranked list, at most one row
    /// per node, and put the cursor back on the row it was on.
    fn rebuild_rows(&mut self) {
        if let Some(dir) = self.picker.browsing().map(str::to_string) {
            self.browse_rows(&dir);
            return;
        }
        let q = Query::parse(&self.picker.query);
        if q.is_empty() {
            // An empty prompt used to mean an empty pane. Under agents that
            // rewrite notes all day, the useful answer to "f, and now
            // what?" is what just changed.
            self.recent_rows();
            return;
        }
        self.score_names();
        let mut scores = self.picker.name_scores.clone();
        let mut rows = self.picker.name_rows.clone();
        let row_of: HashMap<u32, usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(r, row)| match row.target {
                Target::Node(n) => Some((n, r)),
                Target::Pane { .. } => None,
            })
            .collect();
        if !q.is_empty() {
            let pat = search::pattern(&self.picker.query);
            for (rel, (_, hits)) in &self.picker.content {
                // hits outlive reloads, so a file may be gone by now
                let Some(id) = self.g.by_path(rel) else {
                    continue;
                };
                let (node, n) = (id.0, self.g.node(id));
                let more = hits.total.saturating_sub(1);
                // a node that already matched by name keeps that (higher)
                // class and just gains the matching line
                if let Some(&r) = row_of.get(&node) {
                    rows[r].snippet = Some(hits.best.clone());
                    rows[r].more = more;
                    rows[r].more_capped = hits.capped;
                    continue;
                }
                scores[node as usize] =
                    Some(scores[node as usize].unwrap_or(0).max(hits.best.score));
                rows.push(Row {
                    target: Target::Node(node),
                    class: Class::Content,
                    score: hits.best.score,
                    title: n.display_name().to_string(),
                    title_ranges: Vec::new(),
                    subtitle: n.path.clone(),
                    subtitle_ranges: Vec::new(),
                    snippet: Some(hits.best.clone()),
                    more,
                    more_capped: hits.capped,
                    key: n.ident(),
                });
            }
            rows.extend(self.pane_rows(&q, &pat));
        }
        search::rank(&mut rows);
        self.picker.node_scores = scores;
        self.picker.rows = rows;
        // the cursor rides its row's identity, not its index
        let was = self.picker.cursor;
        self.picker.cursor = self
            .picker
            .cursor_key
            .as_ref()
            .and_then(|k| self.picker.rows.iter().position(|r| &r.key == k))
            .unwrap_or(0);
        if self.picker.cursor != was {
            // a streaming batch landed rows ABOVE the cursor: scroll by the
            // same amount so the row under your eye stays under your eye
            // instead of sliding away mid-read
            self.picker.list_offset += (self.picker.cursor as f32 - was as f32) * ROW_H;
            self.picker.scroll = true;
        }
        self.picker.cursor_key = self.picker.cursor_row().map(|r| r.key.clone());
        self.sync_terminal_scores();
    }

    /// Terminal cards as results: their identity (agent, session, cwd)
    /// fuzzy-matched, and their live screen searched literally — "which
    /// pane printed that error" is a question about the graph too.
    fn pane_rows(&mut self, q: &Query, pat: &nucleo_matcher::pattern::Pattern) -> Vec<Row> {
        let mut buf = String::new();
        let mut rows = Vec::new();
        for a in &self.terms.panes {
            let dir = a
                .cwd
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let ident = format!("{} {} {} {}", a.agent, a.session, a.pane, dir);
            let name_hit = search::score_names(
                pat,
                &mut self.matcher,
                Names {
                    display: &ident,
                    aliases: &[],
                    path: "",
                },
            );
            let screen = self
                .terms
                .cache
                .get(&(a.session.clone(), a.pane.clone()))
                .map(|c| {
                    c.rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| {
                            let line: String = row.iter().map(|r| r.text.as_str()).collect();
                            let m = q.match_line(&line, &mut buf)?;
                            Some(search::LineHit {
                                line: i + 1,
                                text: line.trim().to_string(),
                                ranges: Vec::new(),
                                score: m.score,
                            })
                        })
                        .max_by_key(|h| h.score)
                });
            let screen = screen.flatten();
            if name_hit.is_none() && screen.is_none() {
                continue;
            }
            let score = name_hit
                .as_ref()
                .map(|h| h.score)
                .or(screen.as_ref().map(|s| s.score))
                .unwrap_or(0);
            rows.push(Row {
                target: Target::Pane {
                    session: a.session.clone(),
                    pane: a.pane.clone(),
                },
                class: Class::Pane,
                score,
                title: format!("{} · {}", a.agent, a.session),
                title_ranges: Vec::new(),
                subtitle: format!("{} · {}", a.pane, a.cwd.display()),
                subtitle_ranges: Vec::new(),
                snippet: screen,
                more: 0,
                more_capped: false,
                key: format!("pane\t{}\t{}", a.session, a.pane),
            });
        }
        rows
    }

    /// Mirror the pane rows into the card-painting state: every matching
    /// pane lights up, the cursor's card gets the "best" treatment.
    fn sync_terminal_scores(&mut self) {
        let mut scores = vec![None; self.terms.panes.len()];
        for row in &self.picker.rows {
            if let Target::Pane { session, pane } = &row.target
                && let Some(i) = self
                    .terms
                    .panes
                    .iter()
                    .position(|a| &a.session == session && &a.pane == pane)
            {
                scores[i] = Some(row.score);
            }
        }
        self.terms.scores = scores;
        self.terms.best = match self.picker.cursor_row().map(|r| &r.target) {
            Some(Target::Pane { session, pane }) => {
                let key = (session.clone(), pane.clone());
                self.terms
                    .panes
                    .iter()
                    .position(|a| a.session == key.0 && a.pane == key.1)
                    .map(|i| (i, key))
            }
            _ => None,
        };
    }

    /// Glide the camera to the highlighted node once it has been the
    /// highlighted node for a moment. Selection is NOT touched: browsing
    /// results must not open the navigator and squeeze the canvas.
    fn picker_follow(&mut self, ctx: &egui::Context) {
        let follow_delay = Duration::from_secs_f32(self.cfg.follow_delay.max(0.0));
        if !self.picker.searching() && !self.picker.user_moved {
            self.picker.follow = None;
            return;
        }
        match (self.picker.cursor_node(), self.picker.follow) {
            (Some(id), Some((prev, at))) if prev == id => {
                let waited = at.elapsed();
                if waited >= follow_delay {
                    if self.picker.followed != Some(id) {
                        self.picker.followed = Some(id);
                        self.frame_node(id);
                    }
                } else {
                    ctx.request_repaint_after(follow_delay - waited);
                }
            }
            (Some(id), _) => {
                self.picker.follow = Some((id, Instant::now()));
                ctx.request_repaint_after(follow_delay);
            }
            (None, _) => {
                self.picker.follow = None;
                self.picker.followed = None;
            }
        }
    }

    /// Load the preview for whatever the cursor is on. Rebuilt when the
    /// cursor moves, when the previewed FILE changed on disk, and every
    /// frame for a terminal card (its screen is live) — never merely
    /// because a reload happened, or an agent saving some other note would
    /// blink the preview every few seconds.
    /// What the pane is about this frame, previewed at most once per
    /// change. The subject is the finder's highlighted row while the
    /// finder is open, else the selection — ONE subject, ONE preview, so
    /// the pane can never grow a second "what is this file" of its own.
    pub(super) fn sync_pane_preview(&mut self) {
        let row = self
            .picker
            .open
            .then(|| self.picker.cursor_row().cloned())
            .flatten();
        let (key, target, hit) = match &row {
            Some(r) => (
                r.key.clone(),
                r.target.clone(),
                r.snippet.as_ref().map(|s| s.line),
            ),
            None => match self.selected {
                Some(id) => (
                    format!("sel\t{}", self.g.node(id).ident()),
                    Target::Node(id.0),
                    None,
                ),
                None => {
                    self.pane_preview = None;
                    return;
                }
            },
        };
        let same = self.pane_preview.as_ref().is_some_and(|p| p.key == key);
        // a terminal's screen is live, and a file can change under a
        // preview that is otherwise unchanged (agents write constantly)
        let live_screen = matches!(target, Target::Pane { .. });
        let file_changed = self
            .pane_preview
            .as_ref()
            .and_then(|p| p.source.as_ref())
            .is_some_and(|(rel, stamp)| {
                stamp.is_some() && super::images::file_stamp(&self.root.join(rel)) != *stamp
            });
        if same && !live_screen && !file_changed {
            return;
        }
        let q = Query::parse(&self.picker.query);
        self.pane_preview = Some(match target {
            Target::Pane { session, pane } => {
                let r = row.as_ref().expect("pane subjects only come from rows");
                let rows = self
                    .terms
                    .cache
                    .get(&(session.clone(), pane.clone()))
                    .map(|c| {
                        c.rows
                            .iter()
                            .map(|r| r.iter().map(|run| run.text.as_str()).collect::<String>())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Preview {
                    key,
                    title: r.title.clone(),
                    subtitle: r.subtitle.clone(),
                    meta: String::new(),
                    subject: None,
                    body: if rows.is_empty() {
                        PreviewBody::Note("(no screen mirrored yet)".into())
                    } else {
                        PreviewBody::Screen(rows)
                    },
                    focus: None,
                    scroll: false,
                    source: None,
                }
            }
            // a refreshed file keeps its scroll: only a NEW subject re-aims
            // the preview at the hit
            Target::Node(i) if (i as usize) < self.g.nodes.len() => {
                self.node_preview(NodeId(i), key, hit, &q, !same)
            }
            Target::Node(_) => Preview {
                key,
                title: String::new(),
                subtitle: String::new(),
                meta: String::new(),
                subject: None,
                body: PreviewBody::Note("(gone in a reload)".into()),
                focus: None,
                scroll: false,
                source: None,
            },
        });
    }

    fn node_preview(
        &mut self,
        id: NodeId,
        key: String,
        hit: Option<usize>,
        q: &Query,
        scroll: bool,
    ) -> Preview {
        let node = self.g.node(id);
        let (kind, path) = (node.kind, node.path.clone());
        let mut meta = String::new();
        if let Ok(m) = std::fs::metadata(self.root.join(&path)) {
            meta = super::previews::size_and_age(&m);
        }
        let mut focus = None;
        // A matched LINE is the only thing the navigator's rendered
        // preview can't show you, so that is exactly when the raw view
        // takes over. Markdown always reads better than source otherwise.
        let textual =
            kind == NodeKind::File || (kind == NodeKind::Asset && filetype::is_text(&path));
        // Source view when a LINE is what matters — a content hit has to
        // be shown where it lives — or when the reader asked for it (`r`).
        // Otherwise a note reads better rendered.
        let raw = textual && (hit.is_some() || self.cfg.preview_raw);
        let body = if !raw {
            PreviewBody::Node(id)
        } else {
            match vault::read_head(&self.root.join(&path), PREVIEW_BYTES) {
                Ok(text) => {
                    // the scan works on the RAW file, so line numbers here
                    // are the ones an editor's +N expects
                    let start = hit.map_or(1, |h| h.saturating_sub(PREVIEW_BEFORE).max(1));
                    // colouring runs from line ONE — a highlighter's state
                    // is what knows whether line 400 is inside a string
                    let colours =
                        highlight::spans(&path, &text, start + PREVIEW_LINES, self.cfg.light)
                            .unwrap_or_default();
                    let mut buf = String::new();
                    let mut lines = Vec::new();
                    for (i, line) in text.lines().enumerate().skip(start - 1).take(PREVIEW_LINES) {
                        let no = i + 1;
                        let m = q.match_line(line, &mut buf);
                        if hit == Some(no) {
                            focus = Some(lines.len());
                        }
                        let shown = cap_chars(line, PREVIEW_LINE_CAP);
                        // a capped line keeps only the colouring that still
                        // has text under it
                        let spans = colours
                            .get(i)
                            .map(|v| {
                                v.iter()
                                    .filter(|s| s.range.end <= shown.len())
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default();
                        lines.push(PreviewLine {
                            no,
                            text: shown,
                            ranges: m.map(|m| m.ranges).unwrap_or_default(),
                            spans,
                            hit: hit == Some(no),
                        });
                    }
                    PreviewBody::Text(lines)
                }
                Err(e) => PreviewBody::Note(format!("cannot read: {e}")),
            }
        };
        let subtitle = match kind {
            NodeKind::Ghost => format!("[[{path}]] — not written yet"),
            _ => path.clone(),
        };
        Preview {
            key,
            title: self.g.node(id).display_name().to_string(),
            subtitle,
            meta,
            subject: Some(id),
            body,
            focus,
            scroll,
            source: Some((
                path.clone(),
                super::images::file_stamp(&self.root.join(&path)),
            )),
        }
    }

    /// Open a file in $EDITOR, jumping to `line` when the editor takes a
    /// line argument.
    fn open_at_line(&self, id: NodeId, line: Option<usize>) {
        match line {
            Some(l) if self.editable(id) => {
                let path = self.root.join(&self.g.node(id).path);
                if let Err(e) = super::actions::spawn_editor_at(&self.cfg, &path, Some(l)) {
                    eprintln!("failed to open {}: {e}", path.display());
                }
            }
            _ => self.open_in_editor(id),
        }
    }
}

/// First `n` chars of a line (display cap — a minified file is one line).
fn cap_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

// ---------------------------------------------------------------- painting

impl Viewer {
    /// The finder itself: a floating prompt a little BELOW the middle of
    /// the canvas with its results stacked underneath, telescope-style.
    /// It is an Area rather than a panel, so the graph keeps its full
    /// width behind it and the eye stays near the center of the screen
    /// instead of being dragged to the top edge. The preview does not live
    /// here — it stays in the side pane, where previews always are.
    pub(super) fn picker_overlay_ui(&mut self, ctx: &egui::Context) {
        if !self.picker.open {
            return;
        }
        // Centered on the WINDOW. It floats in Foreground order, so it
        // draws over the side pane rather than under it, and the eye finds
        // it in the same place whether or not the pane is open.
        let screen = ctx.content_rect();
        let w = (screen.width() * OVERLAY_W_FRAC)
            .clamp(OVERLAY_W_MIN, OVERLAY_W_MAX)
            .min((screen.width() - 24.0).max(120.0));
        let pos = Pos2::new(
            screen.center().x - w * 0.5,
            screen.top() + screen.height() * self.cfg.finder_y,
        );
        let dim = self.theme.text;
        egui::Area::new(egui::Id::new("tg-picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain(false)
            .show(ctx, |ui| {
                ui.set_width(w);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(w);
                    let browsing = self.picker.browsing().map(str::to_string);
                    ui.horizontal(|ui| {
                        let (tag, hint) = match &browsing {
                            Some(dir) => (
                                if dir.is_empty() {
                                    "/".to_string()
                                } else {
                                    format!("{dir}/")
                                },
                                "filter this folder",
                            ),
                            None => ("find".to_string(), "name, path, or words in the text"),
                        };
                        ui.label(egui::RichText::new(tag).color(if browsing.is_some() {
                            self.theme.dir
                        } else {
                            dim
                        }));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.picker.query)
                                .hint_text(hint)
                                .font(FontId::proportional(15.0))
                                .desired_width(f32::INFINITY),
                        );
                        // the prompt owns the keyboard for as long as the
                        // picker is open: a click anywhere else drops widget
                        // focus, and every keystroke after it would fall
                        // through to the graph keybinds
                        if self.picker.focus_pending || ui.memory(|m| m.focused().is_none()) {
                            resp.request_focus();
                            self.picker.focus_pending = false;
                        }
                    });
                    ui.horizontal(|ui| {
                        let n = self.picker.rows.len();
                        let content = self.picker.content.len();
                        let mut line = if browsing.is_some() {
                            format!("{n} entr{}", if n == 1 { "y" } else { "ies" })
                        } else if self.picker.query.trim().is_empty() {
                            format!("{n} recently edited · type to search everything")
                        } else {
                            format!(
                                "{n} result{} · {content} file{} by content",
                                if n == 1 { "" } else { "s" },
                                if content == 1 { "" } else { "s" }
                            )
                        };
                        if self.picker.scan_hint() {
                            line.push_str(" · scanning…");
                        }
                        ui.label(egui::RichText::new(line).small().color(dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let hints = if browsing.is_some() {
                                "↑↓ move · ↵ open · ⌫ up · ⇥ search all · esc close"
                            } else if self.picker.searching() {
                                "↑↓ move · ↵ jump · ^↵ edit at line · esc close"
                            } else {
                                "↑↓ walk · esc close"
                            };
                            ui.label(egui::RichText::new(hints).small().color(dim));
                        });
                    });
                    // The list is drawn whenever there are rows — with an
                    // empty prompt those are the recently edited files,
                    // which used to be built and then never shown.
                    ui.separator();
                    // The list runs from here to the bottom margin — but
                    // measured against the SCREEN, never against
                    // `ui.available_height()`. An Area sizes its Ui from
                    // last frame's content (`state.size = min_size()`), so
                    // asking it how much room is left feeds the list its
                    // own previous height: one narrow result set shrank the
                    // area, which capped the next list shorter, which
                    // shrank it again. A latch that only ever ratchets
                    // down. The cursor's top is content-independent (prompt
                    // + status are fixed height), so this is stable.
                    let h = (screen.bottom() - 8.0 - ui.cursor().top()).max(ROW_H * 3.0);
                    self.picker_list(ui, h);
                });
            });
    }

    fn picker_list(&mut self, ui: &mut egui::Ui, max_h: f32) {
        if self.picker.rows.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(if self.picker.query.trim().is_empty() {
                    "type to search"
                } else if self.picker.scan_hint() {
                    "no name matches yet — still scanning contents…"
                } else {
                    "nothing matches"
                })
                .color(self.theme.text),
            );
            return;
        }
        let cursor = self.picker.cursor;
        // The list RESERVES its height instead of shrinking to its rows.
        // An egui Area sizes its Ui from last frame's content, so a list
        // that shrank to two results left the Ui too short to hold a
        // bigger one next frame — the finder got smaller and stayed
        // smaller. Reserving also stops the box resizing under the eye on
        // every keystroke, which is what a telescope-style finder wants
        // anyway.
        let mut area = egui::ScrollArea::vertical()
            .id_salt("tg-picker-list")
            .max_height(max_h)
            .min_scrolled_height(max_h)
            .auto_shrink([false, false]);
        if self.picker.scroll {
            self.picker.scroll = false;
            // keep the cursor row inside the viewport with the SMALLEST
            // move that does it — jumping it to the middle on every step
            // makes the list scroll under a stationary eye
            let (top, bottom) = (cursor as f32 * ROW_H, (cursor + 1) as f32 * ROW_H);
            let mut off = self.picker.list_offset;
            off = off.min(top).max(bottom - self.picker.list_h.max(ROW_H));
            area = area.vertical_scroll_offset(off.max(0.0));
        }
        let mut clicked = None;
        let out = area.show_rows(ui, ROW_H, self.picker.rows.len(), |ui, range| {
            for i in range {
                if self.picker_row_ui(ui, i, i == cursor) {
                    clicked = Some(i);
                }
            }
        });
        self.picker.list_offset = out.state.offset.y;
        self.picker.list_h = out.inner_rect.height();
        if let Some(i) = clicked {
            self.picker.cursor = i;
            self.picker.cursor_key = self.picker.cursor_row().map(|r| r.key.clone());
            self.picker_accept(false);
        }
    }

    /// One result row: icon + title + dim path, and under it the matched
    /// line. Returns whether it was clicked.
    fn picker_row_ui(&mut self, ui: &mut egui::Ui, i: usize, current: bool) -> bool {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
        let row = &self.picker.rows[i];
        if current {
            ui.painter()
                .rect_filled(rect, 4.0, self.theme.select.gamma_multiply(0.22));
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, 4.0, self.theme.hover.gamma_multiply(0.10));
        }
        let (glyph, icon_color) = match &row.target {
            Target::Node(n) if (*n as usize) < self.g.nodes.len() => self.node_icon(NodeId(*n)),
            _ => ('\u{f120}', self.theme.web), // a terminal
        };
        let accent = self.theme.select;
        let strong = if current {
            self.theme.hover
        } else {
            self.theme.file
        };
        let dim = self.theme.text;
        let pad = 6.0;
        let mut job = egui::text::LayoutJob::default();
        job.append(
            &glyph.to_string(),
            0.0,
            egui::TextFormat {
                font_id: icon_font(13.0),
                color: icon_color,
                ..Default::default()
            },
        );
        push_marked(
            &mut job,
            &row.title,
            &row.title_ranges,
            8.0,
            13.5,
            strong,
            accent,
        );
        if !row.subtitle.is_empty() {
            push_marked(
                &mut job,
                &row.subtitle,
                &row.subtitle_ranges,
                8.0,
                11.0,
                dim,
                accent,
            );
        }
        job.wrap = one_line(rect.width() - 2.0 * pad);
        let galley = ui.painter().layout_job(job);
        ui.painter()
            .galley(rect.min + Vec2::new(pad, 2.0), galley, strong);
        if let Some(hit) = &row.snippet {
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &format!("{:>4}  ", hit.line),
                0.0,
                egui::TextFormat {
                    font_id: FontId::monospace(10.5),
                    color: dim.gamma_multiply(0.8),
                    ..Default::default()
                },
            );
            push_marked_mono(&mut job, &hit.text, &hit.ranges, 10.5, dim, accent);
            if row.more > 0 {
                job.append(
                    &format!("  +{}{}", row.more, if row.more_capped { "+" } else { "" }),
                    6.0,
                    egui::TextFormat {
                        font_id: FontId::monospace(10.0),
                        color: accent.gamma_multiply(0.8),
                        ..Default::default()
                    },
                );
            }
            job.wrap = one_line(rect.width() - 2.0 * pad);
            let galley = ui.painter().layout_job(job);
            ui.painter()
                .galley(rect.min + Vec2::new(pad, 18.0), galley, dim);
        }
        resp.clicked()
    }
}

/// One-line, ellipsized text wrapping for a row of the given width.
pub(super) fn one_line(max_width: f32) -> egui::text::TextWrapping {
    egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    }
}

/// Append `text` to `job` with `ranges` highlighted — the fuzzy/literal
/// characters that actually matched, so a result explains itself.
fn push_marked(
    job: &mut egui::text::LayoutJob,
    text: &str,
    ranges: &[search::Range],
    lead: f32,
    size: f32,
    base: Color32,
    accent: Color32,
) {
    push_marked_font(
        job,
        text,
        ranges,
        lead,
        FontId::proportional(size),
        base,
        accent,
    );
}

pub(super) fn push_marked_mono(
    job: &mut egui::text::LayoutJob,
    text: &str,
    ranges: &[search::Range],
    size: f32,
    base: Color32,
    accent: Color32,
) {
    push_marked_font(
        job,
        text,
        ranges,
        6.0,
        FontId::monospace(size),
        base,
        accent,
    );
}

/// One line of source: syntax colour underneath, search matches marked
/// with a background rather than a colour of their own — recolouring the
/// hit would erase exactly the colouring the source view is for.
pub(super) fn push_source_line(
    job: &mut egui::text::LayoutJob,
    text: &str,
    matches: &[search::Range],
    spans: &[highlight::Span],
    size: f32,
    base: Color32,
    accent: Color32,
) {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for &(s, e) in matches {
        cuts.push(s);
        cuts.push(e);
    }
    for sp in spans {
        cuts.push(sp.range.start);
        cuts.push(sp.range.end);
    }
    cuts.retain(|c| *c <= text.len() && text.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let hit = matches.iter().any(|&(s, e)| s <= a && b <= e);
        let sp = spans
            .iter()
            .find(|sp| sp.range.start <= a && b <= sp.range.end);
        let color = sp.map_or(base, |s| Color32::from_rgb(s.color.0, s.color.1, s.color.2));
        job.append(
            &text[a..b],
            0.0,
            egui::TextFormat {
                font_id: FontId::monospace(size),
                color,
                italics: sp.is_some_and(|s| s.italic),
                background: if hit {
                    accent.gamma_multiply(0.35)
                } else {
                    Color32::TRANSPARENT
                },
                ..Default::default()
            },
        );
    }
}

fn push_marked_font(
    job: &mut egui::text::LayoutJob,
    text: &str,
    ranges: &[search::Range],
    lead: f32,
    font: FontId,
    base: Color32,
    accent: Color32,
) {
    let fmt = |color: Color32| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut at = 0usize;
    let mut lead = lead;
    for &(s, e) in ranges {
        // ranges are sorted and clipped by their producer, but a stale
        // snippet (refiltered between frames) can still point past the end
        if s < at || e > text.len() || !text.is_char_boundary(s) || !text.is_char_boundary(e) {
            continue;
        }
        if s > at {
            job.append(&text[at..s], lead, fmt(base));
            lead = 0.0;
        }
        job.append(&text[s..e], lead, fmt(accent));
        lead = 0.0;
        at = e;
    }
    if at < text.len() {
        job.append(&text[at..], lead, fmt(base));
    }
}
