//! Keeping the graph and disk in sync: the debounced watcher, the
//! background scan+build worker, carry-over application, and view-state
//! persistence.

use super::*;

pub(super) type ReloadMsg = (u64, anyhow::Result<Graph>);

impl Viewer {
    /// Watch the vault; on a relevant event, stamp the debounce clock and
    /// wake the UI thread. Failure to watch just means no live reload.
    pub(super) fn start_watcher(&mut self, ctx: egui::Context) {
        use notify::Watcher as _;
        let state = self.reload_at.clone();
        let root = self.root.clone();
        let handler = move |res: Result<notify::Event, notify::Error>| {
            let Ok(event) = res else { return };
            let relevant = event.paths.iter().any(|p| vault::watch_relevant(&root, p));
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
    pub(super) fn apply_graph(&mut self, g: Graph) {
        self.last_reload = Some(Instant::now());
        self.reload_error = None;

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
        self.ctx_node = self
            .ctx_node
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
        self.cam_anim = None; // its target NodeId indexes the old graph
        self.conn_cursor = None; // indexes the old graph's link lists
        self.best = None;

        let Derived {
            radius,
            depths,
            haystacks,
            n_files,
            n_dirs,
            n_images,
            n_assets,
            n_webs,
            dir_by_path,
        } = Self::derived(&g);
        self.radius = radius;
        self.depths = depths;
        self.haystacks = haystacks;
        self.n_files = n_files;
        self.n_dirs = n_dirs;
        self.n_images = n_images;
        self.n_assets = n_assets;
        self.n_webs = n_webs;
        self.dir_by_path = dir_by_path;
        self.scores = vec![None; g.nodes.len()];
        self.last_query.clear(); // force a re-score against the new nodes
        self.detail = None; // re-read the body — the pane shows fresh edits
        // evict only thumbnails/excerpts whose file actually changed —
        // reloads are frequent (agents writing notes) and a full clear made
        // every image flicker through its placeholder
        self.thumbs.retain_fresh(&self.root);
        self.previews.retain_fresh(&self.root);
        self.g = g;
        if let Some(sim) = sim {
            self.sim = sim;
        }

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
            camera: Some((self.center.x, self.center.y, self.zoom)),
            cards,
            pins,
            hide_web: !self.show_web,
        }
    }

    /// Debounced view-state save; `force` (exit) skips the debounce. Errors
    /// warn once and go quiet (read-only vaults stay usable).
    pub(super) fn persist_state(&mut self, force: bool) {
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
}

impl Viewer {
    /// Debounce the watcher's events; when the vault has been quiet, kick a
    /// scan+build worker (45ms at 500 files — a visible hitch on the UI
    /// thread, and agents save constantly) and apply finished results,
    /// discarding any superseded by a newer save. Called once per frame.
    pub(super) fn pump_reload(&mut self, ctx: &egui::Context) {
        let due = {
            let mut at = self.reload_at.lock().unwrap();
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
            self.reload_gen += 1;
            let generation = self.reload_gen;
            let root = self.root.clone();
            let tx = self.reload_tx.clone();
            let ctx = ctx.clone();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_graph::{graph, vault};

    fn fixture_viewer() -> Viewer {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
        let scan = vault::scan(&root).expect("fixture scans");
        Viewer::new(graph::build(scan), root)
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
    fn stale_reload_results_are_discarded() {
        let mut v = fixture_viewer();
        let fixture_nodes = v.g.nodes.len();
        v.reload_gen = 2;
        // gen 1 (superseded) arrives first with a different graph; gen 2
        // rebuilds the fixture. Only gen 2 may apply.
        v.reload_tx.send((1, Ok(tiny_graph()))).unwrap();
        let root = v.root.clone();
        v.reload_tx
            .send((2, Ok(graph::build(vault::scan(&root).unwrap()))))
            .unwrap();
        v.pump_reload(&egui::Context::default());
        assert_eq!(
            v.g.nodes.len(),
            fixture_nodes,
            "stale gen-1 graph must not apply"
        );
        assert!(v.last_reload.is_some());
        assert!(v.reload_error.is_none());
    }

    #[test]
    fn reload_errors_are_captured_not_applied() {
        let mut v = fixture_viewer();
        let before = v.g.nodes.len();
        v.reload_gen = 1;
        v.reload_tx
            .send((1, Err(anyhow::anyhow!("disk on fire"))))
            .unwrap();
        v.pump_reload(&egui::Context::default());
        assert_eq!(v.g.nodes.len(), before, "old graph stays on error");
        assert!(v.reload_error.as_deref().unwrap().contains("disk on fire"));
    }

    #[test]
    fn apply_carries_selection_by_ident_and_consumes_pending_select() {
        let mut v = fixture_viewer();
        v.selected = v.g.by_path("index.md");
        assert!(v.selected.is_some());
        v.pending_select = Some("empty.md".to_string());
        let g2 = graph::build(vault::scan(&v.root).unwrap());
        v.apply_graph(g2);
        // pending_select wins the selection; it exists in the new graph
        let sel = v.selected.expect("selection survives");
        assert_eq!(v.g.node(sel).path, "empty.md");
        assert!(v.pending_select.is_none());
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
