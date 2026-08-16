//! The ⚙ settings window: theme (dark/light) and the default agent for
//! the one-click "Launch <agent>" menu button. Persisted per vault in
//! `.text-graph/view` through the normal debounced save.

use eframe::egui::{self, Align2, RichText, Vec2};
use std::time::Instant;
use text_graph::config;

use super::{Theme, Viewer};

impl Viewer {
    /// Persist preferences immediately — they change one at a time, by
    /// hand, and losing the last one to a crash would be baffling. Errors
    /// flash rather than warn to stderr: this window is where the user is
    /// looking.
    pub(super) fn save_config(&mut self) {
        if let Err(e) = config::save(&self.cfg) {
            self.flash = Some((format!("couldn't save settings: {e}"), Instant::now()));
        }
    }

    /// Gear badge + settings window, drawn over the canvas corner
    /// (bottom-right; the health badge owns bottom-left).
    pub(super) fn settings_ui(&mut self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::new("settings-badge"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-10.0, -10.0))
            .show(ctx, |ui| {
                if ui
                    .button(RichText::new("⚙").strong())
                    .on_hover_text("settings")
                    .clicked()
                {
                    self.settings_open = !self.settings_open;
                }
            });
        if !self.settings_open {
            return;
        }

        egui::Window::new("settings")
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-10.0, -44.0))
            .collapsible(false)
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("theme").strong());
                ui.horizontal(|ui| {
                    let mut light = self.cfg.light;
                    ui.radio_value(&mut light, false, "dark");
                    ui.radio_value(&mut light, true, "light");
                    if light != self.cfg.light {
                        self.cfg.light = light;
                        self.theme = Theme::get(light);
                        self.apply_visuals = true;
                        self.save_config();
                    }
                });
                ui.separator();
                ui.label(RichText::new("default agent").strong());
                ui.label(
                    RichText::new("what one click on \"Launch …\" starts")
                        .small()
                        .weak(),
                );
                let mut agent = self.cfg.agent();
                let before = agent.clone();
                egui::ComboBox::from_id_salt("default-agent")
                    .selected_text(agent.clone())
                    .show_ui(ui, |ui| {
                        for a in self.cfg.agent_choices() {
                            ui.selectable_value(&mut agent, a.clone(), a);
                        }
                    });
                if agent != before {
                    self.cfg.default_agent = agent;
                    self.save_config();
                }
            });
    }
}
