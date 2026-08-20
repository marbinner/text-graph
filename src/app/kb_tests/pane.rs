//! The side pane: width ownership, wrap-inside rules, and the one
//! previewer's markdown/source bodies.

use super::*;

/// The side pane opens at a quarter of the window and then belongs to the
/// user: mode switches (ranger ↔ search preview) must not resize it, and
/// only a window too narrow to hold it may.
#[test]
fn the_side_pane_defaults_to_a_share_of_the_window_and_then_stays_put() {
    assert_eq!(
        super::pane_width(1600.0, None),
        480.00003,
        "a share of the window when never set"
    );
    assert_eq!(
        super::pane_width(1600.0, Some(720.0)),
        720.0,
        "what the user dragged to is what they get"
    );
    assert_eq!(
        super::pane_width(600.0, Some(720.0)),
        360.0,
        "…until the window can't hold it: never more than 60% of it"
    );
    assert!(
        super::pane_width(800.0, Some(120.0)) >= 340.0,
        "and never so narrow the preview stops being one"
    );
    // on a window too small for both, the ceiling wins and the canvas
    // keeps its share
    assert!((super::pane_width(400.0, None) - 240.0).abs() < 0.1);
}

/// A dragged width outlives the session it was set in.
#[test]
fn the_pane_width_round_trips_through_the_view_file() {
    use text_graph::state;
    let s = state::ViewState {
        pane_width: Some(512.0),
        ..Default::default()
    };
    assert_eq!(
        state::from_text(&state::to_text(&s)).pane_width,
        Some(512.0)
    );
    let corrupt = state::from_text("text-graph view v1\npane_w\t99999\n");
    assert!(
        corrupt.pane_width.is_some_and(|w| w <= 4000.0),
        "a hand-edited width is clamped, like the camera"
    );
    // the old `pane` key held widths the pane SEEDED itself with, from a
    // window that no longer exists — they are dropped, not honoured
    assert_eq!(
        state::from_text("text-graph view v1\npane\t300\n").pane_width,
        None
    );
}

/// Content must not be able to widen the pane. egui STORES a panel's
/// content-driven rect, so a note the pane can't fit (a wide markdown
/// table, an unwrappable code line, a terminal screen) used to push it
/// open — and the new width stuck, ratcheting further with every wide
/// note walked onto, until the pane covered the canvas and its own
/// columns ran off the window.
#[test]
fn wide_content_scrolls_inside_the_pane_instead_of_widening_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let d = std::env::temp_dir().join(format!("tg-panewide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let row = format!("| {} |\n", "wide-cell-that-never-wraps ".repeat(40));
    std::fs::write(
        d.join("wide.md"),
        format!(
            "# wide\n\n|h|\n|---|\n{row}{row}{row}\n\n`{}`\n",
            "x".repeat(4000)
        ),
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());

    let seen = Arc::new(AtomicU32::new(0));
    let probe = seen.clone();
    let mut h = Harness::new_ui_state(
        move |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            let w = v.side_panel(ui);
            probe.store(w.round() as u32, Ordering::Relaxed);
        },
        viewer,
    );
    super::install_fonts(&h.ctx);
    let id = h.state().g.by_path("wide.md").expect("the wide note");
    h.state_mut().selected = Some(id);
    // several frames: the ratchet needed a stored rect to grow from
    for _ in 0..4 {
        h.step();
    }
    let _ = std::fs::remove_dir_all(&d);

    let win = h.ctx.content_rect().width();
    let got = seen.load(Ordering::Relaxed) as f32;
    let opened_at = super::pane_width(win, None);
    assert!(
        (got - opened_at).abs() <= 1.0,
        "the pane took {got} of a {win} window instead of staying at \
         {opened_at} — wide content widened it again"
    );
    assert_eq!(
        h.state().pane_width,
        None,
        "and nothing was written back: only a DRAG owns the width"
    );
}

