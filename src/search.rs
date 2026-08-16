//! The matching engine behind the picker: fuzzy name/path scoring, literal
//! content scanning, and the ranked row list both produce.
//!
//! Two tiers with deliberately different semantics:
//!
//! - names, aliases and paths are FUZZY (nucleo subsequence scoring), so
//!   `apbn` finds `agent-protocol-benchmark.md`;
//! - file CONTENT is LITERAL: every whitespace-separated term must appear
//!   on the SAME line, smart-cased. Subsequence matching over every line of
//!   a vault matches nearly everything (and costs far more per keystroke) —
//!   the same split ripgrep-backed pickers make.
//!
//! Content is never indexed. [`scan_files`] streams files from disk per
//! query so bodies are never held whole in memory (the architecture rule),
//! and no index can go stale under agents that rewrite notes constantly.
//! Repeat scans are cheap because a query that only grew can reuse the
//! previous scan's *file* set — see [`Query::narrows`].
//!
//! Everything here is deterministic: same vault + same query = same rows,
//! in the same order.

use std::path::Path;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use crate::vault;

/// Files larger than this are skipped by the content scan (a vendored
/// bundle or a 50MB log is never what you are looking for by line).
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Matching lines counted per file; beyond it the count reads "200+".
pub const MAX_LINES_PER_FILE: usize = 200;
/// Files with content hits per scan. Hitting this marks the scan
/// truncated, which also disables narrowing (the file set is incomplete).
pub const MAX_FILES_WITH_HITS: usize = 2000;
/// Bytes of a single line considered for matching and shown as a snippet —
/// minified JSON is one very long line.
pub const MAX_LINE_SCAN: usize = 4096;
const SNIPPET_MAX: usize = 320;
/// Files per emitted batch, so results stream into the list as they land.
const BATCH: usize = 24;
/// Cancellation is checked every this many files (a stat+read each, so the
/// check is far cheaper than the work between two of them).
const CANCEL_EVERY: usize = 8;

/// Why a row matched. Rows sort by this FIRST: what you named beats what
/// merely contains the words.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    Name,
    Alias,
    Path,
    Pane,
    Content,
}

impl Class {
    pub fn label(self) -> &'static str {
        match self {
            Class::Name => "name",
            Class::Alias => "alias",
            Class::Path => "path",
            Class::Pane => "terminal",
            Class::Content => "content",
        }
    }
}

/// A parsed query: whitespace-separated terms plus the smart-case verdict
/// (any uppercase anywhere makes the whole query case-sensitive).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub raw: String,
    /// Terms as compared: ASCII-lowercased unless the query is sensitive.
    pub terms: Vec<String>,
    pub case_sensitive: bool,
}

/// A term's byte range within the line/field it matched.
pub type Range = (usize, usize);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMatch {
    pub score: u32,
    pub ranges: Vec<Range>,
}

