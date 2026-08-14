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
    pub(super) fn apply_graph(&mut self, g: Graph) {
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

    /// Camera + every card arrangement, live or parked, sorted for a
    /// deterministic file.
    pub(super) fn snapshot_state(&self) -> state::ViewState {
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
