//! Vault-health diagnostics: a corner badge that expands into a list of
//! everything currently wrong — parse warnings, unreadable files, ambiguous
//! links, watcher/reload/mirror failures. The graph already collects these;
//! this makes them visible instead of silently shaping the picture.
//!
//! Also home to the frame statistics (⚙ *frame statistics*): every canvas
//! stage is timed every frame — the cost is a handful of `Instant::now`
//! calls — and a report of averages/maxima over the last ~second overlays
//! the canvas corner when the setting is on. Numbers FREEZE when repaints
//! stop: egui paints on demand, so a stale report literally means "nothing
//! is being drawn", which is itself the answer to "why is my fan on".

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, RichText, Vec2};

use super::Viewer;

const BAD: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);

/// One timed span of the frame. `Shell` is everything outside `canvas()`
/// — the side pane, the finder overlay, settings, dialogs.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    Camera,
    Sim,
    Workers,
    Input,
    Cull,
    Hover,
    Scene,
    Edges,
    Nodes,
    Labels,
    Cards,
    Popups,
    Status,
    Shell,
}

impl Stage {
    pub(super) const COUNT: usize = 14;
    const ALL: [Stage; Stage::COUNT] = [
        Stage::Camera,
        Stage::Sim,
        Stage::Workers,
        Stage::Input,
        Stage::Cull,
        Stage::Hover,
        Stage::Scene,
        Stage::Edges,
        Stage::Nodes,
        Stage::Labels,
        Stage::Cards,
        Stage::Popups,
        Stage::Status,
        Stage::Shell,
    ];
    const fn label(self) -> &'static str {
        match self {
            Stage::Camera => "camera",
            Stage::Sim => "sim",
            Stage::Workers => "workers",
            Stage::Input => "input",
            Stage::Cull => "cull",
            Stage::Hover => "hover",
            Stage::Scene => "scene",
            Stage::Edges => "edges",
            Stage::Nodes => "nodes",
            Stage::Labels => "labels",
            Stage::Cards => "cards",
            Stage::Popups => "popups",
            Stage::Status => "status",
            Stage::Shell => "shell",
        }
    }
}

/// Rolling per-stage frame timing. `canvas()` calls [`FrameStats::lap`]
/// between stages, `ui()` closes each frame with [`FrameStats::end_frame`],
/// and roughly once a second the window folds into a [`Report`] snapshot.
pub(super) struct FrameStats {
    acc: [Duration; Stage::COUNT],
    max: [Duration; Stage::COUNT],
    /// Sum of this frame's canvas laps — what `end_frame` subtracts from
    /// the whole-frame time to get `Shell`.
    canvas_this_frame: Duration,
    total_acc: Duration,
    total_max: Duration,
    frames: u32,
    window_start: Instant,
    pub(super) report: Option<Report>,
}

/// Milliseconds, averaged and maxed over the last window.
pub(super) struct Report {
    pub(super) stages: [(f32, f32); Stage::COUNT],
    pub(super) frame_avg: f32,
    pub(super) frame_max: f32,
    pub(super) repaints_per_s: f32,
}

const WINDOW: Duration = Duration::from_secs(1);

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

impl FrameStats {
    pub(super) fn new() -> Self {
        FrameStats {
            acc: [Duration::ZERO; Stage::COUNT],
            max: [Duration::ZERO; Stage::COUNT],
            canvas_this_frame: Duration::ZERO,
            total_acc: Duration::ZERO,
            total_max: Duration::ZERO,
            frames: 0,
            window_start: Instant::now(),
            report: None,
        }
    }

    /// Attribute the time since `since` to `stage`; returns "now" so the
    /// caller chains laps with no gaps between them.
    pub(super) fn lap(&mut self, since: Instant, stage: Stage) -> Instant {
        let now = Instant::now();
        let d = now - since;
        self.acc[stage as usize] += d;
        if d > self.max[stage as usize] {
            self.max[stage as usize] = d;
        }
        self.canvas_this_frame += d;
        now
    }

    /// Close a frame: `total` is the whole `ui()` pass; whatever the
    /// canvas laps didn't cover is the shell (panels, overlays, windows).
    pub(super) fn end_frame(&mut self, total: Duration) {
        let shell = total.saturating_sub(self.canvas_this_frame);
        self.acc[Stage::Shell as usize] += shell;
        if shell > self.max[Stage::Shell as usize] {
            self.max[Stage::Shell as usize] = shell;
        }
        self.canvas_this_frame = Duration::ZERO;
        self.total_acc += total;
        if total > self.total_max {
            self.total_max = total;
        }
        self.frames += 1;
        let elapsed = self.window_start.elapsed();
        if elapsed >= WINDOW {
            let n = self.frames as f32;
            let mut stages = [(0.0, 0.0); Stage::COUNT];
            for (slot, (acc, max)) in stages.iter_mut().zip(self.acc.iter().zip(&self.max)) {
                *slot = (ms(*acc) / n, ms(*max));
            }
            self.report = Some(Report {
                stages,
                frame_avg: ms(self.total_acc) / n,
                frame_max: ms(self.total_max),
                repaints_per_s: n / elapsed.as_secs_f32(),
            });
            self.acc = [Duration::ZERO; Stage::COUNT];
            self.max = [Duration::ZERO; Stage::COUNT];
            self.total_acc = Duration::ZERO;
            self.total_max = Duration::ZERO;
            self.frames = 0;
            self.window_start = Instant::now();
        }
    }
}

