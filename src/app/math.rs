//! Setting a converted formula: `mathtext`'s tree laid out into boxes.
//!
//! A formula is not a line of text, and everything that tried to make it
//! one showed. `\frac{a}{b}` became `a/b` and then needed parentheses to
//! stay true; a script had to be spelled with whatever character Unicode
//! happened to have. So this is a small box model instead — the same one
//! TeX uses, minus everything a note does not need. Each node lays out
//! into a [`Frame`]: a width, an ascent and a descent around a BASELINE
//! at the origin, plus the galleys and rules to paint relative to it.
//! Composition is then arithmetic — a fraction stacks two frames around
//! the math axis and puts a rule between them.
//!
//! A frame carries TWO vertical extents, and the difference is the whole
//! difference between a radical that fits and one that floats. Ascent
//! and descent are the FONT's, which is what keeps baselines and line
//! heights consistent; the ink box is where the glyphs actually are, and
//! Noto Sans Math's ascent runs well above its tallest letter. Anything
//! that has to touch what it draws — a radical's bar, a delimiter grown
//! to its content — measures ink. Anything that has to line up with text
//! uses the font.
//!
//! The one measurement that must be taken rather than assumed is where a
//! galley's baseline is: epaint reports it per glyph and nowhere else,
//! so [`Ctx::text`] reads it off the laid-out galley.
//!
//! Sizes are ems of the size the frame is laid out at, and the ladder
//! (script, then scriptscript, then no smaller) is TeX's.

use std::sync::Arc;

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Vec2, pos2, vec2};
use text_graph::mathtext::Node;

/// A laid-out formula. Everything inside is positioned relative to the
/// baseline at the frame's origin, so composing two frames is adding an
/// offset.
pub(super) struct Frame {
    pub(super) width: f32,
    pub(super) ascent: f32,
    pub(super) descent: f32,
    /// Where the glyphs really are, relative to the baseline, negative
    /// upward. None when the frame draws nothing.
    ink: Option<(f32, f32)>,
    items: Vec<Item>,
}

enum Item {
    /// A galley and where its TOP-LEFT goes, relative to the origin.
    Text(Vec2, Arc<egui::Galley>),
    /// A rule — a fraction bar or a radical's overbar.
    Rule(Rect),
}

impl Frame {
    pub(super) fn height(&self) -> f32 {
        self.ascent + self.descent
    }

    /// The top of the ink, falling back to the font's ascent for a frame
    /// with nothing in it.
    fn ink_top(&self) -> f32 {
        self.ink.map_or(-self.ascent, |(top, _)| top)
    }

    fn ink_bottom(&self) -> f32 {
        self.ink.map_or(self.descent, |(_, bottom)| bottom)
    }

    fn ink_height(&self) -> f32 {
        self.ink_bottom() - self.ink_top()
    }

    fn empty() -> Self {
        Frame {
            width: 0.0,
            ascent: 0.0,
            descent: 0.0,
            ink: None,
            items: Vec::new(),
        }
    }

    /// Put `child` at an offset from this frame's origin, growing the
    /// extents to cover it. Returns the child's width, which is what a
    /// caller advancing along a row needs next.
    fn place(&mut self, child: Frame, at: Vec2) -> f32 {
        self.width = self.width.max(at.x + child.width);
        self.ascent = self.ascent.max(child.ascent - at.y);
        self.descent = self.descent.max(child.descent + at.y);
        if let Some((top, bottom)) = child.ink {
            self.mark_ink(top + at.y, bottom + at.y);
        }
        self.items
            .extend(child.items.into_iter().map(|item| match item {
                Item::Text(o, g) => Item::Text(o + at, g),
                Item::Rule(r) => Item::Rule(r.translate(at)),
            }));
        child.width
    }

    fn mark_ink(&mut self, top: f32, bottom: f32) {
        self.ink = Some(match self.ink {
            Some((t, b)) => (t.min(top), b.max(bottom)),
            None => (top, bottom),
        });
    }

    /// A rule is ink like any other, and the extents have to cover it —
    /// a fraction bar is the only thing at the axis when both halves are
    /// short.
    fn rule(&mut self, rect: Rect) {
        self.ascent = self.ascent.max(-rect.top());
        self.descent = self.descent.max(rect.bottom());
        self.mark_ink(rect.top(), rect.bottom());
        self.items.push(Item::Rule(rect));
    }

    pub(super) fn paint(&self, painter: &egui::Painter, origin: Pos2, color: Color32) {
        let shift = origin.to_vec2();
        for item in &self.items {
            match item {
                Item::Text(at, galley) => painter.galley(origin + *at, galley.clone(), color),
                Item::Rule(r) => {
                    painter.rect_filled(r.translate(shift), 0.0, color);
                }
            }
        }
    }