/// One wide table must not poison the prose after it: egui re-expands a
/// Ui's max_rect to its min_rect after each widget, so the table's width
/// used to become the wrap width for every paragraph that followed —
/// text ran off the pane and was clipped at the window edge, while the
/// table's own far columns were clipped outright. Tables render in their
/// own horizontal scroll regions now (`render_markdown` over
/// `mdview::split_tables`), so the whole body lays out inside the pane.
#[test]
fn a_wide_table_neither_widens_the_prose_after_it_nor_hides_its_columns() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let d = std::env::temp_dir().join(format!("tg-tablewrap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let row = format!("| {} |\n", "wide-cell-that-never-wraps ".repeat(40));
    std::fs::write(
        d.join("note.md"),
        format!(
            "before\n\n|h|\n|---|\n{row}\n\n{}\n",
            "prose after the table that must wrap at the measure ".repeat(40)
        ),
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let seen = Arc::new(AtomicU32::new(0));
    let probe = seen.clone();
    let mut h = Harness::new_ui_state(
        move |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            let w = v.side_panel(ui);
            probe.store(w.round() as u32, Ordering::Relaxed);
        },
        viewer,
    );
    super::install_fonts(&h.ctx);
    let id = h.state().g.by_path("note.md").expect("note");
    h.state_mut().selected = Some(id);
    for _ in 0..4 {
        h.step();
    }
    let pane = seen.load(Ordering::Relaxed) as f32;
    let content = h.state().pane_content_w.get();
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        content <= pane + 1.0,
        "the body laid out {content} wide in a {pane} pane — a wide table \
         is poisoning the layout again"
    );
}

/// Rendered markdown draws in the bundled reading face — the "reading"
/// family must be bound (a missing assets/reading.ttf or a dropped
/// install_fonts call panics epaint the moment a note renders), and the
/// measure only ever narrows.
#[test]
fn the_reading_family_is_bound_and_the_measure_only_narrows() {
    let h = harness();
    let height = h.ctx.fonts_mut(|f| {
        f.row_height(&eframe::egui::FontId::new(
            15.0,
            eframe::egui::FontFamily::Name("reading".into()),
        ))
    });
    assert!(height > 10.0, "reading family resolves to a real font");
    assert_eq!(super::navigator::reading_width(300.0), 300.0);
    assert_eq!(super::navigator::reading_width(1000.0), 620.0);
}

/// Every accessibility label the pane rendered, as (text, screen rect).
/// The rendered-markdown tests read layout off this: the renderer builds
/// no widgets of its own to query by name, only labels.
fn rendered_labels(h: &Harness<'_, Viewer>) -> Vec<(String, eframe::egui::Rect)> {
    use egui_kittest::kittest::NodeT as _;
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let node = n.accesskit_node();
            let text = node.value()?;
            let b = node.bounding_box()?;
            Some((
                text.to_string(),
                eframe::egui::Rect::from_min_max(
                    eframe::egui::pos2(b.x0 as f32, b.y0 as f32),
                    eframe::egui::pos2(b.x1 as f32, b.y1 as f32),
                ),
            ))
        })
        .collect()
}

