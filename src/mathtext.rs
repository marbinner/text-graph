//! Best-effort TeX-to-boxes for math spans in rendered notes.
//!
//! The markdown renderer parses `$…$` / `$$…$$` and delegates drawing to
//! the app; there is no TeX engine anywhere in the stack, and none is
//! wanted (offline, deterministic, no dependencies — the same trade web
//! nodes made against favicon fetching). This module converts the common
//! note-taking subset: greek letters, operators, relations, arrows, big
//! operators, super/subscripts, `\frac`, `\sqrt`, accents, named
//! functions (`\sin`, `\log`, `\lim`), `\begin{…}` environments,
//! `\left…\right` fences and `\text`-style wrappers.
//!
//! The output is a [`Node`] TREE, because a formula is not a line of
//! text and every attempt to flatten it into one showed. A fraction
//! became `a/b` and then needed parentheses to stay true, so
//! `\frac{\pi^2}{6}` read `(𝜋²)/6`. A script had to be spelled with
//! whatever character Unicode happened to have, which gave `x²` here and
//! `e^(zᵢ)` there. Both are structure, not characters: the tree says
//! numerator OVER denominator and base WITH script, and `app/math.rs`
//! sets it — a rule between the two halves, a radical drawn to the
//! height of what it covers, delimiters that grow.
//!
//! What is still resolved into characters is italics, because there it
//! IS a character question: TeX leans variables and stands operators,
//! digits and function names, and Unicode's Mathematical Alphanumeric
//! block is a designed italic a math font draws properly — where a
//! renderer's italics flag only shears an upright glyph. So a `Text`
//! node holds exactly what to draw.
//!
//! Whitespace follows TeX, not the source: a newline inside a span is
//! just a space, runs of spaces collapse, and only `\\` breaks a row.
//! A display block therefore renders as its rows, with no blank lines
//! from the way the author happened to wrap the source.
//!
//! The honesty rule: anything unrecognized keeps its `\name` verbatim —
//! partial prettiness must never hide what the author wrote. Bare braces
//! are TeX grouping and disappear.
//!
//! Every character the tables below can emit has to be drawable, or a
//! converted span reads as a row of replacement boxes. [`glyphs`]
//! enumerates the whole inventory so the app side can hold the font it
//! renders with to it.

/// One node of a converted formula: the structure `app/math.rs` sets.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Characters to draw as they are — italics already resolved.
    Text(String),
    /// Horizontal space, in ems. TeX's, not the author's: see [`space`].
    Space(f32),
    /// A combining mark centred over what it marks. It cannot ride
    /// inside the text, because a mark is laid out at the base's ADVANCE
    /// and lands right of centre on anything but a perfectly square
    /// glyph.
    Accent { base: Box<Node>, mark: char },
    /// A horizontal sequence.
    Row(Vec<Node>),
    /// A base with what rides on it.
    Scripts {
        base: Box<Node>,
        sup: Option<Box<Node>>,
        sub: Option<Box<Node>>,
    },
    /// Numerator over denominator, with a rule between them.
    Frac { num: Box<Node>, den: Box<Node> },
    /// A radical over its body, with an optional index.
    Sqrt {
        index: Option<Box<Node>>,
        body: Box<Node>,
    },
    /// Delimiters that grow to whatever they hold. Either side may be
    /// empty — `\left. … \right)` is a real construction.
    Fence {
        open: String,
        close: String,
        body: Box<Node>,
    },
    /// Rows, stacked and centred: `\\` and the display environments.
    Stack(Vec<Node>),
    /// Cells in columns: matrices and `cases`.
    Grid(Vec<Vec<Node>>),
}

impl Node {
    /// Nothing to draw — an empty span, or one that converted to nothing.
    pub fn is_empty(&self) -> bool {
        match self {
            Node::Text(t) => t.is_empty(),
            Node::Space(_) => false,
            Node::Accent { base, .. } => base.is_empty(),
            Node::Row(v) | Node::Stack(v) => v.iter().all(Node::is_empty),
            Node::Grid(rows) => rows.iter().flatten().all(Node::is_empty),
            _ => false,
        }
    }

    /// The characters in the tree, in order. The structure is lost — this
    /// is for tests and for anything that only needs to know WHAT is in a
    /// formula, never for drawing it.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.write_text(&mut out);
        out
    }

    fn write_text(&self, out: &mut String) {
        match self {
            Node::Text(t) => out.push_str(t),
            Node::Space(_) => out.push(' '),
            Node::Accent { base, mark } => {
                base.write_text(out);
                out.push(*mark);
            }
            Node::Row(v) | Node::Stack(v) => v.iter().for_each(|n| n.write_text(out)),
            Node::Grid(rows) => rows.iter().flatten().for_each(|n| n.write_text(out)),
            Node::Scripts { base, sup, sub } => {
                base.write_text(out);
                for s in [sup, sub].into_iter().flatten() {
                    s.write_text(out);
                }
            }
            Node::Frac { num, den } => {
                num.write_text(out);
                den.write_text(out);
            }
            Node::Sqrt { index, body } => {
                if let Some(i) = index {
                    i.write_text(out);
                }
                out.push('√');
                body.write_text(out);
            }
            Node::Fence { open, close, body } => {
                out.push_str(open);
                body.write_text(out);
                out.push_str(close);
            }
        }
    }
}

