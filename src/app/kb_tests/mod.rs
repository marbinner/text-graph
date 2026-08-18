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
// The whole app namespace, so test bodies keep addressing items the way
// they did when this was one file (`super::pane_width`, …): submodules
// resolve `super::X` through this module's imports.
#[allow(unused_imports)]
use super::*;

mod cards;
mod finder;
mod hover_and_reload;
mod keybinds;
mod pane;
mod settings_window;

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
