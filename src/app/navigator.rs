//! The ranger-style navigator pane: breadcrumb, sibling column with the
//! cursor and find-in-directory prompt, preview column (markdown / dir
//! listing / ghost backrefs). Keyboard walking lives in handle_keys.

use super::*;

impl Viewer {
    pub(super) fn load_body(&self, id: NodeId) -> String {
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
    pub(super) fn nav_find_apply(&mut self) {
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

    /// The connections strip's entries in render order: children, then
    /// outgoing links, then incoming. `]`/`[` walk this list by index, so
    /// the strip render below must build entries in exactly this order.
    pub(super) fn connections(&self, id: NodeId) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.g.node(id).children.clone();
        v.extend(self.g.outlinks(id).map(|l| l.to));
        v.extend(self.g.backlinks(id).map(|l| l.from));
        v
    }

    /// The ranger-style navigator: breadcrumb, sibling column with the
    /// cursor, preview column. Keyboard walking happens in `handle_keys`
    /// (hjkl / gg / G while a node is selected); this renders the state and
    /// accepts clicks.
    pub(super) fn detail_pane(&mut self, ui: &mut egui::Ui) {
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
        // connections computed up front: the columns must leave the strip
        // its room, or their greedy scroll areas push it off-panel
        let kids: Vec<NodeId> = self.g.node(sel).children.clone();
        let outs: Vec<NodeId> = self.g.outlinks(sel).map(|l| l.to).collect();
        let backs: Vec<NodeId> = self.g.backlinks(sel).map(|l| l.from).collect();
        let has_conn = !(kids.is_empty() && outs.is_empty() && backs.is_empty());
        let strip_h = if has_conn { 122.0 } else { 0.0 };
        let col_h = (ui.available_height() - strip_h).max(120.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), col_h), |ui| {
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
                                        if ui
                                            .link(format!("{icon}{}", child.display_name()))
                                            .clicked()
                                        {
                                            jump = Some(c);
                                        }
                                    }
                                });
                        }
                        NodeKind::Image => {
                            if ui.button("open  (Enter / l)").clicked() {
                                self.open_in_editor(sel);
                            }
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("image file").weak());
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
        });
        // ---- connections strip: everything this node touches, color-coded
        // (blue ▸ child folder, gray ▸ child file, amber → outgoing link,
        // purple ← incoming link). Clickable; ] / [ walk the highlight,
        // Enter / l follows it. Entry order MUST match connections().
        if has_conn {
            let mut entries: Vec<(NodeId, egui::Color32, String)> = Vec::new();
            for id in kids {
                let n = self.g.node(id);
                match n.kind {
                    NodeKind::Dir => entries.push((id, DIR, format!("▸ {}/", n.display_name()))),
                    NodeKind::Image => entries.push((id, IMG, format!("▸ {}", n.display_name()))),
                    _ => entries.push((id, FILE, format!("▸ {}", n.display_name()))),
                }
            }
            for id in outs {
                entries.push((id, WIKI, format!("→ {}", self.g.node(id).display_name())));
            }
            for id in backs {
                entries.push((id, LINK_IN, format!("← {}", self.g.node(id).display_name())));
            }
            if self.conn_cursor.is_some_and(|i| i >= entries.len()) {
                self.conn_cursor = None; // list changed under the cursor
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("nav-conn")
                .max_height(110.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        for (idx, (id, color, label)) in entries.iter().enumerate() {
                            let is_cur = self.conn_cursor == Some(idx);
                            let resp = ui.selectable_label(
                                is_cur,
                                egui::RichText::new(label).color(*color).small(),
                            );
                            if is_cur && self.nav_scroll {
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                            if resp.clicked() {
                                jump = Some(*id);
                            }
                        }
                    });
                });
        }
        self.nav_scroll = false;
        if let Some(j) = jump {
            self.selected = Some(j);
            self.frame_node(j);
            self.nav_scroll = true;
            self.conn_cursor = None;
        }
    }
}
