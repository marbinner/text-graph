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
use text_graph::{config, graph, vault};

use super::Viewer;

fn harness() -> Harness<'static, Viewer> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
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
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
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

/// hjkl belong to the CAMERA now, selection or not — choosing a node
/// lives in the finder (f / b). `p` is the one tree move left: up to the
/// parent. s and d zoom, so one hand drives the whole view.
#[test]
fn hjkl_pan_and_sd_zoom_whatever_is_selected() {
    let mut h = harness();
    select(&mut h, "topics/grafér.md");
    let x0 = h.state().center.x;
    h.key_down(Key::H);
    h.run_steps(3);
    h.key_up(Key::H);
    h.step();
    assert!(
        h.state().center.x < x0,
        "h pans left even with a node selected"
    );
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics/grafér.md"),
        "and panning never moves the selection"
    );

    let z0 = h.state().zoom;
    h.key_down(Key::D);
    h.run_steps(3);
    h.key_up(Key::D);
    h.step();
    assert!(h.state().zoom > z0, "d zooms in");
    h.key_down(Key::S);
    h.run_steps(6);
    h.key_up(Key::S);
    h.step();
    assert!(h.state().zoom < z0, "s zooms out");

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
    h.state_mut().fitted = true;
    h.state_mut().zoom = 9.0;
    let center = h.state().center;
    press(&mut h, Key::Num0);
    assert!(
        h.state().zoom < 9.0,
        "0 pulls the zoom back to a whole view"
    );
    assert_eq!(h.state().center, center, "…without moving the camera");

    press(&mut h, Key::G);
    assert!(h.state().fitted, "one g is half a chord — nothing yet");
    press(&mut h, Key::G);
    assert!(!h.state().fitted, "gg refits the whole graph next frame");
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

/// `f` is the finder — with or without a selection (it replaced the old
/// find-in-directory prompt, which only existed in tree-nav mode).
#[test]
fn f_opens_the_picker_with_or_without_a_selection() {
    let mut h = harness();
    press(&mut h, Key::F);
    assert!(h.state().picker.open, "f opens the picker from pan mode");
    press(&mut h, Key::Escape);
    assert!(!h.state().picker.open);

    select(&mut h, "notes/readme.md");
    press(&mut h, Key::F);
    assert!(h.state().picker.open, "and from tree-nav mode");
    press(&mut h, Key::Escape);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("notes/readme.md"),
        "closing it leaves the selection alone"
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
/// dwell, glides the camera — without selecting anything: the selection is
/// what you COMMIT to with Enter, so Esc has to leave you where you were.
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

/// Agents save files constantly, so a reload lands mid-search every few
/// seconds. The name tier re-scores (node indices moved), but nothing the
/// user is looking at may blink: query, cursor row, CONTENT hits (keyed by
/// path, not by node index) and the loaded preview all have to survive —
/// the rescan replaces the hits in place when it lands.
#[test]
fn a_reload_keeps_the_query_cursor_content_hits_and_preview() {
    let mut h = harness();
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "Heading One".into();
    wait_for(&mut h, "the content scan", |v| {
        v.picker.rows.iter().any(|r| r.snippet.is_some())
    });
    let before = h.state().picker.cursor_row().expect("a row").key.clone();
    let hits = h
        .state()
        .picker
        .rows
        .iter()
        .filter(|r| r.snippet.is_some())
        .count();
    assert!(h.state().pane_preview.is_some(), "a preview is loaded");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let rebuilt = graph::build(vault::scan(&root).expect("rescan"));
    h.state_mut().apply_graph(rebuilt);
    h.step();
    assert_eq!(h.state().picker.query, "Heading One", "the query survives");
    assert_eq!(
        h.state().picker.cursor_row().map(|r| r.key.clone()),
        Some(before),
        "and the cursor stays on the same result"
    );
    assert_eq!(
        h.state()
            .picker
            .rows
            .iter()
            .filter(|r| r.snippet.is_some())
            .count(),
        hits,
        "content hits survive the reload — they are keyed by path"
    );
    assert!(
        h.state().pane_preview.is_some(),
        "and the preview is not thrown away"
    );
}

/// The picker's own UI renders headlessly (canvas does not — it would
/// attach tmux mirrors). This exercises the real layout path: the floating
/// prompt and its virtualized result list, plus the side pane's preview
/// with its per-line layout jobs.
#[test]
fn picker_ui_renders_prompt_results_and_preview() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.pump_picker(ui.ctx());
            if v.picker.open {
                v.side_pane(ui);
            }
            v.picker_overlay_ui(ui.ctx());
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
    // the header names the file and gives its path as clickable ancestors
    assert!(
        h.query_by_label_contains("Grafér").is_some(),
        "the preview header names the previewed file"
    );
    assert!(
        h.query_by_label_contains("topics").is_some(),
        "…and its breadcrumb, which is what the ranger's header used to be"
    );
}

