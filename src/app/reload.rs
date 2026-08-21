//! Keeping the graph and disk in sync: the debounced watcher, the
//! background scan+build worker, carry-over application, and view-state
//! persistence.

use super::*;

pub(super) type ReloadMsg = (u64, anyhow::Result<Graph>);

/// The live-reload machinery: the filesystem watcher, its debounce clock,
/// the scan-worker channel, and reload health. One scan+build runs at a
/// time; a debounce that expires mid-scan queues exactly one trailing
/// rescan; results from superseded generations are discarded on arrival.
/// The worker bookkeeping is private to this module — the rest of the app
/// sees only the debounce clock (`event_at`) and the health fields.
pub(super) struct Reload {
    /// Kept alive for the watcher thread; None if watching failed.
    pub(super) _watcher: Option<notify::RecommendedWatcher>,
    /// Timestamp of the last relevant filesystem event (debounce state).
    pub(super) event_at: Arc<Mutex<Option<Instant>>>,
    /// Startup or callback failure from notify, separate from scan/build
    /// failures so a successful recovery scan cannot erase the warning.
    pub(super) watch_error: Arc<Mutex<Option<String>>>,
    /// Monotonic reload request counter — results from superseded requests
    /// are discarded on arrival.
    generation: u64,
    /// A scan+build worker is running — at most ONE at a time (a slow scan
    /// under a fast save cadence must not stack concurrent full walks).
    scan_inflight: bool,
    /// A debounce expired while a scan was in flight; run one trailing
    /// rescan when it lands.
    rescan_queued: bool,
    tx: std::sync::mpsc::Sender<ReloadMsg>,
    rx: std::sync::mpsc::Receiver<ReloadMsg>,
    /// Health state, surfaced by the diagnostics badge.
    pub(super) last_done: Option<Instant>,
    pub(super) error: Option<String>,
}

/// View-state persistence bookkeeping: what was last written, the save
/// debounce clock, the write-failure warning latch, and the unknown line
/// kinds carried through load→save verbatim (forward compatibility).
/// Every field is private to this module — the save path is the only
/// code that may touch them.
pub(super) struct Persist {
    /// View state as last written to `.text-graph/view` (skip no-op saves).
    saved: Option<state::ViewState>,
    /// Line kinds the loaded view file had that this version doesn't know —
    /// written back verbatim on every save.
    unknown: Vec<String>,
    last_save: Instant,
    warned: bool,
}

impl Persist {
    pub(super) fn new(unknown: Vec<String>) -> Self {
        Persist {
            saved: None,
            unknown,
            last_save: Instant::now(),
            warned: false,
        }
    }
}

impl Reload {
    pub(super) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Reload {
            _watcher: None,
            event_at: Arc::new(Mutex::new(None)),
            watch_error: Arc::new(Mutex::new(None)),
            generation: 0,
            scan_inflight: false,
            rescan_queued: false,
            tx,
            rx,
            last_done: None,
            error: None,
        }
    }
}

/// Should this watcher callback schedule a rebuild? A kernel queue
/// overflow arrives as a Rescan-flagged event with NO paths — the one
/// signal that says events were LOST — and a watch error can mean the
/// same; both must count, or the graph goes silently stale after a burst
/// (an in-vault `cargo build` or `git checkout` floods the same queue,
/// since the recursive watch covers even dirs the relevance filter skips).
/// A rebuild here is always a full rescan, so over-triggering is safe.
fn reload_worthy(root: &Path, res: &Result<notify::Event, notify::Error>) -> bool {
    match res {
        Ok(event) => {
            event.need_rescan() || event.paths.iter().any(|p| vault::watch_relevant(root, p))
        }
        Err(_) => true,
    }
}

/// Record a callback result and schedule a recovery scan when warranted.
/// Watcher failures live in their own health channel: a subsequent successful
/// vault scan proves the graph recovered, not that kernel notifications did.
fn record_watch_result(
    root: &Path,
    reload_at: &Mutex<Option<Instant>>,
    watch_error: &Mutex<Option<String>>,
    res: &Result<notify::Event, notify::Error>,
) -> bool {
    if let Err(error) = res {
        *watch_error.lock().unwrap() = Some(error.to_string());
    }
    let worthy = reload_worthy(root, res);
    if worthy {
        *reload_at.lock().unwrap() = Some(Instant::now());
    }
    worthy
}