    /// How many rules the frame draws — a fraction's bar, a radical's
    /// overbar. The test surface for "stacked, not a slash".
    #[cfg(test)]
    pub(super) fn rules(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, Item::Rule(_)))
            .count()
    }
}

/// The math axis — the height a fraction bar and a fence centre on.
const AXIS: f32 = 0.25;
/// Fraction and radical rules.
const RULE: f32 = 0.05;
/// Air between a rule and what it separates.
const GAP: f32 = 0.14;
/// A fraction is wider than its halves, so the bar overhangs.
const FRAC_PAD: f32 = 0.14;
/// Inline, TeX sets a fraction's halves smaller — it has a line to fit
/// inside. A display fraction has the room to stay full size.
const FRAC_INLINE: f32 = 0.85;
/// How far a script moves off the line it rides on.
const SUP_RISE: f32 = 0.45;
const SUB_DROP: f32 = 0.2;
/// …and how much of a tall base's own extent carries it further.
const SCRIPT_CLEAR: f32 = 0.55;
/// TeX's two smaller sizes, as fractions of the span's own.
const SCRIPT: f32 = 0.7;
const SCRIPTSCRIPT: f32 = 0.5;
/// Between stacked rows, and between grid columns.
const ROW_GAP: f32 = 0.32;
const COL_GAP: f32 = 0.7;
/// A delimiter is drawn a touch taller than what it holds…
const FENCE_SLACK: f32 = 1.1;
/// …and never taller than this, or one runaway formula owns the page.
const FENCE_MAX: f32 = 5.0;
/// Display style sets `∑` and friends larger, and puts their limits
/// above and below instead of beside.
const BIG_OP_SCALE: f32 = 1.4;
const BIG_OPS: &str = "∑∏∐∫∬∭∮⋃⋂⨁⨂⋀⋁";
/// The named operators that take limits the same way — `\lim_{x \to 0}`
/// belongs under the word, not beside it. They are words, so unlike the
/// symbols above they are not set any larger.
const LIMIT_WORDS: &[&str] = &[
    "lim", "limsup", "liminf", "max", "min", "sup", "inf", "det", "gcd", "Pr",
];
/// Air between a big operator and a limit riding over it.
const LIMIT_GAP: f32 = 0.1;

pub(super) fn family() -> egui::FontFamily {
    egui::FontFamily::Name("math".into())
}

/// Lay out `node` at `size`, in the math family. `display` is TeX's
/// display style: the setting a `$$…$$` block gets and a `$…$` does not.
pub(super) fn layout(
    ui: &egui::Ui,
    node: &Node,
    size: f32,
    color: Color32,
    display: bool,
) -> Frame {
    let ctx = Ctx {
        ui,
        color,
        display,
        floor: size * SCRIPTSCRIPT,
    };
    ctx.lay(node, size)
}

struct Ctx<'a> {
    ui: &'a egui::Ui,
    color: Color32,
    display: bool,
    /// The smallest a script may get, whatever the nesting.
    floor: f32,
}

