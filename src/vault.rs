//! Vault scanning: walk a directory (every visible file becomes a leaf —
//! markdown parsed, images and other assets as paths), parse frontmatter,
//! and extract wikilink targets (still unresolved strings).
//!
//! No global state — every file is parsed independently. Resolution of the
//! extracted targets happens in [`crate::resolve`].

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pulldown_cmark::{Event, Parser, Tag};

/// One markdown file, parsed but with link targets still unresolved.
#[derive(Debug)]
pub struct RawFile {
    /// Path relative to the vault root, forward-slash separated, with extension.
    pub rel_path: String,
    /// `title:` from frontmatter, if present.
    pub title: Option<String>,
    /// `aliases:` (or singular `alias:`) from frontmatter — alternate link names.
    pub aliases: Vec<String>,
    /// Extracted wikilink targets, in document order.
    pub links: Vec<RawLink>,
    /// External http(s) URLs found in the body, deduplicated by exact
    /// text, in document order. Resolution turns these into Web nodes and
    /// External edges (identity decided there, via weburl::normalize).
    pub externals: Vec<RawExternal>,
    /// Non-fatal parse problem (e.g. invalid frontmatter YAML).
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawLink {
    /// Target with alias / heading / block suffixes stripped, e.g. `topics/rust`.
    pub target: String,
    /// Byte offset of `[[` in the body (kept for future use: previews, jumps).
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct RawExternal {
    /// The URL as written (unnormalized — display keeps the author's form).
    pub url: String,
    /// Byte offset in the body, like [`RawLink::offset`].
    pub offset: usize,
    /// The markdown link text when cited as `[text](url)` — an authored
    /// title for the web node. None for bare URLs and autolinks.
    pub text: Option<String>,
}

#[derive(Debug)]
pub struct ScanError {
    pub rel_path: String,
    pub message: String,
}

#[derive(Debug)]
pub struct VaultScan {
    /// Canonicalized vault root.
    pub root: PathBuf,
    /// Parsed files, sorted by `rel_path` — determinism depends on this,
    /// because NodeIds are assigned in this order.
    pub files: Vec<RawFile>,
    /// Image files (rel paths, sorted). Not parsed — they become Image
    /// nodes with no links of their own.
    pub images: Vec<String>,
    /// Every other visible file (rel paths, sorted) — code, config, data,
    /// binaries. Not parsed; they become Asset nodes.
    pub assets: Vec<String>,
    /// Files that could not be read; the scan continues past them.
    pub errors: Vec<ScanError>,
}

/// Directories skipped regardless of hidden-file handling. The dotdirs are
/// belt and suspenders (the walker's hidden filter already covers them);
/// the build/dependency dirs matter now that every file type becomes a
/// node — a `cargo build` must not flood the graph or storm the watcher.
const SKIPPED_DIRS: &[&str] = &[
    ".obsidian",
    ".trash",
    "node_modules",
    "target",
    "__pycache__",
];

/// Raster formats the scan turns into Image nodes. SVG is excluded — the
/// viewer has no vector rasterizer.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

fn image_ext(ext: &str) -> bool {
    IMAGE_EXTS.iter().any(|x| ext.eq_ignore_ascii_case(x))
}

pub fn scan(root: &Path) -> Result<VaultScan> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot open vault root {}", root.display()))?;
    if !root.is_dir() {
        bail!("vault root {} is not a directory", root.display());
    }

    // Collect every visible file: .md parsed, images and everything else as
    // leaf paths. Hidden files/dirs are skipped (.obsidian, .trash, .git);
    // git-ignore semantics are deliberately disabled — the viewer should
    // show what's on disk, not what git tracks.
    let mut paths: Vec<(String, PathBuf)> = Vec::new();
    let mut images: Vec<String> = Vec::new();
    let mut assets: Vec<String> = Vec::new();
    let mut errors = Vec::new();
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(true)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        // prune SKIPPED_DIRS at the walk itself: descending into a code
        // vault's target/ or node_modules/ stat'd tens of thousands of
        // files per reload just to discard them one by one (and surfaced
        // walk errors for unreadable dirs nobody cares about). depth 0 is
        // the root — a vault literally named "target" must still open.
        .filter_entry(|e| {
            !(e.depth() > 0
                && e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIPPED_DIRS.contains(&n)))
        })
        .build();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // an unreadable subdirectory must not silently vanish from
                // the graph while stats report "errors: 0"
                errors.push(ScanError {
                    rel_path: "(walk)".into(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        let ext = path.extension().and_then(|e| e.to_str());
        if in_skipped_dir(&root, &path) {
            continue;
        }
        if ext.is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            let rel = rel_str(&root, &path);
            paths.push((rel, path));
        } else if ext.is_some_and(image_ext) {
            images.push(rel_str(&root, &path));
        } else {
            assets.push(rel_str(&root, &path));
        }
    }

    // Sort by the relative-path STRING: this is both what makes runs
    // deterministic and the byte order that "first in sorted path order"
    // means everywhere else (docs, ambiguity resolution). PathBuf's
    // component-wise order differs around '/' vs '.'/'-' in dirnames.
    paths.sort();
    images.sort();
    assets.sort();
    // walk errors too: the walker yields them in raw readdir order, and
    // g.errors flows verbatim into stats output and the diag window (read
    // errors, appended below, already follow sorted path order)
    errors.sort_by(|a, b| (&a.rel_path, &a.message).cmp(&(&b.rel_path, &b.message)));

    let mut files = Vec::with_capacity(paths.len());
    for (rel, path) in paths {
        match std::fs::read(&path) {
            Ok(bytes) => files.push(parse_file(rel, &bytes)),
            Err(e) => errors.push(ScanError {
                rel_path: rel,
                message: e.to_string(),
            }),
        }
    }

    Ok(VaultScan {
        root,
        files,
        images,
        assets,
        errors,
    })
}

fn rel_str(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let s = rel.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

fn in_skipped_dir(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| {
            SKIPPED_DIRS
                .iter()
                .any(|s| c.as_os_str().to_str() == Some(s))
        })
}

fn parse_file(rel_path: String, bytes: &[u8]) -> RawFile {
    let cow = String::from_utf8_lossy(bytes);
    // A UTF-8 BOM must not confuse frontmatter detection or the first link.
    let text: &str = cow.strip_prefix('\u{feff}').unwrap_or(&cow);

    let (fm, body, warning) = split_frontmatter(text);
    let links = extract_links(body);
    let externals = extract_externals(body);
    RawFile {
        rel_path,
        title: fm.title,
        aliases: fm.aliases,
        links,
        externals,
        warning,
    }
}

/// Read one file and return its body with BOM and frontmatter stripped — the
/// same rules the scanner applies. Used by the viewer's detail pane, which
/// reads bodies on demand rather than holding them in memory.
pub fn read_body(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let cow = String::from_utf8_lossy(&bytes);
    let text: &str = cow.strip_prefix('\u{feff}').unwrap_or(&cow);
    let (_, body, _) = split_frontmatter(text);
    Ok(body.to_string())
}

/// Should a filesystem event at `p` trigger a vault reload? Hidden
/// components (.obsidian/.git/.text-graph churn — including our own state
/// saves) and skipped dirs (target/, node_modules/ — build churn) never
/// do; every other path does, since every visible file is a node now.
/// The watcher's one filter — a wrong `true` costs a pointless rebuild, a
/// wrong `false` costs a stale graph.
pub fn watch_relevant(root: &Path, p: &Path) -> bool {
    let rel = p.strip_prefix(root).unwrap_or(p);
    let hidden = rel
        .components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with('.')));
    !hidden && !in_skipped_dir(root, p)
}