impl Viewer {
    /// Watch the vault; on a relevant event, stamp the debounce clock and
    /// wake the UI thread. Startup and runtime failures are retained for
    /// the health window instead of being reduced to a generic OFF state.
    pub(super) fn start_watcher(&mut self, ctx: egui::Context) {
        use notify::Watcher as _;
        self.reload._watcher = None;
        *self.reload.watch_error.lock().unwrap() = None;
        let state = self.reload.event_at.clone();
        let watch_error = self.reload.watch_error.clone();
        let handler_error = watch_error.clone();
        let root = self.root.clone();
        let handler = move |res: Result<notify::Event, notify::Error>| {
            if record_watch_result(&root, &state, &handler_error, &res) {
                ctx.request_repaint();
            }
        };
        match notify::recommended_watcher(handler) {
            Ok(mut watcher) => {
                if let Err(error) = watcher.watch(&self.root, notify::RecursiveMode::Recursive) {
                    *watch_error.lock().unwrap() = Some(format!(
                        "cannot watch {} recursively: {error}",
                        self.root.display()
                    ));
                } else {
                    self.reload._watcher = Some(watcher);
                }
            }
            Err(error) => {
                *watch_error.lock().unwrap() = Some(format!("cannot start file watcher: {error}"));
            }
        }
    }

