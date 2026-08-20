//! Best-effort TeX-to-text for math spans in rendered notes.
//!
//! The markdown renderer parses `$…$` / `$$…$$` and delegates drawing to
//! the app; there is no TeX engine anywhere in the stack, and none is
//! wanted (offline, deterministic, no dependencies — the same trade web
//! nodes made against favicon fetching). This module converts the
//! common note-taking subset: greek letters, operators, relations,
//! arrows, big operators, super/subscripts, `\frac`, `\sqrt`, accents,
//! named functions (`\sin`, `\log`, `\lim`), `\begin{…}` environments
//! and `\text`-style wrappers.
//!
//! The output is [`Run`]s, not a string, because two things about a
//! formula cannot be said in plain characters. Scripts: Unicode has
//! superscript forms for some characters and none for the rest, so
//! `x^2` could be spelled and `e^{z_i}` could only degrade to `e^(zᵢ)`.
//! A run carries its LEVEL instead and the app raises or lowers it, so
//! every exponent sets like an exponent. And style: TeX leans variables
//! and stands operators, digits and function names upright, which is
//! what tells `log` from `l·o·g` at a glance — [`Run::italic`] carries
//! that decision per run.
//!
//! One level only. A script inside a script (the `_i` in `e^{z_i}`) has
//! nowhere lower to go, so it flattens into the Unicode forms, and a
//! script that can't be spelled that way falls back to `^(…)`/`_(…)`.
//! Same rule for arguments: `\frac{1}{i^2}` sets its denominator flat.
//!
//! Whitespace follows TeX, not the source: a newline inside a span is
//! just a space, runs of spaces collapse, and only `\\` breaks a line.
//! A display block therefore renders as its rows, with no blank lines
//! from the way the author happened to wrap the source.
//!
//! The honesty rule: anything unrecognized keeps its `\name` verbatim —
//! partial prettiness must never hide what the author wrote. Bare
//! braces are TeX grouping and disappear.
//!
//! Every character the tables below can emit has to be drawable, or a
//! converted span reads as a row of replacement boxes. [`glyphs`]
//! enumerates the whole inventory so the app side can hold the fonts it
//! renders with to it.

/// One stretch of converted math that sets the same way throughout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    /// 0 on the baseline, 1 raised, -1 lowered. Never anything else —
    /// see the module doc on why one level is the whole ladder.
    pub level: i8,
    /// TeX's math italic: letters lean, digits and operators stand, and
    /// a function name or `\text{…}` stands whatever it is made of.
    pub italic: bool,
}

/// Convert one math span (the text between the dollars) to runs.
pub fn to_runs(tex: &str) -> Vec<Run> {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = Parser {
        s: &chars,
        i: 0,
        col: ALIGN_TAB,
        flat: false,
    };
    let mut out = Runs::default();
    p.seq(None, &mut out);
    tidy(out.runs)
}

/// Every character [`to_runs`] can put on screen. The app's font test
/// walks it: a glyph missing from the reading family renders as a
/// replacement box, so the bundled math subset must cover the whole
/// inventory (and the tables must not grow past it).
pub fn glyphs() -> String {
    let mut out = String::from(STANDALONE);
    for (_, s) in SYMBOLS {
        out.push_str(s);
    }
    for (_, c) in ACCENTS {
        out.push(*c);
    }
    for (_, c) in SUPERS.iter().chain(SUBS) {
        out.push(*c);
    }
    for c in ['ℂ', '𝔼', '𝔽', 'ℍ', 'ℕ', 'ℙ', 'ℚ', 'ℝ', 'ℤ'] {
        out.push(c);
    }
    out
}

/// Runs under construction. Every push decides italics per character —
/// TeX's rule is about what the character IS, not about where the
/// author put it — and merges into the previous run when the setting
/// matches, so a formula is a handful of runs rather than one per glyph.
#[derive(Default)]
struct Runs {
    runs: Vec<Run>,
}

impl Runs {
    /// Append `text` at `level`. `upright` forces the whole string
    /// upright: a function name or `\text{…}` is a word, not a product
    /// of variables.
    fn push(&mut self, text: &str, level: i8, upright: bool) {
        for c in text.chars() {
            // a combining mark belongs to the character before it: give
            // it that run, or the accent lands in a run of its own and
            // stops sitting on its base
            if is_combining(c)
                && let Some(r) = self.runs.last_mut()
            {
                r.text.push(c);
                continue;
            }
            self.push_raw(c, level, !upright && c.is_alphabetic());
        }
    }

