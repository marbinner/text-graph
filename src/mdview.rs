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

use std::ops::Range;
use std::path::Path;

use pulldown_cmark::{Event, Parser, Tag};

use crate::graph::{Graph, NodeId, NodeKind};
use crate::vault;

/// URL scheme for in-graph links; the app intercepts these clicks.
pub const SCHEME: &str = "tg://";

/// A preview is a glance, not a bulk-file reader. More importantly, the
/// renderer's generic file loader reads a destination into one Vec before
/// decoding it; bounding files here keeps a planted image path from exhausting
/// the process before the image decoder gets a say.
const MAX_PREVIEW_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const BLOCKED_IMAGE: &str = "*[image blocked]*";

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
fn file_url(path: &Path) -> String {
    format!("<file://{}>", path.display())
}

/// Resolve an image path at PREVIEW time and return a URL only when the final
/// object is a regular, reasonably sized file inside the canonical vault.
/// Canonicalizing both sides rejects symlinks out of the vault; emitting the
/// canonical target also keeps the generic renderer from following the
/// original symlink later.
fn safe_image_url(root: &Path, path: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    if !path.starts_with(&root) {
        return None;
    }
    let meta = path.metadata().ok()?;
    (meta.is_file() && meta.len() <= MAX_PREVIEW_IMAGE_BYTES).then(|| file_url(&path))
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

    // Resolution comes from the graph's per-occurrence index, not its
    // deduplicated edge list: two different spellings can resolve to the
    // same node while both still need clickable preview links.

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
                    if let Some(url) = safe_image_url(root, &root.join(&g.node(id).path)) {
                        reps.push((span_start..inner_end + 2, format!("![]({url})")));
                    }
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
        } else if let Some(id) = g.wikilink_at(source, start) {
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
        if dest.is_empty() || dest.starts_with('#') {
            continue;
        }
        if dest.contains("://") || dest.starts_with("data:") {
            // Only URLs minted above by this function may reach the generic
            // file loader. A note-authored file:// can name anything on the
            // machine; other schemes are unsupported by this local previewer.
            if image {
                reps.push((range, BLOCKED_IMAGE.to_string()));
            }
            continue;
        }
        // resolve ignoring any #fragment (the rewrite replaces the whole
        // dest span — a tg:// jump has no use for the fragment)
        let path_part = dest.split_once('#').map_or(dest.as_str(), |(p, _)| p);
        let Some(resolved) = resolve_relative(source_dir, path_part) else {
            if image {
                reps.push((range, BLOCKED_IMAGE.to_string()));
            }
            continue;
        };
        let new = if image {
            safe_image_url(root, &root.join(&resolved))
        } else {
            g.by_path(&resolved).map(node_url)
        };
        let Some(new) = new else {
            if image {
                reps.push((range, BLOCKED_IMAGE.to_string()));
            }
            continue;
        };
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

    // ---- callouts: `> [!warning] Title` ----
    // The renderer's alert parser wants `[!WARNING]` and nothing else on
    // the line; Obsidian writes any case, an optional fold marker, and an
    // optional title. Normalize the marker and give the title its own
    // quoted line in bold, which is how Obsidian shows it anyway.
    for (span, new) in callout_headers(body, &in_code) {
        reps.push((span, new));
    }

    // ---- Obsidian's own inline marks ----
    // Everything here is a rewrite into plain CommonMark, because the
    // renderer only speaks CommonMark. Code spans and fences are excluded
    // like every other scan above.
    //
    // %%comments%% are not for the reader — Obsidian hides them.
    for (at, len) in spans_between(body, "%%", &in_code) {
        reps.push((at..at + len, String::new()));
    }
    // ==highlight== has no CommonMark spelling; bold is the closest thing
    // that still reads as emphasis rather than as two stray equals signs.
    for (at, len) in spans_between(body, "==", &in_code) {
        let inner = &body[at + 2..at + len - 2];
        reps.push((at..at + len, format!("**{inner}**")));
    }
    // #tags become code chips: visible as one token, and never mistaken
    // for a heading (a heading is `# ` — with the space).
    for (at, len) in tag_spans(body, &in_code) {
        reps.push((at..at + len, format!("`{}`", &body[at..at + len])));
    }
    // ^block-ids are addresses, not text: Obsidian doesn't show them.
    for (at, len) in block_id_spans(body, &in_code) {
        reps.push((at..at + len, String::new()));
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

/// Obsidian callout headers, rewritten to what the alert parser accepts.
/// Returns (span of the header line, replacement).
fn callout_headers(body: &str, in_code: &dyn Fn(usize) -> bool) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in body.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        // the quote prefix: any mix of '>' and spaces (callouts nest)
        let prefix_len = trimmed
            .find(|c: char| c != '>' && c != ' ' && c != '\t')
            .unwrap_or(trimmed.len());
        let (prefix, rest) = trimmed.split_at(prefix_len);
        if !prefix.contains('>') || in_code(start) {
            continue;
        }
        let Some(rest) = rest.strip_prefix("[!") else {
            continue;
        };
        let Some(close) = rest.find(']') else {
            continue;
        };
        let kind = &rest[..close];
        if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        // `[!note]-` / `[!note]+` fold the callout in Obsidian; we always
        // show the contents, so the marker is just dropped
        let title = rest[close + 1..].trim_start_matches(['-', '+']).trim();
        let mut new = format!("{prefix}[!{}]", kind.to_ascii_uppercase());
        if !title.is_empty() {
            new.push('\n');
            new.push_str(prefix.trim_end());
            new.push_str(&format!(" **{title}**"));
        }
        out.push((start..start + trimmed.len(), new));
    }
    out
}

