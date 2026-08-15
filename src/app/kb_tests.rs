//! Viewer state-machine tests, driven through a real egui context via
//! egui_kittest — synthetic events, real frames, no window and no
//! renderer: the keyboard modal logic (handle_keys + update_search), the
//! hover-popup render path, and reload carry-over (apply_graph). The
//! harness never runs canvas — sync_terminals would attach mirrors
//! against the user's default tmux server (house rule).

use std::path::PathBuf;

use eframe::egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use text_graph::{graph, vault};

use super::Viewer;

fn harness() -> Harness<'static, Viewer> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root);
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            v.update_search();
        },
        viewer,
    );
    h.step();
    h
}

fn selected_path(h: &Harness<'_, Viewer>) -> Option<String> {
    h.state()
        .selected
        .map(|id| h.state().g.node(id).path.clone())
}

fn select(h: &mut Harness<'_, Viewer>, path: &str) {
    let id = h.state().g.by_path(path).expect("path exists");
    h.state_mut().selected = Some(id);
}

fn press(h: &mut Harness<'_, Viewer>, key: Key) {
    h.key_press(key);
    h.step();
}

#[test]
fn hover_dwell_renders_a_popup_with_the_file_body() {
    use std::time::{Duration, Instant};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root);
    let mut h = Harness::new_ui_state(|ui, v: &mut Viewer| v.hover_preview_ui(ui), viewer);
    let id = h.state().g.by_path("index.md").expect("index exists");
    let since = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    h.state_mut().hover_since = Some((id, since, eframe::egui::Pos2::new(80.0, 80.0)));
    h.step();
    h.step();
    // the popup carries the display name and the rendered body; index.md's
    // body contains "Heading One"
    assert!(
        h.query_by_label("Index").is_some(),
        "popup title label missing — hover preview did not render"
    );
    assert!(
        h.query_by_label_contains("Heading One").is_some(),
        "popup body missing — file content did not render"
    );
}

#[test]
fn structure_identical_reload_keeps_sim_still_and_dwell_alive() {
    use std::time::Instant;
    let mut h = harness();
    for _ in 0..10_000 {
        if !h.state().sim.active() {
            break;
        }
        h.state_mut().sim.tick(16);
    }
    assert!(!h.state().sim.active(), "sim must settle");
    let id = h.state().g.by_path("index.md").expect("index exists");
    h.state_mut().hover_since = Some((id, Instant::now(), eframe::egui::Pos2::new(5.0, 5.0)));

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let rebuilt = graph::build(vault::scan(&root).expect("rescan"));
    h.state_mut().apply_graph(rebuilt);
    assert!(
        !h.state().sim.active(),
        "an agent saving text (no structural change) must not set the graph in motion"
    );
    assert_eq!(
        h.state().hover_since.map(|(i, ..)| i),
        Some(id),
        "the hover dwell survives a reload (remapped by ident)"
    );

    // a structural change (different links) still reheats
    let mut changed = graph::build(vault::scan(&root).expect("rescan"));
    changed.links.pop();
    h.state_mut().apply_graph(changed);
    assert!(h.state().sim.active(), "structural reloads still re-settle");
}

#[test]
fn hjkl_walks_the_tree_when_a_node_is_selected() {
    let mut h = harness();
    // root's files in sorted order: bom, empty, frontmatter-only, index
    select(&mut h, "bom.md");
    press(&mut h, Key::J);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("empty.md"),
        "j = next sibling"
    );
    press(&mut h, Key::K);
    assert_eq!(selected_path(&h).as_deref(), Some("bom.md"), "k = previous");
    press(&mut h, Key::K);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics"),
        "k crosses from the files into the dirs — one sorted sibling list"
    );
    press(&mut h, Key::J);
    assert_eq!(selected_path(&h).as_deref(), Some("bom.md"));
    press(&mut h, Key::H);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some(""),
        "h = parent (vault root)"
    );
    press(&mut h, Key::L);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("assets"),
        "l enters the root: first child, dirs-first order"
    );
    // G / gg jump to the ends of the sibling list
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::G);
    h.step();
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("index.md"),
        "G = last sibling"
    );
    press(&mut h, Key::G);
    press(&mut h, Key::G);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("assets"),
        "gg = first sibling"
    );
}

#[test]
fn hjkl_pans_the_camera_when_nothing_is_selected() {
    let mut h = harness();
    assert!(h.state().selected.is_none());
    let x0 = h.state().center.x;
    h.key_down(Key::H);
    h.run_steps(3);
    h.key_up(Key::H);
    h.step();
    assert!(h.state().center.x < x0, "h pans left with no selection");

    // with a selection, the same key walks the tree and the camera recenters
    // on the node instead of free-panning
    select(&mut h, "bom.md");
    let sel_before = selected_path(&h);
    press(&mut h, Key::J);
    assert_ne!(selected_path(&h), sel_before, "j navigates, not pans");
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

    // tree moves dismiss a live link cursor
    press(&mut h, Key::CloseBracket);
    assert!(h.state().conn_cursor.is_some());
    press(&mut h, Key::J);
    assert_eq!(h.state().conn_cursor, None, "j clears the link cursor");
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

#[test]
fn f_opens_the_find_prompt_only_in_nav_mode() {
    let mut h = harness();
    press(&mut h, Key::F);
    assert!(h.state().nav_find.is_none(), "no selection, no prompt");
    select(&mut h, "notes/readme.md");
    press(&mut h, Key::F);
    assert!(h.state().nav_find.is_some(), "f opens find-in-directory");
}

#[test]
fn search_enter_jumps_to_the_best_node_and_clears_stale_term_cursor() {
    let mut h = harness();
    h.state_mut().terms.cursor = Some(("tg_x".into(), "%9".into()));
    h.state_mut().search_open = true;
    h.state_mut().query = "grafer".into();
    h.step(); // update_search scores the query
    assert!(h.state().best.is_some(), "fuzzy match found");
    press(&mut h, Key::Enter);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics/grafér.md"),
        "Enter jumps to the best hit"
    );
    assert!(!h.state().search_open, "search closed");
    assert_eq!(
        h.state().terms.cursor,
        None,
        "a stale terminal cursor must not hijack the next Enter"
    );
}