    /// Take `runs` as they are, at `level`. `upright` flattens their
    /// setting — a wrapper that makes a word of its argument.
    fn extend(&mut self, runs: Vec<Run>, level: i8, upright: bool) {
        for r in runs {
            for c in r.text.chars() {
                self.push_raw(c, level, r.italic && !upright);
            }
        }
    }

    /// Append one character with its setting already decided — the
    /// rebuild after [`tidy`], which must not re-derive what it kept.
    fn push_raw(&mut self, c: char, level: i8, italic: bool) {
        match self.runs.last_mut() {
            Some(r) if r.level == level && r.italic == italic => r.text.push(c),
            _ => self.runs.push(Run {
                text: c.to_string(),
                level,
                italic,
            }),
        }
    }

    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Characters the parser emits on its own, outside any table: the radical
/// sign and the fraction slash, the parens the `(…)` fallbacks add, and
/// the two spaces `\,` and `\quad` become.
const STANDALONE: &str = "√/()[]C,\u{2003}\u{2009}";

/// What `&` becomes. TeX draws nothing for an alignment tab, so in
/// `aligned` and friends it simply disappears; in a matrix it separates
/// columns, and an em space is the gap that reads as one.
const ALIGN_TAB: &str = "";
const COLUMN_GAP: &str = "\u{2003}";

/// Environments whose `&` separates COLUMNS instead of marking an
/// alignment point. Everything else (`aligned`, `align`, `gather`,
/// `split`, `equation`, …) is a stack of rows.
const COLUMNED: &[&str] = &[
    "matrix",
    "pmatrix",
    "bmatrix",
    "Bmatrix",
    "vmatrix",
    "Vmatrix",
    "smallmatrix",
    "subarray",
    "array",
    "cases",
];

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    /// What an `&` expands to in the environment being read.
    col: &'static str,
    /// Inside an argument, where a raised or lowered run has nowhere to
    /// go: scripts spell themselves with the Unicode tables instead.
    flat: bool,
}