/// `b` is the ranger now: the same overlay, listing a folder instead of
/// searching everything. Arrows walk the entries, Enter goes INTO a
/// folder (Shift+Enter takes the folder itself), Backspace on an empty
/// filter goes back up, and Enter on a file takes it like any result.
#[test]
fn b_browses_the_folder_in_the_finder_and_walks_the_tree_with_it() {
    let mut h = harness();
    select(&mut h, "bom.md");
    press(&mut h, Key::B);
    assert_eq!(
        h.state().picker.browsing(),
        Some(""),
        "b lists the selection's folder — bom.md lives in the vault root"
    );
    let names: Vec<String> = h
        .state()
        .picker
        .rows
        .iter()
        .map(|r| r.title.clone())
        .collect();
    assert!(
        names.iter().any(|t| t == "assets/") && names.iter().any(|t| t == "bom"),
        "the folder's entries, dirs first, in tree order: {names:?}"
    );
    // a note titled from frontmatter still shows the file it lives in
    let index = h
        .state()
        .picker
        .rows
        .iter()
        .find(|r| r.title == "Index")
        .expect("index.md is listed under its title")
        .subtitle
        .clone();
    assert_eq!(index, "index.md", "…with its filename alongside");

    // walking the list moves the cursor without touching the selection —
    // browsing is choosing, and Enter is what commits
    press(&mut h, Key::ArrowDown);
    assert_eq!(h.state().picker.cursor, 1);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("bom.md"),
        "the selection waits for Enter"
    );

    // Enter on a folder descends into it
    h.state_mut().picker.cursor = 0;
    h.state_mut().picker.cursor_key = h.state().picker.cursor_row().map(|r| r.key.clone());
    press(&mut h, Key::Enter);
    assert_eq!(h.state().picker.browsing(), Some("assets"), "Enter goes in");

    // …and Backspace on an empty filter comes back out, landing on it
    press(&mut h, Key::Backspace);
    assert_eq!(h.state().picker.browsing(), Some(""));
    assert_eq!(
        h.state().picker.cursor_row().map(|r| r.title.clone()),
        Some("assets/".to_string()),
        "the cursor lands on the folder we came from"
    );

    // typing filters this folder — scoped search, same surface, same keys
    h.state_mut().picker.query = "bom".into();
    h.step();
    let names: Vec<String> = h
        .state()
        .picker
        .rows
        .iter()
        .map(|r| r.title.clone())
        .collect();
    assert_eq!(names, vec!["bom".to_string()]);
    assert!(
        h.state().picker.content.is_empty(),
        "and never reads a file: browsing is structural"
    );

    // Enter on a file takes it, like any result
    press(&mut h, Key::Enter);
    assert_eq!(selected_path(&h).as_deref(), Some("bom.md"));
    assert!(!h.state().picker.open, "taking a result closes the overlay");
}