/// Paired inline marks (`%%…%%`, `==…==`): (start, total length) for each
/// pair on ONE line, outside code. Unclosed marks are left alone — a lone
/// `==` in prose is prose.
fn spans_between(body: &str, mark: &str, in_code: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(found) = body[i..].find(mark) {
        let start = i + found;
        let inner_start = start + mark.len();
        let Some(close) = body[inner_start..].find(mark) else {
            break;
        };
        let end = inner_start + close + mark.len();
        i = end;
        let inner = &body[inner_start..end - mark.len()];
        if in_code(start) || inner.is_empty() || inner.contains('\n') {
            continue;
        }
        out.push((start, end - start));
    }
    out
}

/// Obsidian tags: `#` followed by a non-space, not glued to a word (so
/// `a#b` and `#` in a URL fragment stay put) and not a heading marker.
fn tag_spans(body: &str, in_code: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(found) = body[i..].find('#') {
        let start = i + found;
        i = start + 1;
        let before = start.checked_sub(1).map(|p| b[p]);
        if before.is_some_and(|c| !c.is_ascii_whitespace()) || in_code(start) {
            continue; // a#b, an anchor, code
        }
        let rest = &body[start + 1..];
        let len = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '/'))
            .unwrap_or(rest.len());
        // `# heading` has a space right after the hash; a tag never does,
        // and a tag has to have something in it
        if len == 0 || !rest[..len].chars().any(char::is_alphabetic) {
            continue;
        }
        out.push((start, len + 1));
        i = start + 1 + len;
    }
    out
}

