//! Best-effort TeX-to-Unicode for math spans in rendered notes.
//!
//! The markdown renderer parses `$…$` / `$$…$$` and delegates drawing to
//! the app; there is no TeX engine anywhere in the stack, and none is
//! wanted (offline, deterministic, no dependencies — the same trade web
//! nodes made against favicon fetching). This module converts the
//! common note-taking subset to plain Unicode text: greek letters,
//! operators, relations, arrows, big operators, super/subscripts,
//! `\frac`, `\sqrt`, accents, and `\text`-style wrappers.
//!
//! The honesty rule: anything unrecognized keeps its `\name` verbatim,
//! and a script that can't be fully mapped falls back to `^(…)`/`_(…)`
//! — partial prettiness must never hide what the author wrote. Bare
//! braces are TeX grouping and disappear.

/// Convert one math span (the text between the dollars) to display text.
pub fn to_unicode(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = Parser { s: &chars, i: 0 };
    p.seq(None)
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
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
        match name.as_str() {
            // wrappers: the contents are the content
            "text" | "textrm" | "textit" | "textbf" | "mathrm" | "mathbf" | "mathit" | "mathsf"
            | "mathtt" | "mathcal" | "mathbb" | "mathfrak" | "mathscr" | "boldsymbol"
            | "operatorname" => out.push_str(&self.arg()),
            // accents: combining mark after a single-char base
            "vec" => accent(out, &self.arg(), '\u{20d7}'),
            "hat" => accent(out, &self.arg(), '\u{0302}'),
            "bar" => accent(out, &self.arg(), '\u{0304}'),
            "tilde" => accent(out, &self.arg(), '\u{0303}'),
            "dot" => accent(out, &self.arg(), '\u{0307}'),
            "ddot" => accent(out, &self.arg(), '\u{0308}'),
            "frac" | "dfrac" | "tfrac" => {
                let (a, b) = (self.arg(), self.arg());
                out.push_str(&parens_if_wide(&a));
                out.push('/');
                out.push_str(&parens_if_wide(&b));
            }
            "sqrt" => {
                out.push('√');
                if self.s.get(self.i) == Some(&'{') {
                    out.push_str(&parens_if_wide(&self.arg()));
                }
            }
            // sizing/style noise: drop, the delimiter itself follows
            "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "limits" | "nolimits"
            | "displaystyle" | "textstyle" | "mathstrut" => {}
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
}

/// `(x)` when `x` is more than one glyph, `x` alone otherwise.
fn parens_if_wide(s: &str) -> String {
    if s.chars().nth(1).is_some() {
        format!("({s})")
    } else {
        s.to_string()
    }
}

fn accent(out: &mut String, base: &str, mark: char) {
    out.push_str(base);
    if base.chars().count() == 1 {
        out.push(mark);
    }
}

/// Emit a super/subscript: fully mapped when every character has a
/// script form, else the honest `^(…)` fallback.
fn script(out: &mut String, body: &str, table: &[(char, char)], op: char) {
    let mapped: Option<String> = body
        .chars()
        .map(|c| table.iter().find(|(p, _)| *p == c).map(|(_, m)| *m))
        .collect();
    match mapped {
        Some(m) if !m.is_empty() => out.push_str(&m),
        _ => {
            out.push(op);
            out.push_str(&parens_if_wide(body));
        }
    }
}

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
    Some(match name {
        // greek, lower
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" | "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "ϕ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        // greek, upper
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // operators
        "times" => "×",
        "cdot" => "·",
        "pm" => "±",
        "mp" => "∓",
        "div" => "÷",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "bullet" => "•",
        "oplus" => "⊕",
        "ominus" => "⊖",
        "otimes" => "⊗",
        "odot" => "⊙",
        "cap" => "∩",
        "cup" => "∪",
        "setminus" => "∖",
        "wedge" | "land" => "∧",
        "vee" | "lor" => "∨",
        "neg" | "lnot" => "¬",
        // relations
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "equiv" => "≡",
        "approx" => "≈",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "propto" => "∝",
        "ll" => "≪",
        "gg" => "≫",
        "prec" => "≺",
        "succ" => "≻",
        "subset" => "⊂",
        "supset" => "⊃",
        "subseteq" => "⊆",
        "supseteq" => "⊇",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "perp" | "bot" => "⊥",
        "top" => "⊤",
        "parallel" => "∥",
        "mid" => "∣",
        "vdash" => "⊢",
        "dashv" => "⊣",
        "models" => "⊨",
        // arrows
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔",
        "mapsto" => "↦",
        "implies" => "⟹",
        "iff" => "⟺",
        "uparrow" => "↑",
        "downarrow" => "↓",
        "hookrightarrow" => "↪",
        "rightsquigarrow" => "⇝",
        // big operators
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "int" => "∫",
        "iint" => "∬",
        "oint" => "∮",
        "bigcup" => "⋃",
        "bigcap" => "⋂",
        // misc
        "infty" => "∞",
        "partial" => "∂",
        "nabla" => "∇",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "emptyset" | "varnothing" => "∅",
        "aleph" => "ℵ",
        "ell" => "ℓ",
        "hbar" => "ℏ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "wp" => "℘",
        "dots" | "ldots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "angle" => "∠",
        "triangle" => "△",
        "square" | "Box" => "□",
        "diamond" => "⋄",
        "prime" => "′",
        "degree" => "°",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "therefore" => "∴",
        "because" => "∵",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::to_unicode;

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
        assert_eq!(to_unicode(r"\text{if} x > 0"), "if x > 0");
        assert_eq!(to_unicode(r"\mathbb{R}^n"), "Rⁿ");
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
    }
}
