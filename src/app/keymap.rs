//! One keybinding table, one dispatcher.
//!
//! Every graph-action key is a row in [`BINDINGS`]: its chords, its press
//! kind, its guard, the ⚙ key-list row that documents it, a state
//! precondition, and the action. The dispatcher applies those rules
//! centrally and fires at most one row per frame, top to bottom — row
//! order IS priority order, which is what makes chains out of shared
//! chords: the two Esc rows are the dismiss order, the three Enter rows
//! are contexts from most to least specific, and a row whose `when` says
//! no falls through to the next.
//!
//! Two rules every row inherits, which used to be per-branch discipline
//! in an if/else chain (and a working-notes rule enforced by review):
//!
//! - [`Guard::WidgetFree`] is the default posture. egui widgets read key
//!   events without consuming them, so keys typed into a focused text
//!   field (the search prompt, a settings field) reach this dispatcher
//!   too — unguarded branches once re-fit the camera on '0' typed into
//!   the find prompt. The Esc rows opt out ([`Guard::Always`]): egui
//!   surrenders widget focus at frame START on Escape, so a focus guard
//!   can never see the field being escaped out of — a transient text
//!   input must be its own first stage of the Esc dismiss chain (the
//!   search prompt is, via the picker branch in `handle_keys`; the
//!   settings window is, via its `when: settings.open` row here).
//! - [`Press::Fresh`] ignores key-repeat ticks. Anything that ultimately
//!   spawns a process (editor, terminal, agent) or opens a window
//!   belongs in this list — a held key would otherwise spawn one per
//!   repeat tick.
//!
//! This table serves dispatch. The ⚙ window's `KEYS` list (settings.rs)
//! serves learning — its own grouping and order, plus the gestures and
//! sub-mode keys (the picker's, the cards') that are dispatched
//! elsewhere. `every_binding_cites_its_key_list_row` holds the two
//! together: a row here names the key-list row that documents it, so a
//! binding can no longer ship undocumented.
//!
//! Not rows, by design: the picker's own keys (`picker_keys`, modal while
//! it is open), continuous camera movement (`camera_keys`, key_down each
//! frame rather than press events), the `]`/`[` connection stepping
//! (`bracket_keys`, allowed to fire alongside a row in the same frame,
//! exactly like the old code), and Ctrl+Q (`release_everything`, read at
//! the app level so it works even while a terminal card is draining the
//! keyboard).

use std::time::{Duration, Instant};

use eframe::egui::{self, Key};
use text_graph::graph::NodeKind;

use super::Viewer;

/// Modifier requirement for a [`Chord`]. `Any` is for keys where the
/// modifier is part of producing the character ('?' carries Shift on
/// every layout that has it) or where every variant should act (0, Esc,
/// Enter behave the same shifted).
enum Mods {
    None,
    Shift,
    Command,
    Any,
}

struct Chord {
    mods: Mods,
    key: Key,
}

const fn bare(key: Key) -> Chord {
    Chord {
        mods: Mods::None,
        key,
    }
}
const fn shift(key: Key) -> Chord {
    Chord {
        mods: Mods::Shift,
        key,
    }
}
const fn command(key: Key) -> Chord {
    Chord {
        mods: Mods::Command,
        key,
    }
}
const fn any_mods(key: Key) -> Chord {
    Chord {
        mods: Mods::Any,
        key,
    }
}

impl Chord {
    fn pressed(&self, ui: &egui::Ui, press: Press) -> bool {
        let mods_ok = ui.input(|i| match self.mods {
            Mods::None => i.modifiers.is_none(),
            Mods::Shift => i.modifiers.shift_only(),
            Mods::Command => i.modifiers.command,
            Mods::Any => true,
        });
        mods_ok
            && match press {
                Press::Repeat => ui.input(|i| i.key_pressed(self.key)),
                Press::Fresh => pressed_fresh(ui, self.key),
            }
    }
}