/// Read at most `max_bytes` of a file as (lossy) text — for previewing
/// Asset files, which have no frontmatter semantics and can be huge (logs)
/// or binary. A truncated read ends mid-line; callers show it as-is.
pub fn read_head(path: &Path, max_bytes: u64) -> Result<String> {
    use std::io::Read as _;
    let f = std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(max_bytes)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Debug, Default)]
struct FmData {
    title: Option<String>,
    aliases: Vec<String>,
}

/// Split off `--- ... ---` frontmatter. Returns (data, body, warning).
///
/// No opener or no closing delimiter → the whole text is the body. Delimiters
/// present but YAML invalid → warning, body still parsed (links in the body
/// of a file with broken frontmatter must survive).
fn split_frontmatter(text: &str) -> (FmData, &str, Option<String>) {
    let Some((yaml, body)) = frontmatter_span(text) else {
        return (FmData::default(), text, None);
    };
    if yaml.trim().is_empty() {
        return (FmData::default(), body, None);
    }
    // Deserialize into a generic Value rather than a struct: real vaults have
    // numeric/date titles and arbitrary extra fields, none of which should
    // produce warnings.
    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml) {
        Ok(v) => {
            let title = v.get("title").and_then(yaml_str);
            let mut aliases = Vec::new();
            for key in ["aliases", "alias"] {
                match v.get(key) {
                    Some(serde_yaml_ng::Value::Sequence(items)) => {
                        aliases.extend(items.iter().filter_map(yaml_str));
                    }
                    Some(other) => aliases.extend(yaml_str(other)),
                    None => {}
                }
            }
            (FmData { title, aliases }, body, None)
        }
        Err(e) => (
            FmData::default(),
            body,
            Some(format!("invalid frontmatter: {e}")),
        ),
    }
}

