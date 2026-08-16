//! The ⚙ settings window: every preference in `config.rs`, grouped by
//! section, applied live and saved as it changes.
//!
//! Nothing here knows what any individual setting MEANS — the window walks
//! `config::specs()` and renders each row from its declared kind, so a new
//! setting appears (with its help text, its range, and its reset button)
//! without this file changing. `after_change` is the one exception: the
//! short list of settings that need something recomputed the moment they
//! move.
//!
//! Deliberately not modal: values apply on the spot, and the canvas stays
//! live behind the window so a slider can be judged against the graph it
//! is changing.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, RichText, Vec2};
use text_graph::config::{self, Kind, Section, Spec, Value};

use super::{Theme, Viewer};

const WIN_W: f32 = 660.0;
const WIN_H: f32 = 440.0;
/// Section list on the left.
const SIDE_W: f32 = 118.0;
/// A second click within this window confirms "restore defaults".
const CONFIRM: Duration = Duration::from_secs(3);

/// What the right-hand pane is showing. Keys aren't settings, but they
/// belong in the same window: it is where you look when you want to know
/// what the app can do.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Set(Section),
    Keys,
}

pub(super) struct SettingsUi {
    pub(super) open: bool,
    pub(super) tab: Tab,
    pub(super) filter: String,
    /// The text field being typed into and its buffer. Free-text values are
    /// committed on focus loss, never per keystroke: `Spec::apply` trims,
    /// and trimming mid-word makes it impossible to type "code --wait".
    pub(super) editing: Option<(&'static str, String)>,
    /// "restore defaults" was clicked once — a second click confirms.
    armed: Option<Instant>,
}

impl Default for SettingsUi {
    fn default() -> Self {
        SettingsUi {
            open: false,
            tab: Tab::Set(Section::Appearance),
            filter: String::new(),
            editing: None,
            armed: None,
        }
    }
}

impl Viewer {
    /// Persist preferences immediately — they change one at a time, by
    /// hand, and losing the last one to a crash would be baffling. Errors
    /// flash rather than warn to stderr: this window is where the user is
    /// looking.
    pub(super) fn save_config(&mut self) {
        if cfg!(test) {
            return; // headless tests must never write the real user config
        }
        if let Err(e) = config::save(&self.cfg) {
            self.flash = Some((format!("couldn't save settings: {e}"), Instant::now()));
        }
    }

    /// Flip a boolean setting from a keybind, saving it like the window
    /// would — a key and a checkbox must be the same act.
    pub(super) fn toggle_setting(&mut self, key: &str) {
        if let Some(s) = config::spec(key) {
            let on = (s.get)(&self.cfg).as_flag();
            self.set_setting(s, Value::Flag(!on));
            let state = if on { "off" } else { "on" };
            self.flash = Some((format!("{} {state}", s.label), Instant::now()));
        }
    }

    /// Write one setting, do whatever it invalidates, and save.
    fn set_setting(&mut self, s: &'static Spec, v: Value) {
        s.apply(&mut self.cfg, v);
        self.after_change(s.key);
        self.save_config();
    }

    /// The settings that aren't read straight off `self.cfg` each frame.
    /// Everything else is live by construction — the canvas asks the config
    /// while it paints.
    fn after_change(&mut self, key: &str) {
        match key {
            // the pane caches its preview by subject, and the subject
            // hasn't changed — only the way it should be read
            "preview_raw" => self.pane_preview = None,
            "theme_light" => {
                self.theme = Theme::get(self.cfg.light);
                self.apply_visuals = true;
            }
            // radii are derived once per graph, not per frame (they feed
            // hit-testing and the LOD ramps as well as the paint)
            "node_scale" => {
                let radius = Viewer::derived(&self.g, self.cfg.node_scale).radius;
                self.radius = radius;
            }
            _ => {}
        }
    }

    /// `?` — straight to the key list, which is the one thing in here
    /// people look for by name.
    pub(super) fn open_key_help(&mut self) {
        self.settings.open = true;
        self.settings.filter.clear();
        self.settings.tab = Tab::Keys;
    }

    pub(super) fn toggle_settings(&mut self) {
        if self.settings.open {
            self.close_settings();
        } else {
            self.settings.open = true;
        }
    }

    /// Closing commits whatever was half-typed — the window can go away on
    /// Esc or a click on the gear while a text field still holds focus, and
    /// silently dropping the edit would read as "it didn't save".
    pub(super) fn close_settings(&mut self) {
        if let Some((key, buf)) = self.settings.editing.take()
            && let Some(s) = config::spec(key)
        {
            self.set_setting(s, Value::Text(buf));
        }
        self.settings.open = false;
        self.settings.armed = None;
    }