impl Viewer {
    fn diag_count(&self) -> usize {
        self.g.errors.len()
            + self.g.warnings.len()
            + self.g.ambiguities.len()
            + usize::from(
                self.reload._watcher.is_none() || self.reload.watch_error.lock().unwrap().is_some(),
            )
            + usize::from(self.reload.error.is_some())
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
        let watch_error = self.reload.watch_error.lock().unwrap().clone();
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
                        if self.reload._watcher.is_none() {
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
                        if let Some(e) = &self.reload.error {
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
                            for (path, key, message) in &self.g.warnings {
                                if ui
                                    .link(RichText::new(path).color(self.theme.select))
                                    .clicked()
                                {
                                    jump = self.g.by_path(key);
                                }
                                ui.label(RichText::new(message).small().color(self.theme.text));
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
                        if let Some(t) = self.reload.last_done {
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

    /// The frame-statistics overlay (⚙ *frame statistics*): per-stage
    /// avg/max over the last ~second plus the repaint rate, under the
    /// status line. Collection always runs; this only draws.
    pub(super) fn frame_stats_ui(&mut self, ctx: &egui::Context) {
        if !self.cfg.frame_stats {
            return;
        }
        let Some(r) = &self.frames.report else { return };
        egui::Area::new(egui::Id::new("frame-stats"))
            .anchor(Align2::LEFT_TOP, Vec2::new(10.0, 26.0))
            .interactable(false)
            .show(ctx, |ui| {
                let font = FontId::monospace(10.5);
                let head = format!(
                    "frame {:>5.2} ms avg · {:>5.2} max · {:>3.0} repaints/s",
                    r.frame_avg, r.frame_max, r.repaints_per_s
                );
                let mut body = String::new();
                for s in Stage::ALL {
                    let (avg, max) = r.stages[s as usize];
                    body.push_str(&format!("{:<8}{avg:>6.2}{max:>7.2}\n", s.label()));
                }
                egui::Frame::new()
                    .fill(self.theme.bg.gamma_multiply(0.85))
                    .corner_radius(4.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(head)
                                .font(font.clone())
                                .color(self.theme.text),
                        );
                        ui.label(
                            RichText::new(format!("{:<8}{:>6}{:>7}", "", "avg", "max"))
                                .font(font.clone())
                                .color(self.theme.text.gamma_multiply(0.7)),
                        );
                        ui.label(
                            RichText::new(body.trim_end())
                                .font(font)
                                .color(self.theme.text),
                        );
                    });
            });
    }
}

#[cfg(test)]
mod frame_stats_tests {
    use super::*;

    #[test]
    fn laps_average_into_a_report_and_the_window_resets() {
        let mut f = FrameStats::new();
        for _ in 0..4 {
            let t = Instant::now();
            // attribute a fixed made-up duration by faking `since`
            f.lap(t - Duration::from_millis(2), Stage::Nodes);
            f.end_frame(Duration::from_millis(5));
        }
        assert!(f.report.is_none(), "the window hasn't elapsed yet");
        f.window_start = Instant::now() - WINDOW;
        let t = Instant::now();
        f.lap(t - Duration::from_millis(2), Stage::Nodes);
        f.end_frame(Duration::from_millis(5));
        let r = f.report.as_ref().expect("window rolled");
        let (nodes_avg, nodes_max) = r.stages[Stage::Nodes as usize];
        assert!((nodes_avg - 2.0).abs() < 0.5, "avg ≈ 2ms, got {nodes_avg}");
        assert!(
            (2.0..3.0).contains(&nodes_max),
            "max ≈ 2ms, got {nodes_max}"
        );
        assert!((r.frame_avg - 5.0).abs() < 0.5, "frame avg ≈ 5ms");
        let (shell_avg, _) = r.stages[Stage::Shell as usize];
        assert!(
            (shell_avg - 3.0).abs() < 0.6,
            "shell = frame − canvas ≈ 3ms, got {shell_avg}"
        );
        assert_eq!(f.frames, 0, "the window reset");
        assert!(f.total_acc.is_zero());
    }
}