/// Display math is a BLOCK, and the renderer has no block layer: the
/// whole document flows in one wrapping left-to-right Ui. An equation
/// that does not claim a full-measure row lands in whatever is left of
/// the current line — `$$…$$` mid-sentence used to take the few points
/// of leftover row as its wrap width and draw as a 15-point-wide column
/// of single characters down the right margin, taller than the
/// paragraph it interrupted.
#[test]
fn a_display_equation_takes_a_centred_row_of_its_own() {
    let d = std::env::temp_dir().join(format!("tg-mathrow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // the prelude is long enough to fill the measure, so the equation
    // meets the worst case: a row with almost nothing left in it
    std::fs::write(
        d.join("note.md"),
        "Prelude words here that eat some of the row: $$E = mc^2$$ and the tail.\n",
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let mut h = Harness::new_ui_state(
        move |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            v.side_panel(ui);
        },
        viewer,
    );
    super::install_fonts(&h.ctx);
    let id = h.state().g.by_path("note.md").expect("note");
    h.state_mut().selected = Some(id);
    for _ in 0..4 {
        h.step();
    }
    let _ = std::fs::remove_dir_all(&d);

    let labels = rendered_labels(&h);
    let find = |needle: &str| {
        labels
            .iter()
            .find(|(t, _)| t.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} never rendered, only {labels:?}"))
            .1
    };
    let math = find("𝐸 = 𝑚𝑐");
    let prose = find("Prelude words");
    assert!(
        math.width() > math.height(),
        "the equation drew {}x{} — it is wrapping inside a sliver of row again",
        math.width(),
        math.height()
    );
    assert!(
        math.top() >= prose.bottom(),
        "the equation drew inside the line it interrupted, not under it"
    );
    assert!(
        (math.center().x - prose.center().x).abs() < 8.0,
        "the equation centred at {} against a measure centred at {}",
        math.center().x,
        prose.center().x
    );
}

/// An exponent has to LOOK like one, at whatever depth the formula puts
/// it. `mathtext` hands over runs carrying a scale and an absolute rise
/// rather than characters, because Unicode can spell `x²` and has
/// nothing at all for the `z_i` in `e^{z_i}` — that one used to degrade
/// to `e^(zᵢ)`, caret and parens on the page. This is the other half:
/// `math_job` solving `line_height` so each baseline lands where the
/// formula asked, which is the part epaint gives no direct lever for.
#[test]
fn every_script_lands_where_the_formula_puts_it() {
    use std::sync::Arc;
    use std::sync::Mutex;

    const TEX: &str = r"x^{a_b}_c";
    const SIZE: f32 = 20.0;

    let seen: Arc<Mutex<Vec<(char, f32, f32)>>> = Arc::new(Mutex::new(Vec::new()));
    let probe = seen.clone();
    // the first frame runs before install_fonts, and the reading family
    // is not bound yet — laying out against it there panics epaint
    let mut h = Harness::new_ui_state(
        move |ui, ready: &mut bool| {
            if !*ready {
                return;
            }
            let font =
                eframe::egui::FontId::new(SIZE, eframe::egui::FontFamily::Name("math".into()));
            let job = super::navigator::math_job(ui, TEX, &font, eframe::egui::Color32::WHITE);
            let galley = ui.painter().layout_job(job);
            let row = galley.rows.first().expect("one row");
            *probe.lock().expect("no other thread") = row
                .glyphs
                .iter()
                .map(|g| (g.chr, g.pos.y, g.font_height))
                .collect();
        },
        false,
    );
    super::install_fonts(&h.ctx);
    *h.state_mut() = true;
    h.step();
    let glyphs = seen.lock().expect("no other thread").clone();
    let at = |c: char| {
        glyphs
            .iter()
            .find(|g| g.0 == c)
            .copied()
            .unwrap_or_else(|| panic!("{c:?} never laid out in {glyphs:?}"))
    };
    // the letters ARE their math-italic characters by the time they are
    // laid out — `mathtext::lean` resolves the setting into the text
    let base = at('𝑥');
    for run in text_graph::mathtext::to_runs(TEX) {
        let c = run.text.chars().next().expect("no empty runs");
        let glyph = at(c);
        // y grows downward, and `rise` is negative upward, both in ems
        // of the span's size
        let got = (base.1 - glyph.1) / SIZE;
        assert!(
            (got + run.rise).abs() < 0.1,
            "{c:?} landed at {got} em, and the formula asked for {}",
            -run.rise
        );
        assert!(
            (glyph.2 - base.2 * run.scale).abs() < 1.0,
            "{c:?} is set at {} against a line of {}, scale {}",
            glyph.2,
            base.2,
            run.scale
        );
    }
    // and the ladder is one the formula actually describes: down, up,
    // and back down but not all the way
    assert!(at('𝑎').1 < base.1, "the exponent does not ride high");
    assert!(at('𝑐').1 > base.1, "the index does not sit low");
    assert!(
        at('𝑏').1 > at('𝑎').1 && at('𝑏').1 < base.1,
        "a subscript inside a superscript left the exponent"
    );
}

/// A glyph no font in the family owns is not invisible — epaint draws
/// the replacement box in its place. Inter's Latin subset has no
/// operators, no math italics and no combining accents, and neither does
/// egui's default face, so every `$\alpha \in S$` in a note used to
/// reach the reader as a row of boxes. `assets/math.ttf` is the answer,
/// and this is the gate that says when it needs regenerating: a symbol
/// added to `mathtext`'s tables without a rerun of
/// `assets/gen-math-font.sh` fails right here. It also holds the family
/// to covering the span on its OWN — a character that fell through to a
/// fallback would draw off the baseline of the ones around it.
#[test]
fn every_character_mathtext_can_draw_has_a_glyph() {
    let h = harness();
    let math = eframe::egui::FontId::new(15.0, eframe::egui::FontFamily::Name("math".into()));
    let mut missing = String::new();
    h.ctx.fonts_mut(|f| {
        for c in text_graph::mathtext::glyphs().chars() {
            if !f.has_glyph(&math, c) {
                missing.push(c);
            }
        }
    });
    assert!(
        missing.is_empty(),
        "the math family cannot draw {missing:?} — rerun assets/gen-math-font.sh"
    );
}

/// `r` reads the same file the other way: source with line numbers
/// instead of rendered markdown. One previewer with two readings — the
/// toggle is session state, not a setting: persisted, one press pinned
/// every note preview to source across restarts (user-reported bug).
#[test]
fn r_switches_the_preview_between_markdown_and_source() {
    let mut h = harness();
    select(&mut h, "index.md");
    h.step();
    assert!(
        matches!(
            h.state().pane_preview.as_ref().map(|p| &p.body),
            Some(super::picker::PreviewBody::Node(_))
        ),
        "a note reads as rendered markdown by default"
    );
    press(&mut h, Key::R);
    assert!(h.state().pane_raw, "r flips the session toggle");
    assert!(
        matches!(
            h.state().pane_preview.as_ref().map(|p| &p.body),
            Some(super::picker::PreviewBody::Text(_))
        ),
        "…and the pane re-reads it as source, without the subject changing"
    );
    press(&mut h, Key::R);
    assert!(
        matches!(
            h.state().pane_preview.as_ref().map(|p| &p.body),
            Some(super::picker::PreviewBody::Node(_))
        ),
        "and back"
    );
}

/// Prose must wrap at the pane's width. `max_image_width(Some(400))` set
/// a FLOOR under the markdown Ui, so on a narrower pane every paragraph
/// wrapped at 400 and ran off the edge, clipped by the window. The pane
/// also has to open at its share of the CURRENT window: the computed
/// default must never be written back, or the first frame (eframe opens
/// at 1280, before the window manager has its say) freezes it forever.
#[test]
fn the_preview_wraps_inside_the_pane_and_the_default_is_not_persisted() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let d = std::env::temp_dir().join(format!("tg-wrap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("note.md"),
        format!(
            "# note\n\n{}\n",
            "some prose that should wrap inside the pane ".repeat(40)
        ),
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let seen = Arc::new(AtomicU32::new(0));
    let probe = seen.clone();
    let mut h = Harness::new_ui_state(
        move |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            let w = v.side_panel(ui);
            probe.store(w.round() as u32, Ordering::Relaxed);
        },
        viewer,
    );
    super::install_fonts(&h.ctx);
    let id = h.state().g.by_path("note.md").expect("note");
    h.state_mut().selected = Some(id);
    for _ in 0..4 {
        h.step();
    }
    let win = h.ctx.content_rect().width();
    let pane = seen.load(Ordering::Relaxed) as f32;
    let content = h.state().pane_content_w.get();
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        content <= pane + 1.0,
        "the preview laid out {content} wide in a {pane} pane — it will \
         run off the edge and be clipped"
    );
    assert_eq!(
        h.state().pane_width,
        None,
        "a width nobody dragged to must not be written back ({win} window)"
    );
}

/// The pane's width is OURS, not egui's. egui remembers a panel's size
/// in its own memory and only honours `default_size` while it remembers
/// nothing — so one narrow frame (a startup window, a window resize
/// clamping against the 60% ceiling) became the width from then on, and
/// the pane "randomly" stayed small. Shrinking the window and growing it
/// back must give the width back.
#[test]
fn the_pane_recovers_its_width_after_the_window_narrows() {
    use eframe::egui::Vec2;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let seen = Arc::new(AtomicU32::new(0));
    let probe = seen.clone();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1600.0, 800.0))
        .build_ui_state(
            move |ui, v: &mut Viewer| {
                v.pump_picker(ui.ctx());
                let w = v.side_panel(ui);
                probe.store(w.round() as u32, Ordering::Relaxed);
            },
            viewer,
        );
    super::install_fonts(&h.ctx);
    let id = h.state().g.by_path("index.md").expect("index");
    h.state_mut().selected = Some(id);
    h.run();
    let wide = seen.load(Ordering::Relaxed);
    assert!(wide >= 470, "a share of a 1600pt window, got {wide}");

    // a narrow window has to clamp it — the canvas keeps its share
    h.set_size(Vec2::new(500.0, 800.0));
    h.run();
    let squeezed = seen.load(Ordering::Relaxed);
    assert!(squeezed < wide, "a 500pt window clamps it, got {squeezed}");

    // …and giving the space back gives the width back
    h.set_size(Vec2::new(1600.0, 800.0));
    h.run();
    assert_eq!(
        seen.load(Ordering::Relaxed),
        wide,
        "the pane kept a width it was only ever squeezed into"
    );
    assert_eq!(
        h.state().pane_width,
        None,
        "and none of that counted as the user choosing a width"
    );
}