/// An empty find prompt is the launchpad: what changed most recently,
/// newest first. Under agents that rewrite notes all day, that is the
/// useful answer to "f, and now what?" — and it must not light up or
/// move anything until you pick something.
#[test]
fn an_empty_find_prompt_lists_what_changed_last() {
    let d = std::env::temp_dir().join(format!("tg-recent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for name in ["first.md", "second.md", "third.md"] {
        std::fs::write(d.join(name), "x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
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
    let id = h.state().g.by_path("first.md").expect("exists");
    h.state_mut().selected = Some(id);
    press(&mut h, Key::F);
    let titles: Vec<String> = h
        .state()
        .picker
        .rows
        .iter()
        .map(|r| r.title.clone())
        .collect();
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(
        titles,
        vec!["third", "second", "first"],
        "newest first, without typing a thing"
    );
    assert!(
        h.state().picker.node_scores.iter().all(Option::is_none),
        "and nothing dims on the canvas until there is a query"
    );

    press(&mut h, Key::ArrowDown);
    assert_eq!(h.state().picker.cursor, 1);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("first.md"),
        "walking the list is browsing, not selecting"
    );
    press(&mut h, Key::Enter);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("second.md"),
        "Enter takes it, like any other result"
    );
}

/// Tab swaps the source and keeps what you typed: a filter that found
/// nothing in this folder is usually the thing to search the vault for.
#[test]
fn tab_swaps_between_browsing_and_finding_keeping_the_query() {
    let mut h = harness();
    select(&mut h, "index.md");
    press(&mut h, Key::B);
    h.state_mut().picker.query = "grafer".into();
    h.step();
    assert!(
        h.state().picker.rows.is_empty(),
        "no entry of the root folder matches"
    );
    press(&mut h, Key::Tab);
    assert_eq!(h.state().picker.browsing(), None, "tab lands in find");
    assert_eq!(h.state().picker.query, "grafer", "with the query intact");
    wait_for(&mut h, "the vault-wide match", |v| {
        v.picker.rows.iter().any(|r| r.title.contains("Grafér"))
    });
}

/// The keystroke that OPENS the picker must not land in its prompt: the
/// text event for "/" is queued for the same frame in which the field
/// takes focus.
#[test]
fn the_slash_that_opens_the_picker_does_not_land_in_the_prompt() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            v.pump_picker(ui.ctx());
            // like the real ui(): the pane exists only while something is
            // selected or a search is live, and the floating prompt holds
            // the keyboard for exactly as long as the picker is open
            if v.picker.open || v.selected.is_some() {
                v.side_pane(ui);
            }
            v.picker_overlay_ui(ui.ctx());
        },
        viewer,
    );
    super::install_icon_font(&h.ctx);
    h.step();
    // a real keyboard sends both a Text event and a Key event
    h.input_mut()
        .events
        .push(eframe::egui::Event::Text("/".into()));
    h.key_press(Key::Slash);
    h.step();
    h.step();
    assert!(h.state().picker.open, "the picker opened");
    assert_eq!(h.state().picker.query, "", "and its prompt is empty");
}

/// A [[wikilink]] clicked in a preview must jump, never reach the OS
/// browser — and that has to hold in BOTH pane modes, since they share one
/// preview column. Regression risk: the claim used to live in the ranger
/// body, which the search mode does not render.
#[test]
fn tg_links_are_claimed_in_the_search_preview_too() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let target = viewer.g.by_path("topics/grafér.md").expect("target exists");
    let mut h = Harness::new_ui_state(
        move |ui, v: &mut Viewer| {
            // stand in for a click inside the rendered markdown
            ui.ctx().output_mut(|o| {
                o.commands.push(eframe::egui::OutputCommand::OpenUrl(
                    eframe::egui::output::OpenUrl::same_tab(format!(
                        "{}{}",
                        text_graph::mdview::SCHEME,
                        target.0
                    )),
                ));
            });
            v.pump_picker(ui.ctx());
            v.side_pane(ui);
            v.picker_overlay_ui(ui.ctx());
        },
        viewer,
    );
    super::install_icon_font(&h.ctx);
    // search mode: a name match, so the preview is the shared column
    h.state_mut().picker.open = true;
    h.state_mut().picker.query = "index".into();
    h.step();
    h.step();
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("topics/grafér.md"),
        "the tg:// click jumped instead of opening a browser"
    );
    let leaked = h.ctx.output(|o| {
        o.commands
            .iter()
            .any(|c| matches!(c, eframe::egui::OutputCommand::OpenUrl(_)))
    });
    assert!(!leaked, "and the command was claimed, not passed to the OS");
}

