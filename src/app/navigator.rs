//! The side pane: ONE previewer, for whatever is current. The finder's
//! highlighted row while it is open, else the selection — the chooser
//! decides the subject, the pane draws it, and there is no second "what is
//! this file" surface to drift out of sync. Header (glyph, name, clickable
//! breadcrumb, size · age), body (`preview_column` — the one place
//! markdown renders and so the one place `tg://` clicks are claimed), and
//! the selection's connections strip along the bottom (hidden while the
//! overlay is open — `]`/`[` index the SELECTION's list, not somebody
//! else's). Choosing lives in `picker.rs`; keyboard walking lives in
//! `keymap.rs`.
//!
//! Two standing rules:
//!
//! - NOTHING in here may ask for more width than the pane has — egui
//!   stores a panel's content-driven rect, and wide content used to
//!   ratchet the pane open for good. Bodies go through
//!   [`preview_scroll`], every name through [`clipped`]/[`clipped_text`];
//!   guarded by `wide_content_scrolls_inside_the_pane_instead_of_
//!   widening_it`, which fails on the width. The pane's WIDTH itself
//!   belongs to the user (`pane_width` + `side_panel` in mod.rs carry
//!   the ownership and never-persist-a-default rules).
//! - The body cache is keyed by subject identity, so `r` (markdown ⇄
//!   source) and a STRUCTURAL reload must both drop it: the first
//!   changes the reading, the second renumbers the NodeId inside it.

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

/// A preview body scrolls vertically with its layout width pinned to
/// the pane. Prose wraps where the pane ends, and anything wider (a
/// markdown table gets its own horizontal scroll region in
/// `render_markdown`) is clipped at the pane instead of pushing it out
/// over the canvas: egui stores a panel's CONTENT-driven rect, so "too
/// wide to fit" used to mean "wider pane", ratcheting further open with
/// every note you walked onto.
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
            child.set_clip_rect(pane_clip(full, ui.clip_rect()));
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

/// The contained child's clip: X pinned to the pane (overflow must not
/// paint over the canvas), Y left to the scroll viewport's own clip.
/// `full` is the scroll area's inner max_rect, and egui sizes that to
/// the VIEWPORT, origin shifted by the scroll offset — its bottom sits
/// at (viewport bottom − offset), so intersecting Y froze the paint at
/// whatever was visible before the first scroll: scrolling down revealed
/// blank pane instead of the rest of the note.
fn pane_clip(full: egui::Rect, viewport: egui::Rect) -> egui::Rect {
    egui::Rect::from_x_y_ranges(
        full.min.x.max(viewport.min.x)..=full.max.x.min(viewport.max.x),
        viewport.y_range(),
    )
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

/// Rendered markdown wraps at a book-ish measure instead of the full
/// pane — ~66 characters at the reading size. Only ever narrower than
/// what's available, so the wrap-inside-the-pane rule is untouched.
const READING_MEASURE: f32 = 620.0;

pub(super) fn reading_width(available: f32) -> f32 {
    available.min(READING_MEASURE)
}

/// Typography scope for rendered markdown — the pane body and the hover
/// popup share it, so a note reads the same everywhere. Inside: the
/// bundled reading face (Inter) at 15px, a real heading scale (the
/// renderer interpolates per level between Body and Heading, so these two
/// styles set the whole hierarchy), roomier block spacing, and the
/// measure, centered when the surface is wider. The style is scoped to
/// the child Ui — chrome outside the closure is untouched.
pub(super) fn reading_frame<R>(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_width();
    let w = reading_width(avail);
    let lead = ((avail - w) * 0.5).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(lead);
        ui.vertical(|ui| {
            ui.set_max_width(w);
            let reading = egui::FontFamily::Name("reading".into());
            let styles = &mut ui.style_mut().text_styles;
            styles.insert(
                egui::TextStyle::Body,
                egui::FontId::new(15.0, reading.clone()),
            );
            styles.insert(
                egui::TextStyle::Heading,
                egui::FontId::new(28.0, reading.clone()),
            );
            styles.insert(
                egui::TextStyle::Small,
                egui::FontId::new(11.5, reading.clone()),
            );
            styles.insert(egui::TextStyle::Button, egui::FontId::new(15.0, reading));
            styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::new(13.0, egui::FontFamily::Monospace),
            );
            // paragraphs breathe — egui's default ~4px block gap reads as
            // a wall of text at 15px
            ui.style_mut().spacing.item_spacing.y = 8.0;
            f(ui)
        })
        .inner
    })
    .inner
}