/// The source view colours code. The search match keeps its own mark —
/// a background, not a colour — because recolouring the hit would erase
/// exactly the colouring the source view is for.
#[test]
fn the_source_view_is_syntax_coloured_and_keeps_its_match_marks() {
    let d = std::env::temp_dir().join(format!("tg-hl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("code.rs"),
        "fn main() {\n    let greeting = \"hello\"; // a comment\n}\n",
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            v.pump_picker(ui.ctx());
        },
        viewer,
    );
    h.step();
    let id = h.state().g.by_path("code.rs").expect("the code file");
    h.state_mut().selected = Some(id);
    h.step();
    // no `r`, no search hit: code previews as source BECAUSE it is code —
    // there is nothing to render in a .rs, and markdown rendering of it
    // strips exactly the structure a reader is looking for
    assert!(!h.state().pane_raw, "no r pressed");

    let lines = match h.state().pane_preview.as_ref().map(|p| &p.body) {
        Some(super::picker::PreviewBody::Text(lines)) => lines.clone(),
        _ => panic!("expected the source view"),
    };
    let _ = std::fs::remove_dir_all(&d);
    let line = lines
        .iter()
        .find(|l| l.text.contains("greeting"))
        .expect("the line is there");
    let colors: std::collections::HashSet<(u8, u8, u8)> =
        line.spans.iter().map(|s| s.color).collect();
    assert!(
        colors.len() >= 3,
        "keyword, string and comment must not share one colour: {colors:?}"
    );
    assert!(
        line.spans
            .iter()
            .all(|s| s.range.end <= line.text.len() && line.text.is_char_boundary(s.range.start)),
        "spans index into the line as drawn, capping included"
    );
}