/// Convert one math span (the text between the dollars) to a tree.
pub fn to_tree(tex: &str) -> Node {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = Parser {
        s: &chars,
        i: 0,
        tight: false,
    };
    p.rows(None)
}

/// Every character [`to_tree`] can put on screen. The app's font test
/// walks it: a glyph missing from the math family renders as a
/// replacement box, so the bundled subset must cover the whole inventory
/// (and the tables must not grow past it).
pub fn glyphs() -> String {
    let mut out = String::from(STANDALONE);
    // whatever a note's own characters are, they pass through: the
    // printable ASCII a formula is written in has to be drawable, and so
    // does the italic of every letter that leans
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

/// Characters the parser emits on its own, outside any table: the
/// radical sign, and the minus a keyboard hyphen becomes. Spacing is a
/// `Space` node with a width, never a space character.
const STANDALONE: &str = "√−";

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

/// The delimiters a matrix environment draws around itself.
fn matrix_fence(env: &str) -> Option<(&'static str, &'static str)> {
    Some(match env {
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "cases" => ("{", ""),
        _ => return None,
    })
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    /// Inside a script, where TeX drops the spacing between atoms.
    tight: bool,
}

/// A sequence under construction: characters accumulate into one `Text`
/// node until something with structure interrupts them, `&` starts a
/// cell and `\\` starts a row. What comes out is the narrowest node that
/// fits — a `Row` for ordinary math, a `Stack` once anything broke a
/// row, a `Grid` once anything broke a cell.
#[derive(Default)]
struct Seq {
    rows: Vec<Vec<Vec<Node>>>,
    cell: Vec<Node>,
    row: Vec<Vec<Node>>,
    text: String,
    /// Where the most recent atom starts in `text`. A script attaches to
    /// an ATOM, and an atom is as long as what put it there: one
    /// character of the formula's own text, but the whole of `\log` —
    /// `\log_2 n` subscripts the function, not its final `g`.
    atom: Option<usize>,
    /// What the last atom was, for [`space`]. None at the start of a
    /// cell, where nothing needs separating.
    last: Option<Class>,
    /// Inside a script, where TeX drops the spacing it would otherwise
    /// insert.
    tight: bool,
}

impl Seq {
    /// Add `s` as one atom of class `class`, with the space TeX puts in
    /// front of it. A note's own spaces inside `$…$` never get here:
    /// this table is the only thing that separates atoms, which is why
    /// `$a+b$` and `$a + b$` come out the same.
    fn push_atom(&mut self, s: &str, class: Class) {
        if s.is_empty() {
            return;
        }
        // a binary operator with nothing on its left is not binary
        let class = if class == Class::Bin && !binds(self.last) {
            Class::Ord
        } else {
            class
        };
        if let Some(left) = self.last {
            let gap = space(left, class, self.tight);
            if gap > 0.0 {
                self.flush_text();
                self.cell.push(Node::Space(gap));
            }
        }
        self.last = Some(class);
        self.atom = Some(self.text.len());
        self.text.push_str(s);
    }

    /// A word — a function name, a `\text{…}` — which TeX sets as one
    /// atom whatever it is made of.
    fn push_str(&mut self, s: &str) {
        let class = s.chars().next().map_or(Class::Ord, class_of);
        self.push_atom(s, class);
    }

    /// One character of the formula's own text — TeX's math italic
    /// applies here, and nowhere that already decided (a function name,
    /// a `\text{…}`, a symbol out of the table).
    fn push_char(&mut self, c: char) {
        // a combining mark belongs to the character before it
        if is_combining(c) && !self.text.is_empty() {
            self.text.push(c);
            return;
        }
        // TeX sets a binary minus as MINUS SIGN, not as the hyphen the
        // keyboard has — beside a `+` the short one reads as a dash
        let c = if c == '-' { '−' } else { c };
        let class = class_of(c);
        self.push_atom(&leaning(c).unwrap_or(c).to_string(), class);
    }

    /// Space the author asked for by name (`\,`, `\quad`), which is the
    /// only kind that survives — and it replaces whatever the table
    /// would have put there.
    fn push_space(&mut self, ems: f32) {
        self.flush_text();
        self.cell.push(Node::Space(ems));
        self.last = None;
    }

    fn push(&mut self, node: Node) {
        self.push_boxed(node, Class::Inner);
    }

    /// A built node — a fraction, a fence, a script — which spaces like
    /// its class says and can never be part of a run of text.
    fn push_boxed(&mut self, node: Node, class: Class) {
        if node.is_empty() {
            return;
        }
        if let Some(left) = self.last {
            let gap = space(left, class, self.tight);
            if gap > 0.0 {
                self.flush_text();
                self.cell.push(Node::Space(gap));
            }
        }
        self.last = Some(class);
        self.atom = None;
        self.flush_text();
        self.cell.push(node);
    }

    fn flush_text(&mut self) {
        self.atom = None;
        let text = std::mem::take(&mut self.text);
        if !text.is_empty() {
            self.cell.push(Node::Text(text));
        }
    }

    /// The node the last atom produced, taken back off — scripts attach
    /// to what precedes them.
    fn take_last(&mut self) -> Node {
        if let Some(at) = self.atom.take()
            && at < self.text.len()
        {
            return Node::Text(self.text.split_off(at));
        }
        self.flush_text();
        self.cell.pop().unwrap_or(Node::Text(String::new()))
    }

    fn end_cell(&mut self) {
        self.last = None;
        self.text = self.text.trim_end().to_string();
        self.flush_text();
        let cell = std::mem::take(&mut self.cell);
        self.row.push(cell);
    }

    fn end_row(&mut self) {
        self.end_cell();
        let row = std::mem::take(&mut self.row);
        self.rows.push(row);
    }

    fn finish(mut self) -> Node {
        self.end_row();
        let mut rows: Vec<Vec<Vec<Node>>> = self
            .rows
            .into_iter()
            .filter(|r| !r.iter().flatten().all(Node::is_empty))
            .collect();
        let gridded = rows.iter().any(|r| r.len() > 1);
        if gridded {
            return Node::Grid(
                rows.into_iter()
                    .map(|r| r.into_iter().map(row_node).collect())
                    .collect(),
            );
        }
        match rows.len() {
            0 => Node::Text(String::new()),
            1 => row_node(
                rows.pop()
                    .expect("just checked")
                    .into_iter()
                    .flatten()
                    .collect(),
            ),
            _ => Node::Stack(
                rows.into_iter()
                    .map(|r| row_node(r.into_iter().flatten().collect()))
                    .collect(),
            ),
        }
    }
}

/// One node for a list of rows: a lone row needs no `Stack` around it.
fn stack_node(mut rows: Vec<Node>) -> Node {
    match rows.len() {
        0 => Node::Text(String::new()),
        1 => rows.pop().expect("just checked"),
        _ => Node::Stack(rows),
    }
}

/// The cells of an alignment row, put back on one line with the gap the
/// alignment itself would have left.
fn tabbed_row(cells: Vec<Node>) -> Node {
    let mut out: Vec<Node> = Vec::new();
    for cell in cells {
        if !out.is_empty() {
            out.push(Node::Text(" ".into()));
        }
        out.push(cell);
    }
    row_node(out)
}

/// One node for a list of them: a lone child needs no `Row` around it.
fn row_node(mut nodes: Vec<Node>) -> Node {
    match nodes.len() {
        0 => Node::Text(String::new()),
        1 => nodes.pop().expect("just checked"),
        _ => Node::Row(nodes),
    }
}

impl Parser<'_> {
    /// Read a sequence up to `until` (or the end), as rows and cells.
    fn rows(&mut self, until: Option<char>) -> Node {
        let mut seq = Seq {
            tight: self.tight,
            ..Seq::default()
        };
        while let Some(&c) = self.s.get(self.i) {
            if Some(c) == until {
                self.i += 1;
                break;
            }
            self.i += 1;
            match c {
                '\\' => self.command(&mut seq),
                '{' => {
                    // grouping, not a construction: `{ab}` is an atom of
                    // whatever `ab` is, and spaces like it. Only
                    // something BUILT is Inner.
                    match self.rows(Some('}')) {
                        Node::Text(t) => {
                            let class = t.chars().next().map_or(Class::Ord, class_of);
                            seq.push_atom(&t, class);
                        }
                        node => seq.push(node),
                    }
                }
                '}' => {} // stray closer: grouping, not content
                '^' => self.script(&mut seq, true),
                '_' => self.script(&mut seq, false),
                '&' => seq.end_cell(),
                // whitespace in math mode is TeX's to decide, not the
                // author's: `space` puts it between atoms instead
                '~' | ' ' | '\n' | '\r' | '\t' => {}
                _ => seq.push_char(c),
            }
        }
        seq.finish()
    }

    /// A `^` or `_` was consumed: read its argument in script style and
    /// fold it onto the atom before it. The result spaces as its BASE
    /// does — `\sum_{i}` is still an operator, `x^2` still ordinary.
    fn script(&mut self, seq: &mut Seq, up: bool) {
        let class = seq.last.unwrap_or(Class::Ord);
        let base = seq.take_last();
        let outer = std::mem::replace(&mut self.tight, true);
        let arg = self.arg();
        self.tight = outer;
        let (sup, sub) = if up {
            (Some(arg), None)
        } else {
            (None, Some(arg))
        };
        seq.last = None;
        seq.push_boxed(attach(base, sup, sub), class);
    }

    /// One argument: a `{…}` group, a `\command`, or a single character.
    fn arg(&mut self) -> Node {
        match self.s.get(self.i) {
            Some('{') => {
                self.i += 1;
                self.rows(Some('}'))
            }
            Some('\\') => {
                self.i += 1;
                let mut seq = Seq {
                    tight: self.tight,
                    ..Seq::default()
                };
                self.command(&mut seq);
                seq.finish()
            }
            Some(&c) => {
                self.i += 1;
                Node::Text(leaning(c).unwrap_or(c).to_string())
            }
            None => Node::Text(String::new()),
        }
    }

    /// The argument's characters, for the decisions that are about text
    /// rather than structure — an accent's base.
    fn arg_text(&mut self) -> String {
        self.arg().text()
    }

    /// The argument as a NAME: an environment's, and nothing a reader
    /// sees. It has to be read back upright, because the parser leans
    /// every letter it meets and `pmatrix` is not `𝑝𝑚𝑎𝑡𝑟𝑖𝑥`.
    fn arg_name(&mut self) -> String {
        upright(&self.arg_text())
    }

    /// The argument exactly as typed — `\text{…}` is a word, so its
    /// letters do not lean and its spaces are not TeX's to remove.
    fn raw_arg(&mut self) -> String {
        if self.s.get(self.i) != Some(&'{') {
            return self.arg_name();
        }
        self.i += 1;
        let mut out = String::new();
        let mut depth = 1usize;
        while let Some(&c) = self.s.get(self.i) {
            self.i += 1;
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                // an escape puts its character through, whatever it is
                '\\' => {
                    if let Some(&next) = self.s.get(self.i) {
                        self.i += 1;
                        out.push(next);
                    }
                    continue;
                }
                _ => {}
            }
            if depth > 0 {
                out.push(c);
            }
        }
        out
    }

    /// A `\` was consumed: read the command and add what it draws.
    fn command(&mut self, seq: &mut Seq) {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i == start {
            // escaped single character: `\{`, `\\`, `\,` …
            if let Some(&c) = self.s.get(self.i) {
                self.i += 1;
                match c {
                    ',' | ':' | ';' => seq.push_space(3.0 / 18.0), // thin space
                    '!' => {}
                    '|' => seq.push_str("‖"),
                    '\\' => seq.end_row(),
                    ' ' => seq.push_space(4.0 / 18.0),
                    // \{ \} \$ \% \& \# \_ and a lone \
                    _ => seq.push_str(&c.to_string()),
                }
            } else {
                seq.push_str("\\");
            }
            return;
        }
        let name: String = self.s[start..self.i].iter().collect();
        // accents: combining mark after a single-char base
        if let Some(mark) = accent_mark(&name) {
            let base = self.arg();
            seq.push_boxed(
                Node::Accent {
                    base: Box::new(base),
                    mark,
                },
                Class::Ord,
            );
            return;
        }
        match name.as_str() {
            // wrappers that make a WORD of their argument: it stands
            // upright, the way `\text{if}` is a word and not i·f
            "text" | "textrm" | "textbf" | "textsf" | "texttt" | "mathrm" | "mathsf" | "mathtt"
            | "mbox" | "operatorname" => {
                // a word's own spaces are the author's, not TeX's: read
                // the argument as written rather than as math
                let arg = self.raw_arg();
                seq.push_atom(&arg, Class::Ord);
            }
            // wrappers that only change weight or shape: the argument
            // keeps whatever setting its own characters ask for
            "textit" | "mathbf" | "mathit" | "mathcal" | "mathfrak" | "mathscr" | "boldsymbol"
            | "bm" | "pmb" | "overbrace" | "underbrace" => {
                let arg = self.arg();
                seq.push(arg);
            }
            // `\mathbb{R}` has a letter of its own
            "mathbb" => {
                let arg = self.arg_text();
                seq.push_str(&blackboard(&upright(&arg)));
            }
            // named functions are set upright — the name IS the rendering
            _ if FUNCTIONS.contains(&name.as_str()) => seq.push_atom(&name, Class::Op),
            "pmod" => {
                let arg = self.arg();
                seq.push_str(" (mod ");
                seq.push(arg);
                seq.push_str(")");
            }
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let (num, den) = (self.arg(), self.arg());
                seq.push(Node::Frac {
                    num: Box::new(num),
                    den: Box::new(den),
                });
            }
            "binom" | "dbinom" | "tbinom" => {
                let (n, k) = (self.arg(), self.arg());
                seq.push(Node::Fence {
                    open: "(".into(),
                    close: ")".into(),
                    body: Box::new(Node::Stack(vec![n, k])),
                });
            }
            "sqrt" => {
                let index = (self.s.get(self.i) == Some(&'[')).then(|| {
                    self.i += 1;
                    Box::new(self.rows(Some(']')))
                });
                let body = if self.s.get(self.i) == Some(&'{') {
                    self.arg()
                } else {
                    Node::Text(String::new())
                };
                seq.push(Node::Sqrt {
                    index,
                    body: Box::new(body),
                });
            }
            // `\begin{env}` … `\end{env}`
            "begin" => self.environment(seq),
            "end" => {
                let _ = self.arg(); // a stray closer: nothing to draw
            }
            // `\left( … \right)`: delimiters that grow to their content
            "left" => {
                let open = self.delimiter();
                let body = self.until_right();
                let close = self.delimiter();
                seq.push(Node::Fence {
                    open,
                    close,
                    body: Box::new(body),
                });
            }
            // a `\right` with no `\left`: its delimiter is all there is
            "right" | "middle" => {
                let d = self.delimiter();
                seq.push_str(&d);
            }
            // sizing noise: the delimiter itself follows and stands alone
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl"
            | "biggr" | "Biggl" | "Biggr" => {
                let d = self.delimiter();
                seq.push_str(&d);
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
                seq.push_space(1.0);
            }
            "quad" => seq.push_space(1.0),
            "qquad" => seq.push_space(2.0),
            _ => match symbol(&name) {
                Some(s) => seq.push_str(&lean_str(s)),
                None => {
                    // verbatim: the reader sees exactly what they wrote
                    seq.push_str("\\");
                    seq.push_str(&name);
                }
            },
        }
    }

    /// The delimiter after `\left`/`\right`/`\big…`. A `.` is TeX's
    /// invisible one — it belongs to the command, not to the formula.
    fn delimiter(&mut self) -> String {
        match self.s.get(self.i) {
            Some('.') => {
                self.i += 1;
                String::new()
            }
            Some('\\') => {
                self.i += 1;
                let mut seq = Seq {
                    tight: self.tight,
                    ..Seq::default()
                };
                self.command(&mut seq);
                seq.finish().text()
            }
            Some(&c) => {
                self.i += 1;
                c.to_string()
            }
            None => String::new(),
        }
    }

    /// Everything up to the matching `\right`, which is left unconsumed
    /// past its command name so the caller reads its delimiter.
    fn until_right(&mut self) -> Node {
        let start = self.i;
        let (end, resume) = self.find_command("left", "right");
        let body: Vec<char> = self.s[start..end].to_vec();
        self.i = resume;
        let mut sub = Parser {
            s: &body,
            i: 0,
            tight: self.tight,
        };
        sub.rows(None)
    }

    /// `\begin{env}` was just read: draw what it holds up to its `\end`.
    ///
    /// Environments are read as a WHOLE (a sub-parser over the body)
    /// rather than by flipping a flag, so a matrix nested in a `cases`
    /// cannot swallow the outer environment's `\end`.
    fn environment(&mut self, seq: &mut Seq) {
        let env = self.arg_name();
        let base = env.trim_end_matches('*');
        // `\begin{array}{cc}` — the column spec is layout, not content
        if base == "array" && self.s.get(self.i) == Some(&'{') {
            let _ = self.arg();
        }
        let start = self.i;
        let (end, resume) = self.find_command("begin", "end");
        let body: Vec<char> = self.s[start..end].to_vec();
        self.i = if resume < self.s.len() || end < self.s.len() {
            // step past the `\end{…}` the finder stopped on
            self.skip_end(resume)
        } else {
            resume
        };
        let mut sub = Parser {
            s: &body,
            i: 0,
            tight: self.tight,
        };
        let mut inner = sub.rows(None);
        // outside a columned environment an `&` is an alignment tab: it
        // draws nothing, but the columns it lines up do stand apart, and
        // a row flattened without that gap reads `𝑎= 𝑏`
        if !COLUMNED.contains(&base)
            && let Node::Grid(rows) = inner
        {
            inner = stack_node(rows.into_iter().map(tabbed_row).collect());
        }
        match matrix_fence(base) {
            Some((open, close)) => seq.push(Node::Fence {
                open: open.into(),
                close: close.into(),
                body: Box::new(inner),
            }),
            None => seq.push(inner),
        }
    }

    /// Walk past a `\end{…}`'s argument, if one is there.
    fn skip_end(&self, mut at: usize) -> usize {
        while self.s.get(at) == Some(&' ') {
            at += 1;
        }
        if self.s.get(at) == Some(&'{') {
            while at < self.s.len() && self.s[at] != '}' {
                at += 1;
            }
            at = (at + 1).min(self.s.len());
        }
        at
    }

    /// Where the construction open at `self.i` ends: (index of the
    /// `\closer`, index just past its NAME). Nested `\opener`s are
    /// counted, and an unclosed one runs to the end of the span.
    fn find_command(&self, opener: &str, closer: &str) -> (usize, usize) {
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
            if name == opener {
                depth += 1;
            } else if name == closer {
                depth -= 1;
                if depth == 0 {
                    return (j, k);
                }
            } else if k == j + 1 {
                // an escape (`\\`, `\{`) covers two characters
                k = j + 2;
            }
            j = k;
        }
        (self.s.len(), self.s.len())
    }
}

