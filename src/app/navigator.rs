//! The side pane, in its two modes. Browsing (the ranger): breadcrumb,
//! sibling column with the cursor, preview column (markdown / dir listing
//! / ghost backrefs), connections strip. Searching: the highlighted
//! result's preview, while the prompt and its results float over the
//! canvas (see `picker.rs`). Both modes share `preview_column` — which is
//! therefore the one place markdown renders, and so the one place that
//! claims `tg://` link clicks — and keyboard walking lives in handle_keys.

use super::*;

impl Viewer {
    /// Cap on the raw-text detail read for Asset files — logs can be huge,
    /// and the pane is a glance, not an editor.
    const ASSET_DETAIL_CAP: u64 = 64 * 1024;

    pub(super) fn load_body(&self, id: NodeId) -> String {
        let node = self.g.node(id);
        match node.kind {
            NodeKind::File => vault::read_body(&self.root.join(&node.path))
                .map(|b| mdview::prepare(&self.g, &self.root, id, &b))
                .unwrap_or_else(|e| format!("*error reading file:* {e}")),
            NodeKind::Asset if filetype::is_text(&node.path) => {
                vault::read_head(&self.root.join(&node.path), Self::ASSET_DETAIL_CAP)
                    .unwrap_or_else(|e| format!("error reading file: {e}"))
            }
            _ => String::new(),
        }
    }