/// Taking a TERMINAL result must not change the zoom: a focused card
/// expands to a readable size at any zoom, and snapping to CARD_ZOOM (what
/// a double-click does, deliberately) would throw away the overview the
/// search was launched from. Only the double-click gesture zooms.
#[test]
fn the_finder_recenters_on_a_card_without_zooming() {
    let mut h = harness();
    h.state_mut().zoom = 0.4;
    let key = ("tg_claude".to_string(), "%3".to_string());
    h.state_mut().fly_to_card_at(key.clone(), false);
    assert_eq!(h.state().zoom, 0.4, "the finder keeps your zoom");
    assert_eq!(h.state().terms.fly_to, Some(key.clone()), "but recenters");

    h.state_mut().fly_to_card(key);
    assert_eq!(
        h.state().zoom,
        super::terminals::CARD_ZOOM,
        "double-click still flies in"
    );
}

/// The "scanning…" hint is for scans long enough to explain a wait. Every
/// keystroke starts one and every agent save (a vault reload) restarts
/// one, and on a normal vault those finish in milliseconds — showing the
/// hint for each made it strobe in and out.
#[test]
fn the_scanning_hint_stays_quiet_for_short_scans() {
    let mut h = harness();
    press(&mut h, Key::F);
    h.state_mut().picker.query = "heading".into();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    for i in 0..40 {
        h.step();
        assert!(
            !h.state().picker.scan_hint(),
            "the fixture scan is milliseconds long — the hint must stay quiet"
        );
        if i == 20 {
            // an agent saving a note mid-search: a reload, and the rescan
            // it kicks off, must not make the hint blink either
            let rebuilt = graph::build(vault::scan(&root).expect("rescan"));
            h.state_mut().apply_graph(rebuilt);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        h.state().picker.rows.iter().any(|r| r.snippet.is_some()),
        "…while the scan it stayed quiet about actually ran"
    );
}

/// `,` opens the ⚙ window and Esc closes it — and, like the picker, a
/// pending text edit inside it is its own first stage of the dismiss
/// chain (egui drops widget focus at frame START on Escape, so the
/// `widget_free` guard can never see the field being escaped out of).
#[test]
fn comma_opens_settings_and_esc_backs_out_one_stage_at_a_time() {
    let mut h = harness();
    select(&mut h, "index.md");
    press(&mut h, Key::Comma);
    assert!(h.state().settings.open, "',' opens the settings window");

    press(&mut h, Key::Comma);
    assert!(!h.state().settings.open, "',' again closes it");

    press(&mut h, Key::Comma);
    h.state_mut().settings.editing = Some(("editor", "hx".into()));
    press(&mut h, Key::Escape);
    assert!(
        h.state().settings.open,
        "the first Esc abandons the half-typed value, not the window"
    );
    assert!(h.state().settings.editing.is_none(), "…and drops the edit");

    press(&mut h, Key::Escape);
    assert!(!h.state().settings.open, "the second Esc closes the window");
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("index.md"),
        "backing out of settings must not also deselect"
    );
}

/// The window renders itself from the registry: the section's rows, their
/// help text, and a filter that reaches across sections.
#[test]
fn settings_render_the_registry_and_filter_across_sections() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(|ui, v: &mut Viewer| v.settings_ui(ui.ctx()), viewer);
    h.state_mut().settings.open = true;
    h.step();
    h.step();
    assert!(
        h.query_by_label_contains("light theme").is_some(),
        "the appearance section's rows render"
    );
    assert!(
        h.query_by_label_contains("terminal cards stay dark")
            .is_some(),
        "each row explains itself"
    );
    assert!(
        h.query_by_label_contains("default agent").is_none(),
        "another section's rows stay in their section"
    );

    h.state_mut().settings.filter = "agent".into();
    h.step();
    h.step();
    assert!(
        h.query_by_label_contains("default agent").is_some(),
        "the filter reaches settings outside the open section"
    );
}

/// A setting is only real once it survives a restart: the window writes
/// through `Spec::apply`, so what the UI can set is exactly what the file
/// can carry back.
#[test]
fn a_setting_changed_in_the_window_round_trips_through_the_config_file() {
    use text_graph::config::{Value, spec};
    let mut h = harness();
    let s = spec("hover_delay").expect("declared");
    s.apply(&mut h.state_mut().cfg, Value::Num(0.8));
    let text = text_graph::config::to_text(&h.state().cfg);
    let back = text_graph::config::from_text(&text);
    assert_eq!(back.hover_delay, 0.8, "the value comes back as written");
    assert_eq!(back, h.state().cfg, "and nothing else moved");
}