/// Fold a script onto a base, keeping both when `x^a_b` writes them one
/// after the other.
fn attach(base: Node, sup: Option<Node>, sub: Option<Node>) -> Node {
    match base {
        Node::Scripts {
            base,
            sup: had_sup,
            sub: had_sub,
        } => Node::Scripts {
            base,
            sup: sup.map(Box::new).or(had_sup),
            sub: sub.map(Box::new).or(had_sub),
        },
        base => Node::Scripts {
            base: Box::new(base),
            sup: sup.map(Box::new),
            sub: sub.map(Box::new),
        },
    }
}

/// TeX's math italic, resolved into the characters themselves.
fn lean_str(s: &str) -> String {
    s.chars().map(|c| leaning(c).unwrap_or(c)).collect()
}

/// The upright reading of text that leaned — `\text{…}` is a word.
fn upright(s: &str) -> String {
    s.chars().map(|c| standing(c).unwrap_or(c)).collect()
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

/// The inverse of [`leaning`] — what a leaning character was.
fn standing(c: char) -> Option<char> {
    let at = |base: u32, offset: u32| char::from_u32(base + offset);
    let o = c as u32;
    match c {
        '\u{210e}' => Some('h'),
        '𝑎'..='𝑧' => at('a' as u32, o - 0x1d44e),
        '𝐴'..='𝑍' => at('A' as u32, o - 0x1d434),
        '𝛼'..='𝜔' => at('α' as u32, o - 0x1d6fc),
        '𝜗' => Some('ϑ'),
        '𝜖' => Some('ϵ'),
        '𝜙' => Some('ϕ'),
        '𝜚' => Some('ϱ'),
        '𝜛' => Some('ϖ'),
        _ => None,
    }
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

/// Combining marks ride the character before them — the accents this
/// module places, and any the author typed.
fn is_combining(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036f}' | '\u{20d0}'..='\u{20ff}')
}

fn accent_mark(name: &str) -> Option<char> {
    ACCENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, mark)| *mark)
}