    /// The preview column, shared by both pane modes: rendered markdown for
    /// a note, the picture for an image, raw head for a text asset, listing
    /// for a folder, referrers for ghosts and web nodes. Returns a node to
    /// jump to when the user clicks a link inside it. `acting` shows the
    /// open-in-editor button — the ranger has an Enter/l for it, the picker
    /// does not (its Enter takes the result instead).
    pub(super) fn preview_column(
        &mut self,
        ui: &mut egui::Ui,
        id: NodeId,
        acting: bool,
    ) -> Option<NodeId> {
        let mut jump: Option<NodeId> = None;
        let (kind, path) = {
            let n = self.g.node(id);
            let p = if n.path.is_empty() {
                n.name.clone()
            } else {
                n.path.clone()
            };
            (n.kind, p)
        };
        // Re-read only when the node changed or ITS file did. Reloads
        // land every few seconds under working agents, and re-reading (and
        // re-parsing markdown) on each made the preview flicker and lose
        // its scroll. Kinds with no body stamp as None on both sides, so
        // they never look stale.
        let stamp = super::images::file_stamp(&self.root.join(&path));
        if self.detail.as_ref().map(|(i, _)| *i) != Some(id) || self.detail_stamp != stamp {
            self.detail = Some((id, self.load_body(id)));
            self.detail_stamp = stamp;
        }
        match kind {
            NodeKind::File => {
                if acting {
                    if ui.button("open in editor  (Enter / l)").clicked() {
                        self.open_in_editor(id);
                    }
                    ui.add_space(4.0);
                }
                // take/put-back so the markdown cache and the body can
                // be borrowed simultaneously without a per-frame clone
                let detail = self.detail.take();
                if let Some((_, body)) = &detail {
                    egui::ScrollArea::vertical()
                        .id_salt("nav-preview")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            CommonMarkViewer::new().max_image_width(Some(400)).show(
                                ui,
                                &mut self.md_cache,
                                body,
                            );
                        });
                }
                self.detail = detail;
            }
            NodeKind::Dir => {
                let children = self.g.node(id).children.clone();
                egui::ScrollArea::vertical()
                    .id_salt("nav-preview")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(format!("{} entries — l enters", children.len()));
                        ui.add_space(4.0);
                        for c in children {
                            let child = self.g.node(c);
                            let label = if child.kind == NodeKind::Dir {
                                format!("{}/", child.display_name())
                            } else {
                                child.display_name().to_string()
                            };
                            let (glyph, color) = self.node_icon(c);
                            let job =
                                icon_label(glyph, color, &label, ui.visuals().text_color(), 12.5);
                            if ui.link(job).clicked() {
                                jump = Some(c);
                            }
                        }
                    });
            }
            NodeKind::Image => {
                if acting {
                    if ui.button("open  (Enter / l)").clicked() {
                        self.open_in_editor(id);
                    }
                    ui.add_space(4.0);
                }
                let key = self.g.node(id).path.clone();
                let ctx = ui.ctx().clone();
                self.thumbs.request(&ctx, &key, self.root.join(&key));
                match self.thumbs.cache.get(&key) {
                    Some(images::ThumbState::Ready { tex, .. }) => {
                        let size = tex.size_vec2();
                        let w = ui.available_width().min(size.x.max(96.0));
                        let h = w * size.y / size.x.max(1.0);
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(w, h),
                        )));
                        ui.label(
                            egui::RichText::new(format!(
                                "{} × {} (thumbnail)",
                                size.x as u32, size.y as u32
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    Some(images::ThumbState::Failed) => {
                        ui.label(egui::RichText::new("could not decode this image").weak());
                    }
                    _ => {
                        ui.label(egui::RichText::new("loading…").weak());
                    }
                }
            }
            NodeKind::Asset => {
                if acting {
                    if ui.button("open  (Enter / l)").clicked() {
                        self.open_in_editor(id);
                    }
                    ui.add_space(4.0);
                }
                if filetype::is_text(&path) {
                    let detail = self.detail.take();
                    if let Some((_, body)) = &detail {
                        egui::ScrollArea::vertical()
                            .id_salt("nav-preview")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(body.as_str()).monospace().size(11.0),
                                    )
                                    .wrap(),
                                );
                            });
                    }
                    self.detail = detail;
                } else {
                    let size = std::fs::metadata(self.root.join(&path))
                        .map(|m| m.len())
                        .unwrap_or(0);
                    ui.label(
                        egui::RichText::new(format!(
                            "binary file · {size} bytes — Enter opens it externally"
                        ))
                        .weak(),
                    );
                }
            }
            NodeKind::Web => {
                if acting {
                    if ui.button("open in browser  (Enter / l)").clicked() {
                        self.open_in_editor(id);
                    }
                    ui.add_space(4.0);
                }
                ui.label("Cited from:");
                ui.add_space(2.0);
                let refs: Vec<NodeId> = self.g.backlinks(id).map(|l| l.from).collect();
                for r in refs {
                    if ui.link(self.g.node(r).path.clone()).clicked() {
                        jump = Some(r);
                    }
                }
            }
            NodeKind::Ghost => {
                ui.label("Not written yet. Referenced from:");
                ui.add_space(4.0);
                let refs: Vec<NodeId> = self.g.backlinks(id).map(|l| l.from).collect();
                for r in refs {
                    if ui.link(self.g.node(r).path.clone()).clicked() {
                        jump = Some(r);
                    }
                }
            }
        }
        // Clicked [[wikilinks]] in the rendered markdown arrive as OpenUrl
        // commands on our tg:// scheme — claimed HERE, in the one place
        // that renders markdown, so BOTH pane modes intercept them (a
        // missed claim leaks the click to the OS browser). External links
        // pass through untouched and open normally.
        ui.ctx().output_mut(|o| {
            o.commands.retain(|c| {
                if let egui::OutputCommand::OpenUrl(u) = c
                    && let Some(idx) = mdview::parse_url(&u.url)
                {
                    if (idx as usize) < self.g.nodes.len() {
                        jump = Some(NodeId(idx));
                    }
                    return false; // stale ids (pre-reload) are dropped too
                }
                true
            })
        });
        jump
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

    /// The pane, in whichever mode applies: previewing the highlighted
    /// search result (the prompt and its result list float over the canvas
    /// — see `picker_overlay_ui`), or the ranger. A search with an empty
    /// query IS the ranger, and with nothing selected the ranger falls back
    /// to the vault root, so `f` never opens onto a blank pane.
    pub(super) fn side_pane(&mut self, ui: &mut egui::Ui) {
        if self.picker.searching() {
            ui.set_min_width(430.0);
            self.picker_preview_ui(ui);
        } else if let Some(sel) = self
            .selected
            .or_else(|| self.picker.open.then_some(self.g.root))
        {
            self.navigator_body(ui, sel);
        }
    }

    /// The ranger: breadcrumb, sibling column with the cursor, preview
    /// column, connections strip. Keyboard walking happens in `handle_keys`
    /// (hjkl / gg / G while a node is selected); this renders the state and
    /// accepts clicks.
    fn navigator_body(&mut self, ui: &mut egui::Ui, sel: NodeId) {
        // Owned copies so the panel closures below can borrow self freely.
        // (The body itself is loaded by `preview_column`, which the search
        // mode shares.)
        let (display, sub, parent) = {
            let node = self.g.node(sel);
            let sub = if node.path.is_empty() {
                node.name.clone()
            } else {
                node.path.clone()
            };
            (node.display_name().to_string(), sub, node.parent)
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
                if ui
                    .link(egui::RichText::new(name).color(self.theme.dir))
                    .clicked()
                {
                    jump = Some(*a);
                }
                ui.label(egui::RichText::new("/").weak());
            }
            let (glyph, color) = self.node_icon(sel);
            ui.label(icon_label(
                glyph,
                color,
                &display,
                ui.visuals().strong_text_color(),
                14.0,
            ));
        });
        ui.label(
            egui::RichText::new(sub.as_str())
                .small()
                .color(self.theme.text),
        );
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
                                let (glyph, color) = self.node_icon(*c);
                                let text_color = if is_dir {
                                    self.theme.dir
                                } else {
                                    ui.visuals().text_color()
                                };
                                let resp = ui.selectable_label(
                                    *c == sel,
                                    icon_label(glyph, color, &label, text_color, 12.5),
                                );
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
                    jump = self.preview_column(ui, sel, true).or(jump);
                });
            });
        });
        // ---- connections strip: everything this node touches, color-coded
        // (blue ▸ child folder, gray ▸ child file, amber → outgoing link,
        // purple ← incoming link). Clickable; ] / [ walk the highlight,
        // Enter / l follows it. Entry order MUST match connections().
        if has_conn {
            let plain = |text: String, color: egui::Color32| {
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &text,
                    0.0,
                    egui::TextFormat {
                        font_id: FontId::proportional(11.0),
                        color,
                        ..Default::default()
                    },
                );
                job
            };
            let mut entries: Vec<(NodeId, egui::text::LayoutJob)> = Vec::new();
            for id in kids {
                let n = self.g.node(id);
                let label = if n.kind == NodeKind::Dir {
                    format!("{}/", n.display_name())
                } else {
                    n.display_name().to_string()
                };
                let (glyph, color) = self.node_icon(id);
                let text_color = if n.kind == NodeKind::Dir {
                    self.theme.dir
                } else {
                    self.theme.file
                };
                entries.push((id, icon_label(glyph, color, &label, text_color, 11.0)));
            }
            for id in outs {
                entries.push((
                    id,
                    plain(
                        format!("→ {}", self.g.node(id).display_name()),
                        self.theme.wiki,
                    ),
                ));
            }
            for id in backs {
                entries.push((
                    id,
                    plain(
                        format!("← {}", self.g.node(id).display_name()),
                        self.theme.link_in,
                    ),
                ));
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
                        for (idx, (id, job)) in entries.iter().enumerate() {
                            let is_cur = self.conn_cursor == Some(idx);
                            let resp = ui.selectable_label(is_cur, job.clone());
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
