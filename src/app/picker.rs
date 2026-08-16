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
/// Dwell on a result before the camera glides to it — arrowing quickly
/// through 30 rows must not launch 30 glides.
const FOLLOW_DELAY: Duration = Duration::from_millis(120);
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
/// Rows a Ctrl+D / Ctrl+U half-page jump moves.
const HALF_PAGE: isize = 8;
/// The floating finder's width, as a fraction of the canvas and clamped.
const OVERLAY_W_FRAC: f32 = 0.46;
const OVERLAY_W_MIN: f32 = 380.0;
const OVERLAY_W_MAX: f32 = 760.0;
/// Where the prompt sits down the canvas — just below the middle, so the
/// results have room to stack under it without leaving the eye's center.
const PROMPT_Y_FRAC: f32 = 0.52;
/// Height budget for the result list under the prompt.
const LIST_H_FRAC: f32 = 0.36;
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
pub(super) const FRAME_LIFT_FRAC: f32 = 0.5 - PROMPT_Y_FRAC * 0.5;

pub(super) enum ScanMsg {
    Hits(u64, Vec<FileHits>),
    Done(u64, Query, search::ScanOutcome),
}

/// One rendered line of the preview pane.
struct PreviewLine {
    no: usize,
    text: String,
    ranges: Vec<search::Range>,
    /// The line the result row points at.
    hit: bool,
}

enum PreviewBody {
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
    key: String,
    title: String,
    subtitle: String,
    meta: String,
    body: PreviewBody,
    /// Index into `Text` lines that the hit sits on.
    focus: Option<usize>,
    /// Scroll the hit into view on the next frame.
    scroll: bool,
    /// (vault-relative path, stamp) the raw lines were read from — a
    /// reload re-reads only when THIS file changed, so an agent saving
    /// something else can't blink the preview.
    source: Option<(String, Option<super::images::Stamp>)>,
}

