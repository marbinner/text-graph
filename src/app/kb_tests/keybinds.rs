//! The graph-action keys: dispatch guards, Esc/Enter chains, camera
//! keys, Ctrl+Q, and view-state restore clamping.

use super::*;

#[test]
fn w_toggles_web_nodes() {
    let mut h = harness();
    assert!(h.state().show_web, "webs visible by default");
    press(&mut h, Key::W);
    assert!(!h.state().show_web);
    press(&mut h, Key::W);
    assert!(h.state().show_web);
}

/// hjkl belong to the CAMERA now, selection or not — choosing a node
/// lives in the finder (f / b). `p` is the one tree move left: up to the
/// parent. s and d zoom, so one hand drives the whole view.
#[test]
fn hjkl_pan_and_sd_zoom_whatever_is_selected() {
    let mut h = harness();
    select(&mut h, "topics/grafér.md");
    let x0 = h.state().cam.center.x;
    h.key_down(Key::H);
    h.run_steps(3);
    h.key_up(Key::H);
    h.step();
    assert!(
        h.state().cam.center.x < x0,
        "h pans left even with a node selected"
    );
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics/grafér.md"),
        "and panning never moves the selection"
    );

    let z0 = h.state().cam.zoom;
    h.key_down(Key::D);
    h.run_steps(3);
    h.key_up(Key::D);
    h.step();
    assert!(h.state().cam.zoom > z0, "d zooms in");
    h.key_down(Key::S);
    h.run_steps(6);
    h.key_up(Key::S);
    h.step();
    assert!(h.state().cam.zoom < z0, "s zooms out");

    press(&mut h, Key::P);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics"),
        "p goes up to the parent folder"
    );
    press(&mut h, Key::P);
    assert_eq!(selected_path(&h).as_deref(), Some(""), "…and up again");
    press(&mut h, Key::P);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some(""),
        "the root has no parent to climb to"
    );
}

/// `gg` puts the whole graph back on screen; `0` resets only the zoom and
/// leaves you where you were looking.
#[test]
fn gg_refits_the_camera_and_zero_resets_only_the_zoom() {
    let mut h = harness();
    h.state_mut().cam.fitted = true;
    h.state_mut().cam.zoom = 9.0;
    let center = h.state().cam.center;
    press(&mut h, Key::Num0);
    assert!(
        h.state().cam.zoom < 9.0,
        "0 pulls the zoom back to a whole view"
    );
    assert_eq!(h.state().cam.center, center, "…without moving the camera");

    press(&mut h, Key::G);
    assert!(h.state().cam.fitted, "one g is half a chord — nothing yet");
    press(&mut h, Key::G);
    assert!(
        !h.state().cam.fitted,
        "gg refits the whole graph next frame"
    );
}

#[test]
fn brackets_walk_connections_and_enter_follows() {
    let mut h = harness();
    select(&mut h, "index.md");
    // index.md is a file: connections = 4 outlinks (body order), then backlinks
    press(&mut h, Key::CloseBracket);
    assert_eq!(h.state().conn_cursor, Some(0));
    press(&mut h, Key::CloseBracket);
    assert_eq!(h.state().conn_cursor, Some(1), "] steps forward");
    press(&mut h, Key::OpenBracket);
    assert_eq!(h.state().conn_cursor, Some(0), "[ steps back");
    press(&mut h, Key::OpenBracket);
    assert_eq!(h.state().conn_cursor, Some(0), "clamped at the start");
    press(&mut h, Key::Enter);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("projects/rust-app.md"),
        "Enter follows the highlighted connection (index.md's first outlink)"
    );
    assert_eq!(h.state().conn_cursor, None, "consumed by the jump");

    // moving to another node dismisses a live link cursor (it indexes the
    // node you were on); panning does not — the camera is not a move
    press(&mut h, Key::CloseBracket);
    assert!(h.state().conn_cursor.is_some());
    press(&mut h, Key::J);
    assert!(
        h.state().conn_cursor.is_some(),
        "j pans the camera and leaves the link cursor alone"
    );
    press(&mut h, Key::P);
    assert_eq!(h.state().conn_cursor, None, "p moves, so it clears it");
}

#[test]
fn esc_dismisses_link_cursor_then_terminal_cursor_then_selection() {
    let mut h = harness();
    select(&mut h, "index.md");
    h.state_mut().terms.cursor = Some(("tg_claude".into(), "%1".into()));
    press(&mut h, Key::CloseBracket);
    assert!(h.state().conn_cursor.is_some());

    press(&mut h, Key::Escape);
    assert_eq!(h.state().conn_cursor, None, "1st Esc: link cursor");
    assert!(h.state().terms.cursor.is_some());
    assert!(h.state().selected.is_some());

    press(&mut h, Key::Escape);
    assert_eq!(h.state().terms.cursor, None, "2nd Esc: terminal cursor");
    assert!(h.state().selected.is_some());

    press(&mut h, Key::Escape);
    assert_eq!(
        h.state().selected,
        None,
        "3rd Esc: selection — back to pan mode"
    );
}

