//! Best-effort TeX-to-Unicode for math spans in rendered notes.
//!
//! The markdown renderer parses `$…$` / `$$…$$` and delegates drawing to
//! the app; there is no TeX engine anywhere in the stack, and none is
//! wanted (offline, deterministic, no dependencies — the same trade web
//! nodes made against favicon fetching). This module converts the
//! common note-taking subset to plain Unicode text: greek letters,
//! operators, relations, arrows, big operators, super/subscripts,
//! `\frac`, `\sqrt`, accents, named functions (`\sin`, `\log`, `\lim`),
//! `\begin{…}` environments and `\text`-style wrappers.
//!
//! Whitespace follows TeX, not the source: a newline inside a span is
//! just a space, runs of spaces collapse, and only `\\` breaks a line.
//! A display block therefore renders as its rows, with no blank lines
//! from the way the author happened to wrap the source.
//!
//! The honesty rule: anything unrecognized keeps its `\name` verbatim,
//! and a script that can't be fully mapped falls back to `^(…)`/`_(…)`
//! — partial prettiness must never hide what the author wrote. Bare
//! braces are TeX grouping and disappear.
//!
//! Every character the tables below can emit has to be drawable, or a
//! converted span reads as a row of replacement boxes. [`glyphs`]
//! enumerates the whole inventory so the app side can hold the fonts it
//! renders with to it.

/// Convert one math span (the text between the dollars) to display text.
pub fn to_unicode(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = Parser {
        s: &chars,
        i: 0,
        col: ALIGN_TAB,
    };
    tidy(&p.seq(None))
}

/// Every character [`to_unicode`] can put on screen. The app's font test
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
}

