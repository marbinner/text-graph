//! Discovery of agent terminals: which tmux panes should be mirrored.
//!
//! `tg_*` sessions are ours (launched from the graph, milestone E4) and
//! always count. Foreign panes count while their cwd is inside the vault and
//! their foreground command matches the agent allowlist — with a grace
//! period, because `pane_current_command` flips to bash/python while an
//! agent runs a tool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How long a vanished pane's agent identity is remembered (covers scan
/// flicker). While a pane still EXISTS, identity is sticky — see
/// [`Tracker::update`].
pub const GRACE: Duration = Duration::from_secs(10);

/// A raw tmux pane whose cwd is inside the vault (pre-filtering).
#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub session: String,
    pub pane: String, // "%3"
    pub pid: u32,
    pub cwd: PathBuf,
    pub command: String,
}

/// A pane the graph should show as an agent terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentPane {
    pub session: String,
    pub pane: String,
    pub pid: u32,
    pub cwd: PathBuf,
    /// Harness display name: the allowlist token, or the `tg_` session's tag.
    pub agent: String,
    /// True for `tg_*` sessions launched from the graph.
    pub ours: bool,
}

pub fn default_allowlist() -> Vec<String> {
    let mut v: Vec<String> = ["claude", "codex", "pi", "aider", "goose", "opencode", "gemini"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Ok(extra) = std::env::var("TEXT_GRAPH_AGENTS") {
        v.extend(
            extra
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    v
}

/// One-shot scan of the default tmux server. Returns every pane whose cwd is
/// inside `vault`; allowlist and stickiness filtering happen in [`Tracker`].
/// tmux absent or no server running → empty. The path field comes LAST in
/// the format so a tab inside a path (legal in POSIX) can't shear the record.
pub fn scan(vault: &Path) -> Vec<PaneInfo> {
    let out = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_current_path}",
        ])
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    parse_scan(&String::from_utf8_lossy(&out.stdout), vault)
}

pub fn parse_scan(text: &str, vault: &Path) -> Vec<PaneInfo> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.splitn(5, '\t');
            let (Some(session), Some(pane), Some(pid), Some(command), Some(cwd)) =
                (f.next(), f.next(), f.next(), f.next(), f.next())
            else {
                return None;
            };
            let cwd = PathBuf::from(cwd);
            if !cwd.starts_with(vault) {
                return None;
            }
            Some(PaneInfo {
                session: session.to_string(),
                pane: pane.to_string(),
                pid: pid.parse().ok()?,
                cwd,
                command: command.to_string(),
            })
        })
        .collect()
}

/// Stateful filter applying the tg_/allowlist rule with the grace period.
#[derive(Default)]
pub struct Tracker {
    /// (session, pane) → (last time it matched, agent name it matched as)
    last_ok: HashMap<(String, String), (Instant, String)>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(
        &mut self,
        panes: &[PaneInfo],
        allowlist: &[String],
        now: Instant,
    ) -> Vec<AgentPane> {
        let mut out = Vec::new();
        for p in panes {
            let ours = p.session.starts_with("tg_");
            let cmd_base = Path::new(&p.command)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&p.command);
            let matches = ours || allowlist.iter().any(|a| a == cmd_base);
            let key = (p.session.clone(), p.pane.clone());
            if matches {
                let agent = if ours {
                    p.session.split('_').nth(1).unwrap_or("agent").to_string()
                } else {
                    cmd_base.to_string()
                };
                self.last_ok.insert(key.clone(), (now, agent));
            } else if let Some(e) = self.last_ok.get_mut(&key) {
                e.0 = now; // refresh last-seen for sticky entries
            }
            // Sticky: once a pane has been an agent, it stays one while the
            // pane exists — pane_current_command reads bash/python for the
            // whole duration of a tool call (which can far outlast any grace
            // window), and dropping the mirror mid-call would blank the card
            // and steal typing focus at the worst possible moment.
            if let Some((_, agent)) = self.last_ok.get(&key) {
                out.push(AgentPane {
                    session: p.session.clone(),
                    pane: p.pane.clone(),
                    pid: p.pid,
                    cwd: p.cwd.clone(),
                    agent: agent.clone(),
                    ours,
                });
            }
        }
        // forget identities only once the pane itself is gone past the
        // (scan-flicker) grace
        self.last_ok.retain(|k, (t, _)| {
            now.duration_since(*t) <= GRACE
                || panes
                    .iter()
                    .any(|p| p.session == k.0 && p.pane == k.1)
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filters_to_vault() {
        let text = "work\t%1\t100\tclaude\t/v/notes\n\
                    other\t%2\t200\tclaude\t/elsewhere\n\
                    tg_pi_1\t%3\t300\tpi\t/v/notes/topics\n\
                    bad-line\n";
        let panes = parse_scan(text, Path::new("/v/notes"));
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].session, "work");
        assert_eq!(panes[1].pane, "%3");
    }

    #[test]
    fn parse_survives_tabs_in_paths() {
        // path is the last field, so an embedded tab stays part of the path
        let text = "work\t%1\t100\tclaude\t/v/notes/weird\tdir\n";
        let panes = parse_scan(text, Path::new("/v/notes"));
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].cwd, PathBuf::from("/v/notes/weird\tdir"));
        assert_eq!(panes[0].command, "claude");
    }

    fn pane(session: &str, command: &str) -> PaneInfo {
        PaneInfo {
            session: session.into(),
            pane: "%1".into(),
            pid: 42,
            cwd: PathBuf::from("/v"),
            command: command.into(),
        }
    }

    #[test]
    fn identity_is_sticky_while_the_pane_exists() {
        let allow = vec!["claude".to_string()];
        let mut tr = Tracker::new();
        let t0 = Instant::now();

        let active = tr.update(&[pane("work", "/usr/bin/claude")], &allow, t0);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "claude");

        // an hour into a tool call the foreground command still reads bash —
        // the card must not vanish nor lose its label
        let t1 = t0 + Duration::from_secs(3600);
        let active = tr.update(&[pane("work", "bash")], &allow, t1);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "claude");

        // pane gone → identity forgotten once past the scan-flicker grace
        let t2 = t1 + GRACE + Duration::from_secs(1);
        assert!(tr.update(&[], &allow, t2).is_empty());
        let t3 = t2 + Duration::from_secs(1);
        let active = tr.update(&[pane("work", "bash")], &allow, t3);
        assert!(active.is_empty(), "closed pane's identity must not revive");
    }

    #[test]
    fn tg_sessions_always_count_and_carry_their_tag() {
        let mut tr = Tracker::new();
        let active = tr.update(&[pane("tg_codex_2", "bash")], &[], Instant::now());
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "codex");
        assert!(active[0].ours);
    }
}