/// A corrupt/hand-edited view file with a huge-but-finite camera center
/// used to open onto a blank canvas: world_to_screen overflowed to ±inf,
/// nothing painted or hit-tested, and fitted=true blocked the auto-fit
/// that would recover. Restore must clamp center like it clamps zoom.
#[test]
fn corrupt_view_state_camera_is_clamped_on_restore() {
    let d = std::env::temp_dir().join(format!("tg-clamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join(".text-graph")).unwrap();
    std::fs::write(d.join("a.md"), "x").unwrap();
    std::fs::write(
        d.join(".text-graph/view"),
        "text-graph view v1\ncamera\t3e38\t-3e38\t50\ncard\t3e38\t0\t%1\ttg_x\n",
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let v = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        v.cam.center.x.abs() <= 1e6 && v.cam.center.y.abs() <= 1e6,
        "center clamped: was ({}, {})",
        v.cam.center.x,
        v.cam.center.y
    );
    assert_eq!(v.cam.zoom, 50.0, "the sane part of the restore survives");
    let off = v.terms.parked["tg_x"][0].1;
    assert!(off.x.abs() <= 1e5, "card offsets clamped too: {}", off.x);
}

/// egui widgets read key events without consuming them, so every global
/// keybind must check `widget_free`. Regression: typing a filename like
/// "2026-08-10" into a prompt re-fit the camera on each '0', '/' opened
/// the search bar, 'z' framed, and Esc deselected — slamming the
/// navigator shut mid-typing.
#[test]
fn global_keys_do_not_fire_while_a_text_field_has_focus() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut buf = String::new();
    let mut h = Harness::new_ui_state(
        move |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            // a stand-in for the search prompt: any focused TextEdit
            ui.text_edit_singleline(&mut buf).request_focus();
        },
        viewer,
    );
    h.step();
    select(&mut h, "index.md");
    h.state_mut().cam.fitted = true;
    h.step();

    press(&mut h, Key::Num0);
    assert!(
        h.state().cam.fitted,
        "'0' while typing must not re-fit the camera"
    );
    press(&mut h, Key::Slash);
    assert!(
        !h.state().picker.open,
        "'/' while typing must not open the picker"
    );
    press(&mut h, Key::Z);
    assert!(
        h.state().cam.anim.is_none(),
        "'z' while typing must not start a camera glide"
    );
    press(&mut h, Key::F);
    assert!(
        !h.state().picker.open,
        "'f' while typing must not open the picker"
    );
    // Esc is special: egui surrenders widget focus at frame START on
    // Escape, so the focus guard alone can't see a live text field — the
    // search prompt must be its own stage of the dismiss chain
    h.state_mut().picker.open = true;
    h.state_mut().picker.query = "2026".into();
    press(&mut h, Key::Escape);
    assert!(!h.state().picker.open, "Esc closes the picker first…");
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("index.md"),
        "…without deselecting (the navigator must not slam shut)"
    );
}

/// Ctrl+Q is the way back: it drops everything holding the keyboard or
/// the eye, in one press, so `f` works immediately afterwards — from a
/// terminal you were typing into, from an open finder, from anywhere.
#[test]
fn ctrl_q_releases_everything() {
    let mut h = harness();
    select(&mut h, "index.md");
    h.state_mut().terms.focused = Some(("tg_pi".into(), "%1".into()));
    h.state_mut().terms.cursor = Some(("tg_pi".into(), "%1".into()));
    h.state_mut().conn_cursor = Some(0);
    h.state_mut().picker.open = true;
    h.state_mut().settings.open = true;
    let ctx = h.ctx.clone();
    h.state_mut().release_everything(&ctx);
    h.step();

    let v = h.state();
    assert!(
        v.terms.focused.is_none(),
        "the terminal gives the keyboard back"
    );
    assert!(v.terms.cursor.is_none(), "the card cursor goes too");
    assert!(v.selected.is_none(), "and the selection");
    assert!(v.conn_cursor.is_none());
    assert!(!v.picker.open, "an open finder closes");
    assert!(!v.settings.open, "so does the settings window");
    assert!(
        h.ctx.memory(|m| m.focused()).is_none(),
        "and no text field keeps the keys — f has to work next"
    );

    // …and it does: f opens the finder on the very next press
    press(&mut h, Key::F);
    assert!(h.state().picker.open);
}

/// `G` shows the neighbourhood: the zoom follows what the node is
/// connected to, so it means the same thing for a leaf note and for a
/// folder with forty children.
#[test]
fn shift_g_zooms_to_the_selections_neighbourhood() {
    let mut h = harness();
    for _ in 0..2000 {
        if !h.state().sim.active() {
            break;
        }
        h.state_mut().sim.tick(16);
    }
    h.state_mut().cam.zoom = 0.05;

    // the vault root has every top-level entry around it
    select(&mut h, "");
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::G);
    h.step();
    let wide = h.state().cam.zoom;
    assert!(wide > 0.05, "G zooms in from a far-out view: {wide}");
    assert!(h.state().cam.anim.is_some(), "and glides onto the node");

    // a leaf with a couple of links sits tighter than the whole root
    h.state_mut().cam.zoom = 0.05;
    select(&mut h, "topics/grafér.md");
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::G);
    h.step();
    let tight = h.state().cam.zoom;
    assert!(
        tight > wide,
        "a small neighbourhood frames closer than a big one: {tight} vs {wide}"
    );
    assert!(tight <= 6.0, "but never rockets in: {tight}");
}
