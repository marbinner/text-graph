//! Syntax colouring for the source view, as plain data.
//!
//! syntect wants the file from its first line — a highlighter's state is
//! what tells it whether line 400 is inside a string or a comment — so this
//! scans from the top and hands back spans for the lines the pane will
//! actually draw. Colours come out as RGB triples: this module is
//! egui-free like the rest of the library, and the caller decides what a
//! colour means on its canvas.
//!
//! Everything here is best-effort. A file type syntect doesn't know, a
//! file too big to be worth colouring, a theme that failed to load: the
//! answer is `None` and the pane draws the plain text it would have drawn
//! anyway.

use std::ops::Range;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Files past this are left plain: colouring is a read of the WHOLE file
/// (state carries line to line), and a preview is a glance.
pub const MAX_BYTES: usize = 512 * 1024;
/// Lines scanned at most, for the same reason — a generated file can be
/// one enormous minified blob with a million of them.
pub const MAX_LINES: usize = 20_000;

/// A coloured run within one line: byte range into that line, and its RGB.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub color: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// Is this file type one we can colour at all? Cheap enough to ask before
/// reading anything.
pub fn known(path: &str) -> bool {
    syntax_name(path).is_some()
}

fn syntax_name(path: &str) -> Option<&'static syntect::parsing::SyntaxReference> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let ss = syntaxes();
    // by extension first, then by whole filename (Makefile, Dockerfile)
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(file))
}

/// Spans for lines 1..=`upto` of `text`, or `None` when the file type is
/// unknown or the file is too big to colour. Index 0 is line 1; a line
/// with no interesting runs still gets its entry, so the caller can index
/// by line number without thinking about it.
pub fn spans(path: &str, text: &str, upto: usize, light: bool) -> Option<Vec<Vec<Span>>> {
    if text.len() > MAX_BYTES {
        return None;
    }
    let syntax = syntax_name(path)?;
    let themes = themes();
    // syntect ships both; these two read well against our two backgrounds
    let theme = themes
        .themes
        .get(if light {
            "InspiredGitHub"
        } else {
            "base16-ocean.dark"
        })
        .or_else(|| themes.themes.values().next())?;
    let mut hl = HighlightLines::new(syntax, theme);
    let ss = syntaxes();
    let mut out: Vec<Vec<Span>> = Vec::new();
    for line in LinesWithEndings::from(text).take(upto.min(MAX_LINES)) {
        let Ok(runs) = hl.highlight_line(line, ss) else {
            // a broken line stops the colouring, not the preview
            return (!out.is_empty()).then_some(out);
        };
        let mut at = 0usize;
        let mut spans = Vec::new();
        for (style, piece) in runs {
            let range = at..at + piece.len();
            at = range.end;
            // trailing newline isn't drawn, and a run of pure whitespace
            // carries no colour worth an entry
            if piece.trim().is_empty() {
                continue;
            }
            let c = style.foreground;
            spans.push(Span {
                range,
                color: (c.r, c.g, c.b),
                bold: style.font_style.contains(FontStyle::BOLD),
                italic: style.font_style.contains(FontStyle::ITALIC),
            });
        }
        out.push(spans);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_comes_back_in_several_colours() {
        let src = "fn main() {\n    let x = \"hello\"; // hi\n}\n";
        let lines = spans("a.rs", src, 10, false).expect("rust is known");
        assert_eq!(lines.len(), 3);
        let colors: std::collections::HashSet<(u8, u8, u8)> =
            lines[1].iter().map(|s| s.color).collect();
        assert!(
            colors.len() >= 3,
            "keyword, string and comment must not be one colour: {colors:?}"
        );
        // ranges index INTO the line and stay inside it
        let line = "    let x = \"hello\"; // hi\n";
        for s in &lines[1] {
            assert!(s.range.end <= line.len(), "{s:?} runs past its line");
            assert!(line.is_char_boundary(s.range.start) && line.is_char_boundary(s.range.end));
        }
    }

    #[test]
    fn unknown_types_and_giants_stay_plain() {
        assert!(spans("notes.weirdext", "hello\n", 10, false).is_none());
        assert!(!known("notes.weirdext"));
        // syntect's defaults cover the common ones (no TOML, as it
        // happens — an unknown type is not an error, just plain text)
        assert!(known("a.py") && known("a.md") && known("a.sh"));
        let huge = "x\n".repeat(MAX_BYTES);
        assert!(
            spans("a.rs", &huge, 10, false).is_none(),
            "too big to colour"
        );
    }

    #[test]
    fn multibyte_lines_keep_their_byte_ranges() {
        let src = "// héllo wörld ✨\nlet a = 1;\n";
        let lines = spans("a.rs", src, 10, false).expect("known");
        let first = "// héllo wörld ✨\n";
        for s in &lines[0] {
            assert!(first.is_char_boundary(s.range.start) && first.is_char_boundary(s.range.end));
        }
    }

    #[test]
    fn only_the_lines_asked_for_are_returned() {
        let src = (0..50)
            .map(|i| format!("let x{i} = {i};\n"))
            .collect::<String>();
        assert_eq!(spans("a.rs", &src, 5, false).expect("known").len(), 5);
    }
}