    /// Esc while the window is open: abandon a pending text edit first,
    /// then close. Returns false when there was nothing to dismiss.
    pub(super) fn settings_escape(&mut self) -> bool {
        if !self.settings.open {
            return false;
        }
        if self.settings.editing.take().is_none() {
            self.settings.open = false;
            self.settings.armed = None;
        }
        true
    }

    /// Gear badge + settings window, drawn over the canvas corner
    /// (bottom-right; the health badge owns bottom-left).
    pub(super) fn settings_ui(&mut self, ctx: &egui::Context) {
        let mut toggle = false;
        egui::Area::new(egui::Id::new("settings-badge"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-10.0, -10.0))
            .show(ctx, |ui| {
                toggle = ui
                    .button(RichText::new("⚙").strong())
                    .on_hover_text("settings (,)")
                    .clicked();
            });
        if toggle {
            self.toggle_settings();
        }
        if !self.settings.open {
            return;
        }
        egui::Window::new("settings")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(WIN_W, WIN_H))
            .show(ctx, |ui| self.settings_body(ui));
    }

    fn settings_body(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.filter)
                    .desired_width(220.0)
                    .hint_text("filter settings"),
            );
            if !self.settings.filter.is_empty() && ui.small_button("✖").clicked() {
                self.settings.filter.clear();
            }
        });
        ui.separator();

        let filtering = !self.settings.filter.trim().is_empty();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(SIDE_W);
                for sec in Section::ALL {
                    let on = !filtering && self.settings.tab == Tab::Set(sec);
                    if ui
                        .selectable_label(on, sec.title())
                        .on_hover_text(sec.blurb())
                        .clicked()
                    {
                        self.settings.filter.clear();
                        self.settings.tab = Tab::Set(sec);
                    }
                }
                ui.add_space(8.0);
                let on = !filtering && self.settings.tab == Tab::Keys;
                if ui
                    .selectable_label(on, "keys")
                    .on_hover_text("every keybinding (?)")
                    .clicked()
                {
                    self.settings.filter.clear();
                    self.settings.tab = Tab::Keys;
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(WIN_H - 74.0)
                    .show(ui, |ui| match self.settings.tab {
                        Tab::Keys if !filtering => keys_pane(ui),
                        _ => self.settings_rows(ui, filtering),
                    });
            });
        });

        ui.separator();
        self.settings_footer(ui);
    }

    fn settings_rows(&mut self, ui: &mut egui::Ui, filtering: bool) {
        let filter = self.settings.filter.clone();
        // a filter typed while the keys tab is open searches the settings;
        // the section it falls back to is the first one
        let section = match self.settings.tab {
            Tab::Set(s) => s,
            Tab::Keys => Section::Appearance,
        };
        let rows: Vec<&'static Spec> = config::specs()
            .iter()
            .filter(|s| {
                if filtering {
                    s.matches(&filter)
                } else {
                    s.section == section
                }
            })
            .collect();
        if rows.is_empty() {
            ui.label(RichText::new("no setting matches that").weak());
            return;
        }
        if !filtering {
            ui.label(RichText::new(section.blurb()).small().weak());
            ui.add_space(4.0);
        }
        let mut last: Option<Section> = None;
        for s in rows {
            // while filtering, results span sections — say which is which
            if filtering && last != Some(s.section) {
                if last.is_some() {
                    ui.add_space(6.0);
                }
                ui.label(RichText::new(s.section.title()).small().weak());
                last = Some(s.section);
            }
            self.setting_row(ui, s);
        }
    }

    fn setting_row(&mut self, ui: &mut egui::Ui, s: &'static Spec) {
        let label_w = (ui.available_width() * 0.46).max(150.0);
        let mut change = None;
        let mut reset = false;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(label_w);
                ui.label(RichText::new(s.label).strong());
                ui.label(RichText::new(s.help).small().weak());
            });
            ui.vertical(|ui| {
                change = self.setting_widget(ui, s);
                if !s.is_default(&self.cfg) {
                    ui.horizontal(|ui| {
                        reset = ui
                            .small_button("↺")
                            .on_hover_text("back to the default")
                            .clicked();
                    });
                }
            });
        });
        if let Some(v) = change {
            self.set_setting(s, v);
        }
        if reset {
            s.reset(&mut self.cfg);
            self.after_change(s.key);
            self.save_config();
        }
        ui.add_space(8.0);
    }

    /// The widget for one setting, from its declared kind. `None` unless the
    /// user just changed it.
    fn setting_widget(&mut self, ui: &mut egui::Ui, s: &'static Spec) -> Option<Value> {
        match &s.kind {
            Kind::Flag => {
                let mut on = (s.get)(&self.cfg).as_flag();
                let text = if on { "on" } else { "off" };
                ui.checkbox(&mut on, text)
                    .changed()
                    .then_some(Value::Flag(on))
            }
            Kind::Num {
                min,
                max,
                step,
                suffix,
                decimals,
            } => {
                let mut n = (s.get)(&self.cfg).as_num();
                let slider = egui::Slider::new(&mut n, *min..=*max)
                    .step_by(*step as f64)
                    .fixed_decimals(*decimals)
                    .suffix(*suffix);
                ui.add(slider).changed().then_some(Value::Num(n))
            }
            Kind::Choice { options } => {
                let cur = (s.get)(&self.cfg).as_text().to_string();
                let mut sel = cur.clone();
                egui::ComboBox::from_id_salt(s.key)
                    .selected_text(cur.clone())
                    .show_ui(ui, |ui| {
                        for o in options(&self.cfg) {
                            ui.selectable_value(&mut sel, o.clone(), o);
                        }
                    });
                (sel != cur).then_some(Value::Text(sel))
            }
            Kind::Text { hint } => {
                let stored = (s.get)(&self.cfg).as_text().to_string();
                let mut buf = match &self.settings.editing {
                    Some((k, b)) if *k == s.key => b.clone(),
                    _ => stored.clone(),
                };
                let r = ui.add(
                    egui::TextEdit::singleline(&mut buf)
                        .hint_text(*hint)
                        .desired_width(ui.available_width().min(230.0)),
                );
                if r.changed() {
                    self.settings.editing = Some((s.key, buf.clone()));
                }
                let mine = matches!(&self.settings.editing, Some((k, _)) if *k == s.key);
                if r.lost_focus() && mine {
                    self.settings.editing = None;
                    if buf.trim() != stored {
                        return Some(Value::Text(buf));
                    }
                }
                None
            }
        }
    }

    fn settings_footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let armed = self.settings.armed.is_some_and(|t| t.elapsed() < CONFIRM);
            let label = if armed {
                "click again to confirm"
            } else {
                "restore defaults"
            };
            if ui.button(label).clicked() {
                if armed {
                    self.settings.armed = None;
                    let keep = std::mem::take(&mut self.cfg.unknown);
                    self.cfg = config::Config {
                        unknown: keep,
                        ..Default::default()
                    };
                    self.after_change("theme_light");
                    self.save_config();
                } else {
                    self.settings.armed = Some(Instant::now());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let path = config::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "nowhere — set $HOME".into());
                ui.label(RichText::new("stored in your config").small().weak())
                    .on_hover_text(path);
            });
        });
    }
}

