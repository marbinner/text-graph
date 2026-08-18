//! Terminal cards from the keyboard side: Tab stepping, focus
//! surrender, launch placement.

use super::*;

/// Tab walks the terminal cards in a stable order, centering on each and
/// leaving the zoom alone — the card expands because it is the cursor, so
/// it is readable wherever you were standing. Enter then goes INTO it.
#[test]
fn tab_walks_the_cards_in_order_and_expands_them() {
    use text_graph::agents::AgentPane;
    let mut h = harness();
    let pane = |session: &str, pane: &str| AgentPane {
        session: session.into(),
        pane: pane.into(),
        pid: 1,
        cwd: h.state().root.clone(),
        agent: "pi".into(),
        ours: true,
        anchor: None,
    };
    // discovery order is not sorted order: %10 must come after %2, and a
    // string sort gets that backwards
    h.state_mut().terms.panes = vec![
        pane("tg_pi", "%10"),
        pane("tg_claude", "%3"),
        pane("tg_pi", "%2"),
    ];
    let order = h.state().terms.cards_in_order();
    assert_eq!(
        order,
        vec![
            ("tg_claude".to_string(), "%3".to_string()),
            ("tg_pi".to_string(), "%2".to_string()),
            ("tg_pi".to_string(), "%10".to_string()),
        ]
    );

    let zoom = h.state().cam.zoom;
    press(&mut h, Key::Tab);
    assert_eq!(
        h.state().terms.cursor.as_ref(),
        Some(&order[0]),
        "first Tab"
    );
    assert!(
        h.state().terms.is_expanded(&order[0]),
        "the card under the cursor is open"
    );
    assert_eq!(h.state().cam.zoom, zoom, "and the zoom is left alone");
    press(&mut h, Key::Tab);
    assert_eq!(h.state().terms.cursor.as_ref(), Some(&order[1]));
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::Tab);
    h.step();
    assert_eq!(h.state().terms.cursor.as_ref(), Some(&order[0]), "back");
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::Tab);
    h.step();
    assert_eq!(
        h.state().terms.cursor.as_ref(),
        Some(&order[2]),
        "and around the end"
    );

    press(&mut h, Key::Enter);
    assert_eq!(
        h.state().terms.focused.as_ref(),
        Some(&order[2]),
        "Enter takes you into the card the cursor is on"
    );
}

/// Tab is the cards' key, never egui's. egui decides its Tab focus move
/// in `Memory::begin_pass` — before any of our code runs — so the gear
/// and the health badge used to take the keyboard, and the next Tab went
/// to egui's focus navigation instead of the next card.
#[test]
fn tab_never_leaves_focus_on_the_corner_badges() {
    use text_graph::agents::AgentPane;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            let ctx = ui.ctx().clone();
            v.handle_keys(ui);
            // the two corner badges are exactly what Tab used to land on
            v.settings_ui(&ctx);
            v.diag_ui(&ctx);
            v.release_tab_focus(&ctx);
        },
        viewer,
    );
    let cwd = h.state().root.clone();
    h.state_mut().terms.panes = (0..3)
        .map(|i| AgentPane {
            session: format!("tg_pi_{i}"),
            pane: format!("%{i}"),
            pid: 1,
            cwd: cwd.clone(),
            agent: "pi".into(),
            ours: true,
            anchor: None,
        })
        .collect();
    h.step();

    for step in 1..=3 {
        press(&mut h, Key::Tab);
        assert!(
            h.ctx.memory(|m| m.focused()).is_none(),
            "Tab {step} left focus on a widget — the badges must never take it"
        );
        assert_eq!(
            h.state().terms.cursor.as_ref().map(|(s, _)| s.clone()),
            Some(format!("tg_pi_{}", step - 1)),
            "…and every Tab keeps stepping the cards"
        );
    }
}

/// A launched card lands where the user is LOOKING. Its anchor node can
/// be anywhere in the graph — and the card takes the keyboard the moment
/// it appears, so one off the edge of the canvas is a keyboard trap.
///
/// This drives the real paint path (never `sync_terminals`, which would
/// attach mirrors against the user's tmux server): the placement can only
/// be decided where the card's rect at this zoom is known.
#[test]
fn a_launched_card_is_placed_inside_the_view() {
    use eframe::egui;
    use text_graph::agents::AgentPane;
    use text_graph::mirror::{TermCell, TermGrid};

    const CANVAS: egui::Rect = egui::Rect {
        min: egui::Pos2::new(0.0, 0.0),
        max: egui::Pos2::new(900.0, 700.0),
    };

    let key = ("tg_claude".to_string(), "%1".to_string());
    let launch = |away: bool| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
        let scan = vault::scan(&root).expect("fixture scans");
        let mut viewer = Viewer::new(graph::build(scan), root.clone(), config::Config::default());
        // an agent card tethered to a note, exactly as discovery reports it
        viewer.terms.panes = vec![AgentPane {
            session: key.0.clone(),
            pane: key.1.clone(),
            pid: 1,
            cwd: root,
            agent: "claude".into(),
            ours: true,
            anchor: Some("index.md".into()),
        }];
        let cell = TermCell {
            text: " ".into(),
            wide_continuation: false,
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
        };
        let grid = TermGrid {
            cols: 80,
            rows: 24,
            cells: vec![cell; 80 * 24],
            cursor: None,
        };
        viewer
            .terms
            .cache
            .insert(key.clone(), super::terminals::build_cached(&grid));
        // just launched: focused, and waiting to be placed
        viewer.terms.focused = Some(key.clone());
        viewer.terms.cursor = Some(key.clone());
        viewer.terms.place_pending = Some(key.clone());
        let anchor = viewer.g.by_path("index.md").expect("index exists");
        viewer.cam.zoom = 1.0;
        viewer.cam.center = viewer.world_pos(anchor.0 as usize);
        if away {
            // the camera is three screens away from where the card tethers
            viewer.cam.center.x += 3000.0;
        }
        let mut h = Harness::new_ui_state(
            |ui, v: &mut Viewer| {
                let painter = ui.painter().clone();
                let slot = painter.add(egui::Shape::Noop);
                v.paint_terminals(&painter, CANVAS, CANVAS.expand(60.0), slot);
            },
            viewer,
        );
        h.step();
        h
    };

    let h = launch(true);
    let card = h
        .state()
        .terms
        .rects
        .iter()
        .find(|(s, p, _)| (s.clone(), p.clone()) == key)
        .map(|(_, _, r)| *r)
        .expect("an off-canvas card is brought back, so it paints");
    assert_eq!(
        card.center(),
        CANVAS.center(),
        "a card launched out of view lands in the middle of it"
    );
    assert!(
        h.state().terms.offsets.contains_key(&key),
        "and keeps its spot as an ordinary arrangement — draggable, saved"
    );

    // the other half: a card whose anchor is right under the camera is
    // already visible, and must not be yanked to the middle for it
    let h = launch(false);
    assert!(
        h.state().terms.offsets.is_empty(),
        "a card that opens in view is left where its node put it"
    );
}