/// Scalars usable as names: strings and numbers.
fn yaml_str(v: &serde_yaml_ng::Value) -> Option<String> {
    match v {
        serde_yaml_ng::Value::String(s) => Some(s.clone()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// If `text` opens with a `---` line, find the closing `---` line and return
/// (yaml, body). Tolerates CRLF.
fn frontmatter_span(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let yaml_start = text.len() - rest.len();
    let mut pos = yaml_start;
    for line in text[yaml_start..].split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            let yaml = &text[yaml_start..pos];
            let body_start = pos + line.len();
            return Some((yaml, &text[body_start..]));
        }
        pos += line.len();
    }
    None
}

/// Byte ranges of the body where wikilinks must not be recognized: fenced and
/// indented code blocks, inline code spans, and raw HTML.
///
/// Scanning the raw body and *excluding* these ranges avoids the alternative
/// of reassembling fragmented `Text` events (pulldown-cmark splits text at
/// `[` boundaries).
pub(crate) fn excluded_ranges(body: &str) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (event, range) in Parser::new(body).into_offset_iter() {
        match event {
            // The range of a Start event covers the entire element.
            Event::Start(Tag::CodeBlock(_)) => ranges.push(range),
            Event::Code(_) | Event::Html(_) | Event::InlineHtml(_) => ranges.push(range),
            _ => {}
        }
    }
    ranges
}

/// Scan `body` for `[[...]]` wikilinks. `![[embeds]]` and links inside
/// excluded ranges are dropped. Alias (`|`) and heading/block (`#`) suffixes
/// are stripped from the returned target.
/// Extract external http(s) URLs from a body — markdown link targets,
/// autolinks, and bare URLs alike — skipping code (fenced and inline).
/// Deduplicated by exact text (first occurrence's offset wins), document
/// order.
pub fn extract_externals(body: &str) -> Vec<RawExternal> {
    let excluded = excluded_ranges(body);
    let mut out: Vec<RawExternal> = Vec::new();
    let mut i = 0;
    while let Some(found) = body[i..].find("http") {
        let start = i + found;
        let rest = &body[start..];
        let scheme_len = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            i = start + 4;
            continue;
        };
        // URL runs until a delimiter that can't be part of it in markdown
        let end = rest
            .char_indices()
            .find(|(j, ch)| {
                *j >= scheme_len
                    && (ch.is_whitespace()
                        || matches!(ch, ')' | ']' | '>' | '"' | '\'' | '`' | '|'))
            })
            .map_or(rest.len(), |(j, _)| j);
        i = start + end;
        if excluded.iter().any(|r| r.contains(&start)) {
            continue;
        }
        // trailing sentence punctuation is prose, not URL
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        // `[text](url` — the author already titled this link
        let text = body[..start].strip_suffix("](").and_then(|before| {
            let open = before.rfind('[')?;
            let t = before[open + 1..].trim();
            (!t.is_empty() && !t.contains('\n') && t.len() <= 100).then(|| t.to_string())
        });
        if url.len() > scheme_len && !out.iter().any(|u| u.url == url) {
            out.push(RawExternal {
                url: url.to_string(),
                offset: start,
                text,
            });
        }
    }
    out
}

