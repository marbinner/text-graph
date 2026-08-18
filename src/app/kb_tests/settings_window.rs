//! The ⚙ window: registry rendering, live apply + config round-trip,
//! the key list, and dismissal.

use super::*;

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

/// Changing search policy while results are live retires the old content
/// generation immediately, and enabling it again queues a replacement.
#[test]
fn content_search_setting_rebuilds_an_open_finder() {
    let mut h = harness();
    press(&mut h, Key::Slash);
    h.state_mut().picker.query = "Heading One".into();
    wait_for(&mut h, "the initial content scan", |v| {
        v.picker.rows.iter().any(|r| r.snippet.is_some())
    });

    h.state_mut().cfg.content_search = false;
    h.state_mut().after_change("content_search");
    h.step();
    assert!(
        h.state().picker.rows.iter().all(|r| r.snippet.is_none()),
        "turning content search off removes already-rendered content hits"
    );

    h.state_mut().cfg.content_search = true;
    h.state_mut().after_change("content_search");
    wait_for(&mut h, "the replacement content scan", |v| {
        v.picker.rows.iter().any(|r| r.snippet.is_some())
    });
}

/// A full reset must reapply every cached setting side effect, not only the
/// palette. Unknown fields remain intact for forward compatibility.
#[test]
fn restore_defaults_reapplies_cached_settings() {
    let mut h = harness();
    select(&mut h, "index.md");
    h.step();
    assert!(h.state().pane_preview.is_some());

    h.state_mut().cfg.preview_raw = true;
    h.state_mut().cfg.light = true;
    h.state_mut().after_change("theme_light");
    h.state_mut().cfg.node_scale = 1.8;
    h.state_mut().after_change("node_scale");
    h.state_mut().cfg.unknown = vec!["future_setting\t7".into()];
    assert!(h.state().theme.light);

    h.state_mut().restore_default_settings();

    assert!(!h.state().cfg.preview_raw);
    assert!(
        !h.state().theme.light,
        "the default dark palette is restored"
    );
    assert!(
        h.state().apply_visuals,
        "egui visuals are scheduled for the restored palette"
    );
    assert_eq!(
        h.state().derived.radius,
        Viewer::derived(&h.state().g, 1.0).radius,
        "hit-testing radii follow the restored node scale"
    );
    assert!(
        h.state().pane_preview.is_none(),
        "the cached preview is invalidated when preview mode resets"
    );
    assert_eq!(
        h.state().cfg.unknown,
        vec!["future_setting\t7"],
        "unknown settings survive the reset"
    );
}

/// Editing the extra-agent list updates the already-running discovery
/// thread's shared allowlist; it must not require an application restart.
#[test]
fn extra_agents_setting_updates_live_discovery() {
    let mut h = harness();
    assert!(
        !h.state()
            .terms
            .allowlist
            .lock()
            .unwrap()
            .iter()
            .any(|agent| agent == "my-live-agent")
    );

    h.state_mut().cfg.extra_agents = "my-live-agent".into();
    h.state_mut().after_change("extra_agents");

    assert!(
        h.state()
            .terms
            .allowlist
            .lock()
            .unwrap()
            .iter()
            .any(|agent| agent == "my-live-agent")
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
