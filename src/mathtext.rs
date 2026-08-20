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
//! The output is [`Run`]s, not a string, because the two things that
//! make a formula READ as one cannot be said in plain characters.
//!
//! Scripts. Unicode has superscript forms for some characters and none
//! for the rest, so spelling them gave `x²` here and `e^(zᵢ)` there —
//! a caret and parentheses on the page the moment one character in a
//! script had no script form, which for `e^{z_i}` is every time. A run
//! carries a SCALE and a RISE instead, TeX's own ladder: each step is
//! smaller than the last, moves less far than the step before it, and
//! stops at scriptscript. The app places it; nothing has to be spellable.
//!
//! Italics. TeX leans variables and stands operators, digits and
//! function names, which is what tells `log` from `l·o·g` at a glance.
//! That decision is resolved into the CHARACTERS — Unicode's
//! Mathematical Alphanumeric block is a designed italic that a math
//! font draws properly, where a renderer's italics flag only shears an
//! upright glyph. So a run's text is what gets drawn, with no styling
//! left over for the app to apply.
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
#[derive(Clone, Debug, PartialEq)]
struct Piece {
    text: String,
    style: Style,
    /// Whether TeX's math italic applies — resolved into the characters
    /// themselves by [`lean`] before any of this leaves the module.
    italic: bool,
}

/// One stretch of converted math that sets the same way throughout.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// What to draw. Leaning letters are already the Mathematical
    /// Alphanumeric characters that ARE italic, so there is no styling
    /// left for the app to apply — see [`lean`].
    pub text: String,
    /// Size as a fraction of the span's base size: 1 on the main line,
    /// [`SCRIPT`] in a script, [`SCRIPTSCRIPT`] below that.
    pub scale: f32,
    /// Baseline offset in ems of the SPAN's size — not of this run's —
    /// negative upward. Every run's offset is absolute, so the app
    /// places each one without knowing what it is nested in.
    pub rise: f32,
}

/// TeX's two smaller sizes, as fractions of the main line.
pub const SCRIPT: f32 = 0.7;
pub const SCRIPTSCRIPT: f32 = 0.5;
/// How far a script moves off the line it rides on, in ems of THAT
/// line's size — so an exponent's exponent moves less than the first
/// one did, the way the ladder narrows in a real formula.
const SUP_RISE: f32 = 0.45;
const SUB_DROP: f32 = 0.2;

/// Where a stretch of math sits and how big it is: TeX's "style",
/// carried down the parse so a script knows what it is a script OF.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    scale: f32,
    rise: f32,
}

impl Style {
    const BASE: Self = Self {
        scale: 1.0,
        rise: 0.0,
    };

    fn sup(self) -> Self {
        Self {
            scale: self.smaller(),
            rise: self.rise - SUP_RISE * self.scale,
        }
    }

    fn sub(self) -> Self {
        Self {
            scale: self.smaller(),
            rise: self.rise + SUB_DROP * self.scale,
        }
    }

    /// One step down, and then no further: TeX has scriptscript and
    /// nothing smaller, so a fourth-level index stays legible.
    fn smaller(self) -> f32 {
        if self.scale > SCRIPT {
            SCRIPT
        } else {
            SCRIPTSCRIPT
        }
    }
}

/// Convert one math span (the text between the dollars) to runs.
pub fn to_runs(tex: &str) -> Vec<Run> {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = Parser {
        s: &chars,
        i: 0,
        col: ALIGN_TAB,
        style: Style::BASE,
    };
    let mut out = Runs::default();
    p.seq(None, &mut out);
    lean(tidy(out.runs))
}

/// Resolve TeX's math italic into the characters themselves, and merge
/// what is left into runs.
///
/// Unicode has a designed italic for every math letter — the
/// Mathematical Alphanumeric block — and a math font draws those as
/// real italic letterforms. A renderer's "italics" flag only SHEARS the
/// upright glyph, which is what made a formula read as slanted UI text
/// rather than as mathematics. A letter with no such form (`ℓ`, `ℵ`,
/// anything the author typed that is not a math letter) simply stands.
fn lean(pieces: Vec<Piece>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for p in pieces {
        let text: String = if p.italic {
            p.text.chars().map(|c| leaning(c).unwrap_or(c)).collect()
        } else {
            p.text
        };
        match out.last_mut() {
            Some(r) if r.scale == p.style.scale && r.rise == p.style.rise => r.text.push_str(&text),
            _ => out.push(Run {
                text,
                scale: p.style.scale,
                rise: p.style.rise,
            }),
        }
    }
    out
}

