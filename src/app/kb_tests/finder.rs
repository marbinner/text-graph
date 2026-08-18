//! The overlay: find and browse sources, content search, result
//! walking, reload survival, and its floating layout.

use super::*;

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
    h.state_mut().cam.anim = None;
    wait_for(&mut h, "the camera glide", |v| v.cam.anim.is_some());
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
    super::install_fonts(&h.ctx);
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
    super::install_fonts(&h.ctx);
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
    super::install_fonts(&h.ctx);
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
    h.state_mut().cam.zoom = 0.4;
    let key = ("tg_claude".to_string(), "%3".to_string());
    h.state_mut().fly_to_card_at(key.clone(), false);
    assert_eq!(h.state().cam.zoom, 0.4, "the finder keeps your zoom");
    assert_eq!(h.state().terms.fly_to, Some(key.clone()), "but recenters");

    h.state_mut().fly_to_card(key);
    assert_eq!(
        h.state().cam.zoom,
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
    super::install_fonts(&h.ctx);
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
    super::install_fonts(&h.ctx);
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
    h.state_mut().cam.zoom = 0.05;
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