impl Query {
    pub fn parse(raw: &str) -> Query {
        let case_sensitive = raw.chars().any(char::is_uppercase);
        let terms = raw
            .split_whitespace()
            .map(|t| {
                if case_sensitive {
                    t.to_string()
                } else {
                    t.to_ascii_lowercase()
                }
            })
            .collect();
        Query {
            raw: raw.to_string(),
            terms,
            case_sensitive,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// May a scan for this query reuse the set of FILES that matched
    /// `prev`? Only when this query's raw text merely GREW: appending can
    /// extend the last term or add a new one, never relax an existing one,
    /// so every line matching this query also matched `prev` — and hence
    /// its file did too. (Appending can also flip smart case from
    /// insensitive to sensitive, which is likewise a narrowing.) The caller
    /// must additionally know `prev`'s scan finished and was not truncated.
    pub fn narrows(&self, prev: &Query) -> bool {
        !prev.is_empty() && self.raw.starts_with(&prev.raw)
    }

    /// Does every term occur in `line`? Returns their first occurrences as
    /// byte ranges into `line` (sorted) with a rank score. `buf` is a
    /// scratch buffer reused across lines: ASCII-lowercasing preserves byte
    /// length, so ranges stay valid in the original line. (The fold is
    /// ASCII-only — an uppercase non-ASCII letter in a FILE will not match
    /// its lowercase form in the query.)
    pub fn match_line(&self, line: &str, buf: &mut String) -> Option<LineMatch> {
        if self.terms.is_empty() {
            return None;
        }
        let scan = &line[..floor_boundary(line, MAX_LINE_SCAN)];
        let hay: &str = if self.case_sensitive {
            scan
        } else {
            buf.clear();
            buf.push_str(scan);
            buf.make_ascii_lowercase();
            buf.as_str()
        };
        let mut ranges: Vec<Range> = Vec::with_capacity(self.terms.len());
        for t in &self.terms {
            let at = hay.find(t.as_str())?;
            ranges.push((at, at + t.len()));
        }
        ranges.sort_unstable();
        let first = ranges[0].0;
        let mut score = 1_000u32.saturating_sub(first.min(600) as u32);
        if ranges.iter().all(|&(s, _)| word_start(scan, s)) {
            score += 300;
        }
        // a hit in a heading beats the same hit inside a wall of text
        score += 100u32.saturating_sub((scan.len() / 8).min(100) as u32);
        Some(LineMatch { score, ranges })
    }
}

/// Is byte offset `at` the start of a word in `s`?
fn word_start(s: &str, at: usize) -> bool {
    if at == 0 {
        return true;
    }
    s[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_')
}

/// Largest char boundary of `s` at or below `max`.
fn floor_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// One matching line, ready to render: the text is trimmed and capped, and
/// `ranges` index into that trimmed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineHit {
    /// 1-based line number in the file as it is on disk (frontmatter
    /// included — the number an editor's `+N` expects).
    pub line: usize,
    pub text: String,
    pub ranges: Vec<Range>,
    pub score: u32,
}

/// A file's content hits, collapsed to its best line plus a count — the
/// picker shows one row per node, not one per line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHits {
    /// Vault-relative path of the file. Hits are identified by PATH, not
    /// by node index: every vault reload renumbers the node arena, and a
    /// search must survive the agent that saves a file while you type.
    pub rel: String,
    pub best: LineHit,
    pub total: usize,
    /// `total` stopped at [`MAX_LINES_PER_FILE`].
    pub capped: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Stopped early because a newer query superseded this one.
    pub cancelled: bool,
    /// Stopped at [`MAX_FILES_WITH_HITS`] — the file set is incomplete, so
    /// the next query must not narrow against it.
    pub truncated: bool,
    pub files_read: usize,
}

/// Collapse one file's matching lines into its best line and a count.
pub fn file_hits(rel: &str, text: &str, query: &Query, buf: &mut String) -> Option<FileHits> {
    let mut best: Option<LineHit> = None;
    let mut total = 0usize;
    for (i, line) in text.lines().enumerate() {
        let Some(m) = query.match_line(line, buf) else {
            continue;
        };
        total += 1;
        // first line wins ties, so the pick never depends on iteration luck
        if best.as_ref().is_none_or(|b| m.score > b.score) {
            best = Some(snippet(i + 1, line, m));
        }
        if total >= MAX_LINES_PER_FILE {
            return best.map(|best| FileHits {
                rel: rel.to_string(),
                best,
                total,
                capped: true,
            });
        }
    }
    best.map(|best| FileHits {
        rel: rel.to_string(),
        best,
        total,
        capped: false,
    })
}

/// Trim and cap a matched line for display, moving its ranges with it.
fn snippet(line_no: usize, line: &str, m: LineMatch) -> LineHit {
    let lead = line.len() - line.trim_start().len();
    let end = floor_boundary(line.trim_start(), SNIPPET_MAX);
    let text = &line.trim_start()[..end];
    let ranges = m
        .ranges
        .iter()
        .filter(|&&(s, e)| s >= lead && e <= lead + end)
        .map(|&(s, e)| (s - lead, e - lead))
        .collect();
    LineHit {
        line: line_no,
        text: text.to_string(),
        ranges,
        score: m.score,
    }
}

/// Stream `files` from disk, matching each against `query`, emitting hits
/// in batches as they are found (the picker's list fills progressively).
///
/// `cancelled` is polled between files: a superseded query stops mid-scan
/// instead of finishing work nobody will look at. Results are emitted in
/// the order `files` are given, so a caller that passes a sorted list gets
/// a deterministic scan.
pub fn scan_files(
    root: &Path,
    query: &Query,
    files: &[String],
    cancelled: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(Vec<FileHits>),
) -> ScanOutcome {
    let mut out = ScanOutcome::default();
    if query.is_empty() {
        return out;
    }
    let mut batch: Vec<FileHits> = Vec::new();
    let mut with_hits = 0usize;
    let mut buf = String::new();
    for (i, f) in files.iter().enumerate() {
        if i.is_multiple_of(CANCEL_EVERY) && cancelled() {
            out.cancelled = true;
            return out;
        }
        let path = root.join(f);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(text) = vault::read_head(&path, MAX_FILE_BYTES) else {
            continue;
        };
        out.files_read += 1;
        if let Some(hits) = file_hits(f, &text, query, &mut buf) {
            batch.push(hits);
            with_hits += 1;
            if with_hits >= MAX_FILES_WITH_HITS {
                out.truncated = true;
                break;
            }
        }
        if batch.len() >= BATCH {
            emit(std::mem::take(&mut batch));
        }
    }
    if !batch.is_empty() {
        emit(batch);
    }
    out
}

/// The name-ish fields of a node, in class order.
pub struct Names<'a> {
    pub display: &'a str,
    pub aliases: &'a [String],
    pub path: &'a str,
}