impl Parser<'_> {
    /// Consume until the end (or a closing brace when inside a group).
    fn seq(&mut self, until: Option<char>, out: &mut Runs) {
        while let Some(&c) = self.s.get(self.i) {
            if Some(c) == until {
                self.i += 1;
                break;
            }
            self.i += 1;
            match c {
                '\\' => self.command(out),
                '{' => self.seq(Some('}'), out),
                '}' => {} // stray closer: grouping, not content
                '^' => self.script(out, SUPERS, '^', 1),
                '_' => self.script(out, SUBS, '_', -1),
                '&' => out.push(self.col, 0, true),
                // TeX whitespace: source line breaks and tabs are spaces,
                // `~` is a space that doesn't break. Only `\\` ends a line.
                '~' | '\n' | '\r' | '\t' => out.push(" ", 0, true),
                _ => out.push(&c.to_string(), 0, false),
            }
        }
    }

    /// A `^` or `_` was consumed: raise or lower its argument, unless we
    /// are already inside one — then it spells itself, or falls back to
    /// the honest `^(…)`.
    fn script(&mut self, out: &mut Runs, table: &[(char, char)], op: char, level: i8) {
        let body = self.arg();
        if !self.flat {
            out.extend(body, level, false);
            return;
        }
        match map_script(&joined(&body), table) {
            // the spelled forms are letters and digits like any other:
            // `ᵢ` leans because `i` does
            Some(m) => out.push(&m, 0, false),
            None => {
                out.push(&op.to_string(), 0, true);
                out.extend(parens_if_wide(body), 0, false);
            }
        }
    }

    /// One argument, flattened to text: a `{…}` group, a `\command`, or a
    /// single character. Everything inside sets on one level.
    fn arg(&mut self) -> Vec<Run> {
        let flat = std::mem::replace(&mut self.flat, true);
        let mut out = Runs::default();
        match self.s.get(self.i) {
            Some('{') => {
                self.i += 1;
                self.seq(Some('}'), &mut out);
            }
            Some('\\') => {
                self.i += 1;
                self.command(&mut out);
            }
            Some(&c) => {
                self.i += 1;
                out.push(&c.to_string(), 0, false);
            }
            None => {}
        }
        self.flat = flat;
        out.runs
    }

    /// A `\` was consumed: read the command and emit its expansion.
    fn command(&mut self, out: &mut Runs) {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i == start {
            // escaped single character: `\{`, `\\`, `\,` …
            if let Some(&c) = self.s.get(self.i) {
                self.i += 1;
                match c {
                    ',' | ':' | ';' => out.push("\u{2009}", 0, true), // thin space
                    '!' => {}
                    '|' => out.push("‖", 0, true),
                    '\\' => out.push("\n", 0, true),
                    // \{ \} \$ \% \& \# \_ and a lone \
                    _ => out.push(&c.to_string(), 0, true),
                }
            } else {
                out.push("\\", 0, true);
            }
            return;
        }
        let name: String = self.s[start..self.i].iter().collect();
        // accents: combining mark after a single-char base
        if let Some(mark) = accent_mark(&name) {
            let base = self.arg();
            out.push(&accent(&joined(&base), mark), 0, false);
            return;
        }
        match name.as_str() {
            // wrappers that make a WORD of their argument: it stands
            // upright, the way `\text{if}` is a word and not i·f
            "text" | "textrm" | "textbf" | "textsf" | "texttt" | "mathrm" | "mathsf" | "mathtt"
            | "mbox" | "operatorname" => {
                let arg = self.arg();
                out.extend(arg, 0, true);
            }
            // wrappers that only change weight or shape: the argument
            // keeps whatever setting its own characters ask for
            "textit" | "mathbf" | "mathit" | "mathcal" | "mathfrak" | "mathscr" | "boldsymbol"
            | "bm" | "pmb" | "overbrace" | "underbrace" => {
                let arg = self.arg();
                out.extend(arg, 0, false);
            }
            // `\mathbb{R}` has a letter of its own
            "mathbb" => {
                let arg = joined(&self.arg());
                out.push(&blackboard(&arg), 0, true);
            }
            // named functions are set upright — the name IS the rendering
            _ if FUNCTIONS.contains(&name.as_str()) => out.push(&name, 0, true),
            "pmod" => {
                let arg = self.arg();
                out.push(" (mod ", 0, true);
                out.extend(arg, 0, false);
                out.push(")", 0, true);
            }
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let (a, b) = (self.arg(), self.arg());
                out.extend(parens_if_wide(a), 0, false);
                out.push("/", 0, true);
                out.extend(parens_if_wide(b), 0, false);
            }
            "binom" | "dbinom" | "tbinom" => {
                let (n, k) = (self.arg(), self.arg());
                out.push("C(", 0, true);
                out.extend(n, 0, false);
                out.push(", ", 0, true);
                out.extend(k, 0, false);
                out.push(")", 0, true);
            }
            "sqrt" => {
                // `\sqrt[3]{x}`: the index rides as a superscript when it
                // maps, and stays bracketed when it doesn't
                if self.s.get(self.i) == Some(&'[') {
                    self.i += 1;
                    let flat = std::mem::replace(&mut self.flat, true);
                    let mut index = Runs::default();
                    self.seq(Some(']'), &mut index);
                    self.flat = flat;
                    let index = index.text();
                    match map_script(&index, SUPERS) {
                        Some(m) => out.push(&m, 0, true),
                        None => out.push(&format!("[{index}]"), 0, false),
                    }
                }
                out.push("√", 0, true);
                if self.s.get(self.i) == Some(&'{') {
                    let arg = self.arg();
                    out.extend(parens_if_wide(arg), 0, false);
                }
            }
            // `\begin{env}` … `\end{env}`
            "begin" => self.environment(out),
            "end" => {
                let _ = self.arg(); // a stray closer: nothing to draw
            }
            // sizing/style noise: drop, the delimiter itself follows.
            // `\left.` and `\right.` are invisible delimiters — the dot
            // belongs to the command, not to the formula.
            "left" | "right" | "middle" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr"
            | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr" => {
                if self.s.get(self.i) == Some(&'.') {
                    self.i += 1;
                }
            }
            "limits" | "nolimits" | "displaystyle" | "textstyle" | "scriptstyle"
            | "scriptscriptstyle" | "mathstrut" | "nonumber" | "notag" => {}
            // numbering and spacing directives: they and their argument
            // are typesetting instructions, not content
            "label" | "tag" | "phantom" | "hphantom" | "vphantom" => {
                let _ = self.arg();
            }
            "hspace" | "vspace" => {
                let _ = self.arg();
                out.push(" ", 0, true);
            }
            "quad" => out.push("\u{2003}", 0, true),
            "qquad" => out.push("\u{2003}\u{2003}", 0, true),
            _ => match symbol(&name) {
                Some(s) => out.push(s, 0, false),
                None => {
                    // verbatim: the reader sees exactly what they wrote
                    out.push("\\", 0, true);
                    out.push(&name, 0, true);
                }
            },
        }
    }

    /// `\begin{env}` was just read: draw the rows up to its `\end`.
    ///
    /// Environments are read as a WHOLE (a sub-parser over the body)
    /// rather than by flipping a flag, so a matrix nested in a `cases`
    /// restores the outer environment's `&` when it closes.
    fn environment(&mut self, out: &mut Runs) {
        let env = joined(&self.arg());
        let base = env.trim_end_matches('*');
        // `\begin{array}{cc}` — the column spec is layout, not content
        if base == "array" && self.s.get(self.i) == Some(&'{') {
            let _ = self.arg();
        }
        let col = if COLUMNED.contains(&base) {
            COLUMN_GAP
        } else {
            ALIGN_TAB
        };
        let (body_end, resume) = self.find_end();
        let body: Vec<char> = self.s[self.i..body_end].to_vec();
        self.i = resume;
        let mut sub = Parser {
            s: &body,
            i: 0,
            col,
            flat: self.flat,
        };
        let mut rows = Runs::default();
        sub.seq(None, &mut rows);
        // a stack of rows is a block: it starts on its own line when
        // something already precedes it (`x = \begin{cases}…`)
        if rows.text().contains('\n') && !out.text().trim().is_empty() {
            out.push("\n", 0, true);
        }
        out.runs.extend(rows.runs);
    }

    /// Where the environment open at `self.i` ends: (index of its
    /// `\end`, index just past that `\end{…}`). Nested `\begin`s are
    /// counted, and an unclosed environment runs to the end of the span.
    fn find_end(&self) -> (usize, usize) {
        let mut depth = 1usize;
        let mut j = self.i;
        while j < self.s.len() {
            if self.s[j] != '\\' {
                j += 1;
                continue;
            }
            let mut k = j + 1;
            while self.s.get(k).is_some_and(|c| c.is_ascii_alphabetic()) {
                k += 1;
            }
            let name: String = self.s[j + 1..k].iter().collect();
            if name == "begin" {
                depth += 1;
                j = k;
                continue;
            }
            if name != "end" {
                // an escape (`\\`, `\{`) covers two characters
                j = if k > j + 1 { k } else { j + 2 };
                continue;
            }
            let mut m = k;
            while self.s.get(m) == Some(&' ') {
                m += 1;
            }
            if self.s.get(m) == Some(&'{') {
                while m < self.s.len() && self.s[m] != '}' {
                    m += 1;
                }
                m = (m + 1).min(self.s.len());
            }
            depth -= 1;
            if depth == 0 {
                return (j, m);
            }
            j = m;
        }
        (self.s.len(), self.s.len())
    }
}

