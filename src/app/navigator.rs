//! The side pane: ONE previewer, for whatever is current. The finder's
//! highlighted row while it is open, else the selection — the chooser
//! decides the subject, the pane draws it, and there is no second "what is
//! this file" surface to drift out of sync. Header (glyph, name, clickable
//! breadcrumb, size · age), body (`preview_column` — the one place
//! markdown renders and so the one place `tg://` clicks are claimed), and
//! the selection's connections strip along the bottom. Choosing lives in
//! `picker.rs`; keyboard walking lives in handle_keys.

use super::*;
use picker::{Preview, PreviewBody, one_line};

/// Obsidian's callout types, as the renderer's "alerts". Obsidian has a
/// long list with heavy aliasing (`tip` = `hint` = `important`); these are
/// the ones people actually type, and an unknown one still renders as the
/// blockquote it is written as.
fn callouts(theme: &Theme) -> egui_commonmark::AlertBundle {
    let blue = theme.dir;
    let green = egui::Color32::from_rgb(0x3f, 0xa5, 0x5b);
    let amber = theme.wiki;
    let red = egui::Color32::from_rgb(0xd0, 0x50, 0x50);
    let purple = theme.link_in;
    let rows: &[(&str, &str, char, egui::Color32)] = &[
        ("NOTE", "Note", '📝', blue),
        ("INFO", "Info", 'ℹ', blue),
        ("ABSTRACT", "Abstract", '📄', blue),
        ("SUMMARY", "Summary", '📄', blue),
        ("TODO", "Todo", '☑', blue),
        ("TIP", "Tip", '💡', green),
        ("HINT", "Hint", '💡', green),
        ("SUCCESS", "Success", '✔', green),
        ("DONE", "Done", '✔', green),
        ("CHECK", "Check", '✔', green),
        ("QUESTION", "Question", '❓', amber),
        ("FAQ", "FAQ", '❓', amber),
        ("WARNING", "Warning", '⚠', amber),
        ("CAUTION", "Caution", '⚠', amber),
        ("ATTENTION", "Attention", '⚠', amber),
        ("IMPORTANT", "Important", '❗', amber),
        ("FAILURE", "Failure", '✖', red),
        ("FAIL", "Fail", '✖', red),
        ("MISSING", "Missing", '✖', red),
        ("DANGER", "Danger", '⚡', red),
        ("ERROR", "Error", '⚡', red),
        ("BUG", "Bug", '🐛', red),
        ("EXAMPLE", "Example", '📋', purple),
        ("QUOTE", "Quote", '❝', purple),
        ("CITE", "Cite", '❝', purple),
    ];
    egui_commonmark::AlertBundle::from_alerts(
        rows.iter()
            .map(|(id, shown, icon, color)| egui_commonmark::Alert {
                accent_color: *color,
                icon: *icon,
                identifier: (*id).to_string(),
                identifier_rendered: (*shown).to_string(),
            })
            .collect(),
    )
}

/// A preview body scrolls in BOTH directions with its layout width pinned
/// to the pane. Prose still wraps where the pane ends, while a wide
/// markdown table or an unwrappable code line scrolls inside the pane
/// instead of pushing it out over the canvas: egui stores a panel's
/// CONTENT-driven rect, so "too wide to fit" used to mean "wider pane",
/// ratcheting further open with every note you walked onto.
pub(super) fn preview_scroll<R>(
    ui: &mut egui::Ui,
    salt: &str,
    self_width: &std::cell::Cell<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            let w = full.width();
            // A CONTAINED child: `set_max_width` isn't enough, because egui
            // re-expands a Ui's max_rect to its min_rect (placer.rs), so one
            // wide table teaches every paragraph after it to wrap at the
            // table's width — text ran off the pane and got clipped at the
            // window edge. The child is built at exactly the pane's width,
            // clipped to it, and only the rect WE allocate is reported
            // upward, so nothing inside can widen the pane either.
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(
                egui::Rect::from_min_size(full.min, egui::vec2(w, full.height())),
            ));
            child.set_clip_rect(full.intersect(ui.clip_rect()));
            let r = add(&mut child);
            // what the body ACTUALLY laid out at — the wrap-width
            // regression test watches this
            self_width.set(child.min_rect().width());
            let h = child.min_rect().height();
            ui.allocate_rect(
                egui::Rect::from_min_size(full.min, egui::vec2(w, h)),
                egui::Sense::hover(),
            );
            r
        })
        .inner
}

