//! Zoomed-in text previews and the hover popup.
//!
//! Previews: when a textual leaf's screen size crosses the threshold, its
//! card shows the opening lines of the body — the graph-canvas sibling of
//! the detail pane, like terminal cards and image thumbnails. Bodies are
//! never held whole: each cache entry is a small excerpt plus the file's
//! (mtime, len) stamp, and a vault reload evicts only entries whose stamp
//! no longer matches disk (same anti-flicker rule as the thumbnail cache).
//!
//! The hover popup (dwell on any node) leads with metadata — edited/
//! created age, size, lines·words, link counts with external URLs — then
//! the content: markdown for notes, raw text for assets, the image, a
//! folder's listing + subtree stats, a ghost's referencers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use text_graph::vault;

use super::images::{Stamp, file_stamp, fresh};
use super::*;

/// Cap on lines / bytes kept per preview — enough to fill the card at any
/// zoom the canvas allows.
const MAX_LINES: usize = 18;
const MAX_BYTES: usize = 1200;

pub(super) struct Preview {
    pub(super) excerpt: String,
    path: PathBuf,
    stamp: Option<Stamp>,
}

#[derive(Default)]
pub(super) struct Previews {
    cache: HashMap<String, Preview>,
}

impl Previews {
    /// The excerpt for `key` (vault-relative path), reading the file on
    /// first sight. Reads are synchronous — an excerpt is a few KB of one
    /// markdown file, nothing like the image decodes that get a worker.
    pub(super) fn get_or_load(&mut self, key: &str, path: &Path, markdown: bool) -> &str {
        if !self.cache.contains_key(key) {
            let stamp = file_stamp(path);
            // markdown gets frontmatter/BOM stripping; any other text file
            // is read raw (a YAML file opening with `---` is not frontmatter)
            let body = if markdown {
                vault::read_body(path)
            } else {
                vault::read_head(path, 8 * 1024)
            };
            let excerpt = match body {
                Ok(body) => excerpt(&body),
                Err(_) => "(unreadable)".to_string(),
            };
            self.cache.insert(
                key.to_string(),
                Preview {
                    excerpt,
                    path: path.to_path_buf(),
                    stamp,
                },
            );
        }
        &self.cache[key].excerpt
    }

    /// Vault reload: evict only entries whose file changed or vanished.
    pub(super) fn retain_fresh(&mut self) {
        self.cache
            .retain(|_, preview| fresh(&preview.stamp, file_stamp(&preview.path)));
    }
}

/// Popup content width; images fit within it, text wraps to it.
const POPUP_W: f32 = 430.0;
const POPUP_MAX_H: f32 = 460.0;

/// Which corner of the popup sits on the anchor: pick the quadrant that
/// opens toward screen center, so the popup always has room. Shared with
/// the terminal-card peek.
pub(super) fn popup_pivot(anchor: Pos2, screen: Rect) -> Align2 {
    match (anchor.x < screen.center().x, anchor.y < screen.center().y) {
        (true, true) => Align2::LEFT_TOP,
        (true, false) => Align2::LEFT_BOTTOM,
        (false, true) => Align2::RIGHT_TOP,
        (false, false) => Align2::RIGHT_BOTTOM,
    }
}

/// Cap on the raw-text hover preview for Asset files (logs can be huge).
const ASSET_POPUP_CAP: u64 = 32 * 1024;
/// Cap on listed entries in Dir / Ghost popups.
const LIST_CAP: usize = 30;

impl Viewer {
    /// The full-content hover preview: linger on any node — a File renders
    /// its whole body as markdown, an Image the picture at popup size, a
    /// text Asset its raw head, a Dir its listing, a Ghost its referrers.
    /// The hover dwell, as configured — sweeping the cursor across nodes
    /// must not strobe popups. The node popup and the terminal-card peek
    /// share it, so they open on the same beat.
    pub(super) fn hover_delay(&self) -> Duration {
        Duration::from_secs_f32(self.cfg.hover_delay.max(0.0))
    }

