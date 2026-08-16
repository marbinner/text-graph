//! Vault scanning: walk a directory (every visible file becomes a leaf —
//! markdown parsed, images and other assets as paths), parse frontmatter,
//! and extract wikilink targets (still unresolved strings).
//!
//! No global state — every file is parsed independently. Resolution of the
//! extracted targets happens in [`crate::resolve`].

use std::collections::HashMap;
use std::io::Read as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pulldown_cmark::{Event, Parser, Tag};

/// One markdown file, parsed but with link targets still unresolved.
#[derive(Debug)]
pub struct RawFile {
    /// Human-readable path relative to the vault root, with forward slashes.
    pub rel_path: String,
    /// Lossless OS path relative to the vault root.
    pub os_path: PathBuf,
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
pub struct RawPath {
    /// Human-readable, potentially lossy representation for labels/search.
    pub rel_path: String,
    /// Lossless path used for identity and every filesystem operation.
    pub os_path: PathBuf,
}

impl From<&str> for RawPath {
    fn from(path: &str) -> Self {
        Self {
            rel_path: path.to_string(),
            os_path: PathBuf::from(path),
        }
    }
}

impl From<String> for RawPath {
    fn from(path: String) -> Self {
        Self {
            os_path: PathBuf::from(&path),
            rel_path: path,
        }
    }
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
    pub images: Vec<RawPath>,
    /// Every other visible file (rel paths, sorted) — code, config, data,
    /// binaries. Not parsed; they become Asset nodes.
    pub assets: Vec<RawPath>,
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

/// Whether a directory name is deliberately outside the graph. Creation
/// uses the same predicate so it cannot successfully write an invisible
/// note below a subtree the scanner will always prune.
pub(crate) fn is_skipped_dir_name(name: &str) -> bool {
    SKIPPED_DIRS.contains(&name)
}

/// Raster formats the scan turns into Image nodes. SVG is excluded — the
/// viewer has no vector rasterizer.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// Markdown is parsed rather than retained, but an unbounded `fs::read`
/// still lets one planted/sparse file allocate the process away. Large notes
/// remain nodes; links and metadata are taken from this bounded prefix.
const MAX_MARKDOWN_SCAN_BYTES: usize = 8 * 1024 * 1024;
/// Rendered Markdown can be substantially more expensive than its source.
/// The source-mode preview already has its own smaller cap in the app.
const MAX_MARKDOWN_PREVIEW_BYTES: usize = 1024 * 1024;

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
    let mut paths: Vec<(RawPath, PathBuf)> = Vec::new();
    let mut images: Vec<RawPath> = Vec::new();
    let mut assets: Vec<RawPath> = Vec::new();
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
                && e.file_name().to_str().is_some_and(is_skipped_dir_name))
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
        let os_path = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
        let raw = RawPath {
            rel_path: display_rel_path(&os_path),
            os_path,
        };
        if ext.is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            paths.push((raw, path));
        } else if ext.is_some_and(image_ext) {
            images.push(raw);
        } else {
            assets.push(raw);
        }
    }

    // Sort by the relative-path STRING: this is both what makes runs
    // deterministic and the byte order that "first in sorted path order"
    // means everywhere else (docs, ambiguity resolution). PathBuf's
    // component-wise order differs around '/' vs '.'/'-' in dirnames.
    let path_order = |a: &RawPath, b: &RawPath| {
        a.rel_path
            .cmp(&b.rel_path)
            .then_with(|| path_key(&a.os_path).cmp(&path_key(&b.os_path)))
    };
    paths.sort_by(|(a, _), (b, _)| path_order(a, b));
    images.sort_by(&path_order);
    assets.sort_by(&path_order);
    // walk errors too: the walker yields them in raw readdir order, and
    // g.errors flows verbatim into stats output and the diag window (read
    // errors, appended below, already follow sorted path order)
    errors.sort_by(|a, b| (&a.rel_path, &a.message).cmp(&(&b.rel_path, &b.message)));

    let mut files = Vec::with_capacity(paths.len());
    for (rel, path) in paths {
        match read_prefix(&path, MAX_MARKDOWN_SCAN_BYTES) {
            Ok((bytes, truncated)) => files.push(parse_file(rel, &bytes, truncated)),
            Err(e) => errors.push(ScanError {
                rel_path: rel.rel_path,
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

const PATH_KEY_PREFIX: &str = "~text-graph-raw~";

/// Human-readable relative path used in labels and fuzzy matching. Invalid
/// Unicode is replaced for display only; [`path_key`] carries identity.
pub fn display_rel_path(path: &Path) -> String {
    path.iter()
        .map(|component| component.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Stable, collision-free string identity for an OS-relative path. Ordinary
/// UTF-8 components stay unchanged for state-file compatibility. Components
/// with invalid Unicode, plus literal names beginning with the reserved
/// prefix, are represented by that prefix followed by their exact OS units.
pub fn path_key(path: &Path) -> String {
    path.iter()
        .map(|component| {
            if let Some(text) = component.to_str()
                && !text.starts_with(PATH_KEY_PREFIX)
            {
                return text.to_string();
            }
            encoded_component_key(component)
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn encoded_component_key(component: &std::ffi::OsStr) -> String {
    use std::fmt::Write as _;

    let mut key = String::from(PATH_KEY_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        for byte in component.as_bytes() {
            write!(&mut key, "{byte:02x}").expect("writing to a String cannot fail");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        for unit in component.encode_wide() {
            write!(&mut key, "{unit:04x}").expect("writing to a String cannot fail");
        }
    }
    #[cfg(not(any(unix, windows)))]
    for byte in component.as_encoded_bytes() {
        write!(&mut key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    key
}

fn in_skipped_dir(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .parent()
        .is_some_and(|parent| {
            parent
                .components()
                .any(|c| c.as_os_str().to_str().is_some_and(is_skipped_dir_name))
        })
}

fn parse_file(path: RawPath, bytes: &[u8], truncated: bool) -> RawFile {
    let cow = String::from_utf8_lossy(bytes);
    // A UTF-8 BOM must not confuse frontmatter detection or the first link.
    let text: &str = cow.strip_prefix('\u{feff}').unwrap_or(&cow);

    let (fm, body, mut warning) = split_frontmatter(text);
    if truncated {
        let truncation = format!(
            "file exceeds {}; scanned only the prefix",
            limit_label(MAX_MARKDOWN_SCAN_BYTES)
        );
        warning = Some(match warning {
            Some(existing) => format!("{existing}; {truncation}"),
            None => truncation,
        });
    }
    let links = extract_links(body);
    let externals = extract_externals(body);
    RawFile {
        rel_path: path.rel_path,
        os_path: path.os_path,
        title: fm.title,
        aliases: fm.aliases,
        links,
        externals,
        warning,
    }
}

/// Read one file and return its body with BOM and frontmatter stripped — the
/// same rules the scanner applies. Rendered previews are bounded: a note is a
/// glance here, and Markdown expansion makes huge inputs particularly costly.
pub fn read_body(path: &Path) -> Result<String> {
    read_body_limited(path, MAX_MARKDOWN_PREVIEW_BYTES)
}

fn read_body_limited(path: &Path, max_bytes: usize) -> Result<String> {
    let (bytes, truncated) = read_prefix(path, max_bytes)?;
    let cow = String::from_utf8_lossy(&bytes);
    let text: &str = cow.strip_prefix('\u{feff}').unwrap_or(&cow);
    let (_, body, _) = split_frontmatter(text);
    let mut body = body.to_string();
    if truncated {
        body.push_str(&format!(
            "\n\n> **Preview truncated after {}.**\n",
            limit_label(max_bytes)
        ));
    }
    Ok(body)
}

/// Read at most `max_bytes + 1`, using the extra byte only to detect
/// truncation. The returned buffer never exceeds the requested bound.
fn read_prefix(path: &Path, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let truncated = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    Ok((bytes, truncated))
}

fn limit_label(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
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
    let skipped_leaf_dir = p.is_dir()
        && p.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_skipped_dir_name);
    !hidden && !in_skipped_dir(root, p) && !skipped_leaf_dir
}

/// Read at most `max_bytes` of a file as (lossy) text — for previewing
/// Asset files, which have no frontmatter semantics and can be huge (logs)
/// or binary. A truncated read ends mid-line; callers show it as-is.
pub fn read_head(path: &Path, max_bytes: u64) -> Result<String> {
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
/// Deduplicated by exact text in document order: the first occurrence's
/// offset wins, while the first authored label is retained even if it appears
/// on a later duplicate.
pub fn extract_externals(body: &str) -> Vec<RawExternal> {
    let excluded = excluded_ranges(body);
    let mut out: Vec<RawExternal> = Vec::new();
    // Keys borrow from `body`, so deduplication stays linear without keeping
    // a second owned copy of every URL.
    let mut seen: HashMap<&str, usize> = HashMap::new();
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
        if url.len() > scheme_len {
            if let Some(&index) = seen.get(url) {
                if out[index].text.is_none() {
                    out[index].text = text;
                }
            } else {
                seen.insert(url, out.len());
                out.push(RawExternal {
                    url: url.to_string(),
                    offset: start,
                    text,
                });
            }
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

    #[test]
    fn markdown_reads_are_bounded_and_report_truncation() {
        let d = std::env::temp_dir().join(format!("tg-md-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("large.md");
        std::fs::write(&path, b"[[front]] and content beyond the cap").unwrap();

        let (prefix, truncated) = read_prefix(&path, 12).unwrap();
        assert_eq!(prefix, b"[[front]] an");
        assert!(truncated);
        let parsed = parse_file("large.md".into(), &prefix, truncated);
        assert!(
            parsed
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("scanned only the prefix"))
        );

        let body = read_body_limited(&path, 12).unwrap();
        assert!(body.starts_with("[[front]] an"));
        assert!(body.contains("Preview truncated after 12 bytes"));
        assert!(
            body.len() < 128,
            "the truncation notice must not hide an unbounded read"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn exact_limit_markdown_is_not_reported_as_truncated() {
        let d = std::env::temp_dir().join(format!("tg-md-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("exact.md");
        std::fs::write(&path, b"12345678").unwrap();
        let (bytes, truncated) = read_prefix(&path, 8).unwrap();
        assert_eq!(bytes, b"12345678");
        assert!(!truncated);
        assert_eq!(read_body_limited(&path, 8).unwrap(), "12345678");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The walker must PRUNE skipped dirs, not filter their files after
    /// the fact — proven by an unreadable child inside target/: descent
    /// would surface a "(walk)" permission error, pruning surfaces
    /// nothing. (Skips quietly as root, where nothing is unreadable.)
    #[cfg(unix)]
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

    #[test]
    fn skipped_names_are_allowed_for_files_but_not_ancestor_directories() {
        let d = std::env::temp_dir().join(format!("tg-skip-leaf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("target-dir/target")).unwrap();
        std::fs::write(d.join("target"), "an extensionless asset").unwrap();
        std::fs::write(d.join("target-dir/target/hidden.md"), "# hidden").unwrap();

        let scanned = scan(&d).unwrap();
        assert_eq!(
            scanned
                .assets
                .iter()
                .map(|path| path.rel_path.as_str())
                .collect::<Vec<_>>(),
            ["target"]
        );
        assert!(scanned.files.is_empty());
        assert!(watch_relevant(&d, &d.join("target")));

        let _ = std::fs::remove_file(d.join("target"));
        std::fs::create_dir(d.join("target")).unwrap();
        assert!(!watch_relevant(&d, &d.join("target")));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Walk errors must come out sorted — readdir order is not stable
    /// across filesystems/runs, and errors print verbatim in stats and the
    /// diag window. (Skips quietly where permissions can't fail, e.g. root.)
    #[cfg(unix)]
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
    fn later_duplicate_url_contributes_its_first_authored_label() {
        let body = concat!(
            "bare https://example.com/page then ",
            "[Great source](https://example.com/page) and ",
            "[Later name](https://example.com/page)"
        );
        let externals = extract_externals(body);

        assert_eq!(externals.len(), 1);
        assert_eq!(externals[0].offset, body.find("https://").unwrap());
        assert_eq!(externals[0].text.as_deref(), Some("Great source"));
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

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_keys_are_collision_free_and_do_not_claim_reserved_names() {
        use std::os::unix::ffi::OsStringExt as _;

        let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![0x80]));
        let reserved = PathBuf::from("~text-graph-raw~80");
        assert_eq!(path_key(&invalid), "~text-graph-raw~80");
        assert_ne!(
            path_key(&invalid),
            path_key(&reserved),
            "a literal reserved-prefix name must be escaped too"
        );
        assert_eq!(path_key(Path::new("docs/note.md")), "docs/note.md");
    }

    #[cfg(unix)]
    #[test]
    fn scan_keeps_lossy_equal_unix_names_as_distinct_paths() {
        use std::os::unix::ffi::OsStringExt as _;

        let root = std::env::temp_dir().join(format!(
            "tg-vault-raw-paths-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        for (name, body) in [
            (b"note-\x80.md".to_vec(), "first"),
            (b"note-\x81.md".to_vec(), "second"),
        ] {
            let name = std::ffi::OsString::from_vec(name);
            std::fs::write(root.join(name), body).unwrap();
        }

        let scan = scan(&root).unwrap();
        assert_eq!(scan.files.len(), 2);
        assert_eq!(scan.files[0].rel_path, scan.files[1].rel_path);
        assert_ne!(
            path_key(&scan.files[0].os_path),
            path_key(&scan.files[1].os_path)
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