fn symbol(name: &str) -> Option<&'static str> {
    SYMBOLS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// What a piece of a formula IS, for spacing. TeX's atom classes, minus
/// the ones a note never writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// A variable, a digit, a symbol: the default.
    Ord,
    /// An operator that takes limits — `\sum`, and the function names.
    Op,
    /// A binary operator: `+`, `\times`, `\cap`.
    Bin,
    /// A relation: `=`, `\le`, `\to`.
    Rel,
    Open,
    Close,
    /// `,` and `;` — space goes AFTER them, never before.
    Punct,
    /// Something built rather than written: a fraction, a fence.
    Inner,
}

/// TeX's spacing, in ems, for what goes between two atoms.
///
/// This is the table from the TeXbook, in the units it is written in
/// (18 mu to the em): thin is 3, medium 4, thick 5. Two ordinaries touch;
/// a binary operator gets a medium space either side; a relation a thick
/// one. It is why `$a+b=c$` reads as an equation instead of as five
/// characters — and why a note's own spacing inside `$…$` is ignored,
/// exactly as TeX ignores it, so `$a+b$` and `$a + b$` set identically.
///
/// `tight` is the script styles, where TeX drops every space that is
/// parenthesised in its table — an exponent has no room for them.
fn space(left: Class, right: Class, tight: bool) -> f32 {
    use Class::{Bin, Close, Inner, Op, Open, Punct, Rel};
    const THIN: f32 = 3.0 / 18.0;
    const MED: f32 = 4.0 / 18.0;
    const THICK: f32 = 5.0 / 18.0;
    // the parenthesised entries: present at text and display style only
    let sometimes = |mu: f32| if tight { 0.0 } else { mu };
    match (left, right) {
        (Punct, _) => sometimes(THIN),
        (_, Punct) => 0.0,
        (Bin, _) | (_, Bin) => sometimes(MED),
        (Rel, _) | (_, Rel) => sometimes(THICK),
        (Op, _) | (_, Op) => THIN,
        (Inner, _) | (_, Inner) => sometimes(THIN),
        (Open, _) | (_, Close) => 0.0,
        _ => 0.0,
    }
}

