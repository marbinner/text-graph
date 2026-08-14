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

use text_graph::vault;

use super::images::{Stamp, file_stamp, fresh};

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
            let excerpt = match vault::read_body(&path) {
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