enum Trigger {
    Chords(&'static [Chord]),
    /// Tab / Shift+Tab, consumed from egui's input in the snapshot phase
    /// of `handle_keys` — consumption is unconditional (egui's own focus
    /// navigation must never see them), while the ACTION is still guarded
    /// like any row.
    Tab,
    BackTab,
}

impl Trigger {
    fn pressed(&self, ui: &egui::Ui, press: Press, tab: bool, back_tab: bool) -> bool {
        match self {
            Trigger::Chords(chords) => chords.iter().any(|c| c.pressed(ui, press)),
            Trigger::Tab => tab,
            Trigger::BackTab => back_tab,
        }
    }
}

#[derive(Clone, Copy)]
enum Press {
    /// Every press event, key-repeat included.
    Repeat,
    /// First presses only — required for anything that spawns or opens.
    Fresh,
}

enum Guard {
    /// Only when no egui widget holds keyboard focus (the default).
    WidgetFree,
    /// Even while a widget is focused — Esc rows only (see module docs).
    Always,
}

struct Binding {
    trigger: Trigger,
    press: Press,
    guard: Guard,
    /// The ⚙ key-list row (settings.rs `KEYS`) documenting this binding —
    /// tested to exist, so a new row here forces a row there.
    #[cfg_attr(not(test), allow(dead_code))] // read by the doc-consistency test
    doc: &'static str,
    /// State this binding needs. False lets a later row with the same
    /// chord fire instead — that is the whole Esc/Enter chain.
    when: fn(&Viewer) -> bool,
    act: fn(&mut Viewer, &egui::Ui),
}

fn always(_: &Viewer) -> bool {
    true
}

const BINDINGS: &[Binding] = &[
    Binding {
        trigger: Trigger::Chords(&[bare(Key::F), bare(Key::Slash), command(Key::F)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "f  /  Ctrl+F",
        when: always,
        act: |v, _| v.picker.open(),
    },
    // Shift+Tab before Tab: when both arrive in one frame, stepping BACK
    // wins, as it did in the old chain.
    Binding {
        trigger: Trigger::BackTab,
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "Tab / Shift+Tab",
        when: always,
        act: |v, _| {
            v.tab_taken = true;
            v.step_card_cursor(-1);
        },
    },
    Binding {
        trigger: Trigger::Tab,
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "Tab / Shift+Tab",
        when: always,
        act: |v, _| {
            v.tab_taken = true;
            v.step_card_cursor(1);
        },
    },
    // b = the same overlay, listing a folder instead of searching
    // everything. Finding is the default way around the vault; browsing
    // is the deliberate one.
    Binding {
        trigger: Trigger::Chords(&[bare(Key::B)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "b",
        when: always,
        act: |v, _| {
            let dir = v.browse_start();
            v.picker.browse(dir);
        },
    },
    Binding {
        trigger: Trigger::Chords(&[bare(Key::Comma)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: ",",
        when: always,
        act: |v, _| v.toggle_settings(),
    },
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Questionmark)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "?",
        when: always,
        act: |v, _| v.open_key_help(),
    },
    // r = read this as source, or back to rendered markdown. One
    // previewer, two ways of reading — not two previewers. Session
    // state, not a setting: persisted, one press pinned every note
    // preview to source across restarts.
    Binding {
        trigger: Trigger::Chords(&[bare(Key::R)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "r",
        when: always,
        act: |v, _| v.toggle_pane_raw(),
    },
    // The ⚙ window dismisses before the graph does: a pending text edit
    // first, then the window — settings_escape does both stages. The
    // `when` mirrors the old side-effecting call exactly: with the window
    // open it always acts, with it closed it always declined.
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Escape)]),
        press: Press::Repeat,
        guard: Guard::Always,
        doc: "Esc",
        when: |v| v.settings.open,
        act: |v, _| {
            v.settings_escape();
        },
    },
    // Dismiss order: link cursor, terminal cursor, then selection. (The
    // search prompt is handled before the table, while it still has
    // focus — see the picker branch in `handle_keys`.)
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Escape)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "Esc",
        when: always,
        act: |v, _| {
            if v.conn_cursor.take().is_none() && v.terms.cursor.take().is_none() {
                v.selected = None;
            }
        },
    },
    // Enter on a highlighted connection = follow it
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Enter)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "Enter",
        when: |v| v.selected.is_some() && v.conn_cursor.is_some(),
        act: |v, _| {
            let (Some(sel), Some(ci)) = (v.selected, v.conn_cursor) else {
                return;
            };
            if let Some(t) = v.connections(sel).get(ci).copied() {
                v.selected = Some(t);
                v.frame_node(t);
                v.nav_scroll = true;
            }
            v.conn_cursor = None;
        },
    },
    // Enter on the terminal cursor = start typing into it
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Enter)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "Enter",
        when: |v| v.terms.cursor.is_some(),
        act: |v, _| v.terms.focused = v.terms.cursor.clone(),
    },
    // If an egui widget (e.g. the detail pane's button, tab-focused) has
    // focus, Enter already activates it — the guard keeps this row from
    // also firing, or the editor would open twice.
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Enter)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "Enter",
        when: |v| v.selected.is_some(),
        act: |v, _| {
            let Some(sel) = v.selected else { return };
            v.open_in_editor(sel);
        },
    },
    Binding {
        trigger: Trigger::Chords(&[bare(Key::Z)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "z",
        when: |v| v.selected.is_some(),
        act: |v, _| {
            let Some(sel) = v.selected else { return };
            v.frame_node(sel);
        },
    },
    // 0 resets the ZOOM and leaves you where you are; gg resets the whole
    // camera. Splitting them is the difference between "let me see this
    // properly" and "take me back out".
    Binding {
        trigger: Trigger::Chords(&[any_mods(Key::Num0), any_mods(Key::Home)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "0  Home",
        when: always,
        act: |v, ui| {
            let rect = v.cam.last_rect.unwrap_or_else(|| ui.ctx().content_rect());
            if let Some((_, zoom)) = v.whole_graph_view(rect) {
                v.cam.cancel_glide();
                v.cam.zoom = zoom;
            }
        },
    },
    // vim gg: two bare g presses in quick succession refit the camera on
    // the whole graph
    Binding {
        trigger: Trigger::Chords(&[bare(Key::G)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "gg",
        when: always,
        act: |v, _| {
            if v.pending_g
                .is_some_and(|t| t.elapsed() < Duration::from_millis(600))
            {
                v.pending_g = None;
                v.cam.cancel_glide();
                v.cam.fitted = false; // canvas re-fits on the next frame
            } else {
                v.pending_g = Some(Instant::now());
            }
        },
    },
    // G = show me around this node: centered, at a zoom that fits what it
    // is connected to
    Binding {
        trigger: Trigger::Chords(&[shift(Key::G)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "G",
        when: |v| v.selected.is_some(),
        act: |v, ui| {
            let Some(sel) = v.selected else { return };
            let rect = v.cam.last_rect.unwrap_or_else(|| ui.ctx().content_rect());
            v.cam.zoom = v.neighborhood_zoom(sel, rect);
            v.frame_node(sel);
        },
    },
    // p = up to the parent folder — the one tree move worth a key of its
    // own now that browsing lives in the finder. Every parent is a
    // folder, so with `F` hiding those there is nowhere to go: the row
    // goes quiet rather than selecting something off the canvas.
    Binding {
        trigger: Trigger::Chords(&[bare(Key::P)]),
        press: Press::Repeat,
        guard: Guard::WidgetFree,
        doc: "p",
        when: |v| v.show_dirs && v.selected.is_some_and(|sel| v.g.node(sel).parent.is_some()),
        act: |v, _| {
            let Some(parent) = v.selected.and_then(|sel| v.g.node(sel).parent) else {
                return;
            };
            v.selected = Some(parent);
            v.frame_node(parent);
            v.nav_scroll = true;
            v.conn_cursor = None;
        },
    },
    // e = edit the selected text file in a terminal card, in the graph
    Binding {
        trigger: Trigger::Chords(&[bare(Key::E)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "e",
        when: |v| v.terms.tmux_ok && v.selected.is_some_and(|sel| v.editable(sel)),
        act: |v, ui| {
            let Some(sel) = v.selected else { return };
            let ctx = ui.ctx().clone();
            v.edit_in_graph_terminal(&ctx, sel);
        },
    },
    // Toggle folder nodes. Unlike `w`, hiding these takes them OUT of
    // the physics (sim.rs): the Contains spine goes with them and the
    // graph re-settles on wikilinks and gravity alone, so the press is a
    // reflow, not just a repaint. A folder that was selected goes too —
    // a selection nobody can see still owns z/G/e/t/a.
    Binding {
        trigger: Trigger::Chords(&[shift(Key::F)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "F",
        when: always,
        act: |v, _| {
            v.show_dirs = !v.show_dirs;
            if !v.show_dirs
                && v.selected
                    .is_some_and(|sel| v.g.node(sel).kind == NodeKind::Dir)
            {
                v.selected = None;
                v.conn_cursor = None;
            }
            v.set_flash(
                if v.show_dirs {
                    "folders shown — F takes them back out of the graph"
                } else {
                    "folders hidden, and out of the layout — F brings them back"
                }
                .into(),
            );
        },
    },
    // Toggle web (cited-URL) nodes — the sim keeps simulating them, so
    // this never reflows the layout
    Binding {
        trigger: Trigger::Chords(&[bare(Key::W)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "w",
        when: always,
        act: |v, _| {
            v.show_web = !v.show_web;
            v.set_flash(
                if v.show_web {
                    "web links shown — w hides them"
                } else {
                    "web links hidden — w brings them back"
                }
                .into(),
            );
        },
    },
    // t = new terminal at the selected node's folder
    Binding {
        trigger: Trigger::Chords(&[bare(Key::T)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "t",
        when: |v| v.terms.tmux_ok && v.selected.is_some(),
        act: |v, ui| {
            let Some(sel) = v.selected else { return };
            let dir = v.node_dir(sel);
            let ctx = ui.ctx().clone();
            v.new_terminal(&ctx, &dir);
        },
    },
    // a = the DEFAULT agent (⚙ settings) at the selected node's folder
    Binding {
        trigger: Trigger::Chords(&[bare(Key::A)]),
        press: Press::Fresh,
        guard: Guard::WidgetFree,
        doc: "a",
        when: |v| v.terms.tmux_ok && v.selected.is_some(),
        act: |v, ui| {
            let Some(sel) = v.selected else { return };
            let dir = v.node_dir(sel);
            let ctx = ui.ctx().clone();
            let agent = v.cfg.agent();
            v.launch_agent(&ctx, &dir, &agent);
        },
    },
];

/// First presses only — a key REPEAT event never matches. The table's
/// [`Press::Fresh`] rows go through this.
fn pressed_fresh(ui: &egui::Ui, key: Key) -> bool {
    ui.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key {
                    key: k,
                    pressed: true,
                    repeat: false,
                    ..
                } if *k == key
            )
        })
    })
}

impl Viewer {
    pub(super) fn handle_keys(&mut self, ui: &egui::Ui) {
        if self.menu.dialog.is_some() {
            return; // the create dialog owns the keyboard
        }
        // Tab is consumed, not just read: egui's own Tab focus navigation
        // would otherwise move focus into the side pane and swallow the
        // next one. Only while the overlay is CLOSED — Tab is its source
        // swap, and consuming it here would eat that. SHIFT first: a
        // bare-modifier match is not exact, so asking for NONE would
        // swallow Shift+Tab and step forward.
        let (back_tab, tab) = if self.picker.open {
            (false, false)
        } else {
            ui.input_mut(|i| {
                let back = i.consume_key(egui::Modifiers::SHIFT, Key::Tab);
                (back, i.consume_key(egui::Modifiers::NONE, Key::Tab))
            })
        };
        let widget_free = ui.memory(|m| m.focused().is_none());
        if self.picker.open {
            // The picker owns the keyboard while open. Deliberately
            // unguarded: its Enter/Esc/arrows act WHILE its own text
            // field is focused.
            self.picker_keys(ui);
        } else {
            self.dispatch(ui, widget_free, tab, back_tab);
        }
        self.bracket_keys(ui, widget_free);
        self.camera_keys(ui, widget_free);
    }

    /// Walk [`BINDINGS`] top to bottom and fire the first row whose
    /// trigger, guard and `when` all hold — at most one per frame, like
    /// the if/else chain this replaces.
    fn dispatch(&mut self, ui: &egui::Ui, widget_free: bool, tab: bool, back_tab: bool) {
        for b in BINDINGS {
            if matches!(b.guard, Guard::WidgetFree) && !widget_free {
                continue;
            }
            if !b.trigger.pressed(ui, b.press, tab, back_tab) {
                continue;
            }
            if !(b.when)(self) {
                continue;
            }
            (b.act)(self, ui);
            return;
        }
    }

    /// The connections strip is the one thing a SELECTION still walks
    /// with keys: ] and [ step its highlight (children, then outgoing,
    /// then incoming), Enter follows. Everything else about choosing a
    /// node lives in the finder now — hjkl belong to the camera. Runs
    /// beside the table, not in it: stepping may fire in the same frame
    /// as a row.
    fn bracket_keys(&mut self, ui: &egui::Ui, widget_free: bool) {
        let Some(sel) = self.selected.filter(|_| widget_free) else {
            return;
        };
        let (out_jump, back_jump) = ui.input(|i| {
            let m = i.modifiers.is_none();
            (
                m && i.key_pressed(Key::CloseBracket),
                m && i.key_pressed(Key::OpenBracket),
            )
        });
        if out_jump || back_jump {
            let len = self.connections(sel).len() as isize;
            if len > 0 {
                let cur = self.conn_cursor.map(|i| i as isize);
                let next = if out_jump {
                    cur.map_or(0, |i| (i + 1).min(len - 1))
                } else {
                    cur.map_or(len - 1, |i| (i - 1).max(0))
                };
                self.conn_cursor = Some(next as usize);
                self.nav_scroll = true;
            }
        }
    }

    /// The camera owns hjkl unconditionally — no selection mode to switch
    /// out of — with s/d zooming out/in beside them, so one hand drives
    /// the whole view. Continuous while held (key_down, not press
    /// events). Suppressed under the picker: its prompt can lose focus (a
    /// click into the preview), and a bare 'd' must not zoom the canvas
    /// behind an open finder.
    fn camera_keys(&mut self, ui: &egui::Ui, widget_free: bool) {
        if !widget_free || self.picker.open {
            return;
        }
        let (dt, h, j, k, l, zoom_in, zoom_out) = ui.input(|i| {
            let m = i.modifiers.is_none();
            (
                i.stable_dt.min(0.1),
                m && i.key_down(Key::H),
                m && i.key_down(Key::J),
                m && i.key_down(Key::K),
                m && i.key_down(Key::L),
                m && i.key_down(Key::D),
                m && i.key_down(Key::S),
            )
        });
        if h || j || k || l || zoom_in || zoom_out {
            self.cam.cancel_glide(); // manual camera input wins
            let axis = |pos: bool, neg: bool| (pos as i8 - neg as i8) as f32;
            let pan = 2200.0 * dt / self.cam.zoom; // constant screen-space speed
            self.cam.center.x += pan * axis(l, h);
            self.cam.center.y += pan * axis(j, k);
            let zf = 6.0f32.powf(dt * axis(zoom_in, zoom_out));
            self.cam.zoom = (self.cam.zoom * zf).clamp(0.02, 50.0);
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ⚙ key list claims to be the one place every binding appears.
    /// Hold the dispatch table to it: a new row here without a
    /// documenting row there fails, by name.
    #[test]
    fn every_binding_cites_its_key_list_row() {
        for b in BINDINGS {
            assert!(
                crate::app::settings::key_list_has(b.doc),
                "binding documented as {:?} has no row in the ⚙ key list \
                 (settings.rs KEYS) — add one, it is the one list",
                b.doc
            );
        }
    }
}
