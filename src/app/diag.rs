//! Vault-health diagnostics: a corner badge that expands into a list of
//! everything currently wrong — parse warnings, unreadable files, ambiguous
//! links, watcher/reload/mirror failures. The graph already collects these;
//! this makes them visible instead of silently shaping the picture.

use eframe::egui::{self, Align2, Color32, RichText, Vec2};

use super::Viewer;

const BAD: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);

impl Viewer {
    fn diag_count(&self) -> usize {
        self.g.errors.len()
            + self.g.warnings.len()
            + self.g.ambiguities.len()
            + usize::from(self._watcher.is_none() || self.watch_error.lock().unwrap().is_some())
            + usize::from(self.reload_error.is_some())
            + usize::from(self.config_error.is_some())
            + usize::from(self.terms.discovery_error.lock().unwrap().is_some())
            + self.terms.attach_backoff.len()
    }

    /// Badge + expandable health window, drawn over the canvas corner.
    pub(super) fn diag_ui(&mut self, ctx: &egui::Context) {
        let n = self.diag_count();
        if n == 0 {
            self.diag_open = false;
            return;
        }
        egui::Area::new(egui::Id::new("diag-badge"))
            .anchor(Align2::LEFT_BOTTOM, Vec2::new(10.0, -10.0))
            .show(ctx, |ui| {
                let badge = RichText::new(format!("⚠ {n}")).color(BAD).strong();
                if ui
                    .button(badge)
                    .on_hover_text("vault health — click")
                    .clicked()
                {
                    self.diag_open = !self.diag_open;
                }
            });
        if !self.diag_open {
            return;
        }

        let mut jump = None;
        let watch_error = self.watch_error.lock().unwrap().clone();
        let discovery_error = self.terms.discovery_error.lock().unwrap().clone();
        egui::Window::new("vault health")
            .anchor(Align2::LEFT_BOTTOM, Vec2::new(10.0, -44.0))
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                // scrollable: a vault with hundreds of errors/warnings must
                // not grow the window past the screen and hide its own tail
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        if self._watcher.is_none() {
                            let detail = watch_error
                                .as_deref()
                                .unwrap_or("the file watcher failed to start");
                            ui.colored_label(BAD, format!("live reload OFF — {detail}"));
                        } else if let Some(error) = &watch_error {
                            ui.colored_label(BAD, format!("file watcher reported: {error}"));
                            ui.label(
                                RichText::new(
                                    "a recovery scan was scheduled; restart if live reload stops",
                                )
                                .small()
                                .color(self.theme.text),
                            );
                        }
                        if let Some(e) = &self.reload_error {
                            ui.colored_label(BAD, format!("last reload failed: {e}"));
                            ui.label(
                                RichText::new("showing the previous graph until a save succeeds")
                                    .small()
                                    .color(self.theme.text),
                            );
                        }
                        if let Some(e) = &self.config_error {
                            ui.colored_label(BAD, format!("settings could not be loaded: {e}"));
                            ui.label(
                                RichText::new(
                                    "settings saves are disabled; fix the config and restart",
                                )
                                .small()
                                .color(self.theme.text),
                            );
                        }
                        if let Some(error) = &discovery_error {
                            ui.colored_label(BAD, format!("tmux discovery failed: {error}"));
                            ui.label(
                                RichText::new(
                                    "keeping the last cards briefly; discovery is retrying",
                                )
                                .small()
                                .color(self.theme.text),
                            );
                        }
                        // sorted: HashMap order would shuffle the rows between
                        // frames whenever an insert/remove rehashes — reordering
                        // the list under the user's cursor
                        let mut backoff: Vec<&String> = self.terms.attach_backoff.keys().collect();
                        backoff.sort();
                        for s in backoff {
                            ui.colored_label(
                                BAD,
                                format!("can't mirror tmux session {s} (retrying)"),
                            );
                        }
                        if !self.g.errors.is_empty() {
                            ui.separator();
                            ui.label(RichText::new("unreadable files").strong());
                            for (path, msg) in &self.g.errors {
                                ui.colored_label(BAD, format!("{path}: {msg}"));
                            }
                        }
                        if !self.g.warnings.is_empty() {
                            ui.separator();
                            ui.label(RichText::new("parse warnings").strong());
                            for (path, msg) in &self.g.warnings {
                                if ui
                                    .link(RichText::new(path).color(self.theme.select))
                                    .clicked()
                                {
                                    jump = self.g.by_path(path);
                                }
                                ui.label(RichText::new(msg).small().color(self.theme.text));
                            }
                        }
                        if !self.g.ambiguities.is_empty() {
                            ui.separator();
                            ui.label(RichText::new("ambiguous links").strong());
                            for a in &self.g.ambiguities {
                                let src = self.g.node(a.source).path.clone();
                                if ui
                                    .link(
                                        RichText::new(format!(
                                            "{src}: [[{}]] → {}",
                                            a.target,
                                            self.g.node(a.chosen).path
                                        ))
                                        .color(self.theme.wiki),
                                    )
                                    .clicked()
                                {
                                    jump = Some(a.source);
                                }
                            }
                        }
                        if let Some(t) = self.last_reload {
                            ui.separator();
                            ui.label(
                                RichText::new(format!(
                                    "last reload {}s ago",
                                    t.elapsed().as_secs()
                                ))
                                .small()
                                .color(self.theme.text),
                            );
                        }
                    });
            });
        if let Some(id) = jump {
            self.selected = Some(id);
            self.frame_node(id);
            self.nav_scroll = true;
        }
    }
}