/// TeX whitespace, applied after the fact: runs of spaces are one space,
/// a line carries no leading or trailing space, a line with nothing on it
/// is not a line, and the spaces `\,`/`\quad` mint swallow the ordinary
/// ones beside them (`\int_0^1 x \, dx` is one gap, not three).
///
/// It works on the characters, not on the runs, because a space can
/// arrive in one run and the character it should collapse against in
/// the next; the setting rides along and the runs are rebuilt at the
/// end.
fn tidy(runs: Vec<Run>) -> Vec<Run> {
    let math_space = |c: char| c == '\u{2003}' || c == '\u{2009}';
    let chars = runs
        .iter()
        .flat_map(|r| r.text.chars().map(|c| (c, r.level, r.italic)));
    let mut out: Vec<(char, i8, bool)> = Vec::new();
    let mut line_start = 0usize;
    let mut pending: Option<(i8, bool)> = None;
    for (c, level, italic) in chars {
        if c == '\n' {
            pending = None;
            if out.len() == line_start {
                continue; // a line with nothing on it is not a line
            }
            out.push(('\n', 0, false));
            line_start = out.len();
            continue;
        }
        if c == ' ' || c == '\t' {
            pending = Some((level, italic));
            continue;
        }
        if let Some((sl, si)) = pending.take()
            && out.len() > line_start
            && !out.last().is_some_and(|&(p, _, _)| math_space(p))
            && !math_space(c)
        {
            out.push((' ', sl, si));
        }
        out.push((c, level, italic));
    }
    while out.last().is_some_and(|&(c, _, _)| c == '\n') {
        out.pop();
    }
    let mut rebuilt = Runs::default();
    for (c, level, italic) in out {
        rebuilt.push_raw(c, level, italic);
    }
    rebuilt.runs
}

