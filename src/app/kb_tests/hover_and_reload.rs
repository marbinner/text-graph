//! Hover dwell → popup, and reload carry-over keeping hover, dwell and
//! the sim steady underneath it.

use super::*;

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