impl Ctx<'_> {
    /// One step down the ladder.
    fn smaller(&self, size: f32) -> f32 {
        (size * SCRIPT).max(self.floor)
    }

    fn lay(&self, node: &Node, size: f32) -> Frame {
        match node {
            Node::Text(t) => self.text(t, size),
            Node::Row(children) => self.row(children, size),
            Node::Scripts { base, sup, sub } => {
                self.scripts(base, sup.as_deref(), sub.as_deref(), size)
            }
            Node::Frac { num, den } => self.frac(num, den, size),
            Node::Sqrt { index, body } => self.sqrt(index.as_deref(), body, size),
            Node::Fence { open, close, body } => self.fence(open, close, body, size),
            Node::Stack(rows) => self.stack(rows, size),
            Node::Grid(rows) => self.grid(rows, size),
        }
    }

    fn text(&self, text: &str, size: f32) -> Frame {
        if text.is_empty() {
            return Frame::empty();
        }
        let font = FontId::new(size, family());
        let galley = self
            .ui
            .painter()
            .layout_no_wrap(text.to_owned(), font, self.color);
        // epaint puts a glyph's baseline in `pos.y`, and reports it
        // nowhere else — every vertical decision here hangs off it
        let ascent = galley
            .rows
            .first()
            .and_then(|r| r.glyphs.first())
            .map_or(size * 0.8, |g| g.pos.y);
        let bounds = galley.mesh_bounds;
        let ink = bounds
            .is_positive()
            .then(|| (bounds.top() - ascent, bounds.bottom() - ascent));
        let measured = galley.size();
        Frame {
            width: measured.x,
            ascent,
            descent: (measured.y - ascent).max(0.0),
            ink,
            items: vec![Item::Text(vec2(0.0, -ascent), galley)],
        }
    }

    fn row(&self, children: &[Node], size: f32) -> Frame {
        let mut out = Frame::empty();
        let mut x = 0.0;
        for child in children {
            x += out.place(self.lay(child, size), vec2(x, 0.0));
        }
        out.width = x;
        out
    }

    fn scripts(&self, base: &Node, sup: Option<&Node>, sub: Option<&Node>, size: f32) -> Frame {
        // `\sum_{i=1}^{n}` in display style stacks its limits, the way a
        // display sum does on paper
        if self.display
            && let Some((op, scale)) = limit_base(base)
        {
            return self.limits(op, scale, sup, sub, size);
        }
        let base = self.lay(base, size);
        let small = self.smaller(size);
        let (bw, ba, bd) = (base.width, base.ascent, base.descent);
        let mut out = Frame::empty();
        out.place(base, Vec2::ZERO);
        let mut widest: f32 = 0.0;
        if let Some(node) = sup {
            // a tall base carries its scripts further out
            let y = -(SUP_RISE * size).max(ba * SCRIPT_CLEAR);
            widest = widest.max(out.place(self.lay(node, small), vec2(bw, y)));
        }
        if let Some(node) = sub {
            let y = (SUB_DROP * size).max(bd * SCRIPT_CLEAR);
            widest = widest.max(out.place(self.lay(node, small), vec2(bw, y)));
        }
        out.width = bw + widest;
        out
    }

    /// A big operator with its limits over and under it.
    fn limits(
        &self,
        op: &str,
        scale: f32,
        sup: Option<&Node>,
        sub: Option<&Node>,
        size: f32,
    ) -> Frame {
        let glyph = self.text(op, size * scale);
        let small = self.smaller(size);
        let over = sup.map(|n| self.lay(n, small));
        let under = sub.map(|n| self.lay(n, small));
        let gap = LIMIT_GAP * size;
        let width = glyph
            .width
            .max(over.as_ref().map_or(0.0, |f| f.width))
            .max(under.as_ref().map_or(0.0, |f| f.width));
        // the operator centres its ink on the axis; the limits clear it
        let glyph_y = -AXIS * size - (glyph.ink_top() + glyph.ink_bottom()) / 2.0;
        let (top, bottom) = (glyph.ink_top() + glyph_y, glyph.ink_bottom() + glyph_y);
        let at = vec2((width - glyph.width) / 2.0, glyph_y);
        let mut out = Frame::empty();
        out.place(glyph, at);
        if let Some(frame) = over {
            let at = vec2((width - frame.width) / 2.0, top - gap - frame.descent);
            out.place(frame, at);
        }
        if let Some(frame) = under {
            let at = vec2((width - frame.width) / 2.0, bottom + gap + frame.ascent);
            out.place(frame, at);
        }
        out.width = width;
        out
    }

    fn frac(&self, num: &Node, den: &Node, size: f32) -> Frame {
        let inner = if self.display {
            size
        } else {
            size * FRAC_INLINE
        };
        let num = self.lay(num, inner);
        let den = self.lay(den, inner);
        let axis = -AXIS * size;
        let rule = (RULE * size).max(1.0);
        let gap = GAP * size;
        let pad = FRAC_PAD * size;
        let width = num.width.max(den.width) + 2.0 * pad;
        let num_at = vec2(
            (width - num.width) / 2.0,
            axis - rule / 2.0 - gap - num.ink_bottom(),
        );
        let den_at = vec2(
            (width - den.width) / 2.0,
            axis + rule / 2.0 + gap - den.ink_top(),
        );
        let mut out = Frame::empty();
        out.place(num, num_at);
        out.place(den, den_at);
        out.rule(Rect::from_min_max(
            pos2(0.0, axis - rule / 2.0),
            pos2(width, axis + rule / 2.0),
        ));
        out.width = width;
        out
    }

    fn sqrt(&self, index: Option<&Node>, body: &Node, size: f32) -> Frame {
        let body = self.lay(body, size);
        let rule = (RULE * size).max(1.0);
        let gap = GAP * size;
        // the bar clears the body's INK, not its font box, or it floats
        let bar_y = body.ink_top() - gap - rule;
        let want = body.ink_bottom() - bar_y;
        let radical = self.stretched("√", want, size);
        let mut out = Frame::empty();
        let mut x = 0.0;
        if let Some(node) = index {
            let frame = self.lay(node, self.smaller(self.smaller(size)));
            let at = vec2(0.0, bar_y + 0.4 * want - frame.ink_bottom());
            x += out.place(frame, at);
        }
        // the radical's own ink top meets the bar
        let at = vec2(x, bar_y - radical.ink_top());
        x += out.place(radical, at);
        let body_width = body.width;
        out.place(body, vec2(x, 0.0));
        out.rule(Rect::from_min_max(
            pos2(x, bar_y),
            pos2(x + body_width, bar_y + rule),
        ));
        out.width = x + body_width;
        out
    }

    fn fence(&self, open: &str, close: &str, body: &Node, size: f32) -> Frame {
        let body = self.lay(body, size);
        let want = body.ink_height() * FENCE_SLACK;
        let middle = (body.ink_top() + body.ink_bottom()) / 2.0;
        let mut out = Frame::empty();
        let mut x = 0.0;
        x += self.delimiter(&mut out, x, open, want, middle, size);
        x += out.place(body, vec2(x, 0.0));
        x += self.delimiter(&mut out, x, close, want, middle, size);
        out.width = x;
        out
    }

    /// One side of a fence, grown to `want` and centred on what it holds
    /// rather than sitting on the baseline — a parenthesis around a
    /// fraction has to straddle the bar, not hang off the line.
    fn delimiter(
        &self,
        out: &mut Frame,
        x: f32,
        text: &str,
        want: f32,
        middle: f32,
        size: f32,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let glyph = self.stretched(text, want, size);
        let centre = (glyph.ink_top() + glyph.ink_bottom()) / 2.0;
        out.place(glyph, vec2(x, middle - centre))
    }

    /// A glyph laid out big enough for its INK to span `want` points —
    /// the closest a text renderer gets to TeX's growing delimiters.
    fn stretched(&self, text: &str, want: f32, size: f32) -> Frame {
        let natural = self.text(text, size);
        let height = natural.ink_height();
        if height <= 0.0 || want <= height {
            return natural;
        }
        let mut grown = self.text(text, size * (want / height).min(FENCE_MAX));
        // a grown delimiter is drawn, not set: its font box would push
        // the line apart around a glyph that only has to reach as far as
        // its ink does
        grown.ascent = -grown.ink_top();
        grown.descent = grown.ink_bottom();
        grown
    }

    fn stack(&self, rows: &[Node], size: f32) -> Frame {
        let frames: Vec<Frame> = rows.iter().map(|r| self.lay(r, size)).collect();
        let gap = ROW_GAP * size;
        let total: f32 =
            frames.iter().map(Frame::height).sum::<f32>() + gap * (frames.len().max(1) - 1) as f32;
        let width = frames.iter().map(|f| f.width).fold(0.0, f32::max);
        let mut out = Frame::empty();
        // a stack centres on the axis, like everything else taller than
        // the line it sits on
        let mut y = -AXIS * size - total / 2.0;
        for frame in frames {
            let height = frame.height();
            let at = vec2((width - frame.width) / 2.0, y + frame.ascent);
            out.place(frame, at);
            y += height + gap;
        }
        out.width = width;
        out
    }

    fn grid(&self, rows: &[Vec<Node>], size: f32) -> Frame {
        let frames: Vec<Vec<Frame>> = rows
            .iter()
            .map(|row| row.iter().map(|c| self.lay(c, size)).collect())
            .collect();
        let columns = frames.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0.0f32; columns];
        for row in &frames {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.width);
            }
        }
        let gap = ROW_GAP * size;
        let col_gap = COL_GAP * size;
        let rise: Vec<(f32, f32)> = frames
            .iter()
            .map(|r| {
                (
                    r.iter().map(|c| c.ascent).fold(0.0, f32::max),
                    r.iter().map(|c| c.descent).fold(0.0, f32::max),
                )
            })
            .collect();
        let total: f32 =
            rise.iter().map(|(a, d)| a + d).sum::<f32>() + gap * (frames.len().max(1) - 1) as f32;
        let width: f32 = widths.iter().sum::<f32>() + col_gap * (columns.max(1) - 1) as f32;
        let mut out = Frame::empty();
        let mut y = -AXIS * size - total / 2.0;
        for (row, (ascent, descent)) in frames.into_iter().zip(rise) {
            let mut x = 0.0;
            for (i, cell) in row.into_iter().enumerate() {
                let at = vec2(x + (widths[i] - cell.width) / 2.0, y + ascent);
                out.place(cell, at);
                x += widths[i] + col_gap;
            }
            y += ascent + descent + gap;
        }
        out.width = width;
        out
    }
}

/// What a node is, if it takes its limits over and under: the operator
/// text and how much larger to set it. `\sum` and `\lim` do, `x` does
/// not.
fn limit_base(node: &Node) -> Option<(&str, f32)> {
    let Node::Text(t) = node else { return None };
    let mut chars = t.chars();
    let lone = chars.next().is_some_and(|c| BIG_OPS.contains(c)) && chars.next().is_none();
    if lone {
        return Some((t.as_str(), BIG_OP_SCALE));
    }
    LIMIT_WORDS
        .contains(&t.as_str())
        .then_some((t.as_str(), 1.0))
}