/// `\mathbb{R}` is ℝ wherever a double-struck letter exists; the rest of
/// the argument keeps its plain letters.
fn blackboard(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'C' => 'ℂ',
            'E' => '𝔼',
            'F' => '𝔽',
            'H' => 'ℍ',
            'N' => 'ℕ',
            'P' => 'ℙ',
            'Q' => 'ℚ',
            'R' => 'ℝ',
            'Z' => 'ℤ',
            other => other,
        })
        .collect()
}

/// The text of a stretch of runs, for the decisions that are about
/// characters rather than setting: which Unicode script form to use,
/// which double-struck letter, whether a fraction needs parentheses.
fn joined(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// Combining marks ride the character before them — the accents this
/// module places, and any the author typed.
fn is_combining(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036f}' | '\u{20d0}'..='\u{20ff}')
}

/// `(x)` when `x` is more than one glyph, `x` alone otherwise.
fn parens_if_wide(runs: Vec<Run>) -> Vec<Run> {
    if joined(&runs).chars().nth(1).is_none() {
        return runs;
    }
    let paren = |t: &str| Run {
        text: t.to_string(),
        level: 0,
        italic: false,
    };
    let mut out = vec![paren("(")];
    out.extend(runs);
    out.push(paren(")"));
    out
}

fn accent_mark(name: &str) -> Option<char> {
    ACCENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, mark)| *mark)
}

fn accent(base: &str, mark: char) -> String {
    let mut out = base.to_string();
    if base.chars().count() == 1 {
        out.push(mark);
    }
    out
}

/// The script forms of `body`, or None when a character has none.
fn map_script(body: &str, table: &[(char, char)]) -> Option<String> {
    let mapped: Option<String> = body
        .chars()
        .map(|c| table.iter().find(|(p, _)| *p == c).map(|(_, m)| *m))
        .collect();
    mapped.filter(|m| !m.is_empty())
}

/// Accents, as a combining mark placed after a single-character base.
const ACCENTS: &[(&str, char)] = &[
    ("vec", '\u{20d7}'),
    ("overrightarrow", '\u{20d7}'),
    ("hat", '\u{0302}'),
    ("widehat", '\u{0302}'),
    ("bar", '\u{0304}'),
    ("overline", '\u{0304}'),
    ("tilde", '\u{0303}'),
    ("widetilde", '\u{0303}'),
    ("dot", '\u{0307}'),
    ("ddot", '\u{0308}'),
    ("underline", '\u{0332}'),
    ("check", '\u{030c}'),
    ("breve", '\u{0306}'),
    ("acute", '\u{0301}'),
    ("grave", '\u{0300}'),
];

/// Function names TeX sets upright. They render as themselves — the
/// backslash is markup, not something a reader should see.
const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh",
    "coth", "log", "ln", "lg", "exp", "det", "dim", "deg", "gcd", "ker", "hom", "arg", "max",
    "min", "sup", "inf", "lim", "limsup", "liminf", "Pr", "mod", "bmod",
];

const SUPERS: &[(char, char)] = &[
    ('0', '⁰'),
    ('1', '¹'),
    ('2', '²'),
    ('3', '³'),
    ('4', '⁴'),
    ('5', '⁵'),
    ('6', '⁶'),
    ('7', '⁷'),
    ('8', '⁸'),
    ('9', '⁹'),
    ('+', '⁺'),
    ('-', '⁻'),
    ('=', '⁼'),
    ('(', '⁽'),
    (')', '⁾'),
    ('a', 'ᵃ'),
    ('b', 'ᵇ'),
    ('c', 'ᶜ'),
    ('d', 'ᵈ'),
    ('e', 'ᵉ'),
    ('f', 'ᶠ'),
    ('g', 'ᵍ'),
    ('h', 'ʰ'),
    ('i', 'ⁱ'),
    ('j', 'ʲ'),
    ('k', 'ᵏ'),
    ('l', 'ˡ'),
    ('m', 'ᵐ'),
    ('n', 'ⁿ'),
    ('o', 'ᵒ'),
    ('p', 'ᵖ'),
    ('r', 'ʳ'),
    ('s', 'ˢ'),
    ('t', 'ᵗ'),
    ('u', 'ᵘ'),
    ('v', 'ᵛ'),
    ('w', 'ʷ'),
    ('x', 'ˣ'),
    ('y', 'ʸ'),
    ('z', 'ᶻ'),
    ('T', 'ᵀ'),
];