pub(super) struct Picker {
    pub(super) open: bool,
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
    cursor_key: Option<String>,
    /// Keep the cursor row in view on the next paint.
    scroll: bool,
    list_offset: f32,
    list_h: f32,
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
    content: HashMap<String, (u64, FileHits)>,
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
    pub(super) preview: Option<Preview>,
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
            preview: None,
            follow: None,
            followed: None,
            user_moved: false,
        }
    }

    /// Is a non-empty query live? Drives the canvas lit mask.
    pub(super) fn searching(&self) -> bool {
        self.open && !self.query.trim().is_empty()
    }

    pub(super) fn open(&mut self) {
        self.open = true;
        self.focus_pending = true;
        self.dirty = true;
    }

    pub(super) fn close(&mut self) {
        self.open = false;
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
        self.preview = None;
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
        let (esc, enter, ctrl, alt) = ui.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.modifiers.command,
                i.modifiers.alt,
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
        // An empty query IS the ranger: the arrows drive its sibling
        // column, exactly like j/k do when the prompt isn't focused. There
        // is no result list to walk yet, and stepping an invisible one
        // would move the camera for no visible reason.
        if !self.picker.searching() {
            if step != 0 {
                self.walk_siblings(step);
            }
            if enter {
                self.picker.close(); // nothing to take — back to plain browsing
            }
            return;
        }
        if step != 0 {
            self.picker.move_cursor(step);
        }
        if enter {
            // Ctrl/Alt+Enter opens the file in $EDITOR at the matched line
            self.picker_accept(ctrl || alt);
        }
    }

    /// Step the ranger's cursor by `delta` siblings (arrow keys with an
    /// empty prompt). With nothing selected yet, the first step enters the
    /// vault root, so `f` + ↓ starts browsing.
    fn walk_siblings(&mut self, delta: isize) {
        let to = match self.selected {
            Some(sel) => self.g.nav_sibling(sel, delta),
            None => self.g.nav_enter(self.g.root),
        };
        if let Some(t) = to {
            self.selected = Some(t);
            self.frame_node(t);
            self.nav_scroll = true;
            self.conn_cursor = None;
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
        self.load_preview();
    }

    /// Kick a content scan for `q` on a worker thread. Candidate files are
    /// every textual leaf in path order — or, when the query merely grew
    /// and the last scan finished whole, only the files that matched it.
    fn start_scan(&mut self, ctx: &egui::Context, q: Query) {
        self.picker.generation += 1;
        let generation = self.picker.generation;
        self.picker.live.store(generation, Ordering::Relaxed);
        if q.is_empty() {
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
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let cancelled = || live.load(Ordering::Relaxed) != generation;
            let outcome = search::scan_files(&root, &q, &files, &cancelled, &mut |batch| {
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
                key: n.ident(),
            });
        }
        self.picker.name_scores = scores;
        self.picker.name_rows = rows;
    }

    /// Merge the three sources — cached fuzzy name hits, streamed content
    /// hits, live terminal panes — into one ranked list, at most one row
    /// per node, and put the cursor back on the row it was on.
    fn rebuild_rows(&mut self) {
        let q = Query::parse(&self.picker.query);
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
        if !self.picker.searching() && !self.picker.user_moved {
            self.picker.follow = None;
            return;
        }
        match (self.picker.cursor_node(), self.picker.follow) {
            (Some(id), Some((prev, at))) if prev == id => {
                let waited = at.elapsed();
                if waited >= FOLLOW_DELAY {
                    if self.picker.followed != Some(id) {
                        self.picker.followed = Some(id);
                        self.frame_node(id);
                    }
                } else {
                    ctx.request_repaint_after(FOLLOW_DELAY - waited);
                }
            }
            (Some(id), _) => {
                self.picker.follow = Some((id, Instant::now()));
                ctx.request_repaint_after(FOLLOW_DELAY);
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
    fn load_preview(&mut self) {
        let Some(row) = self.picker.cursor_row().cloned() else {
            self.picker.preview = None;
            return;
        };
        let same_row = self
            .picker
            .preview
            .as_ref()
            .is_some_and(|p| p.key == row.key);
        let live_screen = matches!(row.target, Target::Pane { .. });
        let file_changed = self
            .picker
            .preview
            .as_ref()
            .and_then(|p| p.source.as_ref())
            .is_some_and(|(rel, stamp)| {
                stamp.is_some() && super::images::file_stamp(&self.root.join(rel)) != *stamp
            });
        if same_row && !live_screen && !file_changed {
            return;
        }
        let q = Query::parse(&self.picker.query);
        self.picker.preview = Some(match &row.target {
            Target::Pane { session, pane } => {
                let key = (session.clone(), pane.clone());
                let rows = self
                    .terms
                    .cache
                    .get(&key)
                    .map(|c| {
                        c.rows
                            .iter()
                            .map(|r| r.iter().map(|run| run.text.as_str()).collect::<String>())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Preview {
                    key: row.key.clone(),
                    title: row.title.clone(),
                    subtitle: row.subtitle.clone(),
                    meta: String::new(),
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
            // a refreshed file keeps its scroll: only a NEW row re-aims the
            // preview at the hit
            Target::Node(i) => self.node_preview(NodeId(*i), &row, &q, !same_row),
        });
    }

    fn node_preview(&mut self, id: NodeId, row: &Row, q: &Query, scroll: bool) -> Preview {
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
        let body = match row.snippet.as_ref().map(|s| s.line).filter(|_| textual) {
            None => PreviewBody::Node(id),
            Some(hit) => match vault::read_head(&self.root.join(&path), PREVIEW_BYTES) {
                Ok(text) => {
                    // the scan works on the RAW file, so line numbers here
                    // are the ones an editor's +N expects
                    let start = hit.saturating_sub(PREVIEW_BEFORE).max(1);
                    let mut buf = String::new();
                    let mut lines = Vec::new();
                    for (i, line) in text.lines().enumerate().skip(start - 1).take(PREVIEW_LINES) {
                        let no = i + 1;
                        let m = q.match_line(line, &mut buf);
                        if hit == no {
                            focus = Some(lines.len());
                        }
                        lines.push(PreviewLine {
                            no,
                            text: cap_chars(line, PREVIEW_LINE_CAP),
                            ranges: m.map(|m| m.ranges).unwrap_or_default(),
                            hit: hit == no,
                        });
                    }
                    PreviewBody::Text(lines)
                }
                Err(e) => PreviewBody::Note(format!("cannot read: {e}")),
            },
        };
        let subtitle = match kind {
            NodeKind::Ghost => format!("[[{path}]] — not written yet"),
            _ => path.clone(),
        };
        Preview {
            key: row.key.clone(),
            title: self.g.node(id).display_name().to_string(),
            subtitle,
            meta,
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
                if let Err(e) = super::actions::spawn_editor_at(&path, Some(l)) {
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
        // center over the CANVAS, not the window: the side pane holds the
        // preview, and an overlay centered on the window would sit half
        // underneath it
        let screen = self.last_canvas_rect.unwrap_or_else(|| ctx.content_rect());
        let w = (screen.width() * OVERLAY_W_FRAC)
            .clamp(OVERLAY_W_MIN, OVERLAY_W_MAX)
            .min((screen.width() - 24.0).max(120.0));
        let pos = Pos2::new(
            screen.center().x - w * 0.5,
            screen.top() + screen.height() * PROMPT_Y_FRAC,
        );
        let list_h = (screen.height() * LIST_H_FRAC).clamp(120.0, 520.0);
        let dim = self.theme.text;
        egui::Area::new(egui::Id::new("tg-picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain_to(screen.shrink(8.0))
            .show(ctx, |ui| {
                ui.set_width(w);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(w);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("find").color(dim));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.picker.query)
                                .hint_text("name, path, or words in the text")
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
                        let mut line = if self.picker.query.trim().is_empty() {
                            "type to search names, paths, contents and terminals".to_string()
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
                            let hints = if self.picker.searching() {
                                "↑↓ move · ↵ jump · ^↵ edit at line · esc close"
                            } else {
                                "↑↓ walk · esc close"
                            };
                            ui.label(egui::RichText::new(hints).small().color(dim));
                        });
                    });
                    if self.picker.searching() {
                        ui.separator();
                        self.picker_list(ui, list_h);
                    }
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
        let mut area = egui::ScrollArea::vertical()
            .id_salt("tg-picker-list")
            .max_height(max_h)
            .auto_shrink([false, true]);
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
                    &format!("  +{}", row.more),
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

    /// The highlighted result, previewed in the side pane — the same place
    /// a walked-to node previews, so the eye never has to learn a second
    /// spot for "what is this file".
    pub(super) fn picker_preview_ui(&mut self, ui: &mut egui::Ui) {
        let Some(preview) = self.picker.preview.take() else {
            return;
        };
        let dim = self.theme.text;
        ui.add_space(2.0);
        ui.label(egui::RichText::new(&preview.title).strong().size(14.0));
        ui.label(egui::RichText::new(&preview.subtitle).small().color(dim));
        if !preview.meta.is_empty() {
            ui.label(egui::RichText::new(&preview.meta).small().color(dim));
        }
        ui.add_space(3.0);
        let accent = self.theme.select;
        let text_color = self.theme.file;
        match &preview.body {
            PreviewBody::Text(lines) => {
                // measure one line rather than asking Fonts for a row
                // height — `ui.fonts()` hands out a read-only view
                let line_h = ui
                    .painter()
                    .layout_no_wrap("0".into(), FontId::monospace(11.5), dim)
                    .size()
                    .y;
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("tg-picker-preview")
                    .auto_shrink([false, false]);
                if preview.scroll
                    && let Some(focus) = preview.focus
                {
                    // land the hit a few lines below the top edge, with the
                    // context above it visible
                    area = area.vertical_scroll_offset(focus.saturating_sub(6) as f32 * line_h);
                }
                area.show_rows(ui, line_h, lines.len(), |ui, range| {
                    for l in &lines[range] {
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            &format!("{:>5} ", l.no),
                            0.0,
                            egui::TextFormat {
                                font_id: FontId::monospace(11.5),
                                color: dim.gamma_multiply(if l.hit { 1.0 } else { 0.55 }),
                                ..Default::default()
                            },
                        );
                        push_marked_mono(&mut job, &l.text, &l.ranges, 11.5, text_color, accent);
                        job.wrap = one_line(ui.available_width());
                        let galley = ui.painter().layout_job(job);
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), line_h),
                            Sense::hover(),
                        );
                        if l.hit {
                            ui.painter()
                                .rect_filled(rect, 2.0, accent.gamma_multiply(0.16));
                        }
                        ui.painter().galley(rect.min, galley, text_color);
                    }
                });
            }
            PreviewBody::Screen(rows) => {
                egui::ScrollArea::vertical()
                    .id_salt("tg-picker-screen")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for r in rows {
                            ui.label(
                                egui::RichText::new(r.as_str())
                                    .monospace()
                                    .size(11.0)
                                    .color(text_color),
                            );
                        }
                    });
            }
            PreviewBody::Node(id) => {
                // a name match, a folder, a picture: the pane shows exactly
                // what walking to it would show
                if let Some(j) = self.preview_column(ui, *id, false) {
                    // a link clicked inside the preview is a jump, and a
                    // jump ends the search like taking a result does
                    self.selected = Some(j);
                    self.frame_node(j);
                    self.picker.close();
                }
            }
            PreviewBody::Note(note) => {
                ui.label(egui::RichText::new(note.as_str()).color(dim));
            }
        }
        self.picker.preview = Some(Preview {
            scroll: false,
            ..preview
        });
    }
}

/// One-line, ellipsized text wrapping for a row of the given width.
fn one_line(max_width: f32) -> egui::text::TextWrapping {
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

fn push_marked_mono(
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