    /// Swap a freshly built graph in, carrying over sim positions,
    /// selection, and search identity by path so an edit ripples the layout
    /// instead of re-settling it. Identity is `Node::ident` (ghosts
    /// namespaced — see graph.rs). The expensive scan+build happens on a
    /// worker thread (see `ui`); only this cheap carry-over runs here.
    pub(super) fn apply_graph(&mut self, g: Graph) {
        self.reload.last_done = Some(Instant::now());
        self.reload.error = None;

        // A reload that changes no node identities and no link endpoints
        // (an agent streaming text into existing notes — the common case)
        // keeps the CURRENT sim untouched: zero motion. Reheating on every
        // save kept the graph in near-constant drift under busy agents,
        // which made node hover (and its dwell popup) impossible to land.
        let same_structure = self.g.nodes.len() == g.nodes.len()
            && self
                .g
                .nodes
                .iter()
                .zip(&g.nodes)
                .all(|(a, b)| a.ident() == b.ident())
            && self.g.links.len() == g.links.len()
            && self
                .g
                .links
                .iter()
                .zip(&g.links)
                .all(|(a, b)| (a.from, a.to) == (b.from, b.to));
        let sim = if same_structure {
            None
        } else {
            let old_pos: HashMap<String, (f32, f32)> = self
                .g
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.ident(), (self.sim.x[i], self.sim.y[i])))
                .collect();
            let mut sim = Sim::new(&g);
            sim.configure(self.cfg.spread, self.cfg.freeze, !self.show_dirs);
            for (i, node) in g.nodes.iter().enumerate() {
                if let Some(&(x, y)) = old_pos.get(&node.ident()) {
                    sim.x[i] = x;
                    sim.y[i] = y;
                }
            }
            sim.calm();
            Some(sim)
        };

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
        self.menu.node = self
            .menu
            .node
            .and_then(|id| by_ident.get(&self.g.node(id).ident()).copied());
        // hover and its dwell REMAP like the selection — clearing them made
        // the preview popup unlandable under busy agents (a reload every
        // few seconds reset the dwell forever)
        self.hover = self
            .hover
            .and_then(|id| by_ident.get(&self.g.node(id).ident()).copied());
        self.hover_since = self.hover_since.take().and_then(|(id, t, a)| {
            by_ident
                .get(&self.g.node(id).ident())
                .map(|&nid| (nid, t, a))
        });
        self.hover_body = None; // body may have changed on disk — re-read
        // the glide REMAPS too: cancelling parked the camera mid-flight
        // whenever a reload landed inside the 180ms window (hjkl walking
        // while an agent saves), leaving the new selection off-center
        self.cam.anim = self.cam.anim.take().and_then(|(from, id, t)| {
            by_ident
                .get(&self.g.node(id).ident())
                .map(|&nid| (from, nid, t))
        });
        self.conn_cursor = None; // indexes the old graph's link lists

        self.derived = Self::derived(&g, self.cfg.node_scale);
        // rows and content hits index the OLD arena — the picker re-derives
        // them (keeping its query and its cursor's identity)
        self.picker.on_reload(g.nodes.len());
        if !same_structure {
            // The cached body carries tg:// links built from NODE INDEXES;
            // a structural reload renumbers them, so clicking one would
            // jump somewhere else entirely. A text-only reload (the common
            // case under agents) keeps the body — `preview_column` re-reads
            // it by (mtime, len) if the file itself changed.
            self.detail = None;
            self.detail_stamp = None;
            // the pane's preview is keyed by SUBJECT identity, which a
            // structural reload doesn't change — but the NodeId inside it
            // does, and a stale one previews (and jumps to) another node
            self.pane_preview = None;
        }
        // evict only thumbnails/excerpts whose file actually changed —
        // reloads are frequent (agents writing notes) and a full clear made
        // every image flicker through its placeholder
        self.thumbs.retain_fresh();
        self.previews.retain_fresh();
        self.g = g;
        if let Some(sim) = sim {
            self.sim = sim;
        }

        // a note we just created: select and frame it the moment it lands
        if let Some(p) = self.menu.pending_select.clone()
            && let Some(i) = self
                .g
                .nodes
                .iter()
                .position(|n| n.kind != NodeKind::Ghost && n.ident() == p)
        {
            self.menu.pending_select = None;
            self.selected = Some(NodeId(i as u32));
            self.frame_node(NodeId(i as u32));
        }
    }

    /// Camera + every card arrangement, live or parked, sorted for a
    /// deterministic file.
    pub(super) fn snapshot_state(&self) -> state::ViewState {
        let mut cards: Vec<state::CardPos> = self
            .terms
            .offsets
            .iter()
            .map(|((s, p), off)| state::CardPos {
                session: s.clone(),
                pane: p.clone(),
                dx: off.x,
                dy: off.y,
            })
            .collect();
        for (s, list) in &self.terms.parked {
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
        let mut pins: Vec<(String, String)> = self.terms.pinned.keys().cloned().collect();
        for (s, list) in &self.terms.parked_pins {
            for (p, ()) in list {
                pins.push((s.clone(), p.clone()));
            }
        }
        pins.sort();
        state::ViewState {
            camera: Some((self.cam.center.x, self.cam.center.y, self.cam.zoom)),
            cards,
            pins,
            pane_width: self.pane_width,
            hide_web: !self.show_web,
            hide_dirs: !self.show_dirs,
            // theme and default agent live in the per-user config now;
            // these two are migration-only fields (see state.rs)
            light: None,
            default_agent: None,
            unknown: self.persist.unknown.clone(),
        }
    }

    /// Debounced view-state save; `force` (exit) skips the debounce. Errors
    /// warn once and go quiet (read-only vaults stay usable).
    pub(super) fn persist_state(&mut self, force: bool) {
        if !force && self.persist.last_save.elapsed() < Duration::from_secs(3) {
            return;
        }
        let s = self.snapshot_state();
        if self.persist.saved.as_ref() == Some(&s) {
            return;
        }
        self.persist.last_save = Instant::now();
        match state::save(&self.root, &s) {
            Ok(()) => self.persist.saved = Some(s),
            Err(e) => {
                if !self.persist.warned {
                    eprintln!("couldn't save view state: {e}");
                    self.persist.warned = true;
                }
            }
        }
    }
}

