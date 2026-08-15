//! Things the user does TO the vault and system: right-click menu,
//! note/folder creation dialog, opening editors and terminal windows.

use super::*;

/// State of the "New note / New folder" dialog (opened via right-click).
pub(super) struct CreateDialog {
    folder: bool,
    /// Vault-relative target directory ("" = root) and its display label.
    dir: String,
    label: String,
    buf: String,
    /// Focus the text field on the next frame (open / after an error).
    focus: bool,
    err: Option<String>,
}

/// Editors that run inside a terminal and therefore need one opened for them.
pub(super) const TERMINAL_EDITORS: &[&str] = &[
    "vim", "nvim", "vi", "nano", "micro", "hx", "helix", "kak", "vis", "ne",
];

pub(super) fn spawn_editor(file: &Path) -> std::io::Result<()> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    let Some(editor) = editor else {
        return detached(std::process::Command::new("xdg-open").arg(file));
    };
    // $EDITOR may carry args ("code --wait") — split on whitespace
    let mut parts = editor.split_whitespace();
    let prog = parts.next().unwrap_or("xdg-open").to_string();
    let args: Vec<String> = parts.map(str::to_string).collect();
    let base = Path::new(&prog)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&prog);
    if TERMINAL_EDITORS.contains(&base)
        && let Some(mut term) = new_terminal_window()
    {
        term.arg(&prog).args(&args).arg(file);
        return detached(&mut term);
    }
    detached(std::process::Command::new(&prog).args(&args).arg(file))
}

/// A command that opens a new terminal-emulator window and runs whatever is
/// appended to it. $TERMINAL wins; otherwise the first emulator on PATH.
pub(super) fn new_terminal_window() -> Option<std::process::Command> {
    let mk = |bin: &str, extra: &[&str]| -> std::process::Command {
        let mut c = std::process::Command::new(bin);
        c.args(extra); // user-supplied flags go before the command separator
        let base = Path::new(bin)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(bin);
        match base {
            "gnome-terminal" => {
                c.arg("--");
            }
            "wezterm" => {
                c.args(["start", "--"]);
            }
            "kitty" | "foot" => {} // these take the command directly
            _ => {
                c.arg("-e"); // the de-facto convention
            }
        }
        c
    };
    if let Ok(term) = std::env::var("TERMINAL") {
        // "$TERMINAL" may carry flags ("foot -a floating"), like $EDITOR
        let mut words = term.split_whitespace();
        if let Some(bin) = words.next() {
            return Some(mk(bin, &words.collect::<Vec<_>>()));
        }
    }
    [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "foot",
        "alacritty",
        "kitty",
        "wezterm",
        "xterm",
    ]
    .into_iter()
    .find(|bin| on_path(bin))
    .map(|bin| mk(bin, &[]))
}

pub(super) fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Spawn fully detached from our stdio, so even a mis-detected terminal
/// editor can never take over the terminal the viewer was launched from.
pub(super) fn detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

impl Viewer {
    /// The directory the context menu's actions apply to (vault-relative,
    /// "" = root) and a human label for it.
    pub(super) fn ctx_dir(&self) -> (String, String) {
        let dir = self
            .ctx_node
            .map(|id| {
                let n = self.g.node(id);
                match n.kind {
                    NodeKind::Dir => n.path.clone(),
                    _ => n
                        .parent
                        .map(|p| self.g.node(p).path.clone())
                        .unwrap_or_default(),
                }
            })
            .unwrap_or_default();
        let label = if dir.is_empty() {
            self.root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("vault")
                .to_string()
        } else {
            dir.clone()
        };
        (dir, label)
    }

    /// Right-click menu: card lifecycle first (when a card was clicked),
    /// then creation anchored at the clicked node's directory.
    pub(super) fn context_menu_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(170.0);
        if let Some((s, p)) = self.ctx_card.clone()
            // only while the pane is still alive
            && self.terms.panes.iter().any(|a| a.session == s && a.pane == p)
        {
            if ui.button("Attach in terminal…").clicked() {
                self.attach_external(&s, &p);
            }
            ui.menu_button("Kill terminal", |ui| {
                ui.label(
                    egui::RichText::new("ends whatever is running there")
                        .weak()
                        .small(),
                );
                if ui.button(format!("Kill {s} {p}")).clicked() {
                    self.kill_pane(&s, &p);
                }
            });
            ui.separator();
        }
        // a ghost is a referenced-but-unwritten note: offer to make it real
        if let Some(id) = self.ctx_node
            && self.g.node(id).kind == NodeKind::Ghost
        {
            let target = self.g.node(id).path.clone();
            if ui.button(format!("Write \"{target}\"")).clicked() {
                let res = create::ghost_rel_path(&target)
                    .and_then(|rel| create::write_note(&self.root, &rel).map(|_| rel));
                match res {
                    Ok(rel) => {
                        self.pending_select = Some(rel.clone());
                        self.set_flash(format!("created {rel}"));
                        *self.reload_at.lock().unwrap() = Some(Instant::now());
                    }
                    Err(e) => self.set_flash(format!("can't create: {e}")),
                }
            }
            return;
        }