/// Math spans (`$…$` / `$$…$$`), drawn as Unicode-substituted text —
/// `mathtext` carries the conversion contract (best-effort by design,
/// unknown TeX stays verbatim). Without a math renderer the parser
/// swallows the span entirely, so a note's `$\delta = 2$` would simply
/// vanish from the reading view. Inline math flows italic in the
/// reading face; display math gets its own centered line.
const RENDER_MATH: &egui_commonmark::RenderMathFn = &|ui, tex, inline| {
    let text = text_graph::mathtext::to_unicode(tex);
    if inline {
        ui.label(egui::RichText::new(text).italics());
    } else {
        ui.add_space(4.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(text).italics().size(17.0));
        });
        ui.add_space(4.0);
    }
};

/// The one CommonMark viewer configuration, shared by the pane and the
/// hover popup so the two renderings can never drift: vault-checked
/// images only, Obsidian callouts as themed alerts, fenced code in the
/// same syntect themes as the source view, and math as Unicode text.
pub(super) fn markdown_viewer(theme: &Theme) -> CommonMarkViewer<'static> {
    CommonMarkViewer::new()
        // mdview emits every allowed image as an explicit, vault-checked
        // file URL. Never let a leftover absolute/relative destination
        // become a file URL by renderer convention.
        .explicit_image_uri_scheme(true)
        .alerts(callouts(theme))
        .syntax_theme_dark("base16-ocean.dark")
        .syntax_theme_light("InspiredGitHub")
        .render_math_fn(Some(RENDER_MATH))
}

/// The one place a note body reaches the CommonMark viewer, shared by
/// the pane and the hover popup: prose segments flow at the measure,
/// and each table renders inside its own horizontal scroll region
/// (`mdview::split_tables` finds them). Split because one wide table
/// used to poison every paragraph after it — egui re-expands a Ui's
/// max_rect to its min_rect after each widget, so the table's width
/// became the new wrap width and the prose ran off the pane, clipped at
/// the window edge. The viewer re-reads its wrap width per `show()`
/// call, so restarting it per segment restores the measure, and the
/// scroll region makes a wide table's far columns reachable instead of
/// clipped away. Guarded by `a_wide_table_neither_widens_the_prose_
/// after_it_nor_hides_its_columns`.
pub(super) fn render_markdown(
    ui: &mut egui::Ui,
    theme: &Theme,
    cache: &mut CommonMarkCache,
    body: &str,
) {
    // images take the reading column's width, never a fixed 400: that
    // constant set a FLOOR under the markdown Ui, so prose wrapped at
    // 400 and ran off a narrower pane, clipped at the window edge
    let w = ui.available_width().max(80.0) as usize;
    for (i, seg) in mdview::split_tables(body).into_iter().enumerate() {
        match seg {
            mdview::Segment::Prose(text) => {
                markdown_viewer(theme)
                    .max_image_width(Some(w))
                    .show(ui, cache, text);
            }
            mdview::Segment::Table(text) => {
                egui::ScrollArea::horizontal()
                    .id_salt(("md-table", i))
                    .show(ui, |ui| {
                        markdown_viewer(theme)
                            .max_image_width(Some(w))
                            .show(ui, cache, text);
                    });
            }
        }
    }
}

/// The renderer re-parses the body every frame (immediate mode), so what
/// reaches it is bounded well below the 1 MiB read cap — a preview is a
/// glance, and a megabyte of per-frame Markdown parsing is a hitch.
const RENDER_CAP: usize = 192 * 1024;