/// A fuzzy hit on one of those fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameHit {
    pub class: Class,
    pub score: u32,
    /// The field that matched, for display.
    pub field: String,
    /// Matched character positions as byte ranges into `field`.
    pub ranges: Vec<Range>,
}

pub fn pattern(raw: &str) -> Pattern {
    Pattern::parse(raw, CaseMatching::Smart, Normalization::Smart)
}

/// Score a node's name fields, taking the FIRST class that matches: what a
/// note is called outranks where it happens to live. Fields are scored
/// separately (not as one concatenated haystack) so a subsequence can never
/// straddle a name and a path and manufacture a junk match.
pub fn score_names(pat: &Pattern, matcher: &mut Matcher, n: Names<'_>) -> Option<NameHit> {
    let mut buf = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut hit = |class: Class, field: &str, matcher: &mut Matcher| -> Option<NameHit> {
        idx.clear();
        let score = pat.indices(Utf32Str::new(field, &mut buf), matcher, &mut idx)?;
        idx.sort_unstable();
        idx.dedup();
        Some(NameHit {
            class,
            score,
            field: field.to_string(),
            ranges: char_ranges(field, &idx),
        })
    };
    if let Some(h) = hit(Class::Name, n.display, matcher) {
        return Some(h);
    }
    let alias = n
        .aliases
        .iter()
        .filter_map(|a| hit(Class::Alias, a, matcher))
        .max_by_key(|h| h.score);
    if let Some(h) = alias {
        return Some(h);
    }
    if n.path.is_empty() {
        return None;
    }
    hit(Class::Path, n.path, matcher)
}

/// Matched CHAR positions → byte ranges in `field`, merging runs so
/// highlighting paints one span per contiguous match.
fn char_ranges(field: &str, chars: &[u32]) -> Vec<Range> {
    let mut out: Vec<Range> = Vec::new();
    let bytes: Vec<usize> = field.char_indices().map(|(b, _)| b).collect();
    for &c in chars {
        let Some(&start) = bytes.get(c as usize) else {
            continue;
        };
        let end = bytes.get(c as usize + 1).copied().unwrap_or(field.len());
        match out.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    out
}

/// What a row points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Index into the node arena of the graph the rows were built from.
    Node(u32),
    Pane {
        session: String,
        pane: String,
    },
}

/// One entry in the picker's list: at most one per node, carrying whatever
/// made it match.
#[derive(Clone, Debug)]
pub struct Row {
    pub target: Target,
    pub class: Class,
    pub score: u32,
    pub title: String,
    pub title_ranges: Vec<Range>,
    /// Where the row lives (a path, a pane's cwd) — and, when the match was
    /// on an alias or a path rather than the name, what matched.
    pub subtitle: String,
    pub subtitle_ranges: Vec<Range>,
    /// The matched line, for content and terminal rows.
    pub snippet: Option<LineHit>,
    /// Further matching lines in the same file, beyond the one shown.
    pub more: usize,
    /// Stable identity across rebuilds — node indices are renumbered by
    /// every reload, so the cursor rides this instead of an index.
    pub key: String,
}