impl Parser<'_> {
    /// Consume until the end (or a closing brace when inside a group).
    fn seq(&mut self, until: Option<char>) -> String {
        let mut out = String::new();
        while let Some(&c) = self.s.get(self.i) {
            if Some(c) == until {
                self.i += 1;
                break;
            }
            self.i += 1;
            match c {
                '\\' => self.command(&mut out),
                '{' => out.push_str(&self.seq(Some('}'))),
                '}' => {} // stray closer: grouping, not content
                '^' => script(&mut out, &self.arg(), SUPERS, '^'),
                '_' => script(&mut out, &self.arg(), SUBS, '_'),
                '&' => out.push_str(self.col),
                // TeX whitespace: source line breaks and tabs are spaces,
                // `~` is a space that doesn't break. Only `\\` ends a line.
                '~' | '\n' | '\r' | '\t' => out.push(' '),
                _ => out.push(c),
            }
        }
        out
    }

    /// One argument: a `{…}` group, a `\command`, or a single character.
    fn arg(&mut self) -> String {
        match self.s.get(self.i) {
            Some('{') => {
                self.i += 1;
                self.seq(Some('}'))
            }
            Some('\\') => {
                self.i += 1;
                let mut out = String::new();
                self.command(&mut out);
                out
            }
            Some(&c) => {
                self.i += 1;
                c.to_string()
            }
            None => String::new(),
        }
    }

    /// A `\` was consumed: read the command and emit its expansion.
    fn command(&mut self, out: &mut String) {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i == start {
            // escaped single character: `\{`, `\\`, `\,` …
            if let Some(&c) = self.s.get(self.i) {
                self.i += 1;
                match c {
                    ',' | ':' | ';' => out.push('\u{2009}'), // thin space
                    '!' => {}
                    '|' => out.push('‖'),
                    '\\' => out.push('\n'),
                    _ => out.push(c), // \{ \} \$ \% \& \# \_ and a lone \
                }
            } else {
                out.push('\\');
            }
            return;
        }
        let name: String = self.s[start..self.i].iter().collect();
        // accents: combining mark after a single-char base
        if let Some(mark) = accent_mark(&name) {
            accent(out, &self.arg(), mark);
            return;
        }
        match name.as_str() {
            // wrappers: the contents are the content
            "text" | "textrm" | "textit" | "textbf" | "textsf" | "texttt" | "mathrm" | "mathbf"
            | "mathit" | "mathsf" | "mathtt" | "mathcal" | "mathfrak" | "mathscr"
            | "boldsymbol" | "bm" | "pmb" | "mbox" | "operatorname" | "overbrace"
            | "underbrace" => out.push_str(&self.arg()),
            // `\mathbb{R}` has a letter of its own
            "mathbb" => {
                let arg = self.arg();
                out.push_str(&blackboard(&arg));
            }
            // named functions are set upright — the name IS the rendering
            _ if FUNCTIONS.contains(&name.as_str()) => out.push_str(&name),
            "pmod" => {
                let arg = self.arg();
                out.push_str(&format!(" (mod {arg})"));
            }
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let (a, b) = (self.arg(), self.arg());
                out.push_str(&parens_if_wide(&a));
                out.push('/');
                out.push_str(&parens_if_wide(&b));
            }
            "binom" | "dbinom" | "tbinom" => {
                let (n, k) = (self.arg(), self.arg());
                out.push_str(&format!("C({n}, {k})"));
            }
            "sqrt" => {
                // `\sqrt[3]{x}`: the index rides as a superscript when it
                // maps, and stays bracketed when it doesn't
                if self.s.get(self.i) == Some(&'[') {
                    self.i += 1;
                    let index = self.seq(Some(']'));
                    match map_script(&index, SUPERS) {
                        Some(m) => out.push_str(&m),
                        None => out.push_str(&format!("[{index}]")),
                    }
                }
                out.push('√');
                if self.s.get(self.i) == Some(&'{') {
                    out.push_str(&parens_if_wide(&self.arg()));
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
                out.push(' ');
            }
            "quad" => out.push('\u{2003}'),
            "qquad" => out.push_str("\u{2003}\u{2003}"),
            _ => match symbol(&name) {
                Some(s) => out.push_str(s),
                None => {
                    // verbatim: the reader sees exactly what they wrote
                    out.push('\\');
                    out.push_str(&name);
                }
            },
        }
    }

    /// `\begin{env}` was just read: draw the rows up to its `\end`.
    ///
    /// Environments are read as a WHOLE (a sub-parser over the body)
    /// rather than by flipping a flag, so a matrix nested in a `cases`
    /// restores the outer environment's `&` when it closes.
    fn environment(&mut self, out: &mut String) {
        let env = self.arg();
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
        };
        let rows = sub.seq(None);
        // a stack of rows is a block: it starts on its own line when
        // something already precedes it (`x = \begin{cases}…`)
        if rows.contains('\n') && !out.trim().is_empty() {
            out.push('\n');
        }
        out.push_str(&rows);
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
fn tidy(s: &str) -> String {
    let math_space = |c: char| c == '\u{2003}' || c == '\u{2009}';
    let mut lines: Vec<String> = Vec::new();
    for line in s.split('\n') {
        let mut out = String::new();
        let mut pending = false;
        for c in line.chars() {
            if c == ' ' || c == '\t' {
                pending = true;
                continue;
            }
            if pending {
                if !out.is_empty() && !out.ends_with(math_space) && !math_space(c) {
                    out.push(' ');
                }
                pending = false;
            }
            out.push(c);
        }
        if !out.is_empty() {
            lines.push(out);
        }
    }
    lines.join("\n")
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

/// `(x)` when `x` is more than one glyph, `x` alone otherwise.
fn parens_if_wide(s: &str) -> String {
    if s.chars().nth(1).is_some() {
        format!("({s})")
    } else {
        s.to_string()
    }
}

fn accent_mark(name: &str) -> Option<char> {
    ACCENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, mark)| *mark)
}

fn accent(out: &mut String, base: &str, mark: char) {
    out.push_str(base);
    if base.chars().count() == 1 {
        out.push(mark);
    }
}

/// The script forms of `body`, or None when a character has none.
fn map_script(body: &str, table: &[(char, char)]) -> Option<String> {
    let mapped: Option<String> = body
        .chars()
        .map(|c| table.iter().find(|(p, _)| *p == c).map(|(_, m)| *m))
        .collect();
    mapped.filter(|m| !m.is_empty())
}