fn cap_for_render(mut body: String) -> String {
    if body.len() > RENDER_CAP {
        let mut cut = RENDER_CAP;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n\n*— preview truncated; open in the editor for the rest —*");
    }
    body
}

impl Viewer {
    /// Cap on the raw-text detail read for Asset files — logs can be huge,
    /// and the pane is a glance, not an editor.
    const ASSET_DETAIL_CAP: u64 = 64 * 1024;

    /// A note body, read, bounded, and rewritten for display — the one
    /// path from disk to rendered markdown (pane and popup both).
    pub(super) fn read_markdown(&self, id: NodeId, path: &Path) -> String {
        vault::read_body(path)
            .map(|b| mdview::prepare(&self.g, &self.root, id, &cap_for_render(b)))
            .unwrap_or_else(|e| format!("*error reading file:* {e}"))
    }

    /// `r`: read the previewed note as source (numbered lines) instead of
    /// rendered markdown, and back. Session state, deliberately not
    /// persisted — as a saved setting, one press silently pinned every
    /// note preview to source across restarts.
    pub(super) fn toggle_pane_raw(&mut self) {
        self.pane_raw = !self.pane_raw;
        // the pane caches its preview by subject, and the subject hasn't
        // changed — only the way it should be read
        self.pane_preview = None;
        self.set_flash(
            if self.pane_raw {
                "reading as source"
            } else {
                "reading as markdown"
            }
            .to_string(),
        );
    }

    pub(super) fn load_body(&self, id: NodeId) -> String {
        let node = self.g.node(id);
        let Some(path) = node.absolute_path(&self.root) else {
            return String::new();
        };
        match node.kind {
            NodeKind::File => self.read_markdown(id, &path),
            NodeKind::Asset if filetype::is_text(&node.path) => {
                vault::read_head(&path, Self::ASSET_DETAIL_CAP)
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
        let (kind, path, abs_path) = {
            let node = self.g.node(id);
            let display = if node.path.is_empty() {
                node.name.clone()
            } else {
                node.path.clone()
            };
            (node.kind, display, node.absolute_path(&self.root))
        };
        // Re-read only when the node changed or ITS file did. Reloads
        // land every few seconds under working agents, and re-reading (and
        // re-parsing markdown) on each made the preview flicker and lose
        // its scroll. Kinds with no body stamp as None on both sides, so
        // they never look stale.
        let stamp = abs_path.as_deref().and_then(super::images::file_stamp);
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
                        reading_frame(ui, |ui| {
                            render_markdown(ui, &theme, &mut self.md_cache, body);
                        });
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
                let node = self.g.node(id);
                let key = node.path_key();
                let ctx = ui.ctx().clone();
                if let Some(abs) = node.absolute_path(&self.root) {
                    self.thumbs.request(&ctx, &key, abs);
                }
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
                    let size = abs_path
                        .as_deref()
                        .and_then(|path| std::fs::metadata(path).ok())
                        .map(|metadata| metadata.len())
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

#[cfg(test)]
mod tests {
    use super::{egui, pane_clip};

    /// Scrolled down, the inner max_rect's origin has moved up by the
    /// offset while its height stays the viewport's — the old
    /// full∩viewport clip ended (offset) short of the viewport bottom,
    /// so everything below the first screenful painted as blank pane.
    #[test]
    fn the_scrolled_pane_clip_keeps_the_viewport_bottom() {
        let viewport =
            egui::Rect::from_min_max(egui::Pos2::new(100.0, 50.0), egui::Pos2::new(500.0, 950.0));
        let full = egui::Rect::from_min_max(
            egui::Pos2::new(100.0, -350.0),
            egui::Pos2::new(480.0, 550.0),
        );
        let clip = pane_clip(full, viewport);
        assert_eq!(
            clip.max.y, viewport.max.y,
            "content below the first screenful must paint"
        );
        assert_eq!(clip.min.y, viewport.min.y);
        assert_eq!(clip.min.x, 100.0);
        assert_eq!(clip.max.x, 480.0, "the pane edge still clips X");
    }
}