    /// Non-interactable, tooltip layer, anchored where the dwell began.
    pub(super) fn hover_preview_ui(&mut self, ui: &egui::Ui) {
        let Some((id, since, anchor)) = self.hover_since else {
            return;
        };
        if !self.cfg.hover_previews {
            return;
        }
        let kind = self.g.node(id).kind;
        let elapsed = since.elapsed();
        let dwell = self.hover_delay();
        if elapsed < dwell {
            // wake exactly when the dwell completes
            ui.ctx().request_repaint_after(dwell - elapsed);
            return;
        }
        let screen = ui.ctx().content_rect();
        let pivot = popup_pivot(anchor, screen);
        let off = Vec2::new(
            if pivot.0[0] == egui::Align::Min {
                16.0
            } else {
                -16.0
            },
            if pivot.0[1] == egui::Align::Min {
                14.0
            } else {
                -14.0
            },
        );
        egui::Area::new(egui::Id::new("hover-preview"))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .pivot(pivot)
            .fixed_pos(anchor + off)
            .constrain_to(screen.shrink(6.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(POPUP_W);
                    let node = self.g.node(id);
                    let name = node.display_name().to_string();
                    let path = node.path.clone();
                    let path_key = node.path_key();
                    let abs_path = node.absolute_path(&self.root);
                    let externals: Vec<String> = self
                        .g
                        .outlinks(id)
                        .filter(|l| l.kind == LinkKind::External)
                        .map(|l| self.g.node(l.to).path.clone())
                        .collect();
                    ui.label(egui::RichText::new(name).strong());
                    ui.label(egui::RichText::new(&path).small().color(self.theme.text));
                    // ---- metadata: times, size, reference counts ----
                    if matches!(kind, NodeKind::File | NodeKind::Asset | NodeKind::Image)
                        && let Some(abs_path) = abs_path.as_deref()
                        && let Ok(meta) = std::fs::metadata(abs_path)
                    {
                        ui.label(
                            egui::RichText::new(size_and_age(&meta))
                                .small()
                                .color(self.theme.text),
                        );
                    }
                    match kind {
                        NodeKind::File => {
                            let out = self
                                .g
                                .outlinks(id)
                                .filter(|l| l.kind == LinkKind::WikiLink)
                                .count();
                            let inn = self.g.backlinks(id).count();
                            let mut line = format!("links: {out} out · {inn} in");
                            if !externals.is_empty() {
                                line.push_str(&format!(" · {} external", externals.len()));
                            }
                            ui.label(egui::RichText::new(line).small().color(self.theme.text));
                            for url in externals.iter().take(6) {
                                ui.label(
                                    egui::RichText::new(format!("↗ {url}"))
                                        .small()
                                        .color(self.theme.wiki),
                                );
                            }
                            if externals.len() > 6 {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "… and {} more",
                                        externals.len() - 6
                                    ))
                                    .small()
                                    .weak(),
                                );
                            }
                        }
                        NodeKind::Asset | NodeKind::Image => {
                            let inn = self.g.backlinks(id).count();
                            if inn > 0 {
                                ui.label(
                                    egui::RichText::new(format!("links: {inn} in"))
                                        .small()
                                        .color(self.theme.text),
                                );
                            }
                        }
                        _ => {}
                    }
                    ui.separator();
                    match kind {
                        NodeKind::File | NodeKind::Asset => {
                            let text_asset = kind == NodeKind::Asset && filetype::is_text(&path);
                            if kind == NodeKind::Asset && !text_asset {
                                let size = abs_path
                                    .as_deref()
                                    .and_then(|path| std::fs::metadata(path).ok())
                                    .map(|metadata| metadata.len())
                                    .unwrap_or(0);
                                ui.label(
                                    egui::RichText::new(format!("binary file · {size} bytes"))
                                        .weak(),
                                );
                                return;
                            }
                            if self.hover_body.as_ref().map(|(i, _)| *i) != Some(id) {
                                let body = if let Some(abs_path) = abs_path.as_deref() {
                                    if text_asset {
                                        vault::read_head(abs_path, ASSET_POPUP_CAP)
                                            .unwrap_or_else(|e| format!("error reading file: {e}"))
                                    } else {
                                        self.read_markdown(id, abs_path)
                                    }
                                } else {
                                    "*error reading file:* missing filesystem path".to_string()
                                };
                                self.hover_body = Some((id, body));
                            }
                            // take/put-back so the markdown cache and the
                            // body can be borrowed simultaneously
                            let hb = self.hover_body.take();
                            if let Some((_, body)) = &hb {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} lines · {} words",
                                        body.lines().count(),
                                        body.split_whitespace().count()
                                    ))
                                    .small()
                                    .weak(),
                                );
                                egui::ScrollArea::vertical()
                                    .id_salt("hover-preview-scroll")
                                    .max_height(POPUP_MAX_H)
                                    .show(ui, |ui| {
                                        if text_asset {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(body.as_str())
                                                        .monospace()
                                                        .size(11.0),
                                                )
                                                .wrap(),
                                            );
                                        } else {
                                            let theme = self.theme;
                                            navigator::reading_frame(ui, |ui| {
                                                navigator::render_markdown(
                                                    ui,
                                                    &theme,
                                                    &mut self.md_cache,
                                                    body,
                                                );
                                            });
                                        }
                                    });
                            }
                            self.hover_body = hb;
                        }
                        NodeKind::Image => {
                            let ctx = ui.ctx().clone();
                            if let Some(abs_path) = abs_path.clone() {
                                self.thumbs.request(&ctx, &path_key, abs_path);
                            }
                            match self.thumbs.cache.get(&path_key) {
                                Some(images::ThumbState::Ready { tex, .. }) => {
                                    let size = tex.size_vec2();
                                    let w = (POPUP_W - 12.0).min(size.x.max(128.0));
                                    let h = w * size.y / size.x.max(1.0);
                                    ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                        tex.id(),
                                        egui::vec2(w, h),
                                    )));
                                }
                                Some(images::ThumbState::Failed) => {
                                    ui.label(
                                        egui::RichText::new("could not decode this image").weak(),
                                    );
                                }
                                _ => {
                                    ui.label(egui::RichText::new("loading…").weak());
                                }
                            }
                        }
                        NodeKind::Dir => {
                            let children = self.g.node(id).children.clone();
                            let direct_dirs = children
                                .iter()
                                .filter(|c| self.g.node(**c).kind == NodeKind::Dir)
                                .count();
                            let s = self.g.subtree_stats(id);
                            let th_text = self.theme.text;
                            let small =
                                move |t: String| egui::RichText::new(t).small().color(th_text);
                            ui.label(small(format!(
                                "direct: {} entries ({} folders)",
                                children.len(),
                                direct_dirs
                            )));
                            let mut rec = format!("recursive: {} files", s.files);
                            if s.images > 0 {
                                rec.push_str(&format!(" · {} images", s.images));
                            }
                            if s.assets > 0 {
                                rec.push_str(&format!(" · {} assets", s.assets));
                            }
                            if s.dirs > 0 {
                                rec.push_str(&format!(" · {} folders", s.dirs));
                            }
                            ui.label(small(rec));
                            ui.label(small(format!(
                                "links from its files: {} wiki · {} external",
                                s.wiki_out, s.external_out
                            )));
                            ui.add_space(2.0);
                            for c in children.iter().take(LIST_CAP) {
                                let child = self.g.node(*c);
                                let (icon, suffix) = match child.kind {
                                    NodeKind::Dir => (filetype::ICON_FOLDER, "/"),
                                    NodeKind::Image => (filetype::ICON_IMAGE, ""),
                                    _ => (filetype::icon_of(&child.path), ""),
                                };
                                let color =
                                    Color32::from_rgb(icon.color.0, icon.color.1, icon.color.2);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(
                                        egui::RichText::new(icon.glyph)
                                            .font(icon_font(12.0))
                                            .color(color),
                                    );
                                    ui.label(format!("{}{suffix}", child.display_name()));
                                });
                            }
                            if children.len() > LIST_CAP {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "… and {} more",
                                        children.len() - LIST_CAP
                                    ))
                                    .weak(),
                                );
                            }
                        }
                        NodeKind::Web => {
                            ui.label(egui::RichText::new("cited from:").weak());
                            ui.add_space(2.0);
                            let refs: Vec<String> = self
                                .g
                                .backlinks(id)
                                .map(|l| self.g.node(l.from).path.clone())
                                .collect();
                            for r in refs.iter().take(LIST_CAP) {
                                ui.label(r.as_str());
                            }
                            if refs.len() > LIST_CAP {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "… and {} more",
                                        refs.len() - LIST_CAP
                                    ))
                                    .weak(),
                                );
                            }
                        }
                        NodeKind::Ghost => {
                            ui.label(
                                egui::RichText::new("not written yet — referenced from:").weak(),
                            );
                            ui.add_space(2.0);
                            let refs: Vec<String> = self
                                .g
                                .backlinks(id)
                                .map(|l| self.g.node(l.from).path.clone())
                                .collect();
                            for r in refs.iter().take(LIST_CAP) {
                                ui.label(r.as_str());
                            }
                            if refs.len() > LIST_CAP {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "… and {} more",
                                        refs.len() - LIST_CAP
                                    ))
                                    .weak(),
                                );
                            }
                        }
                    }
                });
            });
    }
}

