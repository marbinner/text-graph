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
    assert!(h.state().picker.preview.is_some(), "a preview is loaded");

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
        h.state().picker.preview.is_some(),
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
    assert!(
        h.query_by_label_contains("topics/grafér.md").is_some(),
        "the preview header names the previewed file"
    );
}

/// An empty prompt IS the ranger: no result rows, nothing lit on the
/// canvas, and the arrows walk the sibling column exactly like j/k. The
/// pane opens on the vault root when nothing is selected yet.
#[test]
fn an_empty_prompt_leaves_the_ranger_in_place_and_the_arrows_walk_it() {
    let mut h = harness();
    select(&mut h, "bom.md");
    press(&mut h, Key::F);
    h.step();
    assert!(h.state().picker.open);
    assert!(
        h.state().picker.rows.is_empty(),
        "an empty query ranks nothing"
    );
    assert!(
        h.state().picker.node_scores.iter().all(Option::is_none),
        "and dims nothing on the canvas"
    );

    press(&mut h, Key::ArrowDown);
    assert_eq!(
        selected_path(&h).as_deref(),
        Some("empty.md"),
        "↓ steps to the next sibling, like j"
    );
    press(&mut h, Key::ArrowUp);
    assert_eq!(selected_path(&h).as_deref(), Some("bom.md"), "↑ steps back");
    assert!(h.state().cam_anim.is_some(), "the camera follows the walk");

    // …and with nothing selected, the first step enters the vault root
    h.state_mut().selected = None;
    press(&mut h, Key::ArrowDown);
    assert_eq!(selected_path(&h).as_deref(), Some("assets"));
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