impl Viewer {
    /// Debounce the watcher's events; when the vault has been quiet, kick a
    /// scan+build worker (45ms at 500 files — a visible hitch on the UI
    /// thread, and agents save constantly) and apply finished results,
    /// discarding any superseded by a newer save. Called once per frame.
    pub(super) fn pump_reload(&mut self, ctx: &egui::Context) {
        let due = {
            let mut at = self.reload.event_at.lock().unwrap();
            match *at {
                Some(t) if t.elapsed() >= Duration::from_millis(300) => {
                    *at = None;
                    true
                }
                Some(_) => {
                    ctx.request_repaint_after(Duration::from_millis(120));
                    false
                }
                None => false,
            }
        };
        if due {
            if self.reload.scan_inflight {
                // single-flight: a slow scan under a fast save cadence must
                // not stack concurrent full walks — remember and re-fire
                // one trailing rescan when the running one lands
                self.reload.rescan_queued = true;
            } else {
                self.reload.scan_inflight = true;
                self.reload.generation += 1;
                let generation = self.reload.generation;
                let root = self.root.clone();
                let tx = self.reload.tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let res = vault::scan(&root).map(graph::build);
                    let _ = tx.send((generation, res));
                    ctx.request_repaint();
                });
            }
        }
        while let Ok((generation, res)) = self.reload.rx.try_recv() {
            self.reload.scan_inflight = false;
            if self.reload.rescan_queued {
                self.reload.rescan_queued = false;
                // already past its debounce — due again on the next frame
                *self.reload.event_at.lock().unwrap() = Some(
                    Instant::now()
                        .checked_sub(Duration::from_millis(300))
                        .unwrap_or_else(Instant::now),
                );
                ctx.request_repaint();
            }
            if generation != self.reload.generation {
                continue; // superseded by a newer save — discard stale build
            }
            match res {
                Ok(g) => self.apply_graph(g),
                Err(e) => self.reload.error = Some(format!("{e:#}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_graph::{graph, vault};

    fn fixture_viewer() -> Viewer {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
        let scan = vault::scan(&root).expect("fixture scans");
        Viewer::new(graph::build(scan), root, config::Config::default())
    }

    /// A throwaway 1-file vault, built and cleaned per test.
    fn tiny_graph() -> Graph {
        let d = std::env::temp_dir().join(format!(
            "tg-reload-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("only.md"), "hello").unwrap();
        let g = graph::build(vault::scan(&d).expect("tiny scans"));
        let _ = std::fs::remove_dir_all(&d);
        g
    }

    #[test]
    fn overflow_and_errors_trigger_a_rescan_but_irrelevant_paths_do_not() {
        let root = PathBuf::from("/v");
        // inotify queue overflow: Rescan flag, empty paths — events were
        // lost, the rebuild MUST fire (regression: `.any()` over no paths
        // was false and the graph went permanently stale)
        let overflow =
            notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(reload_worthy(&root, &Ok(overflow)));
        // watcher errors likewise mean "assume something was missed"
        assert!(reload_worthy(
            &root,
            &Err(notify::Error::generic("inotify broke"))
        ));
        // but ordinary events keep the relevance filter: hidden dot-dir
        // saves must never schedule a rebuild (reload-loop guard)
        let save =
            notify::Event::new(notify::EventKind::Other).add_path(root.join(".text-graph/view"));
        assert!(!reload_worthy(&root, &Ok(save)));
        let edit = notify::Event::new(notify::EventKind::Other).add_path(root.join("note.md"));
        assert!(reload_worthy(&root, &Ok(edit)));
    }

    #[test]
    fn watcher_callback_errors_are_retained_and_schedule_recovery() {
        let root = PathBuf::from("/v");
        let reload_at = Mutex::new(None);
        let watch_error = Mutex::new(None);
        let error = Err(notify::Error::generic("inotify queue failed"));

        assert!(record_watch_result(&root, &reload_at, &watch_error, &error));
        assert!(reload_at.lock().unwrap().is_some());
        assert!(
            watch_error
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|message| message.contains("inotify queue failed"))
        );

        let irrelevant =
            Ok(notify::Event::new(notify::EventKind::Other)
                .add_path(root.join(".text-graph/view")));
        assert!(!record_watch_result(
            &root,
            &reload_at,
            &watch_error,
            &irrelevant
        ));
        assert!(
            watch_error.lock().unwrap().is_some(),
            "an ordinary later event must not erase a watcher failure"
        );
    }

    #[test]
    fn stale_reload_results_are_discarded() {
        let mut v = fixture_viewer();
        let fixture_nodes = v.g.nodes.len();
        v.reload.generation = 2;
        // gen 1 (superseded) arrives first with a different graph; gen 2
        // rebuilds the fixture. Only gen 2 may apply.
        v.reload.tx.send((1, Ok(tiny_graph()))).unwrap();
        let root = v.root.clone();
        v.reload
            .tx
            .send((2, Ok(graph::build(vault::scan(&root).unwrap()))))
            .unwrap();
        v.pump_reload(&egui::Context::default());
        assert_eq!(
            v.g.nodes.len(),
            fixture_nodes,
            "stale gen-1 graph must not apply"
        );
        assert!(v.reload.last_done.is_some());
        assert!(v.reload.error.is_none());
    }

    #[test]
    fn reload_errors_are_captured_not_applied() {
        let mut v = fixture_viewer();
        let before = v.g.nodes.len();
        v.reload.generation = 1;
        v.reload
            .tx
            .send((1, Err(anyhow::anyhow!("disk on fire"))))
            .unwrap();
        v.pump_reload(&egui::Context::default());
        assert_eq!(v.g.nodes.len(), before, "old graph stays on error");
        assert!(v.reload.error.as_deref().unwrap().contains("disk on fire"));
    }

    /// A debounce expiring while a scan runs must not stack a second
    /// worker — it queues ONE trailing rescan for when the result lands.
    #[test]
    fn scans_are_single_flight_with_a_trailing_rescan() {
        let mut v = fixture_viewer();
        let ctx = egui::Context::default();
        v.reload.scan_inflight = true;
        *v.reload.event_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
        let gen_before = v.reload.generation;
        v.pump_reload(&ctx);
        assert_eq!(
            v.reload.generation, gen_before,
            "no second worker while one runs"
        );
        assert!(v.reload.rescan_queued, "the expired debounce is remembered");

        // the in-flight scan lands -> applied, and the trailing rescan is
        // re-armed as an already-due debounce
        let root = v.root.clone();
        v.reload
            .tx
            .send((gen_before, Ok(graph::build(vault::scan(&root).unwrap()))))
            .unwrap();
        v.pump_reload(&ctx);
        assert!(!v.reload.scan_inflight);
        assert!(!v.reload.rescan_queued);
        assert!(
            v.reload.event_at.lock().unwrap().is_some(),
            "trailing rescan re-armed as an already-due debounce"
        );
    }

    /// A reload landing inside the 180ms glide window must not park the
    /// camera mid-flight — the target remaps by ident like selection does.
    #[test]
    fn camera_glide_survives_reload_remapped_by_ident() {
        let mut v = fixture_viewer();
        let id = v.g.by_path("index.md").expect("index exists");
        v.frame_node(id);
        assert!(v.cam.anim.is_some());
        let g2 = graph::build(vault::scan(&v.root).unwrap());
        v.apply_graph(g2);
        let target = v.cam.anim.map(|(_, i, _)| v.g.node(i).path.clone());
        assert_eq!(
            target.as_deref(),
            Some("index.md"),
            "glide continues toward the remapped node"
        );
    }

    #[test]
    fn apply_carries_selection_by_ident_and_consumes_pending_select() {
        let mut v = fixture_viewer();
        v.selected = v.g.by_path("index.md");
        assert!(v.selected.is_some());
        v.menu.pending_select = Some("empty.md".to_string());
        let g2 = graph::build(vault::scan(&v.root).unwrap());
        v.apply_graph(g2);
        // pending_select wins the selection; it exists in the new graph
        let sel = v.selected.expect("selection survives");
        assert_eq!(v.g.node(sel).path, "empty.md");
        assert!(v.menu.pending_select.is_none());
    }

    #[test]
    fn snapshot_merges_live_and_parked_arrangements_sorted() {
        let mut v = fixture_viewer();
        v.terms
            .offsets
            .insert(("zeta".into(), "%1".into()), Vec2::new(1.0, 2.0));
        v.terms
            .parked
            .insert("alpha".into(), vec![("%9".into(), Vec2::new(3.0, 4.0))]);
        let s = v.snapshot_state();
        assert!(s.camera.is_some());
        let keys: Vec<(&str, &str)> = s
            .cards
            .iter()
            .map(|c| (c.session.as_str(), c.pane.as_str()))
            .collect();
        assert_eq!(
            keys,
            [("alpha", "%9"), ("zeta", "%1")],
            "live + parked both saved, deterministically sorted"
        );
    }
}
