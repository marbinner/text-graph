//! The ⚙ settings window: theme (dark/light) and the default agent for
//! the one-click "Launch <agent>" menu button. Persisted per vault in
//! `.text-graph/view` through the normal debounced save.

use eframe::egui::{self, Align2, RichText, Vec2};
use text_graph::agents;

use super::{Theme, Viewer};

impl Viewer {
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
                    let mut light = self.theme.light;
                    ui.radio_value(&mut light, false, "dark");
                    ui.radio_value(&mut light, true, "light");
                    if light != self.theme.light {
                        self.theme = Theme::get(light);
                        self.apply_visuals = true;
                    }
                });
                ui.separator();
                ui.label(RichText::new("default agent").strong());
                ui.label(
                    RichText::new("what one click on \"Launch …\" starts")
                        .small()
                        .weak(),
                );
                egui::ComboBox::from_id_salt("default-agent")
                    .selected_text(self.default_agent.clone())
                    .show_ui(ui, |ui| {
                        for agent in agents::default_allowlist() {
                            ui.selectable_value(&mut self.default_agent, agent.clone(), agent);
                        }
                    });
            });
    }
}
