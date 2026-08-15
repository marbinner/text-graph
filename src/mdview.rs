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

/// Resolve a markdown-relative destination against the source note's
/// directory ("" = vault root), normalizing `.`/`..` — standard markdown
/// resolves relative to the FILE, not the collection root. None for
/// absolute paths and anything that escapes the vault: a rewrite there
/// could mint `file://` URLs outside it.
fn resolve_relative(source_dir: &str, dest: &str) -> Option<String> {
    if dest.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = source_dir.split('/').filter(|s| !s.is_empty()).collect();
    for c in dest.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?; // pop of an empty stack = escape above root
            }
            seg => parts.push(seg),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
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
            // a path-qualified embed resolves by PATH first —
            // ![[b/cover.png]] must not grab a/cover.png just because
            // its leaf name matches earlier in sorted order
            let resolved = g
                .by_path(target)
                .or_else(|| g.by_path(&format!("{target}.md")))
                .or_else(|| resolve_name(g, target));
            match resolved {
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

    // ---- footnote-style citations: [^target] where target is a vault
    // file (a wiki convention: sources cited by path). The label resolves
    // by exact path, path + ".md", then bare-name lookup; the link shows
    // the note's display name. Real footnotes ([^1], and any [^x]:
    // definition line) stay untouched. ----
    // A label with a `[^label]:` definition anywhere in the body is a real
    // footnote, never a citation — even when a vault file shares the name.
    let footnote_defs: std::collections::HashSet<&str> = body
        .lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix("[^")?;
            let end = rest.find("]:")?;
            Some(&rest[..end])
        })
        .collect();
    let mut i = 0;
    while let Some(found) = body[i..].find("[^") {
        let start = i + found;
        let label_start = start + 2;
        let Some(close) = body[label_start..].find(']') else {
            break;
        };
        let label_end = label_start + close;
        i = label_end + 1;
        let label = &body[label_start..label_end];
        if in_code(start) || label.is_empty() || label.contains('\n') || label.contains('[') {
            continue;
        }
        if body[label_end + 1..].starts_with(':') {
            continue; // a footnote definition, not a reference
        }
        if footnote_defs.contains(label) {
            continue; // a reference to a real footnote — leave it alone
        }
        let id = g
            .by_path(label)
            .or_else(|| g.by_path(&format!("{label}.md")))
            .or_else(|| resolve_name(g, label));
        if let Some(id) = id {
            let name = g
                .node(id)
                .display_name()
                .replace('[', "\\[")
                .replace(']', "\\]");
            reps.push((start..label_end + 1, format!("[^{name}]({})", node_url(id))));
        }
    }

    // ---- relative destinations in standard markdown links/images ----
    // Resolved against the SOURCE note's directory (regression:
    // docs/source.md linking setup.md used to look up the vault root's
    // setup.md instead of docs/setup.md).
    let source_dir = g.node(source).path.rsplit_once('/').map_or("", |(d, _)| d);
    for (event, range) in Parser::new(body).into_offset_iter() {
        let (dest, image) = match &event {
            Event::Start(Tag::Link { dest_url, .. }) => (dest_url.to_string(), false),
            Event::Start(Tag::Image { dest_url, .. }) => (dest_url.to_string(), true),
            _ => continue,
        };
        if dest.is_empty() || dest.contains("://") || dest.starts_with('#') {
            continue;
        }
        // resolve ignoring any #fragment (the rewrite replaces the whole
        // dest span — a tg:// jump has no use for the fragment)
        let path_part = dest.split_once('#').map_or(dest.as_str(), |(p, _)| p);
        let Some(resolved) = resolve_relative(source_dir, path_part) else {
            continue;
        };
        let new = if image {
            root.join(&resolved)
                .exists()
                .then(|| file_url(root, &resolved))
        } else {
            g.by_path(&resolved).map(node_url)
        };
        let Some(new) = new else { continue };
        // the destination appears verbatim AFTER the `](` separator —
        // searching the whole construct hit the LABEL first whenever the
        // label contains the dest string (self-labeled [p](p) links),
        // splicing the URL into the visible text. The LAST `](` is the
        // real separator (an earlier one belongs to an image nested in
        // the label); reference-style constructs have none and are left
        // alone, as are escapes/percent-encoding (skip quietly).
        let construct = &body[range.clone()];
        let Some(sep) = construct.rfind("](") else {
            continue;
        };
        let Some(at) = construct[sep + 2..].find(&dest) else {
            continue;
        };
        let abs = range.start + sep + 2 + at;
        let mut span = abs..abs + dest.len();
        // `[x](<dest with spaces>)`: the angle brackets belong to the
        // dest — widen the span so the rewrite doesn't nest them
        // (`<<file://…>>` renders broken)
        if span.start > 0
            && body.as_bytes().get(span.start - 1) == Some(&b'<')
            && body.as_bytes().get(span.end) == Some(&b'>')
        {
            span = span.start - 1..span.end + 1;
        }
        reps.push((span, new));
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

    /// `![[b/cover.png]]` must embed b's cover even when a/cover.png
    /// sorts first (path beats leaf lookup), and `[^n]` with a real
    /// `[^n]:` definition must stay a footnote even when n.md exists.
    #[test]
    fn embeds_prefer_exact_paths_and_defined_footnotes_stay_footnotes() {
        let d = std::env::temp_dir().join(format!("tg-mdview-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for dir in ["a", "b"] {
            std::fs::create_dir_all(d.join(dir)).unwrap();
            std::fs::write(d.join(dir).join("cover.png"), "x").unwrap();
        }
        std::fs::write(d.join("n.md"), "x").unwrap();
        std::fs::write(d.join("note.md"), "x").unwrap();
        let g = crate::graph::build(vault::scan(&d).unwrap());
        let src = g.by_path("note.md").unwrap();
        let n = g.by_path("n.md").unwrap();

        assert_eq!(
            prepare(&g, &d, src, "![[b/cover.png]]"),
            format!("![]({})", file_url(&d, "b/cover.png")),
            "the qualified path wins over sorted-leaf order"
        );
        let real = "See [^n].\n\n[^n]: an actual footnote";
        assert_eq!(
            prepare(&g, &d, src, real),
            real,
            "a defined footnote is never hijacked as a citation"
        );
        assert_eq!(
            prepare(&g, &d, src, "See [^n]."),
            format!("See [^{}]({}).", g.node(n).display_name(), node_url(n)),
            "without a definition the label still cites the note"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Standard markdown resolves relative to the FILE: docs/source.md
    /// linking `setup.md` means docs/setup.md (regression: it resolved
    /// against the vault root). `..` normalizes; escapes and absolute
    /// paths stay untouched so `file://` can never point outside the
    /// vault; a `<bracketed>` dest must not gain nested brackets.
    #[test]
    fn relative_dests_resolve_against_the_source_note_directory() {
        let d = std::env::temp_dir().join(format!("tg-mdview-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/source.md"), "x").unwrap();
        std::fs::write(d.join("docs/setup.md"), "x").unwrap();
        std::fs::write(d.join("docs/pic.png"), "x").unwrap();
        std::fs::write(d.join("root.md"), "x").unwrap();
        std::fs::write(d.join("setup.md"), "DECOY at root").unwrap();
        let g = crate::graph::build(vault::scan(&d).unwrap());
        let src = g.by_path("docs/source.md").unwrap();
        let in_docs = g.by_path("docs/setup.md").unwrap();
        let at_root = g.by_path("root.md").unwrap();

        let out = prepare(
            &g,
            &d,
            src,
            "[s](setup.md) [r](../root.md) [f](./setup.md#sec)",
        );
        assert_eq!(
            out,
            format!(
                "[s]({}) [r]({}) [f]({})",
                node_url(in_docs),
                node_url(at_root),
                node_url(in_docs)
            ),
            "file-relative, .. and ./ + #fragment all resolve from docs/"
        );

        let out = prepare(&g, &d, src, "![p](pic.png) ![b](<pic.png>)");
        let url = file_url(&d, "docs/pic.png");
        assert_eq!(
            out,
            format!("![p]({url}) ![b]({url})"),
            "images resolve from the source dir; <> dests don't nest brackets"
        );

        let hostile = "[e](../../etc/passwd) [a](/etc/passwd) ![i](../../../etc/passwd)";
        assert_eq!(
            prepare(&g, &d, src, hostile),
            hostile,
            "escapes above the vault and absolute paths are never rewritten"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Regression: for self-labeled links `[p](p)` the rewrite found the
    /// dest string in the LABEL (it comes first in the construct) and
    /// spliced `tg://N` into the visible text, leaving the real
    /// destination a dangling relative path that leaked to the OS opener.
    #[test]
    fn relative_link_rewrite_targets_the_destination_not_the_label() {
        let d = std::env::temp_dir().join(format!("tg-mdview-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/setup.md"), "# setup").unwrap();
        std::fs::write(d.join("pic.png"), "not really a png").unwrap();
        std::fs::write(d.join("note.md"), "x").unwrap();
        let g = crate::graph::build(vault::scan(&d).unwrap());
        let src = g.by_path("note.md").unwrap();
        let dest = g.by_path("docs/setup.md").unwrap();

        let out = prepare(&g, &d, src, "See [docs/setup.md](docs/setup.md).");
        assert_eq!(
            out,
            format!("See [docs/setup.md]({}).", node_url(dest)),
            "label stays readable; only the destination becomes tg://"
        );
        // the image twin: a self-labeled alt text must survive too
        let out = prepare(&g, &d, src, "![pic.png](pic.png)");
        assert_eq!(out, format!("![pic.png]({})", file_url(&d, "pic.png")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