pub fn extract_links(body: &str) -> Vec<RawLink> {
    let excluded = excluded_ranges(body);
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(found) = body[i..].find("[[") {
        let start = i + found;
        let inner_start = start + 2;
        let Some(close) = body[inner_start..].find("]]") else {
            break;
        };
        let inner_end = inner_start + close;
        let inner = &body[inner_start..inner_end];

        // "[[a[[b]]" or "[[[note]]]" — treat the last `[[` before the close
        // as the real opener. Searching from start+1 (not inner_start)
        // catches openers that OVERLAP the first one (the "[[[" run), which
        // a search inside `inner` misses.
        if let Some(nested) = body[start + 1..inner_end].find("[[") {
            i = start + 1 + nested;
            continue;
        }
        i = inner_end + 2;

        if excluded.iter().any(|r| r.contains(&start)) {
            continue;
        }
        if start > 0 && bytes[start - 1] == b'!' {
            continue; // ![[embed]] — out of scope for v1
        }
        if inner.contains('\n') {
            continue; // wikilinks don't span lines
        }
        let target = inner
            .split('|')
            .next()
            .unwrap_or("") // strip alias
            .split('#')
            .next()
            .unwrap_or("") // strip heading / ^block
            .trim();
        if target.is_empty() {
            continue; // [[]] or [[#heading]] — empty / same-file reference
        }
        out.push(RawLink {
            target: target.to_string(),
            offset: start,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walker must PRUNE skipped dirs, not filter their files after
    /// the fact — proven by an unreadable child inside target/: descent
    /// would surface a "(walk)" permission error, pruning surfaces
    /// nothing. (Skips quietly as root, where nothing is unreadable.)
    #[test]
    fn skipped_dirs_are_never_descended() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = std::env::temp_dir().join(format!("tg-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("target/locked")).unwrap();
        std::fs::write(d.join("target/build.log"), "x").unwrap();
        std::fs::write(d.join("a.md"), "x").unwrap();
        std::fs::set_permissions(
            d.join("target/locked"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let scan = scan(&d).unwrap();
        let _ = std::fs::set_permissions(
            d.join("target/locked"),
            std::fs::Permissions::from_mode(0o755),
        );
        let _ = std::fs::remove_dir_all(&d);
        assert!(
            scan.errors.is_empty(),
            "a walk error from inside target/ proves descent: {:?}",
            scan.errors.first().map(|e| &e.message)
        );
        assert!(scan.assets.is_empty(), "target/ contents never surface");
        assert_eq!(scan.files.len(), 1);
    }

    /// Walk errors must come out sorted — readdir order is not stable
    /// across filesystems/runs, and errors print verbatim in stats and the
    /// diag window. (Skips quietly where permissions can't fail, e.g. root.)
    #[test]
    fn walk_errors_are_sorted_not_readdir_ordered() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = std::env::temp_dir().join(format!("tg-walkerr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for name in ["b-locked", "a-locked"] {
            let sub = d.join(name);
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let scan = scan(&d).unwrap();
        for name in ["b-locked", "a-locked"] {
            let _ = std::fs::set_permissions(d.join(name), std::fs::Permissions::from_mode(0o755));
        }
        let _ = std::fs::remove_dir_all(&d);
        if scan.errors.len() < 2 {
            return; // running as root — unreadable dirs read fine
        }
        let msgs: Vec<&String> = scan.errors.iter().map(|e| &e.message).collect();
        let mut sorted = msgs.clone();
        sorted.sort();
        assert_eq!(msgs, sorted, "error order must not depend on readdir");
    }

    #[test]
    fn watch_relevance_rules() {
        let root = Path::new("/v");
        let rel = |p: &str| watch_relevant(root, &root.join(p));
        assert!(rel("notes/a.md"), "markdown edits reload");
        assert!(rel("a.MD"), "case-insensitive extension");
        assert!(rel("newdir"), "extensionless = dir create/rename");
        assert!(rel("assets/pic.png"), "images are nodes — they reload");
        assert!(rel("assets/data.bin"), "every visible file is a node now");
        assert!(rel("src/main.rs"), "code files too");
        assert!(!rel("target/debug/foo.d"), "build churn must not reload");
        assert!(!rel("node_modules/x/index.js"), "dependency churn neither");
        assert!(
            !rel(".text-graph/view"),
            "our own state saves must not loop"
        );
        assert!(!rel(".text-graph/view.tmp"), "nor the temp file");
        assert!(!rel(".git/index.md"), "hidden dirs never do, even with .md");
        assert!(!rel("notes/.hidden.md"), "hidden files neither");
        // a path outside the root is judged on its own components
        assert!(!watch_relevant(root, Path::new("/elsewhere/.git/x.md")));
    }

    fn targets(body: &str) -> Vec<String> {
        extract_links(body).into_iter().map(|l| l.target).collect()
    }

    #[test]
    fn externals_from_md_links_autolinks_and_bare_urls() {
        let body = "see [docs](https://docs.rs/tmux) or <https://example.com/a>\n\
                    bare https://foo.bar/baz. and dup https://docs.rs/tmux\n\
                    ```\nhttps://in-code.example\n```\n\
                    not a url: httpx://nope http alone";
        let ex = extract_externals(body);
        let urls: Vec<&str> = ex.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://docs.rs/tmux",
                "https://example.com/a",
                "https://foo.bar/baz"
            ]
        );
        assert!(extract_externals("no links here").is_empty());
        assert!(
            extract_externals("`https://inline.example`").is_empty(),
            "inline code is excluded"
        );
        assert!(
            extract_externals("https://").is_empty(),
            "a bare scheme is not a URL"
        );
    }

    #[test]
    fn plain_alias_heading_block_path() {
        assert_eq!(
            targets("[[a]] [[b|alias]] [[c#h]] [[d#^blk]] [[dir/e]]"),
            ["a", "b", "c", "d", "dir/e"]
        );
    }

    #[test]
    fn fenced_code_is_not_a_link() {
        let body = "before\n\n```text\n[[trap]]\n```\n\nafter [[real]]\n";
        assert_eq!(targets(body), ["real"]);
    }

    #[test]
    fn indented_code_is_not_a_link() {
        let body = "para\n\n    [[trap]]\n\n[[real]]\n";
        assert_eq!(targets(body), ["real"]);
    }

    #[test]
    fn inline_code_is_not_a_link() {
        assert_eq!(targets("`[[trap]]` and [[real]]"), ["real"]);
    }

    #[test]
    fn embeds_are_skipped() {
        assert_eq!(targets("![[img.png]] and [[real]]"), ["real"]);
    }

    #[test]
    fn empty_and_heading_only_are_skipped() {
        assert_eq!(targets("[[]] [[#heading]] [[  ]]"), Vec::<String>::new());
    }

    #[test]
    fn bracket_runs_take_innermost_opener() {
        // regression: "[[[note]]]" used to extract the garbage target "[note"
        assert_eq!(targets("[[[note]]]"), ["note"]);
        assert_eq!(targets("[[[[x]]"), ["x"]);
        assert_eq!(targets("a [[[b]] c"), ["b"]);
        assert_eq!(targets("[[a[[b]]"), ["b"]);
    }

    #[test]
    fn frontmatter_valid() {
        let (fm, body, w) = split_frontmatter("---\ntitle: Hi\n---\nbody");
        assert_eq!(fm.title.as_deref(), Some("Hi"));
        assert_eq!(body, "body");
        assert!(w.is_none());
    }

    #[test]
    fn frontmatter_crlf() {
        let (fm, body, w) = split_frontmatter("---\r\ntitle: Hi\r\n---\r\nbody");
        assert_eq!(fm.title.as_deref(), Some("Hi"));
        assert_eq!(body, "body");
        assert!(w.is_none());
    }

    #[test]
    fn frontmatter_numeric_title_is_fine() {
        let (fm, _, w) = split_frontmatter("---\ntitle: 42\n---\n");
        assert_eq!(fm.title.as_deref(), Some("42"));
        assert!(w.is_none());
    }

    #[test]
    fn frontmatter_aliases_list_and_singular() {
        let (fm, _, w) = split_frontmatter("---\naliases: [SAE, Sparse Autoencoder]\n---\n");
        assert!(w.is_none());
        assert_eq!(fm.aliases, ["SAE", "Sparse Autoencoder"]);
        let (fm, _, _) = split_frontmatter("---\nalias: One\n---\n");
        assert_eq!(fm.aliases, ["One"]);
    }

    #[test]
    fn frontmatter_garbage_warns_but_body_survives() {
        let (fm, body, w) = split_frontmatter("---\ntitle: [broken\n---\n[[x]]");
        assert!(fm.title.is_none());
        assert!(w.is_some());
        assert_eq!(extract_links(body).len(), 1);
    }

    #[test]
    fn no_closing_delimiter_means_no_frontmatter() {
        let (fm, body, w) = split_frontmatter("---\ntitle: Hi\nno close");
        assert!(fm.title.is_none() && w.is_none());
        assert!(body.starts_with("---"));
    }
}