/// Appearance settings are read at paint time, so what can be checked
/// headlessly is the derivations they feed: the label LOD ramp and the
/// radii (which also drive hit-testing, not just the picture).
#[test]
fn label_density_and_node_scale_move_what_they_feed() {
    use text_graph::graph::NodeKind;
    let r = 2.1; // just under the default leaf ramp
    assert_eq!(
        super::label_lod(NodeKind::File, r, 1.0),
        0.0,
        "at default density this label is still hidden"
    );
    assert!(
        super::label_lod(NodeKind::File, r, 2.0) > 0.0,
        "denser labels reach readable size at less zoom"
    );
    assert!(
        super::label_lod(NodeKind::Dir, r, 1.0) > super::label_lod(NodeKind::File, r, 1.0),
        "the dirs-surface-earlier offset survives the shift"
    );

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let g = graph::build(vault::scan(&root).expect("fixture scans"));
    let base = Viewer::derived(&g, 1.0).radius;
    let bigger = Viewer::derived(&g, 1.5).radius;
    assert!(
        base.iter()
            .zip(&bigger)
            .all(|(a, b)| (b - a * 1.5).abs() < 1e-3),
        "node scale multiplies every radius, cap included"
    );
}

/// Content search off makes the picker a name/alias/path finder — no
/// worker, no snippets — while name matching keeps working.
#[test]
fn content_search_can_be_turned_off() {
    let mut h = harness();
    h.state_mut().cfg.content_search = false;
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "Heading One".into();
    for _ in 0..25 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert!(
        h.state().picker.rows.iter().all(|r| r.snippet.is_none()),
        "no file was scanned for content"
    );
    h.state_mut().picker.query = "index".into();
    wait_for(&mut h, "the name match", |v| !v.picker.rows.is_empty());
    assert!(
        h.state()
            .picker
            .rows
            .iter()
            .any(|r| r.title.contains("Index")),
        "names still match with content search off"
    );
}

/// The hover popup is a setting: off means a dwell opens nothing, at any
/// delay.
#[test]
fn hover_previews_can_be_turned_off() {
    use std::time::{Duration, Instant};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(|ui, v: &mut Viewer| v.hover_preview_ui(ui), viewer);
    let id = h.state().g.by_path("index.md").expect("index exists");
    let since = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    h.state_mut().cfg.hover_previews = false;
    h.state_mut().hover_since = Some((id, since, eframe::egui::Pos2::new(80.0, 80.0)));
    h.step();
    h.step();
    assert!(
        h.query_by_label("Index").is_none(),
        "the dwell popup must stay closed when previews are off"
    );
}

/// `?` goes straight to the key list — the one thing in the settings
/// window people look for by name. There was no help surface at all
/// before it.
#[test]
fn question_mark_opens_the_key_list() {
    let mut h = harness();
    press(&mut h, Key::Questionmark);
    assert!(h.state().settings.open);
    assert!(
        matches!(h.state().settings.tab, super::settings::Tab::Keys),
        "? lands on the keys tab, not wherever the window was left"
    );
}

#[test]
fn the_key_list_renders_and_a_filter_falls_back_to_settings() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(|ui, v: &mut Viewer| v.settings_ui(ui.ctx()), viewer);
    h.state_mut().open_key_help();
    h.step();
    h.step();
    assert!(
        h.query_by_label_contains("a selected node").is_some(),
        "the key groups render"
    );
    assert!(
        h.query_by_label_contains("launch the default agent there")
            .is_some(),
        "…with what each key does"
    );

    // typing in the filter box is always a search for a SETTING
    h.state_mut().settings.filter = "dwell".into();
    h.step();
    h.step();
    assert!(
        h.query_by_label_contains("hover dwell").is_some(),
        "a filter reaches the settings even from the keys tab"
    );
}

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
    super::install_icon_font(&h.ctx);
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

/// `r` reads the same file the other way: source with line numbers
/// instead of rendered markdown. One previewer with two readings — the
/// toggle is a setting, so it is the same act as the checkbox and it
/// survives a restart.
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
    assert!(h.state().cfg.preview_raw, "r flips the setting");
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

