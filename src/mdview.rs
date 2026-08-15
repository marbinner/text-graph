//! Obsidian-flavored preprocessing for markdown previews.
//!
//! Rewrites a note's body so a plain CommonMark renderer shows it the way
//! Obsidian would: `[[wikilinks]]` become real links on a `tg://<node>`
//! scheme (the app intercepts clicks and jumps to the node instead of
//! opening a browser), `![[image embeds]]` become inline images on
//! `file://` URIs, note embeds degrade to links, and relative markdown
//! link/image destinations are resolved against the vault (to `tg://` for
//! notes, absolute `file://` for images). Code spans and fences stay
//! byte-for-byte untouched. Pure string work — egui-free, headless-tested.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use pulldown_cmark::{Event, Parser, Tag};

use crate::graph::{Graph, NodeId, NodeKind};
use crate::vault;

/// URL scheme for in-graph links; the app intercepts these clicks.
pub const SCHEME: &str = "tg://";

pub fn node_url(id: NodeId) -> String {
    format!("{SCHEME}{}", id.0)
}

/// The node index in a `tg://` URL, if it is one. Callers must bounds-check
/// against the CURRENT graph — the string may outlive a reload.
pub fn parse_url(url: &str) -> Option<u32> {
    url.strip_prefix(SCHEME)?.parse().ok()
}

/// Strip alias and heading/block suffixes from a wikilink inner text —
/// mirrors what resolution does.
fn strip_target(inner: &str) -> &str {
    inner
        .split('|')
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
        .trim()
}

/// What Obsidian displays for a wikilink: the alias if present, else the
/// full inner text (heading suffix included).
fn display_text(inner: &str) -> &str {
    inner.rsplit_once('|').map_or(inner, |(_, a)| a.trim())
}

/// Case-insensitive leaf lookup by name (images/assets keep extensions,
/// file names are stems) — resolution for embeds, which the edge extractor
/// deliberately skips.
fn resolve_name(g: &Graph, target: &str) -> Option<NodeId> {
    let last = target.rsplit_once('/').map_or(target, |(_, f)| f);
    g.nodes.iter().enumerate().find_map(|(i, n)| {
        (matches!(n.kind, NodeKind::File | NodeKind::Image | NodeKind::Asset)
            && n.name.eq_ignore_ascii_case(last))
        .then_some(NodeId(i as u32))
    })
}

/// A `file://` URI for a vault-relative path, angle-bracketed so markdown
/// tolerates spaces in the path.
fn file_url(root: &Path, rel: &str) -> String {
    format!("<file://{}>", root.join(rel).display())
}

/// Rewrite `body` (the source node's `read_body` output) for display.
pub fn prepare(g: &Graph, root: &Path, source: NodeId, body: &str) -> String {
    let excluded = vault::excluded_ranges(body);
    let in_code = |at: usize| excluded.iter().any(|r| r.contains(&at));

    // Resolution for wikilinks comes from the graph's own edges, matched
    // back to occurrences by byte offset (so display uses exactly what the
    // resolver decided); duplicate occurrences of the same target reuse the
    // first edge's node.
    let by_offset: HashMap<usize, NodeId> = g.outlinks(source).map(|l| (l.offset, l.to)).collect();
    let mut by_target: HashMap<String, NodeId> = HashMap::new();
    for raw in vault::extract_links(body) {
        if let Some(&to) = by_offset.get(&raw.offset) {
            by_target.entry(raw.target).or_insert(to);
        }
    }

    let mut reps: Vec<(Range<usize>, String)> = Vec::new();

    // ---- wikilinks and embeds: [[inner]] / ![[inner]] ----
    let mut i = 0;
    while let Some(found) = body[i..].find("[[") {
        let start = i + found;
        let inner_start = start + 2;
        let Some(close) = body[inner_start..].find("]]") else {
            break;
        };
        let inner_end = inner_start + close;
        // nested-opener quirk: the last [[ before ]] is the real opener
        if let Some(nested) = body[start + 1..inner_end].find("[[") {
            i = start + 1 + nested;
            continue;
        }
        i = inner_end + 2;
        let inner = &body[inner_start..inner_end];
        if in_code(start) || inner.contains('\n') || strip_target(inner).is_empty() {
            continue;
        }
        let embed = start > 0 && body.as_bytes()[start - 1] == b'!';
        let span_start = if embed { start - 1 } else { start };
        let target = strip_target(inner);
        if embed {
            match resolve_name(g, target) {
                Some(id) if matches!(g.node(id).kind, NodeKind::Image) => {
                    reps.push((
                        span_start..inner_end + 2,
                        format!("![]({})", file_url(root, &g.node(id).path)),
                    ));
                }
                // a note embed degrades to a link on the note
                Some(id) => {
                    reps.push((
                        span_start..inner_end + 2,
                        format!("[{}]({})", display_text(inner), node_url(id)),
                    ));
                }
                None => {}
            }
        } else if let Some(&id) = by_target.get(target) {
            reps.push((
                span_start..inner_end + 2,
                format!("[{}]({})", display_text(inner), node_url(id)),
            ));
        }
    }

    // ---- relative destinations in standard markdown links/images ----
    for (event, range) in Parser::new(body).into_offset_iter() {
        let (dest, image) = match &event {
            Event::Start(Tag::Link { dest_url, .. }) => (dest_url.to_string(), false),
            Event::Start(Tag::Image { dest_url, .. }) => (dest_url.to_string(), true),
            _ => continue,
        };
        if dest.is_empty() || dest.contains("://") || dest.starts_with('#') {
            continue;
        }
        let new = if image {
            root.join(&dest).exists().then(|| file_url(root, &dest))
        } else {
            g.by_path(&dest).map(node_url)
        };
        let Some(new) = new else { continue };
        // the destination appears verbatim inside the construct; replace
        // that occurrence only (skip quietly on escapes/percent-encoding)
        let Some(at) = body[range.clone()].find(&dest) else {
            continue;
        };
        let abs = range.start + at;
        reps.push((abs..abs + dest.len(), new));
    }

    // apply back-to-front so earlier offsets stay valid; drop overlaps
    // (defensive — the scans target disjoint constructs)
    reps.sort_by_key(|(r, _)| r.start);
    reps.dedup_by(|b, a| b.0.start < a.0.end);
    let mut out = body.to_string();
    for (r, new) in reps.into_iter().rev() {
        out.replace_range(r, &new);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tg_urls_round_trip() {
        assert_eq!(parse_url(&node_url(NodeId(7))), Some(7));
        assert_eq!(parse_url("https://x"), None);
        assert_eq!(parse_url("tg://nope"), None);
    }

    #[test]
    fn wikilink_display_and_target_stripping() {
        assert_eq!(strip_target("a/b#h|shown"), "a/b");
        assert_eq!(display_text("a/b#h|shown"), "shown");
        assert_eq!(display_text("a/b#h"), "a/b#h");
    }
}