/// Emit a super/subscript: fully mapped when every character has a
/// script form, else the honest `^(…)` fallback.
fn script(out: &mut String, body: &str, table: &[(char, char)], op: char) {
    match map_script(body, table) {
        Some(m) => out.push_str(&m),
        None => {
            out.push(op);
            out.push_str(&parens_if_wide(body));
        }
    }
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
    use super::{glyphs, to_unicode};

    #[test]
    fn plain_symbols_and_greek() {
        assert_eq!(to_unicode(r"\delta = 2"), "δ = 2");
        assert_eq!(to_unicode(r"\alpha \to \beta"), "α → β");
        assert_eq!(to_unicode(r"\forall x \in S"), "∀ x ∈ S");
    }

    #[test]
    fn scripts_map_when_every_char_can() {
        assert_eq!(to_unicode(r"x^2 + y_i"), "x² + yᵢ");
        assert_eq!(to_unicode(r"\sum_{i=0}^{n} x_i"), "∑ᵢ₌₀ⁿ xᵢ");
        assert_eq!(to_unicode(r"A^T"), "Aᵀ");
        // π has no superscript form: the honest fallback keeps the TeX shape
        assert_eq!(to_unicode(r"e^{i\pi}"), "e^(iπ)");
    }

    #[test]
    fn fractions_roots_and_wrappers() {
        assert_eq!(to_unicode(r"\frac{1}{2}"), "1/2");
        assert_eq!(to_unicode(r"\frac{a+b}{c}"), "(a+b)/c");
        assert_eq!(to_unicode(r"\sqrt{x+1}"), "√(x+1)");
        assert_eq!(to_unicode(r"\sqrt[3]{x}"), "³√x");
        assert_eq!(to_unicode(r"\sqrt[n+1]{x}"), "ⁿ⁺¹√x");
        // an index with no script form keeps its brackets
        assert_eq!(to_unicode(r"\sqrt[\pi]{x}"), "[π]√x");
        assert_eq!(to_unicode(r"\binom{n}{k}"), "C(n, k)");
        assert_eq!(to_unicode(r"\text{if} x > 0"), "if x > 0");
        assert_eq!(to_unicode(r"\mathbb{R}^n"), "ℝⁿ");
    }

    #[test]
    fn accents_ride_single_char_bases() {
        assert_eq!(to_unicode(r"\vec{x}"), "x\u{20d7}");
        assert_eq!(to_unicode(r"\hat{y} = \bar{x}"), "y\u{0302} = x\u{0304}");
    }

    #[test]
    fn unknown_commands_stay_verbatim() {
        assert_eq!(to_unicode(r"\foobar + 1"), r"\foobar + 1");
        // grouping braces disappear, the name does not
        assert_eq!(to_unicode(r"\undefinedcmd{x}"), r"\undefinedcmdx");
    }

    #[test]
    fn sizing_noise_drops_and_delimiters_stay() {
        assert_eq!(to_unicode(r"\left( \frac{1}{2} \right)"), "( 1/2 )");
        assert_eq!(to_unicode(r"\langle u, v \rangle"), "⟨ u, v ⟩");
        // an invisible delimiter takes its dot with it
        assert_eq!(to_unicode(r"\left\{ x \right."), "{ x");
    }

    #[test]
    fn named_functions_lose_only_their_backslash() {
        assert_eq!(to_unicode(r"\sin x + \cos y"), "sin x + cos y");
        assert_eq!(
            to_unicode(r"\lim_{x \to 0} \frac{\sin x}{x} = 1"),
            "lim_(x → 0) (sin x)/x = 1"
        );
        assert_eq!(to_unicode(r"\log_2 n"), "log₂ n");
    }

    /// A display block wraps where the author wrapped the source, and TeX
    /// does not care: only `\\` ends a line. The whole span used to come
    /// through with the newlines the `$$` fences left on it, so every
    /// equation drew with a blank line above and below.
    #[test]
    fn source_line_breaks_are_spaces_and_only_a_double_backslash_breaks() {
        assert_eq!(to_unicode("\nE = mc^2\n"), "E = mc²");
        assert_eq!(to_unicode("a\n= b"), "a = b");
        assert_eq!(to_unicode(r"a \\ b"), "a\nb");
        assert_eq!(to_unicode("x    +     y"), "x + y");
        // a minted space swallows the ordinary ones beside it
        assert_eq!(to_unicode(r"\int_0^1 x^2 \, dx"), "∫₀¹ x²\u{2009}dx");
    }

    #[test]
    fn environments_become_rows() {
        assert_eq!(
            to_unicode("\\begin{aligned}\na &= b \\\\\nc &= d\n\\end{aligned}"),
            "a = b\nc = d"
        );
        assert_eq!(
            to_unicode(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "a\u{2003}b\nc\u{2003}d"
        );
        // an environment that follows something starts its own line
        assert_eq!(
            to_unicode(r"x = \begin{cases} 1 & p \\ 0 & q \end{cases}"),
            "x =\n1\u{2003}p\n0\u{2003}q"
        );
        // a matrix inside cases restores the outer environment's `&`
        assert_eq!(
            to_unicode(r"\begin{aligned} a &= \begin{matrix} 1 & 2 \end{matrix} \end{aligned}"),
            "a = 1\u{2003}2"
        );
        // an unclosed environment still draws its rows
        assert_eq!(to_unicode(r"\begin{aligned} a &= b"), "a = b");
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