/// The class of a character. Everything not named here is ordinary,
/// which is the right default: a letter, a digit, a `\partial`.
fn class_of(c: char) -> Class {
    match c {
        '+' | '−' | '±' | '∓' | '×' | '÷' | '∗' | '⋆' | '∘' | '•' | '∩' | '∪' | '⊓' | '⊔' | '⊎'
        | '∖' | '∧' | '∨' | '⊕' | '⊖' | '⊗' | '⊙' | '·' => Class::Bin,
        '=' | '<' | '>' | '≤' | '≥' | '≠' | '≡' | '≈' | '≍' | '∼' | '≃' | '≅' | '∝' | '≪' | '≫'
        | '≺' | '≻' | '⪯' | '⪰' | '⊂' | '⊃' | '⊆' | '⊇' | '⊊' | '⊋' | '∈' | '∉' | '∋' | '⊥'
        | '⊤' | '∥' | '∣' | '∤' | '⊢' | '⊣' | '⊨' | '→' | '←' | '↔' | '⇒' | '⇐' | '⇔' | '↦'
        | '⟶' | '⟵' | '⟹' | '⟸' | '⟺' | '⟼' | '↑' | '↓' | '↕' | '↪' | '↩' | '⇝' | ':' => {
            Class::Rel
        }
        '(' | '[' | '{' | '⟨' | '⌈' | '⌊' => Class::Open,
        ')' | ']' | '}' | '⟩' | '⌉' | '⌋' | '!' => Class::Close,
        ',' | ';' => Class::Punct,
        _ if BIG_OPS.contains(c) => Class::Op,
        _ => Class::Ord,
    }
}