/// Rank rows: class first (a name beats a mention), then score, then key —
/// never insertion order, so results are stable while a scan streams in.
pub fn rank(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        a.class
            .cmp(&b.class)
            .then(b.score.cmp(&a.score))
            .then(a.key.cmp(&b.key))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Query {
        Query::parse(s)
    }

    #[test]
    fn smart_case_follows_the_query() {
        assert!(!q("foo bar").case_sensitive);
        assert_eq!(q("Foo").terms, vec!["Foo"]);
        assert!(q("Foo").case_sensitive);
        assert_eq!(q("FOO bar").terms, vec!["FOO", "bar"]);
        assert_eq!(q("  spaced   out ").terms, vec!["spaced", "out"]);
        assert!(q("   ").is_empty());
    }

    #[test]
    fn every_term_must_share_the_line() {
        let mut buf = String::new();
        let m = q("agent tmux").match_line("the agent talks to tmux", &mut buf);
        let m = m.expect("both terms present");
        assert_eq!(m.ranges, vec![(4, 9), (19, 23)]);
        assert!(
            q("agent missing")
                .match_line("the agent talks to tmux", &mut buf)
                .is_none()
        );
        // case-insensitive by default, sensitive once the query shouts
        assert!(q("AGENT").match_line("the agent", &mut buf).is_none());
        assert!(q("agent").match_line("the AGENT", &mut buf).is_some());
    }

    #[test]
    fn ranges_are_byte_offsets_into_the_original_line() {
        let mut buf = String::new();
        let line = "héllo WORLD"; // é is 2 bytes: byte offsets ≠ char offsets
        let m = q("world").match_line(line, &mut buf).expect("matches");
        let (s, e) = m.ranges[0];
        assert_eq!(&line[s..e], "WORLD");
    }

    #[test]
    fn earlier_word_start_hits_score_higher() {
        let mut buf = String::new();
        let early = q("tmux").match_line("tmux control mode", &mut buf).unwrap();
        let late = q("tmux")
            .match_line("a long sentence that eventually says tmux", &mut buf)
            .unwrap();
        assert!(early.score > late.score);
        let mid_word = q("mux").match_line("tmux", &mut buf).unwrap();
        let at_start = q("mux").match_line("mux", &mut buf).unwrap();
        assert!(at_start.score > mid_word.score, "word starts win");
    }

    #[test]
    fn long_lines_are_capped_without_panicking() {
        let mut buf = String::new();
        let mut line = "x".repeat(MAX_LINE_SCAN * 2);
        line.push_str("needle");
        assert!(
            q("needle").match_line(&line, &mut buf).is_none(),
            "past the scan cap"
        );
        // a multibyte char straddling the cap must not split mid-char
        let wide = "é".repeat(MAX_LINE_SCAN);
        assert!(q("zz").match_line(&wide, &mut buf).is_none());
    }

    #[test]
    fn narrowing_only_when_the_query_grew() {
        assert!(q("agent p").narrows(&q("agent")));
        assert!(q("agentX").narrows(&q("agent")), "case escalation narrows");
        assert!(!q("agen").narrows(&q("agent")), "a backspace widens");
        assert!(!q("other").narrows(&q("agent")));
        assert!(!q("agent").narrows(&q("")), "nothing to narrow from");
    }

    #[test]
    fn file_hits_keeps_the_best_line_and_counts_the_rest() {
        let mut buf = String::new();
        let text = "intro\ntmux here\nunrelated\nand tmux again\n";
        let h = file_hits("notes/a.md", text, &q("tmux"), &mut buf).expect("hits");
        assert_eq!(h.rel, "notes/a.md");
        assert_eq!(h.total, 2);
        assert!(!h.capped);
        assert_eq!(h.best.line, 2, "1-based, frontmatter included");
        assert_eq!(h.best.text, "tmux here");
        assert!(file_hits("a.md", text, &q("absent"), &mut buf).is_none());
    }

    #[test]
    fn per_file_line_count_is_capped() {
        let mut buf = String::new();
        let text = "hit\n".repeat(MAX_LINES_PER_FILE + 50);
        let h = file_hits("a.md", &text, &q("hit"), &mut buf).expect("hits");
        assert_eq!(h.total, MAX_LINES_PER_FILE);
        assert!(h.capped);
    }

    #[test]
    fn snippets_trim_indentation_and_keep_ranges_aligned() {
        let mut buf = String::new();
        let text = "        deeply indented tmux line\n";
        let h = file_hits("a.md", text, &q("tmux"), &mut buf).expect("hits");
        assert_eq!(h.best.text, "deeply indented tmux line");
        let (s, e) = h.best.ranges[0];
        assert_eq!(&h.best.text[s..e], "tmux");
    }

    fn fixture_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault")
    }

    fn fixture_files() -> Vec<String> {
        let scan = vault::scan(&fixture_root()).expect("fixture scans");
        let mut files: Vec<String> = scan.files.iter().map(|f| f.rel_path.clone()).collect();
        files.sort();
        files
    }

    #[test]
    fn scanning_the_fixture_vault_is_deterministic() {
        let files = fixture_files();
        let run = || {
            let mut got: Vec<FileHits> = Vec::new();
            let out = scan_files(
                &fixture_root(),
                &q("heading"),
                &files,
                &|| false,
                &mut |b| got.extend(b),
            );
            assert!(!out.cancelled && !out.truncated);
            got
        };
        let a = run();
        let b = run();
        assert!(!a.is_empty(), "the fixture vault has headings");
        assert_eq!(a, b, "same vault + same query = same hits, same order");
        let paths: Vec<&str> = a.iter().map(|h| h.rel.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "emitted in the order the files were given");
    }

    #[test]
    fn a_superseded_scan_stops_early() {
        let files = fixture_files();
        let mut got = Vec::new();
        let out = scan_files(&fixture_root(), &q("e"), &files, &|| true, &mut |b: Vec<
            FileHits,
        >| {
            got.extend(b)
        });
        assert!(out.cancelled);
        assert_eq!(out.files_read, 0, "cancelled before the first read");
        assert!(got.is_empty());
    }

    #[test]
    fn an_empty_query_scans_nothing() {
        let out = scan_files(
            &fixture_root(),
            &q("  "),
            &fixture_files(),
            &|| false,
            &mut |_| panic!("must not emit"),
        );
        assert_eq!(out, ScanOutcome::default());
    }

    #[test]
    fn name_fields_are_scored_in_class_order() {
        let mut m = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let aliases = vec!["Second Brain".to_string()];
        let n = Names {
            display: "rust-app",
            aliases: &aliases,
            path: "projects/rust-app.md",
        };
        let hit = score_names(&pattern("rustapp"), &mut m, n).expect("name matches");
        assert_eq!(hit.class, Class::Name);
        assert_eq!(hit.field, "rust-app");

        let n = Names {
            display: "rust-app",
            aliases: &aliases,
            path: "projects/rust-app.md",
        };
        let hit = score_names(&pattern("projects/"), &mut m, n).expect("path matches");
        assert_eq!(hit.class, Class::Path, "no name match falls through");

        let n = Names {
            display: "20260815",
            aliases: &aliases,
            path: "notes/20260815.md",
        };
        let hit = score_names(&pattern("second brain"), &mut m, n).expect("alias matches");
        assert_eq!(hit.class, Class::Alias);
        assert_eq!(hit.field, "Second Brain");
    }

    #[test]
    fn name_highlight_ranges_survive_multibyte_names() {
        let mut m = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let n = Names {
            display: "grafér",
            aliases: &[],
            path: "topics/grafér.md",
        };
        let hit = score_names(&pattern("gr"), &mut m, n).expect("matches");
        for &(s, e) in &hit.ranges {
            assert!(hit.field.is_char_boundary(s) && hit.field.is_char_boundary(e));
        }
        let lit: String = hit.ranges.iter().map(|&(s, e)| &hit.field[s..e]).collect();
        assert_eq!(lit, "gr");
    }

    #[test]
    fn ranking_puts_names_first_then_score_then_key() {
        let row = |class, score, key: &str| Row {
            target: Target::Node(0),
            class,
            score,
            title: key.to_string(),
            title_ranges: vec![],
            subtitle: String::new(),
            subtitle_ranges: vec![],
            snippet: None,
            more: 0,
            key: key.to_string(),
        };
        let mut rows = vec![
            row(Class::Content, 900, "c-high"),
            row(Class::Name, 10, "n-low"),
            row(Class::Content, 900, "a-high"),
            row(Class::Path, 500, "p"),
        ];
        rank(&mut rows);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["n-low", "p", "a-high", "c-high"]);
    }
}