/// Trailing `^block-id` anchors — an address for a link to point at, not
/// something a reader wants to see.
fn block_id_spans(body: &str, in_code: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if let Some(hat) = trimmed.rfind(" ^") {
            let id = &trimmed[hat + 2..];
            if !id.is_empty()
                && id.chars().all(|c| c.is_alphanumeric() || c == '-')
                && !in_code(at + hat)
            {
                out.push((at + hat, trimmed.len() - hat));
            }
        }
        at += line.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-note vault on disk — `prepare` reads the graph for its
    /// source node, so an empty Graph has nothing to be the source of.
    fn one_note_vault(tag: &str) -> (std::path::PathBuf, Graph, NodeId) {
        let d = std::env::temp_dir().join(format!("tg-mdview-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("note.md"), "x").unwrap();
        let g = crate::graph::build(vault::scan(&d).unwrap());
        let src = g.by_path("note.md").unwrap();
        (d, g, src)
    }

    /// Obsidian's inline marks, rewritten into CommonMark the renderer
    /// can actually draw — and never inside code, where they are text.
    #[test]
    fn obsidian_inline_marks_render_as_markdown() {
        let (d, g, src) = one_note_vault("obsidian");
        let out = prepare(
            &g,
            &d,
            src,
            "Some ==important== bit %%not for the reader%% here.\n\
             Tagged #rust and #deep/work today. ^block-42\n\
             `==code==` and `#nope` stay literal.\n\
             # A heading keeps its hash\n\
             See a#b and https://x/y#frag.\n",
        );
        let _ = std::fs::remove_dir_all(&d);
        assert!(
            out.contains("**important**"),
            "==x== reads as emphasis: {out}"
        );
        assert!(
            !out.contains("not for the reader"),
            "%%comments%% are hidden"
        );
        assert!(
            out.contains("`#rust`") && out.contains("`#deep/work`"),
            "{out}"
        );
        assert!(!out.contains("^block-42"), "block ids are addresses: {out}");
        assert!(
            out.contains("`==code==`") && out.contains("`#nope`"),
            "code is literal"
        );
        assert!(out.contains("# A heading"), "a heading is not a tag");
        assert!(out.contains("a#b"), "a hash inside a word is not a tag");
        assert!(out.contains("https://x/y#frag"), "nor is a URL fragment");
    }

    /// Callouts: any case, an optional fold marker, an optional title —
    /// all normalized to the one spelling the renderer's alert parser
    /// accepts, with the title kept as its own bold line.
    #[test]
    fn callout_headers_are_normalized() {
        let (d, g, src) = one_note_vault("callout");
        let out = prepare(
            &g,
            &d,
            src,
            "> [!warning] Mind the gap\n> body\n\n\
             > [!tip]-\n> folded in obsidian, open here\n\n\
             >> [!note] nested\n\n\
             > [!not a callout] stays\n",
        );
        let _ = std::fs::remove_dir_all(&d);
        assert!(
            out.contains("> [!WARNING]\n> **Mind the gap**"),
            "marker uppercased, title moved to its own line: {out}"
        );
        assert!(
            out.contains("> [!TIP]"),
            "the fold marker is dropped: {out}"
        );
        assert!(out.contains(">> [!NOTE]"), "nesting is kept: {out}");
        assert!(
            out.contains("[!not a callout]"),
            "a bracket that isn't a type is left alone: {out}"
        );
    }

    #[test]
    fn unclosed_marks_are_left_alone() {
        let (d, g, src) = one_note_vault("unclosed");
        let out = prepare(
            &g,
            &d,
            src,
            "2 == 2 is true, and 50%% of nothing.\nx ^ y is a caret.\n",
        );
        let _ = std::fs::remove_dir_all(&d);
        assert!(out.contains("2 == 2"), "a lone == is arithmetic: {out}");
        assert!(out.contains("x ^ y"), "a lone ^ is a caret: {out}");
    }

    #[test]
    fn tg_urls_round_trip() {
        assert_eq!(parse_url(&node_url(NodeId(7))), Some(7));
        assert_eq!(parse_url("https://x"), None);
        assert_eq!(parse_url("tg://nope"), None);
    }

    #[test]
    fn every_wikilink_occurrence_survives_edge_deduplication() {
        let d = std::env::temp_dir().join(format!("tg-mdview-occ-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dir")).unwrap();
        let body = "[[target|bare]] [[dir/target|qualified]] [[target|again]]";
        std::fs::write(d.join("source.md"), body).unwrap();
        std::fs::write(d.join("dir/target.md"), "# target").unwrap();
        let g = crate::graph::build(vault::scan(&d).unwrap());
        let source = g.by_path("source.md").unwrap();
        let target = g.by_path("dir/target.md").unwrap();

        assert_eq!(
            g.outlinks(source).filter(|link| link.to == target).count(),
            1,
            "topology still deduplicates the shared destination"
        );
        for raw in vault::extract_links(body) {
            assert_eq!(g.wikilink_at(source, raw.offset), Some(target));
        }
        let out = prepare(&g, &d, source, body);
        assert_eq!(
            out.matches(&format!("({})", node_url(target))).count(),
            3,
            "each spelling and duplicate must be clickable: {out}"
        );
        let _ = std::fs::remove_dir_all(&d);
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
            format!("![]({})", file_url(&d.join("b/cover.png"))),
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
        let url = file_url(&d.join("docs/pic.png"));
        assert_eq!(
            out,
            format!("![p]({url}) ![b]({url})"),
            "images resolve from the source dir; <> dests don't nest brackets"
        );

        let hostile = "[e](../../etc/passwd) [a](/etc/passwd) ![i](../../../etc/passwd)";
        assert_eq!(
            prepare(&g, &d, src, hostile),
            format!("[e](../../etc/passwd) [a](/etc/passwd) {BLOCKED_IMAGE}"),
            "escaping links remain user-clickable text, but images cannot reach the file loader"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Regression: for self-labeled links `[p](p)` the rewrite found the
    /// dest string in the LABEL (it comes first in the construct) and
    /// spliced `tg://N` into the visible text, leaving the real
    /// destination a dangling relative path that leaked to the OS opener.
    #[test]
    fn image_destinations_are_confined_to_regular_vault_files() {
        let (d, g, src) = one_note_vault("safe-images");
        std::fs::write(d.join("pic.png"), "small").unwrap();
        let outside = d.with_extension("outside.png");
        std::fs::write(&outside, "private").unwrap();
        let huge = d.join("huge.png");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_PREVIEW_IMAGE_BYTES + 1)
            .unwrap();

        let body = format!(
            "![ok](pic.png) ![absolute]({}) ![explicit](file:///dev/zero)              ![remote](https://example.com/a.png) ![huge](huge.png)",
            outside.display()
        );
        let out = prepare(&g, &d, src, &body);
        assert!(
            out.starts_with(&format!("![ok]({})", file_url(&d.join("pic.png")))),
            "a small regular file in the vault remains renderable: {out}"
        );
        assert_eq!(
            out.matches(BLOCKED_IMAGE).count(),
            4,
            "absolute, explicit, remote and oversized images are blocked: {out}"
        );

        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(unix)]
    #[test]
    fn image_symlinks_cannot_escape_the_vault() {
        let (d, g, src) = one_note_vault("safe-image-link");
        let outside = d.with_extension("outside-link.png");
        std::fs::write(&outside, "private").unwrap();
        std::os::unix::fs::symlink(&outside, d.join("link.png")).unwrap();

        assert_eq!(prepare(&g, &d, src, "![x](link.png)"), BLOCKED_IMAGE);

        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(d);
    }

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
        assert_eq!(out, format!("![pic.png]({})", file_url(&d.join("pic.png"))));
        let _ = std::fs::remove_dir_all(&d);
    }
}