/// The operators that take limits, and are an `Op` for spacing.
const BIG_OPS: &str = "∑∏∐∫∬∭∮⋃⋂⨁⨂⋀⋁";

/// A binary operator with nothing to bind on its left is not binary —
/// the `-` of `-b`, or the one after a `(`. TeX calls it ordinary there,
/// and so the space in front of it goes away.
fn binds(left: Option<Class>) -> bool {
    matches!(
        left,
        Some(Class::Ord | Class::Close | Class::Inner) | Some(Class::Op)
    )
}

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
    use super::{Node, glyphs, to_tree};

    /// The tree, written out so an assertion can read like the formula:
    /// `frac(a, b)`, `sqrt(x)`, `x^{2}`, `[a b]` for a row, `/` between
    /// stacked rows and `|` between grid cells. Leaning letters are the
    /// Mathematical Alphanumeric characters themselves — an assertion
    /// showing `𝛿` is showing exactly what gets drawn.
    fn show(tex: &str) -> String {
        fn go(n: &Node) -> String {
            match n {
                Node::Text(t) => t.clone(),
                // a space shows as the fraction of an em it is, so an
                // assertion can read the spacing TeX asked for
                Node::Space(ems) => format!("<{:.0}>", ems * 18.0),
                Node::Accent { base, mark } => format!("{}{mark}", go(base)),
                Node::Row(v) => format!("[{}]", v.iter().map(go).collect::<Vec<_>>().join("")),
                Node::Stack(v) => {
                    format!(
                        "stack[{}]",
                        v.iter().map(go).collect::<Vec<_>>().join(" / ")
                    )
                }
                Node::Grid(rows) => format!(
                    "grid[{}]",
                    rows.iter()
                        .map(|r| r.iter().map(go).collect::<Vec<_>>().join(" | "))
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
                Node::Scripts { base, sup, sub } => {
                    let mut out = go(base);
                    if let Some(x) = sub {
                        out += &format!("_{{{}}}", go(x));
                    }
                    if let Some(x) = sup {
                        out += &format!("^{{{}}}", go(x));
                    }
                    out
                }
                Node::Frac { num, den } => format!("frac({}, {})", go(num), go(den)),
                Node::Sqrt { index, body } => match index {
                    Some(i) => format!("root({}, {})", go(i), go(body)),
                    None => format!("sqrt({})", go(body)),
                },
                Node::Fence { open, close, body } => {
                    format!("fence{open}{}{close}", go(body))
                }
            }
        }
        go(&to_tree(tex))
    }

    #[test]
    fn plain_symbols_and_greek() {
        assert_eq!(show(r"\delta = 2"), "[𝛿<5>=<5>2]");
        assert_eq!(show(r"\alpha \to \beta"), "[𝛼<5>→<5>𝛽]");
        assert_eq!(show(r"\forall x \in S"), "[∀𝑥<5>∈<5>𝑆]");
    }

    /// TeX's math italic: a letter is a variable and leans, a digit or an
    /// operator stands, and a function name is a word — which is what
    /// tells `log` from three letters multiplied together. It is resolved
    /// into the CHARACTERS, because Unicode has a designed italic and a
    /// renderer's italics flag only shears the upright glyph.
    #[test]
    fn letters_lean_and_everything_else_stands() {
        assert_eq!(show(r"2x + 3y = 0"), "[2𝑥<4>+<4>3𝑦<5>=<5>0]");
        assert_eq!(show(r"\log n"), "[log<3>𝑛]");
        assert_eq!(show(r"\text{if } x > 0"), "[if 𝑥<5>><5>0]");
        assert_eq!(show(r"\mathrm{d}x"), "d𝑥");
        // …and `\text` puts back what leaned inside it
        assert_eq!(show(r"\text{max}"), "max");
    }

    /// TeX ignores the spaces a formula is WRITTEN with and inserts its
    /// own by what the atoms are, which is why `$a+b=c$` reads as an
    /// equation rather than as five characters — and why writing it
    /// roomily changes nothing.
    #[test]
    fn spacing_comes_from_the_atoms_not_from_the_source() {
        assert_eq!(show("a+b=c"), show("a  +  b  =  c"));
        assert_eq!(show("a+b=c"), "[𝑎<4>+<4>𝑏<5>=<5>𝑐]");
        // a binary operator with nothing on its left is not binary
        assert_eq!(show("-b"), "−𝑏");
        assert_eq!(show("(-b)"), "(−𝑏)");
        // …and punctuation is followed, never preceded
        assert_eq!(show("f(x, y)"), "[𝑓(𝑥,<3>𝑦)]");
        // an operator name takes a thin space before what it applies to
        assert_eq!(show(r"\sin x"), "[sin<3>𝑥]");
        // a script has no room for any of it
        assert_eq!(show("e^{a+b}"), "𝑒^{𝑎+𝑏}");
    }

    /// A fraction is a BOX, so it needs no slash and no parentheses to
    /// stay true — `\frac{\pi^2}{6}` used to read `(𝜋²)/6`.
    #[test]
    fn fractions_roots_and_binomials_keep_their_structure() {
        assert_eq!(show(r"\frac{1}{2}"), "frac(1, 2)");
        assert_eq!(show(r"\frac{a+b}{c}"), "frac([𝑎<4>+<4>𝑏], 𝑐)");
        // a keyboard hyphen is a minus sign in a formula
        assert_eq!(show(r"a - b"), "[𝑎<4>−<4>𝑏]");
        assert_eq!(show(r"\frac{\pi^2}{6}"), "frac(𝜋^{2}, 6)");
        // a fraction keeps the setting of what is inside it
        assert_eq!(show(r"\frac{\sin x}{x}"), "frac([sin<3>𝑥], 𝑥)");
        assert_eq!(show(r"\sqrt{x+1}"), "sqrt([𝑥<4>+<4>1])");
        assert_eq!(show(r"\sqrt[3]{x}"), "root(3, 𝑥)");
        assert_eq!(show(r"\binom{n}{k}"), "fence(stack[𝑛 / 𝑘])");
        assert_eq!(show(r"\mathbb{R}^n"), "ℝ^{𝑛}");
    }

    /// A script attaches to an ATOM, and an atom is as long as whatever
    /// put it there: one character of the formula's own text, but the
    /// whole of a function name.
    #[test]
    fn scripts_attach_to_the_atom_before_them() {
        assert_eq!(show(r"x^2 + y_i"), "[𝑥^{2}<4>+<4>𝑦_{𝑖}]");
        assert_eq!(show(r"abc^2"), "[𝑎𝑏𝑐^{2}]");
        assert_eq!(show(r"\log_2 n"), "[log_{2}<3>𝑛]");
        assert_eq!(show(r"\sum_{i=1}^{n}"), "∑_{𝑖=1}^{𝑛}");
        // both scripts on one base, written either way round
        assert_eq!(show(r"x_a^b"), "𝑥_{𝑎}^{𝑏}");
        assert_eq!(show(r"x^b_a"), "𝑥_{𝑎}^{𝑏}");
        // a script of a script — nothing to spell, so nothing to give up
        assert_eq!(show(r"e^{z_i}"), "𝑒^{𝑧_{𝑖}}");
    }

    /// An accent has to stay in its base's text, or it is laid out on its
    /// own and stops sitting on the letter it belongs to.
    #[test]
    fn accents_ride_single_char_bases() {
        assert_eq!(show(r"\vec{x}"), "𝑥\u{20d7}");
        assert_eq!(show(r"\hat{y} = \bar{x}"), "[𝑦\u{0302}<5>=<5>𝑥\u{0304}]");
    }

    #[test]
    fn unknown_commands_stay_verbatim() {
        assert_eq!(show(r"\foobar + 1"), r"[\foobar<4>+<4>1]");
        // grouping braces disappear, the name does not
        assert_eq!(show(r"\undefinedcmd{x}"), r"\undefinedcmd𝑥");
    }

    /// `\left…\right` is a pair that grows around what it holds, and an
    /// invisible delimiter takes its dot with it.
    #[test]
    fn fences_pair_up_and_delimiters_stay() {
        assert_eq!(show(r"\left( \frac{1}{2} \right)"), "fence(frac(1, 2))");
        assert_eq!(show(r"\left\{ x \right."), "fence{𝑥");
        assert_eq!(show(r"\langle u, v \rangle"), "[⟨𝑢,<3>𝑣⟩]");
        // a nested pair closes its own
        assert_eq!(show(r"\left( \left| x \right| \right)"), "fence(fence|𝑥|)");
    }

    #[test]
    fn named_functions_lose_only_their_backslash() {
        assert_eq!(show(r"\sin x + \cos y"), "[sin<3>𝑥<4>+<4>cos<3>𝑦]");
        assert_eq!(
            show(r"\lim_{x \to 0} \frac{\sin x}{x} = 1"),
            "[lim_{𝑥→0}<3>frac([sin<3>𝑥], 𝑥)<5>=<5>1]"
        );
    }

    /// A display block wraps where the author wrapped the source, and TeX
    /// does not care: only `\\` ends a row. The whole span used to come
    /// through with the newlines the `$$` fences left on it, so every
    /// equation drew with a blank line above and below.
    #[test]
    fn source_line_breaks_are_spaces_and_only_a_double_backslash_breaks() {
        assert_eq!(show("\nE = mc^2\n"), "[𝐸<5>=<5>𝑚𝑐^{2}]");
        assert_eq!(show("a\n= b"), "[𝑎<5>=<5>𝑏]");
        assert_eq!(show(r"a \\ b"), "stack[𝑎 / 𝑏]");
        assert_eq!(show("x    +     y"), "[𝑥<4>+<4>𝑦]");
        // a minted space swallows the ordinary ones beside it
        assert_eq!(show(r"x \, dx"), "[𝑥<3>𝑑𝑥]");
    }

    #[test]
    fn environments_become_rows_and_grids() {
        assert_eq!(
            show("\\begin{aligned}\na &= b \\\\\nc &= d\n\\end{aligned}"),
            "stack[[𝑎 [=<5>𝑏]] / [𝑐 [=<5>𝑑]]]"
        );
        assert_eq!(
            show(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "fence(grid[𝑎 | 𝑏 / 𝑐 | 𝑑])"
        );
        // cases keeps its brace, and only on the left
        assert_eq!(
            show(r"\begin{cases} 1 & x > 0 \\ 0 & x < 0 \end{cases}"),
            "fence{grid[1 | [𝑥<5>><5>0] / 0 | [𝑥<5><<5>0]]"
        );
        // a matrix inside cases closes its own \end
        assert_eq!(
            show(r"\begin{aligned} a &= \begin{matrix} 1 & 2 \end{matrix} \end{aligned}"),
            "[𝑎 [=<5>grid[1 | 2]]]"
        );
        // an unclosed environment still draws what it holds
        assert_eq!(show(r"\begin{aligned} a &= b"), "[𝑎 [=<5>𝑏]]");
    }

    /// Nothing may reach the screen that the bundled math font can't
    /// draw — the app-side companion test holds the font to this list.
    #[test]
    fn the_glyph_inventory_covers_every_table() {
        let g = glyphs();
        for c in ['ℝ', '𝛼', '∑', '√', '\u{20d7}', '⟹', '𝑥', '𝐴', 'ℎ'] {
            assert!(g.contains(c), "{c:?} is missing from the inventory");
        }
        // an unrecognized command reaches the reader as its own TeX, so
        // the backslash and the letters spelling it have to be drawable
        assert!(g.contains('\\'), "verbatim TeX needs its backslash");
        // never U+25FB: epaint draws it as its replacement box, and a
        // face holding it becomes the family's replacement face, after
        // which nothing in that face reports as drawable
        assert!(!g.contains('◻'), "U+25FB is epaint's replacement char");
    }

    #[test]
    fn an_empty_span_draws_nothing() {
        assert!(to_tree("").is_empty());
        assert!(to_tree("   ").is_empty());
        assert!(!to_tree("x").is_empty());
    }
}
