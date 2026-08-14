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

/// How long a previously-matching pane stays "active" after its foreground
/// command stops matching (tool-call flicker).
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
/// inside `vault`; allowlist and grace filtering happen in [`Tracker`].
/// tmux absent or no server running → empty.
pub fn scan(vault: &Path) -> Vec<PaneInfo> {
    let out = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_path}\t#{pane_current_command}",
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
            let mut f = l.split('\t');
            let (Some(session), Some(pane), Some(pid), Some(cwd), Some(command)) =
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
            }
            if let Some((t, agent)) = self.last_ok.get(&key)
                && now.duration_since(*t) <= GRACE
            {
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
        // forget panes that no longer exist and are past grace
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
        let text = "work\t%1\t100\t/v/notes\tclaude\n\
                    other\t%2\t200\t/elsewhere\tclaude\n\
                    tg_pi_1\t%3\t300\t/v/notes/topics\tpi\n\
                    bad-line\n";
        let panes = parse_scan(text, Path::new("/v/notes"));
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].session, "work");
        assert_eq!(panes[1].pane, "%3");
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
    fn tracker_grace_keeps_agent_name_through_tool_calls() {
        let allow = vec!["claude".to_string()];
        let mut tr = Tracker::new();
        let t0 = Instant::now();

        let active = tr.update(&[pane("work", "/usr/bin/claude")], &allow, t0);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "claude");

        // foreground command flips to bash during a tool call — still active,
        // still labeled claude
        let t1 = t0 + Duration::from_secs(5);
        let active = tr.update(&[pane("work", "bash")], &allow, t1);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "claude");

        // past the grace period it drops
        let t2 = t1 + GRACE + Duration::from_secs(1);
        let active = tr.update(&[pane("work", "bash")], &allow, t2);
        assert!(active.is_empty());
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