/// The pane's preview is cached by SUBJECT identity, which a structural
/// reload leaves alone — but the NodeId inside it moves, and a stale one
/// would preview (and jump to) a different node.
#[test]
fn a_structural_reload_reaims_the_pane_at_the_right_node() {
    let d = std::env::temp_dir().join(format!("tg-reaim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("keeper.md"), "# keeper\n").unwrap();
    std::fs::write(d.join("zzz.md"), "# zzz\n").unwrap();
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
    select(&mut h, "keeper.md");
    h.step();
    let subject = h.state().pane_preview.as_ref().and_then(|p| p.subject);
    assert_eq!(subject, h.state().selected, "the pane is on the selection");

    // a new file sorts BEFORE it, shifting every index after it
    std::fs::write(d.join("aaa.md"), "# aaa\n").unwrap();
    let rebuilt = graph::build(vault::scan(&d).expect("rescan"));
    h.state_mut().apply_graph(rebuilt);
    h.step();
    let subject = h
        .state()
        .pane_preview
        .as_ref()
        .and_then(|p| p.subject)
        .expect("still previewing something");
    let path = h.state().g.node(subject).path.clone();
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(path, "keeper.md", "and it followed the file, not the index");
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
    super::install_icon_font(&h.ctx);
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

/// The finder's list fills the window below the prompt. It used to be
/// capped at a fraction of the canvas, which on a laptop screen left room
/// for two or three results — and with an empty prompt the list wasn't
/// drawn at all, so the recently-edited rows were built and then never
/// shown.
#[test]
fn the_finder_list_fills_the_window_and_shows_the_empty_prompt_rows() {
    let d = std::env::temp_dir().join(format!("tg-listh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for i in 0..40 {
        std::fs::write(d.join(format!("note{i:02}.md")), "x").unwrap();
    }
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            v.pump_picker(ui.ctx());
            v.picker_overlay_ui(ui.ctx());
        },
        viewer,
    );
    super::install_icon_font(&h.ctx);
    // pin the prompt high so the assertion is about the list FILLING what
    // is below it, not about where this build puts the prompt
    h.state_mut().cfg.finder_y = 0.25;
    press(&mut h, Key::F);
    h.step();
    let win = h.ctx.content_rect().height();
    let rows_shown = h.state().picker.list_h / 34.0;
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        !h.state().picker.rows.is_empty(),
        "the empty prompt lists what changed last"
    );
    assert!(
        h.state().picker.list_h > 0.0,
        "…and those rows are actually DRAWN — the list used to render \
         only while a query was live"
    );
    assert!(
        rows_shown >= 8.0,
        "only {rows_shown:.1} rows fit in a {win}pt window — the list is \
         supposed to run to the bottom margin"
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
    super::install_icon_font(&h.ctx);
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

/// The result list must grow back. An egui `Area` sizes its Ui from LAST
/// frame's content, so measuring the list against `available_height()`
/// fed it its own previous height: one search with two hits shrank the
/// overlay, which capped the next list shorter, which shrank it again —
/// a latch that only ratcheted down, and the finder never recovered its
/// height until you closed it.
#[test]
fn a_narrow_result_set_does_not_shrink_the_finder_for_good() {
    let d = std::env::temp_dir().join(format!("tg-shrink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for i in 0..40 {
        std::fs::write(d.join(format!("note{i:02}.md")), "x").unwrap();
    }
    std::fs::write(d.join("lonely.md"), "x").unwrap();
    let scan = vault::scan(&d).expect("scans");
    let viewer = Viewer::new(graph::build(scan), d.clone(), config::Config::default());
    let mut h = Harness::new_ui_state(
        |ui, v: &mut Viewer| {
            v.handle_keys(ui);
            v.pump_picker(ui.ctx());
            v.picker_overlay_ui(ui.ctx());
        },
        viewer,
    );
    super::install_icon_font(&h.ctx);
    press(&mut h, Key::F);

    let query = |h: &mut Harness<'_, Viewer>, q: &str| {
        h.state_mut().picker.query = q.into();
        for _ in 0..3 {
            h.step();
        }
        (h.state().picker.rows.len(), h.state().picker.list_h)
    };
    let (many, tall) = query(&mut h, "note");
    let (few, short) = query(&mut h, "lonely");
    let (_, back) = query(&mut h, "note");
    let _ = std::fs::remove_dir_all(&d);

    assert!(many > 10 && few <= 2, "{many} then {few} rows");
    assert!(
        tall > 5.0 * 34.0,
        "the list has room for its results: {tall}"
    );
    assert_eq!(
        (short, back),
        (tall, tall),
        "the finder holds its height: it must not shrink to a narrow \
         result set (and then be stuck there), nor resize under the eye \
         on every keystroke"
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
    assert!(!h.state().cfg.preview_raw, "the default settings");

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
    super::install_icon_font(&h.ctx);
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

    let zoom = h.state().zoom;
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
    assert_eq!(h.state().zoom, zoom, "and the zoom is left alone");
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

/// Whatever the finder highlights is opened on the canvas while you look
/// at it — a card expands, a note becomes its preview box — so the graph
/// shows the thing itself instead of a dot you would have to zoom into.
#[test]
fn the_finders_highlight_opens_the_node_on_the_canvas() {
    let mut h = harness();
    press(&mut h, Key::F);
    h.state_mut().picker.query = "grafer".into();
    wait_for(&mut h, "the name match", |v| !v.picker.rows.is_empty());
    let id = h.state().g.by_path("topics/grafér.md").expect("the note");
    assert_eq!(
        h.state().highlighted_node(),
        Some(id),
        "the highlighted row is the highlighted node"
    );
    // zoomed far out, where it would otherwise be a dot
    h.state_mut().zoom = 0.05;
    h.step();
    let rect = h
        .state()
        .node_box(id, eframe::egui::Pos2::ZERO, 1.0)
        .expect("an opened node has a box, however far out you are");
    assert!(
        rect.width() > 60.0,
        "and it is big enough to read: {rect:?}"
    );

    press(&mut h, Key::Escape);
    assert_eq!(
        h.state().highlighted_node(),
        None,
        "closing the finder closes it again"
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

/// Clicking anywhere outside the settings window closes it, like the
/// terminal cards' own click-away — and commits a pending text edit
/// rather than dropping it on the floor.
#[test]
fn a_click_outside_the_settings_window_closes_it() {
    use eframe::egui::{Event, PointerButton, Pos2};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture scans");
    let viewer = Viewer::new(graph::build(scan), root, config::Config::default());
    let mut h = Harness::new_ui_state(|ui, v: &mut Viewer| v.settings_ui(ui.ctx()), viewer);
    h.state_mut().settings.open = true;
    h.run();

    // a click in the middle of the window changes nothing
    let mid = h.ctx.content_rect().center();
    let click = |h: &mut Harness<'_, Viewer>, pos: Pos2| {
        h.event(Event::PointerMoved(pos));
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        h.step();
    };
    click(&mut h, mid);
    assert!(h.state().settings.open, "a click inside keeps it open");

    // a half-typed value is committed, not dropped, when it closes
    h.state_mut().settings.editing = Some(("editor", "hx".into()));
    click(&mut h, Pos2::new(4.0, 4.0));
    assert!(!h.state().settings.open, "a click outside closes it");
    assert_eq!(h.state().cfg.editor, "hx", "and keeps what was typed");
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
    h.state_mut().zoom = 0.05;

    // the vault root has every top-level entry around it
    select(&mut h, "");
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::G);
    h.step();
    let wide = h.state().zoom;
    assert!(wide > 0.05, "G zooms in from a far-out view: {wide}");
    assert!(h.state().cam_anim.is_some(), "and glides onto the node");

    // a leaf with a couple of links sits tighter than the whole root
    h.state_mut().zoom = 0.05;
    select(&mut h, "topics/grafér.md");
    h.key_press_modifiers(eframe::egui::Modifiers::SHIFT, Key::G);
    h.step();
    let tight = h.state().zoom;
    assert!(
        tight > wide,
        "a small neighbourhood frames closer than a big one: {tight} vs {wide}"
    );
    assert!(tight <= 6.0, "but never rockets in: {tight}");
}