/// Obsidian callouts render as callouts, and its inline marks render as
/// what they mean: `> [!warning]` gets a heading and an accent, `==x==`
/// reads as emphasis, `%%x%%` is not shown at all, `#tag` is a chip.
#[test]
fn obsidian_flavored_markdown_renders() {
    let d = std::env::temp_dir().join(format!("tg-obsidian-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("note.md"),
        "# Title\n\n> [!warning] Mind the gap\n> The platform curves.\n\n\
         A ==marked== phrase, a #topic tag, %%a private aside%% and\n\
         a `fn code()` span. ^para-1\n",
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            v.side_panel(ui);
        },
        viewer,
    );
    super::install_fonts(&h.ctx);
    let id = h.state().g.by_path("note.md").expect("note");
    h.state_mut().selected = Some(id);
    h.run();
    let has = |s: &str| h.query_by_label_contains(s).is_some();
    let (warning, aside, marked, tag, blockid) = (
        has("Warning"),
        has("private aside"),
        has("marked"),
        has("#topic"),
        has("para-1"),
    );
    let _ = std::fs::remove_dir_all(&d);
    assert!(warning, "a callout renders under its own heading");
    assert!(!aside, "%%comments%% are for the writer, not the reader");
    assert!(marked, "==highlight== keeps its text");
    assert!(tag, "#tags render");
    assert!(!blockid, "^block-ids are addresses, not text");
}