const SUBS: &[(char, char)] = &[
    ('0', '₀'),
    ('1', '₁'),
    ('2', '₂'),
    ('3', '₃'),
    ('4', '₄'),
    ('5', '₅'),
    ('6', '₆'),
    ('7', '₇'),
    ('8', '₈'),
    ('9', '₉'),
    ('+', '₊'),
    ('-', '₋'),
    ('=', '₌'),
    ('(', '₍'),
    (')', '₎'),
    ('a', 'ₐ'),
    ('e', 'ₑ'),
    ('h', 'ₕ'),
    ('i', 'ᵢ'),
    ('j', 'ⱼ'),
    ('k', 'ₖ'),
    ('l', 'ₗ'),
    ('m', 'ₘ'),
    ('n', 'ₙ'),
    ('o', 'ₒ'),
    ('p', 'ₚ'),
    ('r', 'ᵣ'),
    ('s', 'ₛ'),
    ('t', 'ₜ'),
    ('u', 'ᵤ'),
    ('v', 'ᵥ'),
    ('x', 'ₓ'),
];

fn symbol(name: &str) -> Option<&'static str> {
    SYMBOLS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// TeX name → what it draws. A table rather than a `match` so the glyph
/// inventory stays enumerable — see [`glyphs`].
const SYMBOLS: &[(&str, &str)] = &[
    // greek, lower
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("varpi", "ϖ"),
    ("rho", "ρ"),
    ("varrho", "ϱ"),
    ("sigma", "σ"),
    ("varsigma", "ς"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "ϕ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    // greek, upper
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    // operators
    ("times", "×"),
    ("cdot", "·"),
    ("pm", "±"),
    ("mp", "∓"),
    ("div", "÷"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "•"),
    ("oplus", "⊕"),
    ("ominus", "⊖"),
    ("otimes", "⊗"),
    ("odot", "⊙"),
    ("cap", "∩"),
    ("cup", "∪"),
    ("sqcap", "⊓"),
    ("sqcup", "⊔"),
    ("uplus", "⊎"),
    ("setminus", "∖"),
    ("wedge", "∧"),
    ("land", "∧"),
    ("vee", "∨"),
    ("lor", "∨"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("dagger", "†"),
    ("ddagger", "‡"),
    // relations
    ("leq", "≤"),
    ("le", "≤"),
    ("leqslant", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("geqslant", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("equiv", "≡"),
    ("approx", "≈"),
    ("asymp", "≍"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("cong", "≅"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("prec", "≺"),
    ("preceq", "⪯"),
    ("succ", "≻"),
    ("succeq", "⪰"),
    ("subset", "⊂"),
    ("supset", "⊃"),
    ("subseteq", "⊆"),
    ("supseteq", "⊇"),
    ("subsetneq", "⊊"),
    ("supsetneq", "⊋"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("perp", "⊥"),
    ("bot", "⊥"),
    ("top", "⊤"),
    ("parallel", "∥"),
    ("mid", "∣"),
    ("nmid", "∤"),
    ("vdash", "⊢"),
    ("dashv", "⊣"),
    ("models", "⊨"),
    // arrows
    ("to", "→"),
    ("rightarrow", "→"),
    ("longrightarrow", "⟶"),
    ("leftarrow", "←"),
    ("gets", "←"),
    ("longleftarrow", "⟵"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Longrightarrow", "⟹"),
    ("Leftarrow", "⇐"),
    ("Longleftarrow", "⟸"),
    ("Leftrightarrow", "⇔"),
    ("mapsto", "↦"),
    ("longmapsto", "⟼"),
    ("implies", "⟹"),
    ("impliedby", "⟸"),
    ("iff", "⟺"),
    ("uparrow", "↑"),
    ("downarrow", "↓"),
    ("updownarrow", "↕"),
    ("hookrightarrow", "↪"),
    ("hookleftarrow", "↩"),
    ("rightsquigarrow", "⇝"),
    // big operators
    ("sum", "∑"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("bigcup", "⋃"),
    ("bigcap", "⋂"),
    ("bigoplus", "⨁"),
    ("bigotimes", "⨂"),
    ("bigwedge", "⋀"),
    ("bigvee", "⋁"),
    // misc
    ("infty", "∞"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nexists", "∄"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("complement", "∁"),
    ("aleph", "ℵ"),
    ("ell", "ℓ"),
    ("hbar", "ℏ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("wp", "℘"),
    ("dots", "…"),
    ("ldots", "…"),
    ("cdots", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("angle", "∠"),
    ("triangle", "△"),
    ("square", "□"),
    ("Box", "□"),
    ("blacksquare", "∎"),
    ("diamond", "⋄"),
    ("prime", "′"),
    ("degree", "°"),
    ("colon", ":"),
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("lbrace", "{"),
    ("rbrace", "}"),
    ("lbrack", "["),
    ("rbrack", "]"),
    ("lvert", "|"),
    ("rvert", "|"),
    ("vert", "|"),
    ("lVert", "‖"),
    ("rVert", "‖"),
    ("Vert", "‖"),
    ("lceil", "⌈"),
    ("rceil", "⌉"),
    ("lfloor", "⌊"),
    ("rfloor", "⌋"),
    ("therefore", "∴"),
    ("because", "∵"),
];

#[cfg(test)]
mod tests {
    use super::{glyphs, to_runs};

    /// The runs, written out so an assertion can read like the formula:
    /// `^{…}`/`_{…}` around a raised or lowered stretch, `*…*` around
    /// what leans. Consecutive runs on one level group into one pair of
    /// braces — the split between `x` and `= 0` inside a subscript is
    /// about setting, not about structure.
    fn show(tex: &str) -> String {
        let mut out = String::new();
        let runs = to_runs(tex);
        let mut i = 0;
        while i < runs.len() {
            let level = runs[i].level;
            let mut inner = String::new();
            while i < runs.len() && runs[i].level == level {
                let r = &runs[i];
                if r.italic {
                    inner.push('*');
                    inner.push_str(&r.text);
                    inner.push('*');
                } else {
                    inner.push_str(&r.text);
                }
                i += 1;
            }
            match level {
                1 => out.push_str(&format!("^{{{inner}}}")),
                -1 => out.push_str(&format!("_{{{inner}}}")),
                _ => out.push_str(&inner),
            }
        }
        out
    }

    #[test]
    fn plain_symbols_and_greek() {
        assert_eq!(show(r"\delta = 2"), "*δ* = 2");
        assert_eq!(show(r"\alpha \to \beta"), "*α* → *β*");
        assert_eq!(show(r"\forall x \in S"), "∀ *x* ∈ *S*");
    }

    /// TeX's math italic: a letter is a variable and leans, a digit or
    /// an operator stands, and a function name is a word — which is what
    /// tells `log` from three letters multiplied together.
    #[test]
    fn letters_lean_and_everything_else_stands() {
        assert_eq!(show(r"2x + 3y = 0"), "2*x* + 3*y* = 0");
        assert_eq!(show(r"\log n"), "log *n*");
        assert_eq!(show(r"\text{if } x > 0"), "if *x* > 0");
        assert_eq!(show(r"\mathrm{d}x"), "d*x*");
    }

    /// A raised or lowered RUN, not a spelled-out Unicode character:
    /// `e^{z_i}` has no superscript-with-a-subscript to spell, and used
    /// to degrade to `e^(zᵢ)` — the caret and parens the reader sees
    /// whenever one character in a script has no script form.
    #[test]
    fn scripts_are_levels_and_nest_one_deep() {
        assert_eq!(show(r"x^2 + y_i"), "*x*^{2} + *y*_{*i*}");
        assert_eq!(show(r"e^{i\pi}"), "*e*^{*iπ*}");
        assert_eq!(show(r"\sum_{i=0}^{n} x_i"), "∑_{*i*=0}^{*n*} *x*_{*i*}");
        assert_eq!(show(r"A^T"), "*A*^{*T*}");
        // the second level down has nowhere to go: it spells itself
        assert_eq!(show(r"e^{z_i}"), "*e*^{*zᵢ*}");
        // …and falls back honestly when it cannot be spelled
        assert_eq!(show(r"e^{z_{\pi\pi}}"), "*e*^{*z*_(*ππ*)}");
    }

    #[test]
    fn fractions_roots_and_wrappers() {
        assert_eq!(show(r"\frac{1}{2}"), "1/2");
        assert_eq!(show(r"\frac{a+b}{c}"), "(*a*+*b*)/*c*");
        // a fraction keeps the setting of what is inside it
        assert_eq!(show(r"\frac{\sin x}{x}"), "(sin *x*)/*x*");
        assert_eq!(show(r"\sqrt{x+1}"), "√(*x*+1)");
        assert_eq!(show(r"\sqrt[3]{x}"), "³√*x*");
        assert_eq!(show(r"\sqrt[n+1]{x}"), "ⁿ⁺¹√*x*");
        // an index with no script form keeps its brackets
        assert_eq!(show(r"\sqrt[\pi]{x}"), "[*π*]√*x*");
        assert_eq!(show(r"\binom{n}{k}"), "C(*n*, *k*)");
        assert_eq!(show(r"\mathbb{R}^n"), "ℝ^{*n*}");
    }

    /// An accent has to stay in its base's run, or it is laid out on its
    /// own and stops sitting on the letter it belongs to.
    #[test]
    fn accents_ride_single_char_bases() {
        assert_eq!(show(r"\vec{x}"), "*x\u{20d7}*");
        assert_eq!(show(r"\hat{y} = \bar{x}"), "*y\u{0302}* = *x\u{0304}*");
        assert_eq!(to_runs(r"\vec{x}").len(), 1);
    }

    #[test]
    fn unknown_commands_stay_verbatim() {
        assert_eq!(show(r"\foobar + 1"), r"\foobar + 1");
        // grouping braces disappear, the name does not
        assert_eq!(show(r"\undefinedcmd{x}"), r"\undefinedcmd*x*");
    }

    #[test]
    fn sizing_noise_drops_and_delimiters_stay() {
        assert_eq!(show(r"\left( \frac{1}{2} \right)"), "( 1/2 )");
        assert_eq!(show(r"\langle u, v \rangle"), "⟨ *u*, *v* ⟩");
        // an invisible delimiter takes its dot with it
        assert_eq!(show(r"\left\{ x \right."), "{ *x*");
    }

    #[test]
    fn named_functions_lose_only_their_backslash() {
        assert_eq!(show(r"\sin x + \cos y"), "sin *x* + cos *y*");
        assert_eq!(
            show(r"\lim_{x \to 0} \frac{\sin x}{x} = 1"),
            "lim_{*x* → 0} (sin *x*)/*x* = 1"
        );
        assert_eq!(show(r"\log_2 n"), "log_{2} *n*");
    }

    /// A display block wraps where the author wrapped the source, and TeX
    /// does not care: only `\\` ends a line. The whole span used to come
    /// through with the newlines the `$$` fences left on it, so every
    /// equation drew with a blank line above and below.
    #[test]
    fn source_line_breaks_are_spaces_and_only_a_double_backslash_breaks() {
        assert_eq!(show("\nE = mc^2\n"), "*E* = *mc*^{2}");
        assert_eq!(show("a\n= b"), "*a* = *b*");
        assert_eq!(show(r"a \\ b"), "*a*\n*b*");
        assert_eq!(show("x    +     y"), "*x* + *y*");
        // a minted space swallows the ordinary ones beside it
        assert_eq!(show(r"\int_0^1 x^2 \, dx"), "∫_{0}^{1} *x*^{2}\u{2009}*dx*");
    }

    #[test]
    fn environments_become_rows() {
        assert_eq!(
            show("\\begin{aligned}\na &= b \\\\\nc &= d\n\\end{aligned}"),
            "*a* = *b*\n*c* = *d*"
        );
        assert_eq!(
            show(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "*a*\u{2003}*b*\n*c*\u{2003}*d*"
        );
        // an environment that follows something starts its own line
        assert_eq!(
            show(r"x = \begin{cases} 1 & p \\ 0 & q \end{cases}"),
            "*x* =\n1\u{2003}*p*\n0\u{2003}*q*"
        );
        // a matrix inside cases restores the outer environment's `&`
        assert_eq!(
            show(r"\begin{aligned} a &= \begin{matrix} 1 & 2 \end{matrix} \end{aligned}"),
            "*a* = 1\u{2003}2"
        );
        // an unclosed environment still draws its rows
        assert_eq!(show(r"\begin{aligned} a &= b"), "*a* = *b*");
    }

    /// Nothing may reach the screen that the bundled math font can't
    /// draw — the app-side companion test holds the font to this list.
    #[test]
    fn the_glyph_inventory_covers_every_table() {
        let g = glyphs();
        for c in ['ℝ', 'α', '∑', '√', 'ᵢ', '\u{20d7}', '⟹'] {
            assert!(g.contains(c), "{c:?} is missing from the inventory");
        }
        assert!(
            !g.contains('\\'),
            "a backslash is verbatim TeX, not a glyph"
        );
    }
}