/// One line, ellipsized at `w` — every NAME in the pane's columns goes
/// through this, for the same reason: an over-long filename is content
/// that would otherwise widen the pane.
fn clipped(mut job: egui::text::LayoutJob, w: f32) -> egui::text::LayoutJob {
    job.wrap = one_line(w);
    job
}

/// An ellipsized plain-text job (for the link lists, which have no icon).
fn clipped_text(text: &str, color: egui::Color32, size: f32, w: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            ..Default::default()
        },
    );
    clipped(job, w)
}

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
    /// open-in-editor button — a selected node's Enter does that, while
    /// the finder's Enter takes the result instead.
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
                let theme = self.theme;
                if let Some((_, body)) = &detail {
                    preview_scroll(ui, "nav-preview", &self.pane_content_w, |ui| {
                        // images take the pane's width, never a fixed 400:
                        // that constant set a FLOOR under the markdown Ui,
                        // so prose wrapped at 400 and ran off a narrower
                        // pane, clipped at the window edge
                        let w = ui.available_width().max(80.0) as usize;
                        CommonMarkViewer::new()
                            // mdview emits every allowed image as an explicit,
                            // vault-checked file URL. Never let a leftover
                            // absolute/relative destination become a file URL
                            // by renderer convention.
                            .explicit_image_uri_scheme(true)
                            .max_image_width(Some(w))
                            .alerts(callouts(&theme))
                            // fenced code is highlighted by the same
                            // syntect themes the source view uses, so a
                            // snippet reads the same in either
                            .syntax_theme_dark("base16-ocean.dark")
                            .syntax_theme_light("InspiredGitHub")
                            .show(ui, &mut self.md_cache, body);
                    });
                }
                self.detail = detail;
            }
            NodeKind::Dir => {
                let children = self.g.node(id).children.clone();
                preview_scroll(ui, "nav-preview", &self.pane_content_w, |ui| {
                    ui.label(format!("{} entries — l enters", children.len()));
                    ui.add_space(4.0);
                    let w = ui.available_width();
                    for c in children {
                        let child = self.g.node(c);
                        let label = if child.kind == NodeKind::Dir {
                            format!("{}/", child.display_name())
                        } else {
                            child.display_name().to_string()
                        };
                        let (glyph, color) = self.node_icon(c);
                        let job = icon_label(glyph, color, &label, ui.visuals().text_color(), 12.5);
                        if ui.link(clipped(job, w)).clicked() {
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
                        preview_scroll(ui, "nav-preview", &self.pane_content_w, |ui| {
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
                let (w, color) = (ui.available_width(), ui.visuals().hyperlink_color);
                for r in refs {
                    let job = clipped_text(&self.g.node(r).path, color, 12.5, w);
                    if ui.link(job).clicked() {
                        jump = Some(r);
                    }
                }
            }
            NodeKind::Ghost => {
                ui.label("Not written yet. Referenced from:");
                ui.add_space(4.0);
                let refs: Vec<NodeId> = self.g.backlinks(id).map(|l| l.from).collect();
                let (w, color) = (ui.available_width(), ui.visuals().hyperlink_color);
                for r in refs {
                    let job = clipped_text(&self.g.node(r).path, color, 12.5, w);
                    if ui.link(job).clicked() {
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

    /// The pane draws whatever `sync_pane_preview` (in the picker pump)
    /// decided the current subject is.
    pub(super) fn side_pane(&mut self, ui: &mut egui::Ui) {
        self.preview_pane(ui);
    }

    /// THE previewer. Whatever the pane is about — a finder result, the
    /// selection, a terminal pane — arrives here as one `Preview` and is
    /// drawn one way. Two choosers used to mean two previews that drifted
    /// apart; now the chooser only decides the subject.
    fn preview_pane(&mut self, ui: &mut egui::Ui) {
        let Some(preview) = self.pane_preview.take() else {
            return;
        };
        let dim = self.theme.text;
        ui.add_space(2.0);
        let mut jump = self.preview_header(ui, &preview);
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
                        picker::push_source_line(
                            &mut job, &l.text, &l.ranges, &l.spans, 11.5, text_color, accent,
                        );
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
                // a terminal screen is 80+ monospace columns wide and
                // cannot wrap: it is clipped to the pane rather than
                // widening it (see preview_scroll)
                preview_scroll(ui, "tg-picker-screen", &self.pane_content_w, |ui| {
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
                // a note, a folder, a picture: rendered markdown / listing /
                // thumbnail, and the one place tg:// clicks are claimed
                jump = self.preview_column(ui, *id, !self.picker.open).or(jump);
            }
            PreviewBody::Note(note) => {
                ui.label(egui::RichText::new(note.as_str()).color(dim));
            }
        }
        // the connections strip belongs to the SELECTION — ] and [ walk it
        // by index, and while the finder is open the pane is showing
        // somebody else's node
        if !self.picker.open
            && let Some(sel) = preview.subject.filter(|id| Some(*id) == self.selected)
        {
            jump = self.connections_strip(ui, sel).or(jump);
        }
        self.pane_preview = Some(Preview {
            scroll: false,
            ..preview
        });
        if let Some(j) = jump {
            // a click inside the pane is a jump: it takes the selection,
            // frames it, and ends a search the way taking a result does
            self.selected = Some(j);
            self.frame_node(j);
            self.nav_scroll = true;
            self.conn_cursor = None;
            if self.picker.open {
                self.picker.close();
            }
        }
    }

    /// The header every preview wears: file-type glyph, name, the path as
    /// clickable ancestors, and size · age. The breadcrumb is what the
    /// ranger's own header used to be, the one piece of its chrome that
    /// belongs with "what is this file" rather than with choosing one.
    fn preview_header(&mut self, ui: &mut egui::Ui, preview: &Preview) -> Option<NodeId> {
        let dim = self.theme.text;
        let mut jump = None;
        match preview.subject {
            Some(id) => {
                let mut chain: Vec<NodeId> = Vec::new();
                let mut cur = self.g.node(id).parent;
                while let Some(p) = cur {
                    chain.push(p);
                    cur = self.g.node(p).parent;
                }
                chain.reverse();
                let (glyph, color) = self.node_icon(id);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    ui.label(icon_label(
                        glyph,
                        color,
                        &preview.title,
                        ui.visuals().strong_text_color(),
                        14.0,
                    ));
                });
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    for a in &chain {
                        let name = self.g.node(*a).display_name().to_string();
                        if ui
                            .link(egui::RichText::new(name).small().color(self.theme.dir))
                            .clicked()
                        {
                            jump = Some(*a);
                        }
                        ui.label(egui::RichText::new("/").small().weak());
                    }
                    if chain.is_empty() {
                        ui.label(egui::RichText::new(&preview.subtitle).small().color(dim));
                    }
                });
            }
            None => {
                ui.label(egui::RichText::new(&preview.title).strong().size(14.0));
                ui.label(egui::RichText::new(&preview.subtitle).small().color(dim));
            }
        }
        if !preview.meta.is_empty() {
            ui.label(egui::RichText::new(&preview.meta).small().color(dim));
        }
        jump
    }

    /// Everything this node touches, color-coded and clickable (blue ▸
    /// child folder, gray ▸ child file, amber → outgoing, purple ←
    /// incoming). `]` / `[` walk the highlight, Enter / l follows it.
    fn connections_strip(&mut self, ui: &mut egui::Ui, sel: NodeId) -> Option<NodeId> {
        let mut jump = None;
        // Entry order MUST match connections() — ] and [ index this list.
        let kids: Vec<NodeId> = self.g.node(sel).children.clone();
        let outs: Vec<NodeId> = self.g.outlinks(sel).map(|l| l.to).collect();
        let backs: Vec<NodeId> = self.g.backlinks(sel).map(|l| l.from).collect();
        if !(kids.is_empty() && outs.is_empty() && backs.is_empty()) {
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
                        let cap = ui.available_width().min(240.0);
                        for (idx, (id, job)) in entries.iter().enumerate() {
                            let is_cur = self.conn_cursor == Some(idx);
                            let resp = ui.selectable_label(is_cur, clipped(job.clone(), cap));
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
        jump
    }
}