/// The Mathematical Alphanumeric italic of a letter, when there is one.
/// Uppercase Greek is absent on purpose: TeX sets `\Gamma` upright.
fn leaning(c: char) -> Option<char> {
    let at = |base: u32, offset: u32| char::from_u32(base + offset);
    match c {
        'h' => Some('\u{210e}'), // the one hole in the italic latin block
        'a'..='z' => at(0x1d44e, c as u32 - 'a' as u32),
        'A'..='Z' => at(0x1d434, c as u32 - 'A' as u32),
        'α'..='ω' => at(0x1d6fc, c as u32 - 'α' as u32),
        'ϑ' => Some('𝜗'),
        'ϵ' => Some('𝜖'),
        'ϕ' => Some('𝜙'),
        'ϱ' => Some('𝜚'),
        'ϖ' => Some('𝜛'),
        _ => None,
    }
}

/// Every character [`to_runs`] can put on screen. The app's font test
/// walks it: a glyph missing from the reading family renders as a
/// replacement box, so the bundled math subset must cover the whole
/// inventory (and the tables must not grow past it).
pub fn glyphs() -> String {
    let mut out = String::from(STANDALONE);
    // whatever a note's own characters are, they pass through: the
    // printable ASCII a formula is written in has to be drawable, and
    // so does the italic of every letter that leans
    for c in ' '..='~' {
        out.push(c);
        if let Some(italic) = leaning(c) {
            out.push(italic);
        }
    }
    for (_, s) in SYMBOLS {
        out.push_str(s);
        out.extend(s.chars().filter_map(leaning));
    }
    for (_, c) in ACCENTS {
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
    runs: Vec<Piece>,
}

impl Runs {
    /// Append `text` at `level`. `upright` forces the whole string
    /// upright: a function name or `\text{…}` is a word, not a product
    /// of variables.
    fn push(&mut self, text: &str, style: Style, upright: bool) {
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
            self.push_raw(c, style, !upright && c.is_alphabetic());
        }
    }

    /// Take `runs` as they are. `upright` flattens their setting — a
    /// wrapper that makes a word of its argument.
    fn extend(&mut self, runs: Vec<Piece>, upright: bool) {
        for r in runs {
            let style = r.style;
            for c in r.text.chars() {
                if is_combining(c)
                    && let Some(last) = self.runs.last_mut()
                {
                    last.text.push(c);
                    continue;
                }
                self.push_raw(c, style, r.italic && !upright);
            }
        }
    }

    /// Append one character with its setting already decided — the
    /// rebuild after [`tidy`], which must not re-derive what it kept.
    fn push_raw(&mut self, c: char, style: Style, italic: bool) {
        match self.runs.last_mut() {
            Some(r) if r.style == style && r.italic == italic => r.text.push(c),
            _ => self.runs.push(Piece {
                text: c.to_string(),
                style,
                italic,
            }),
        }
    }

    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Characters the parser emits on its own, outside any table: the radical
/// sign and the fraction slash, the parens a wide argument gets, and the
/// two spaces `\,` and `\quad` become.
const STANDALONE: &str = "√/()C,\u{2003}\u{2009}";

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
    /// Where what we are reading sits: the main line, or a script of a
    /// script of it.
    style: Style,
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
                '^' => self.script(out, Style::sup),
                '_' => self.script(out, Style::sub),
                '&' => out.push(self.col, self.style, true),
                // TeX whitespace: source line breaks and tabs are spaces,
                // `~` is a space that doesn't break. Only `\\` ends a line.
                '~' | '\n' | '\r' | '\t' => out.push(" ", self.style, true),
                _ => out.push(&c.to_string(), self.style, false),
            }
        }
    }

    /// A `^` or `_` was consumed: read its argument one step off the
    /// line we are on. The step is relative, so `e^{z_i}`'s index lands
    /// below the exponent rather than below the line.
    fn script(&mut self, out: &mut Runs, step: fn(Style) -> Style) {
        let outer = self.style;
        self.style = step(outer);
        let body = self.arg();
        self.style = outer;
        out.extend(body, false);
    }

    /// One argument: a `{…}` group, a `\command`, or a single character.
    fn arg(&mut self) -> Vec<Piece> {
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
                out.push(&c.to_string(), self.style, false);
            }
            None => {}
        }
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
                    ',' | ':' | ';' => out.push("\u{2009}", self.style, true), // thin space
                    '!' => {}
                    '|' => out.push("‖", self.style, true),
                    '\\' => out.push("\n", self.style, true),
                    // \{ \} \$ \% \& \# \_ and a lone \
                    _ => out.push(&c.to_string(), self.style, true),
                }
            } else {
                out.push("\\", self.style, true);
            }
            return;
        }
        let name: String = self.s[start..self.i].iter().collect();
        // accents: combining mark after a single-char base
        if let Some(mark) = accent_mark(&name) {
            let base = self.arg();
            out.push(&accent(&joined(&base), mark), self.style, false);
            return;
        }
        match name.as_str() {
            // wrappers that make a WORD of their argument: it stands
            // upright, the way `\text{if}` is a word and not i·f
            "text" | "textrm" | "textbf" | "textsf" | "texttt" | "mathrm" | "mathsf" | "mathtt"
            | "mbox" | "operatorname" => {
                let arg = self.arg();
                out.extend(arg, true);
            }
            // wrappers that only change weight or shape: the argument
            // keeps whatever setting its own characters ask for
            "textit" | "mathbf" | "mathit" | "mathcal" | "mathfrak" | "mathscr" | "boldsymbol"
            | "bm" | "pmb" | "overbrace" | "underbrace" => {
                let arg = self.arg();
                out.extend(arg, false);
            }
            // `\mathbb{R}` has a letter of its own
            "mathbb" => {
                let arg = joined(&self.arg());
                out.push(&blackboard(&arg), self.style, true);
            }
            // named functions are set upright — the name IS the rendering
            _ if FUNCTIONS.contains(&name.as_str()) => out.push(&name, self.style, true),
            "pmod" => {
                let arg = self.arg();
                out.push(" (mod ", self.style, true);
                out.extend(arg, false);
                out.push(")", self.style, true);
            }
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let (a, b) = (self.arg(), self.arg());
                out.extend(parens_if_wide(a), false);
                out.push("/", self.style, true);
                out.extend(parens_if_wide(b), false);
            }
            "binom" | "dbinom" | "tbinom" => {
                let (n, k) = (self.arg(), self.arg());
                out.push("C(", self.style, true);
                out.extend(n, false);
                out.push(", ", self.style, true);
                out.extend(k, false);
                out.push(")", self.style, true);
            }
            "sqrt" => {
                // `\sqrt[3]{x}`: the index rides as a superscript when it
                // maps, and stays bracketed when it doesn't
                if self.s.get(self.i) == Some(&'[') {
                    self.i += 1;
                    let outer = self.style;
                    self.style = outer.sup();
                    let mut index = Runs::default();
                    self.seq(Some(']'), &mut index);
                    self.style = outer;
                    out.extend(index.runs, false);
                }
                out.push("√", self.style, true);
                if self.s.get(self.i) == Some(&'{') {
                    let arg = self.arg();
                    out.extend(parens_if_wide(arg), false);
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
                out.push(" ", self.style, true);
            }
            "quad" => out.push("\u{2003}", self.style, true),
            "qquad" => out.push("\u{2003}\u{2003}", self.style, true),
            _ => match symbol(&name) {
                Some(s) => out.push(s, self.style, false),
                None => {
                    // verbatim: the reader sees exactly what they wrote
                    out.push("\\", self.style, true);
                    out.push(&name, self.style, true);
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
            style: self.style,
        };
        let mut rows = Runs::default();
        sub.seq(None, &mut rows);
        // a stack of rows is a block: it starts on its own line when
        // something already precedes it (`x = \begin{cases}…`)
        if rows.text().contains('\n') && !out.text().trim().is_empty() {
            out.push("\n", self.style, true);
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
fn tidy(runs: Vec<Piece>) -> Vec<Piece> {
    let math_space = |c: char| c == '\u{2003}' || c == '\u{2009}';
    let chars = runs
        .iter()
        .flat_map(|r| r.text.chars().map(move |c| (c, r.style, r.italic)));
    let mut out: Vec<(char, Style, bool)> = Vec::new();
    let mut line_start = 0usize;
    let mut pending: Option<(Style, bool)> = None;
    for (c, style, italic) in chars {
        if c == '\n' {
            pending = None;
            if out.len() == line_start {
                continue; // a line with nothing on it is not a line
            }
            out.push(('\n', Style::BASE, false));
            line_start = out.len();
            continue;
        }
        if c == ' ' || c == '\t' {
            pending = Some((style, italic));
            continue;
        }
        if let Some((ss, si)) = pending.take()
            && out.len() > line_start
            && !out.last().is_some_and(|&(p, _, _)| math_space(p))
            && !math_space(c)
        {
            out.push((' ', ss, si));
        }
        out.push((c, style, italic));
    }
    while out.last().is_some_and(|&(c, _, _)| c == '\n') {
        out.pop();
    }
    let mut rebuilt = Runs::default();
    for (c, style, italic) in out {
        rebuilt.push_raw(c, style, italic);
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
fn joined(runs: &[Piece]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// Combining marks ride the character before them — the accents this
/// module places, and any the author typed.
fn is_combining(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036f}' | '\u{20d0}'..='\u{20ff}')
}

/// `(x)` when `x` is more than one glyph, `x` alone otherwise.
fn parens_if_wide(runs: Vec<Piece>) -> Vec<Piece> {
    if joined(&runs).chars().nth(1).is_none() {
        return runs;
    }
    let style = runs.first().map_or(Style::BASE, |r| r.style);
    let paren = |t: &str, style: Style| Piece {
        text: t.to_string(),
        style,
        italic: false,
    };
    let mut out = vec![paren("(", style)];
    out.extend(runs);
    out.push(paren(")", style));
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
    // U+25A1, not the U+25FB that would otherwise do: epaint draws
    // U+25FB as its replacement box, and a face holding the replacement
    // character becomes the family's replacement face — after which
    // nothing in it reports as drawable
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
    /// `^{…}`/`_{…}` around a raised or lowered stretch. Leaning letters
    /// are the Mathematical Alphanumeric characters themselves, so they
    /// need no marker — an assertion showing `𝛿` is showing exactly what
    /// gets drawn.
    ///
    /// It rebuilds the nesting from the runs' scale and rise — every run
    /// carries an ABSOLUTE setting, and this walks back to the tree that
    /// produced it. A run whose setting is neither the current one nor a
    /// step off it means the ladder is broken, and this panics rather
    /// than paper over it.
    fn show(tex: &str) -> String {
        let mut out = String::new();
        let mut stack = vec![super::Style::BASE];
        for run in to_runs(tex) {
            let style = super::Style {
                scale: run.scale,
                rise: run.rise,
            };
            while *stack.last().expect("the base never pops") != style {
                let top = *stack.last().expect("the base never pops");
                if style == top.sup() {
                    out.push_str("^{");
                    stack.push(style);
                } else if style == top.sub() {
                    out.push_str("_{");
                    stack.push(style);
                } else {
                    assert!(stack.len() > 1, "{style:?} is off the ladder in {tex:?}");
                    stack.pop();
                    out.push('}');
                }
            }
            out.push_str(&run.text);
        }
        out.push_str(&"}".repeat(stack.len() - 1));
        out
    }

    #[test]
    fn plain_symbols_and_greek() {
        assert_eq!(show(r"\delta = 2"), "𝛿 = 2");
        assert_eq!(show(r"\alpha \to \beta"), "𝛼 → 𝛽");
        assert_eq!(show(r"\forall x \in S"), "∀ 𝑥 ∈ 𝑆");
    }

    /// TeX's math italic: a letter is a variable and leans, a digit or
    /// an operator stands, and a function name is a word — which is what
    /// tells `log` from three letters multiplied together.
    #[test]
    fn letters_lean_and_everything_else_stands() {
        assert_eq!(show(r"2x + 3y = 0"), "2𝑥 + 3𝑦 = 0");
        assert_eq!(show(r"\log n"), "log 𝑛");
        assert_eq!(show(r"\text{if } x > 0"), "if 𝑥 > 0");
        assert_eq!(show(r"\mathrm{d}x"), "d𝑥");
    }

    /// A script is a SETTING, not a spelled-out character. Unicode has
    /// superscript forms for some characters and none for the rest, so
    /// spelling them meant `x²` here and `e^(zᵢ)` there — a caret and
    /// parentheses on the page the moment one character in a script had
    /// no script form, which for `e^{z_i}` is every time.
    #[test]
    fn scripts_are_a_setting_and_nest_as_deep_as_the_formula() {
        assert_eq!(show(r"x^2 + y_i"), "𝑥^{2} + 𝑦_{𝑖}");
        assert_eq!(show(r"e^{i\pi}"), "𝑒^{𝑖𝜋}");
        assert_eq!(show(r"\sum_{i=0}^{n} x_i"), "∑_{𝑖=0}^{𝑛} 𝑥_{𝑖}");
        assert_eq!(show(r"A^T"), "𝐴^{𝑇}");
        // a script of a script: nowhere Unicode can go, and where the
        // old spelling gave up
        assert_eq!(show(r"e^{z_i}"), "𝑒^{𝑧_{𝑖}}");
        assert_eq!(show(r"e^{z_{\pi}}"), "𝑒^{𝑧_{𝜋}}");
    }

    /// TeX's ladder: each step is smaller than the last and moves less
    /// far than the step before it, and it stops at scriptscript so a
    /// deeply nested index is still legible.
    #[test]
    fn the_script_ladder_narrows_and_then_stops() {
        let runs = to_runs(r"a^{b^{c^{d}}}");
        let scales: Vec<f32> = runs.iter().map(|r| r.scale).collect();
        assert_eq!(
            scales,
            vec![1.0, super::SCRIPT, super::SCRIPTSCRIPT, super::SCRIPTSCRIPT]
        );
        let rises: Vec<f32> = runs.iter().map(|r| r.rise).collect();
        assert_eq!(rises[0], 0.0);
        for pair in rises.windows(2) {
            assert!(pair[1] < pair[0], "each step rides higher than the last");
        }
        for i in 1..rises.len() - 1 {
            assert!(
                rises[i] - rises[i + 1] < rises[i - 1] - rises[i] + f32::EPSILON,
                "and the steps get shorter"
            );
        }
        // a subscript inside a superscript comes back DOWN, but stays
        // above the line it started from
        let inner = to_runs(r"e^{z_i}").last().expect("the index").rise;
        assert!(inner < 0.0, "the index of an exponent is still up there");
    }

    #[test]
    fn fractions_roots_and_wrappers() {
        assert_eq!(show(r"\frac{1}{2}"), "1/2");
        assert_eq!(show(r"\frac{a+b}{c}"), "(𝑎+𝑏)/𝑐");
        // a fraction keeps the setting of what is inside it
        assert_eq!(show(r"\frac{\sin x}{x}"), "(sin 𝑥)/𝑥");
        assert_eq!(show(r"\sqrt{x+1}"), "√(𝑥+1)");
        assert_eq!(show(r"\sqrt[3]{x}"), "^{3}√𝑥");
        assert_eq!(show(r"\sqrt[n+1]{x}"), "^{𝑛+1}√𝑥");
        assert_eq!(show(r"\binom{n}{k}"), "C(𝑛, 𝑘)");
        assert_eq!(show(r"\mathbb{R}^n"), "ℝ^{𝑛}");
    }

    /// An accent has to stay in its base's run, or it is laid out on its
    /// own and stops sitting on the letter it belongs to.
    #[test]
    fn accents_ride_single_char_bases() {
        assert_eq!(show(r"\vec{x}"), "𝑥\u{20d7}");
        assert_eq!(show(r"\hat{y} = \bar{x}"), "𝑦\u{0302} = 𝑥\u{0304}");
        assert_eq!(to_runs(r"\vec{x}").len(), 1);
    }

    #[test]
    fn unknown_commands_stay_verbatim() {
        assert_eq!(show(r"\foobar + 1"), r"\foobar + 1");
        // grouping braces disappear, the name does not
        assert_eq!(show(r"\undefinedcmd{x}"), r"\undefinedcmd𝑥");
    }

    #[test]
    fn sizing_noise_drops_and_delimiters_stay() {
        assert_eq!(show(r"\left( \frac{1}{2} \right)"), "( 1/2 )");
        assert_eq!(show(r"\langle u, v \rangle"), "⟨ 𝑢, 𝑣 ⟩");
        // an invisible delimiter takes its dot with it
        assert_eq!(show(r"\left\{ x \right."), "{ 𝑥");
    }

    #[test]
    fn named_functions_lose_only_their_backslash() {
        assert_eq!(show(r"\sin x + \cos y"), "sin 𝑥 + cos 𝑦");
        assert_eq!(
            show(r"\lim_{x \to 0} \frac{\sin x}{x} = 1"),
            "lim_{𝑥 → 0} (sin 𝑥)/𝑥 = 1"
        );
        assert_eq!(show(r"\log_2 n"), "log_{2} 𝑛");
    }

    /// A display block wraps where the author wrapped the source, and TeX
    /// does not care: only `\\` ends a line. The whole span used to come
    /// through with the newlines the `$$` fences left on it, so every
    /// equation drew with a blank line above and below.
    #[test]
    fn source_line_breaks_are_spaces_and_only_a_double_backslash_breaks() {
        assert_eq!(show("\nE = mc^2\n"), "𝐸 = 𝑚𝑐^{2}");
        assert_eq!(show("a\n= b"), "𝑎 = 𝑏");
        assert_eq!(show(r"a \\ b"), "𝑎\n𝑏");
        assert_eq!(show("x    +     y"), "𝑥 + 𝑦");
        // a minted space swallows the ordinary ones beside it
        assert_eq!(show(r"\int_0^1 x^2 \, dx"), "∫_{0}^{1} 𝑥^{2}\u{2009}𝑑𝑥");
    }

    #[test]
    fn environments_become_rows() {
        assert_eq!(
            show("\\begin{aligned}\na &= b \\\\\nc &= d\n\\end{aligned}"),
            "𝑎 = 𝑏\n𝑐 = 𝑑"
        );
        assert_eq!(
            show(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "𝑎\u{2003}𝑏\n𝑐\u{2003}𝑑"
        );
        // an environment that follows something starts its own line
        assert_eq!(
            show(r"x = \begin{cases} 1 & p \\ 0 & q \end{cases}"),
            "𝑥 =\n1\u{2003}𝑝\n0\u{2003}𝑞"
        );
        // a matrix inside cases restores the outer environment's `&`
        assert_eq!(
            show(r"\begin{aligned} a &= \begin{matrix} 1 & 2 \end{matrix} \end{aligned}"),
            "𝑎 = 1\u{2003}2"
        );
        // an unclosed environment still draws its rows
        assert_eq!(show(r"\begin{aligned} a &= b"), "𝑎 = 𝑏");
    }

    /// Nothing may reach the screen that the bundled math font can't
    /// draw — the app-side companion test holds the font to this list.
    #[test]
    fn the_glyph_inventory_covers_every_table() {
        let g = glyphs();
        for c in ['ℝ', 'α', '𝛼', '∑', '√', '\u{20d7}', '⟹', '𝑥', '𝐴', 'ℎ'] {
            assert!(g.contains(c), "{c:?} is missing from the inventory");
        }
        // an unrecognized command reaches the reader as its own TeX, so
        // the backslash and the letters spelling it have to be drawable
        assert!(g.contains('\\'), "verbatim TeX needs its backslash");
        // …and the inventory is what the tables can EMIT, so a leaning
        // letter is listed as the character it actually becomes
        assert_eq!(
            to_runs("x").first().map(|r| r.text.as_str()),
            Some("𝑥"),
            "a variable is the italic character, not a styling flag"
        );
    }
}
