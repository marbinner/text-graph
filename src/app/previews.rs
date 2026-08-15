//! Zoomed-in text previews for File nodes: when a note's screen size
//! crosses the preview threshold, its card shows the opening lines of the
//! body — the graph-canvas sibling of the detail pane, like terminal cards
//! and image thumbnails.
//!
//! Bodies are never held whole: each cache entry is a small excerpt plus
//! the file's (mtime, len) stamp, and a vault reload evicts only entries
//! whose stamp no longer matches disk (same anti-flicker rule as the
//! thumbnail cache).

use std::collections::HashMap;
use std::path::Path;
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
    pub(super) fn get_or_load(&mut self, root: &Path, key: &str) -> &str {
        if !self.cache.contains_key(key) {
            let path = root.join(key);
            let stamp = file_stamp(&path);
            // markdown gets frontmatter/BOM stripping; any other text file
            // is read raw (a YAML file opening with `---` is not frontmatter)
            let is_md = filetype::ext_of(key).is_some_and(|e| e.eq_ignore_ascii_case("md"));
            let body = if is_md {
                vault::read_body(&path)
            } else {
                vault::read_head(&path, 8 * 1024)
            };
            let excerpt = match body {
                Ok(body) => excerpt(&body),
                Err(_) => "(unreadable)".to_string(),
            };
            self.cache
                .insert(key.to_string(), Preview { excerpt, stamp });
        }
        &self.cache[key].excerpt
    }

    /// Vault reload: evict only entries whose file changed or vanished.
    pub(super) fn retain_fresh(&mut self, root: &Path) {
        self.cache
            .retain(|key, p| fresh(&p.stamp, file_stamp(&root.join(key))));
    }
}

/// Dwell before the hover preview opens — sweeping the cursor across nodes
/// must not strobe popups.
const HOVER_DELAY: Duration = Duration::from_millis(350);
/// Popup content width; images fit within it, text wraps to it.
const POPUP_W: f32 = 430.0;
const POPUP_MAX_H: f32 = 460.0;

/// Which corner of the popup sits on the anchor: pick the quadrant that
/// opens toward screen center, so the popup always has room.
fn popup_pivot(anchor: Pos2, screen: Rect) -> Align2 {
    match (anchor.x < screen.center().x, anchor.y < screen.center().y) {
        (true, true) => Align2::LEFT_TOP,
        (true, false) => Align2::LEFT_BOTTOM,
        (false, true) => Align2::RIGHT_TOP,
        (false, false) => Align2::RIGHT_BOTTOM,
    }
}

impl Viewer {
    /// The full-content hover preview: linger on a File node and its whole
    /// body renders as markdown; on an Image node, the picture at popup
    /// size. Non-interactable, tooltip layer, anchored where the dwell
    /// began.
    pub(super) fn hover_preview_ui(&mut self, ui: &egui::Ui) {
        let Some((id, since, anchor)) = self.hover_since else {
            return;
        };
        let kind = self.g.node(id).kind;
        if !matches!(kind, NodeKind::File | NodeKind::Image) {
            return;
        }
        let elapsed = since.elapsed();
        if elapsed < HOVER_DELAY {
            // wake exactly when the dwell completes
            ui.ctx().request_repaint_after(HOVER_DELAY - elapsed);
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
                    let (name, path) = (node.display_name().to_string(), node.path.clone());
                    ui.label(egui::RichText::new(name).strong());
                    ui.label(egui::RichText::new(&path).small().color(TEXT));
                    ui.separator();
                    match kind {
                        NodeKind::File => {
                            if self.hover_body.as_ref().map(|(i, _)| *i) != Some(id) {
                                let body = vault::read_body(&self.root.join(&path))
                                    .unwrap_or_else(|e| format!("*error reading file:* {e}"));
                                self.hover_body = Some((id, body));
                            }
                            // take/put-back so the markdown cache and the
                            // body can be borrowed simultaneously
                            let hb = self.hover_body.take();
                            if let Some((_, body)) = &hb {
                                egui::ScrollArea::vertical()
                                    .id_salt("hover-preview-scroll")
                                    .max_height(POPUP_MAX_H)
                                    .show(ui, |ui| {
                                        CommonMarkViewer::new().show(ui, &mut self.md_cache, body);
                                    });
                            }
                            self.hover_body = hb;
                        }
                        NodeKind::Image => {
                            let ctx = ui.ctx().clone();
                            self.thumbs.request(&ctx, &path, self.root.join(&path));
                            match self.thumbs.cache.get(&path) {
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
                        _ => {}
                    }
                });
            });
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