/// The one-line metadata strip a file gets in popups and the picker's
/// preview header: when it was last touched, when it was born, how big.
pub(super) fn size_and_age(meta: &std::fs::Metadata) -> String {
    let mut bits: Vec<String> = Vec::new();
    if let Ok(m) = meta.modified() {
        bits.push(format!("edited {}", ago(m)));
    }
    if let Ok(c) = meta.created() {
        bits.push(format!("created {}", ago(c)));
    }
    bits.push(human_size(meta.len()));
    bits.join(" · ")
}

/// Compact relative timestamp for popup metadata ("3h ago"). Clock skew or
/// future mtimes degrade to "just now".
pub(super) fn ago(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        86_400..=2_591_999 => format!("{}d ago", secs / 86_400),
        2_592_000..=31_535_999 => format!("{}mo ago", secs / 2_592_000),
        _ => format!("{}y ago", secs / 31_536_000),
    }
}

fn human_size(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    }
}

/// First lines of a body, bounded in both lines and bytes, cut on a char
/// boundary.
fn excerpt(body: &str) -> String {
    let mut out = body.lines().take(MAX_LINES).collect::<Vec<_>>().join("\n");
    if out.len() > MAX_BYTES {
        let mut i = MAX_BYTES;
        while !out.is_char_boundary(i) {
            i -= 1;
        }
        out.truncate(i);
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_opens_toward_screen_center() {
        let screen = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        assert_eq!(
            popup_pivot(Pos2::new(100.0, 100.0), screen),
            Align2::LEFT_TOP
        );
        assert_eq!(
            popup_pivot(Pos2::new(900.0, 100.0), screen),
            Align2::RIGHT_TOP
        );
        assert_eq!(
            popup_pivot(Pos2::new(100.0, 700.0), screen),
            Align2::LEFT_BOTTOM
        );
        assert_eq!(
            popup_pivot(Pos2::new(900.0, 700.0), screen),
            Align2::RIGHT_BOTTOM
        );
    }

    #[test]
    fn ago_buckets_and_human_sizes() {
        use std::time::{Duration, SystemTime};
        let at = |secs: u64| SystemTime::now() - Duration::from_secs(secs);
        assert_eq!(ago(at(5)), "just now");
        assert_eq!(ago(at(90)), "1m ago");
        assert_eq!(ago(at(7_200)), "2h ago");
        assert_eq!(ago(at(200_000)), "2d ago");
        assert_eq!(ago(at(6_000_000)), "2mo ago");
        assert_eq!(ago(at(40_000_000)), "1y ago");
        assert_eq!(ago(SystemTime::now() + Duration::from_secs(60)), "just now");

        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2_048), "2.0 KB");
        assert_eq!(human_size(5_242_880), "5.0 MB");
    }

    #[test]
    fn excerpt_bounds_lines_and_bytes() {
        let many = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>();
        let e = excerpt(&many.join("\n"));
        assert_eq!(e.lines().count(), MAX_LINES);
        assert!(e.starts_with("line 0"));

        let long = "x".repeat(MAX_BYTES * 2);
        let e = excerpt(&long);
        assert!(e.len() <= MAX_BYTES + '…'.len_utf8());
        assert!(e.ends_with('…'));
    }

    #[test]
    fn excerpt_cuts_on_char_boundaries() {
        // a multibyte char straddling the byte cap must not panic
        let s = "é".repeat(MAX_BYTES); // 2 bytes each
        let e = excerpt(&s);
        assert!(e.len() <= MAX_BYTES + '…'.len_utf8());
        assert!(e.chars().all(|c| c == 'é' || c == '…'));
    }

    #[test]
    fn short_bodies_pass_through() {
        assert_eq!(excerpt("hello\nworld"), "hello\nworld");
        assert_eq!(excerpt(""), "");
    }
}