/// Every keybinding, grouped the way they are learned. This is the ONE
/// list — the README's table is written from it, and a binding that
/// isn't here is a binding nobody can find.
const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "camera",
        &[
            ("h j k l", "pan"),
            ("s / d", "zoom out / in"),
            ("drag / scroll", "pan / zoom toward the cursor"),
            ("gg", "back out to the whole graph"),
            ("0  Home", "reset the zoom, stay where you are"),
            ("z", "center on the selection"),
        ],
    ),
    (
        "finding",
        &[
            (
                "f  /  Ctrl+F",
                "open the picker: names, aliases, paths, contents",
            ),
            ("↑ ↓  Ctrl+P Ctrl+N", "move through results"),
            ("PageUp/Down  Ctrl+U/D", "half-page jumps"),
            ("Enter", "take the result and keep browsing from it"),
            ("Ctrl+Enter", "open the file at the matched line"),
            ("Esc", "close the picker, keeping your selection"),
        ],
    ),
    (
        "a selected node",
        &[
            ("p", "up to the parent folder"),
            ("] / [", "walk the connections strip"),
            ("r", "read the preview as source, or back to markdown"),
            ("Enter", "open in the editor (folders: the file manager)"),
            ("Esc", "dismiss what's transient, then deselect"),
        ],
    ),
    (
        "doing things",
        &[
            ("e", "edit the file in a terminal card, in the graph"),
            ("t", "new terminal card at this node's folder"),
            ("a", "launch the default agent there"),
            ("right-click", "new note or folder, launches, card actions"),
        ],
    ),
    (
        "terminal cards",
        &[
            ("Tab / Shift+Tab", "step through the cards, expanding each"),
            ("Enter", "go into the card the cursor is on"),
            ("click", "focus it — the keyboard goes to the pane"),
            ("Ctrl+click", "pin it open at any zoom"),
            ("drag", "arrange it around its anchor"),
            ("Ctrl+Q  /  click away", "release focus back to the graph"),
            ("dwell", "peek at a compact card full-screen"),
        ],
    ),
    (
        "view",
        &[
            ("w", "show or hide web (cited-URL) nodes"),
            (",", "these settings"),
            ("?", "this list"),
        ],
    ),
];

fn keys_pane(ui: &mut egui::Ui) {
    for (group, rows) in KEYS {
        ui.label(RichText::new(*group).strong());
        ui.add_space(2.0);
        egui::Grid::new(group)
            .num_columns(2)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                for (keys, what) in *rows {
                    ui.label(RichText::new(*keys).monospace());
                    ui.label(RichText::new(*what).weak());
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
    }
}