        let (dir, label) = self.ctx_dir();
        ui.label(egui::RichText::new(format!("in {label}/")).weak().small());
        if ui.button("New note…").clicked() {
            self.open_create(false, dir.clone(), label.clone());
        }
        if ui.button("New folder…").clicked() {
            self.open_create(true, dir.clone(), label.clone());
        }
        if self.terms.tmux_ok {
            ui.separator();
            if ui.button("New terminal").clicked() {
                let path = self.ctx_path(&dir);
                match agents::launch_shell(None, &path) {
                    Ok(name) => self.set_flash(format!("opened terminal — session {name}")),
                    Err(e) => self.set_flash(format!("terminal failed: {e}")),
                }
            }
            ui.menu_button("Launch agent", |ui| {
                for agent in agents::default_allowlist() {
                    if ui.button(&agent).clicked() {
                        let ctx = ui.ctx().clone();
                        self.launch_agent(&ctx, &dir, &agent);
                    }
                }
            });
        }
    }

    /// Absolute path for a vault-relative dir ("" = root).
    pub(super) fn ctx_path(&self, dir: &str) -> PathBuf {
        if dir.is_empty() {
            self.root.clone()
        } else {
            self.root.join(dir)
        }
    }

    pub(super) fn open_create(&mut self, folder: bool, dir: String, label: String) {
        self.terms.focused = None; // the dialog owns the keyboard now
        self.close_search();
        self.create = Some(CreateDialog {
            folder,
            dir,
            label,
            buf: String::new(),
            focus: true,
            err: None,
        });
    }

    /// The centered "New note / New folder" window, while `self.create` is on.
    pub(super) fn create_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.create.take() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(if dlg.folder { "New folder" } else { "New note" })
            .id(egui::Id::new("tg-create"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("in {}/", dlg.label)).weak());
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut dlg.buf)
                        .hint_text(if dlg.folder {
                            "folder or sub/folder"
                        } else {
                            "name or sub/name"
                        })
                        .desired_width(260.0),
                );
                if dlg.focus {
                    resp.request_focus();
                    dlg.focus = false;
                }
                if let Some(e) = &dlg.err {
                    ui.colored_label(SELECT, e);
                }
                submit = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.horizontal(|ui| {
                    submit |= ui.button("Create").clicked();
                    cancel =
                        ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
                });
            });
        if submit {
            let res = if dlg.folder {
                create::folder_rel_path(&dlg.dir, &dlg.buf)
                    .and_then(|rel| create::make_folder(&self.root, &rel).map(|_| rel))
            } else {
                create::note_rel_path(&dlg.dir, &dlg.buf)
                    .and_then(|rel| create::write_note(&self.root, &rel).map(|_| rel))
            };
            match res {
                Ok(rel) if dlg.folder => {
                    // empty dirs are pruned from the graph, deliberately
                    self.set_flash(format!(
                        "created {rel}/ — appears once it holds a note (\"sub/name\" in New note also creates folders)"
                    ));
                }
                Ok(rel) => {
                    self.pending_select = Some(rel.clone());
                    self.set_flash(format!("created {rel}"));
                    *self.reload_at.lock().unwrap() = Some(Instant::now());
                }
                Err(e) => {
                    dlg.err = Some(e.to_string());
                    dlg.focus = true;
                    self.create = Some(dlg); // stay open for a correction
                }
            }
        } else if !cancel {
            self.create = Some(dlg);
        }
    }

    /// Open the selection externally — always in a NEW window. Files go to
    /// the editor: terminal editors ($EDITOR=nvim etc.) are wrapped in a
    /// fresh terminal emulator instead of hijacking whatever terminal
    /// launched the viewer; GUI editors open their own windows anyway. Dirs
    /// open in the file manager, images in the system viewer; ghosts have
    /// nothing to open.
    pub(super) fn open_in_editor(&self, id: NodeId) {
        let node = self.g.node(id);
        let path = self.root.join(&node.path);
        let result = match node.kind {
            NodeKind::File => spawn_editor(&path),
            NodeKind::Dir | NodeKind::Image => {
                detached(std::process::Command::new("xdg-open").arg(&path))
            }
            NodeKind::Ghost => return,
        };
        if let Err(e) = result {
            eprintln!("failed to open {}: {e}", path.display());
        }
    }
}
