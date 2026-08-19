//! Discovery of agent terminals: which tmux panes should be mirrored.
//!
//! Sessions carrying text-graph's explicit tmux owner marker always count.
//! Foreign panes count while their cwd is inside the vault and
//! their foreground command matches the agent allowlist — with a grace
//! period, because `pane_current_command` flips to bash/python while an
//! agent runs a tool.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a vanished pane's agent identity is remembered (covers scan
/// flicker). While a pane still EXISTS, identity is sticky — see
/// [`Tracker::update`].
pub const GRACE: Duration = Duration::from_secs(10);

/// Session-scoped tmux option proving the graph created a session. Names are
/// only human-readable conventions and must never grant resize privileges.
const OWNER_OPTION: &str = "@tg_owner";
const OWNER_VALUE: &str = "text-graph";
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(1);

/// Run one short tmux lifecycle command with a deadline. Tmux output is tiny,
/// so polling before `wait_with_output` is safe; a wedged server is killed
/// rather than occupying a worker and its in-flight UI slot forever.
fn lifecycle_output(cmd: &mut Command) -> std::io::Result<Output> {
    lifecycle_output_with_timeout(cmd, LIFECYCLE_TIMEOUT)
}

fn lifecycle_output_with_timeout(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("tmux command timed out after {}s", timeout.as_secs_f32()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A raw tmux pane whose cwd is inside the vault (pre-filtering).
#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub session: String,
    pub pane: String, // "%3"
    pub pid: u32,
    pub cwd: PathBuf,
    pub command: String,
    /// True only when the session carries our exact ownership marker.
    pub owned: bool,
    /// The session's `@tg_anchor` user option — a vault-relative path the
    /// card tethers to (edit sessions pin to their file). Stored IN tmux,
    /// so it survives viewer restarts and dies with the session.
    pub anchor: Option<String>,
    /// `#{window_activity}` — epoch seconds of the window's last output,
    /// None when tmux reported nothing numeric. Window-scoped: exact for
    /// our single-pane `tg_` sessions, an approximation for foreign
    /// multi-pane windows (where the mirror's own per-pane output
    /// timestamps are the precise signal). Verified against tmux 3.4 that
    /// it advances for DETACHED sessions, so telling busy from idle needs
    /// no client attached.
    pub activity: Option<u64>,
}

/// A pane the graph should show as an agent terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentPane {
    pub session: String,
    pub pane: String,
    pub pid: u32,
    pub cwd: PathBuf,
    /// Harness display name: the allowlist token, or an owned session's tag.
    pub agent: String,
    /// True only for sessions carrying text-graph's ownership marker.
    pub ours: bool,
    /// See [`PaneInfo::anchor`].
    pub anchor: Option<String>,
    // Deliberately NO activity field: the viewer's discovery thread
    // republishes (and repaints) whenever this snapshot compares unequal,
    // and a per-second timestamp would make every scan differ. Callers
    // wanting freshness join [`PaneInfo::activity`] on (session, pane).
}

pub fn default_allowlist() -> Vec<String> {
    let mut v: Vec<String> = [
        "claude", "codex", "pi", "aider", "goose", "opencode", "gemini",
    ]
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

/// Launch `agent` in a new detached `tg_` session cwd'd at `dir`, returning
/// the session name (`tg_<agent>`, `tg_<agent>_2`, … first free). The
/// `-x/-y` set the clientless session's size — our control-mode mirror never
/// sends size hints, so it stays stable until a real terminal attaches. The
/// session dies with the agent process, which removes the card.
/// `socket` is for tests (private `-L` server); the GUI passes `None`.
pub fn launch(socket: Option<&str>, dir: &Path, agent: &str) -> std::io::Result<String> {
    // keep the slug '_'-free: the Tracker reads the tag as split('_').nth(1)
    let slug: String = agent
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    let slug = if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    };
    launch_named(socket, dir, &slug, Some(OsStr::new(agent)), None)
}

/// Launch a plain interactive terminal (tmux's default-shell) in a
/// `tg_term` session at `dir` — a shell card in the graph, no agent.
pub fn launch_shell(socket: Option<&str>, dir: &Path) -> std::io::Result<String> {
    launch_named(socket, dir, "term", None, None)
}

/// Launch an editor on a file in a tg_edit session cwd'd at the supplied
/// directory, with its card pinned to the collision-free anchor key. The
/// session dies with the editor.
pub fn launch_edit(
    socket: Option<&str>,
    dir: &Path,
    editor: &str,
    file: &Path,
    anchor: &str,
) -> std::io::Result<String> {
    let command = edit_command(editor, file);
    launch_named(socket, dir, "edit", Some(&command), Some(anchor))
}

/// The PATH a launched agent should resolve against: the server's global
/// PATH (from whoever started the server — normally the user's real shell)
/// joined with our own. The viewer itself may run with a stripped PATH
/// (IDE/launcher environments), and tmux seeds a new session's environment
/// from its CLIENT — us — so without this an agent binary can be
/// unfindable in the pane while being findable in every terminal the user
/// owns; the pane then dies in milliseconds and the session evaporates
/// before discovery ever sees it. (`new-session -e PATH=…` was tried and
/// does not apply to the initial pane.) With no server running, the
/// well-known user bin dirs below are the rescue — the fresh server would
/// otherwise inherit our stripped env with nothing to borrow.
fn launch_path(socket: Option<&str>) -> Option<String> {
    let server = (|| {
        let mut c = Command::new("tmux");
        if let Some(l) = socket {
            c.args(["-L", l]);
        }
        c.args(["show-environment", "-g", "PATH"]);
        let out = lifecycle_output(&mut c).ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Some(text.strip_prefix("PATH=")?.trim_end().to_string())
    })();
    let client = std::env::var("PATH").ok().filter(|s| !s.is_empty());
    // Well-known user bin dirs (only those that exist) rescue the
    // NO-SERVER case: the server we are about to start inherits OUR env,
    // and there is no healthy global PATH to borrow — without these,
    // "Launch pi" from an IDE-started viewer dies on a fresh server even
    // though pi sits in ~/.npm-global/bin.
    let mut extras: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        for d in [
            ".local/bin",
            ".npm-global/bin",
            ".cargo/bin",
            ".deno/bin",
            ".opencode/bin",
            "bin",
        ] {
            let p = format!("{home}/{d}");
            if std::path::Path::new(&p).is_dir() {
                extras.push(p);
            }
        }
    }
    if std::path::Path::new("/usr/local/bin").is_dir() {
        extras.push("/usr/local/bin".into());
    }
    let merged = merge_paths(
        server
            .iter()
            .chain(client.iter())
            .flat_map(|s| s.split(':'))
            .chain(extras.iter().map(String::as_str)),
    );
    (!merged.is_empty()).then_some(merged)
}

/// Join PATH components in order, dropping empties and duplicates (first
/// occurrence wins) — keeps the wrapped command line short and stable.
fn merge_paths<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for p in parts {
        if !p.is_empty() && seen.insert(p) {
            out.push(p);
        }
    }
    out.join(":")
}

/// The pane command for an agent launch: `PATH='…' exec <cmd>` when a
/// known-good PATH is available (single-quoted, quote-safe), the raw
/// command otherwise. tmux runs a string command via `/bin/sh -c`, so this
/// stays free of shell rc files and interactivity.
fn path_cmd(path: Option<&str>, cmd: &OsStr) -> OsString {
    let mut command = OsString::new();
    if let Some(path) = path {
        command.push(format!("PATH='{}' exec ", path.replace('\'', r"'\''")));
    }
    command.push(cmd);
    command
}

fn edit_command(editor: &str, file: &Path) -> OsString {
    let mut command = OsString::from("env COLORFGBG='15;0' ");
    command.push(editor);
    command.push(" ");

    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
        let mut quoted = Vec::with_capacity(file.as_os_str().as_bytes().len() + 2);
        quoted.push(b'\'');
        for &byte in file.as_os_str().as_bytes() {
            if byte == b'\'' {
                quoted.extend_from_slice(br"'\''");
            } else {
                quoted.push(byte);
            }
        }
        quoted.push(b'\'');
        command.push(OsString::from_vec(quoted));
    }
    #[cfg(not(unix))]
    command.push(format!(
        "'{}'",
        file.to_string_lossy().replace('\'', r"'\''")
    ));

    command
}

fn tmux_start_directory(dir: &Path) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
        let bytes = dir.as_os_str().as_bytes();
        let mut escaped = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            escaped.push(byte);
            if byte == b'#' {
                escaped.push(byte);
            }
        }
        OsString::from_vec(escaped)
    }
    #[cfg(not(unix))]
    {
        OsString::from(dir.to_string_lossy().replace('#', "##"))
    }
}

fn launch_named(
    socket: Option<&str>,
    dir: &Path,
    slug: &str,
    cmd: Option<&OsStr>,
    anchor: Option<&str>,
) -> std::io::Result<String> {
    let tmux = |args: &[&str]| {
        let mut c = Command::new("tmux");
        if let Some(l) = socket {
            c.args(["-L", l]);
        }
        c.args(args);
        c
    };
    let good_path = cmd.is_some().then(|| launch_path(socket)).flatten();
    for n in 1..=99u32 {
        let name = if n == 1 {
            format!("tg_{slug}")
        } else {
            format!("tg_{slug}_{n}")
        };
        // '=' prefix = exact session-name match, not prefix match
        let taken = lifecycle_output(&mut tmux(&["has-session", "-t", &format!("={name}")]))?
            .status
            .success();
        if taken {
            continue;
        }
        let mut c = tmux(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{session_id}",
            "-s",
            &name,
            "-x",
            "90",
            "-y",
            "26",
            "-c",
        ]);
        // tmux format-expands the `-c` start-directory (same machinery as
        // the `-c "#{pane_current_path}"` idiom — verified against a real
        // server), so a literal `#` must be doubled or `#H`-style aliases
        // mangle the cwd and `#(cmd)` even runs a format job.
        c.arg(tmux_start_directory(dir));
        if let Some(cmd) = cmd {
            c.arg(path_cmd(good_path.as_deref(), cmd));
        }
        let created = lifecycle_output(&mut c)?;
        if created.status.success() {
            // Target the immutable session id, not its reusable name: a
            // command that exits instantly can make the name available
            // before this call, and we must never mark a replacement owned.
            let session_id = String::from_utf8_lossy(&created.stdout).trim().to_string();
            if !session_id.starts_with('$') {
                return Err(std::io::Error::other(format!(
                    "tmux did not return a session id for {name}"
                )));
            }
            let marked = lifecycle_output(&mut tmux(&[
                "set-option",
                "-t",
                &session_id,
                OWNER_OPTION,
                OWNER_VALUE,
            ]))?
            .status
            .success();
            if !marked {
                return Err(std::io::Error::other(format!(
                    "could not mark tmux session {name} as owned"
                )));
            }
            if let Some(a) = anchor {
                // best-effort: a failed set-option just means the card
                // falls back to its cwd's dir node. The immutable session
                // id also prevents attaching the anchor to a reused name.
                let _ = lifecycle_output(&mut tmux(&[
                    "set-option",
                    "-t",
                    &session_id,
                    "@tg_anchor",
                    a,
                ]));
            }
            return Ok(name);
        }
        // probe→create isn't atomic: if the name was claimed in between
        // (another viewer instance), move on to the next; anything else is
        // a real failure
        let lost_race = lifecycle_output(&mut tmux(&["has-session", "-t", &format!("={name}")]))?
            .status
            .success();
        if !lost_race {
            return Err(std::io::Error::other(format!(
                "tmux new-session {name} failed"
            )));
        }
    }
    Err(std::io::Error::other("all tg_ session names taken"))
}

/// Kill one pane on the default server with the same bounded lifecycle
/// command behavior as launches.
pub fn kill_pane(pane: &str) -> std::io::Result<bool> {
    let mut command = Command::new("tmux");
    command.args(["kill-pane", "-t", pane]);
    Ok(lifecycle_output(&mut command)?.status.success())
}

/// One-shot scan of a tmux server (`socket` selects a private `-L` server
/// for tests; the viewer passes None for the default one). Returns every
/// pane whose cwd is inside `vault`; allowlist and stickiness filtering
/// happen in [`Tracker`]. tmux absent or no server running → empty.
///
/// Separator: TAB. tmux octal-escapes non-printables in format output
/// (a 0x1f separator arrives as the literal text `\037` — tried it), but
/// tab passes through raw. The path field comes LAST so a tab inside a
/// path can't shear the record; a tab inside a *session name* (legal but
/// pathological) shifts the pid field, fails its numeric parse, and drops
/// that line safely — the degradation is a missing card, never a
/// mis-parsed one. `#{window_activity}` sits second-to-last for the same
/// reason: a new field must never come after the path.
pub fn scan(socket: Option<&str>, vault: &Path) -> Result<Vec<PaneInfo>, String> {
    let mut command = Command::new("tmux");
    if let Some(socket) = socket {
        command.args(["-L", socket]);
    }
    command.args([
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{@tg_owner}\t#{@tg_anchor}\t#{window_activity}\t#{pane_current_path}",
    ]);
    let out = match lifecycle_output_with_timeout(&mut command, DISCOVERY_TIMEOUT) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.to_string()),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if no_server(&err) {
            return Ok(Vec::new()); // the normal "no agents anywhere" state
        }
        // server present but the scan errored (dying/wedged server): a
        // FAILURE, so the caller can keep its last snapshot instead of
        // tearing down every card and mirror over one bad poll
        return Err(err.trim().to_string());
    }
    Ok(parse_scan_bytes(&out.stdout, vault))
}

/// A non-zero `list-panes` exit that just means "no server on this
/// socket" — tmux phrases it both ways depending on version/state.
fn no_server(stderr: &str) -> bool {
    stderr.contains("no server running") || stderr.contains("error connecting")
}

pub fn parse_scan(text: &str, vault: &Path) -> Vec<PaneInfo> {
    parse_scan_bytes(text.as_bytes(), vault)
}

fn parse_scan_bytes(text: &[u8], vault: &Path) -> Vec<PaneInfo> {
    text.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut fields = line.splitn(8, |byte| *byte == b'\t');
            let (
                Some(session),
                Some(pane),
                Some(pid),
                Some(command),
                Some(owner),
                Some(anchor),
                Some(activity),
                Some(cwd),
            ) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            )
            else {
                return None;
            };
            let cwd = path_from_bytes(cwd);
            if !cwd.starts_with(vault) {
                return None;
            }
            Some(PaneInfo {
                session: String::from_utf8_lossy(session).into_owned(),
                pane: String::from_utf8_lossy(pane).into_owned(),
                pid: std::str::from_utf8(pid).ok()?.parse().ok()?,
                cwd,
                command: String::from_utf8_lossy(command).into_owned(),
                owned: owner == OWNER_VALUE.as_bytes(),
                anchor: (!anchor.is_empty()).then(|| String::from_utf8_lossy(anchor).into_owned()),
                activity: std::str::from_utf8(activity)
                    .ok()
                    .and_then(|a| a.trim().parse().ok()),
            })
        })
        .collect()
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    let bytes = crate::tmux::unescape_octal(bytes);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        PathBuf::from(OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Stateful filter applying the owner-marker/allowlist rule with grace.
#[derive(Default)]
pub struct Tracker {
    /// (session, pane) → (last seen, agent name, pane root pid). The pid
    /// pins identity to the actual pane: pane ids restart at %0 on a new
    /// tmux server, so (session, pane) alone could revive a remembered
    /// agent onto an unrelated pane created within the grace window.
    last_ok: HashMap<(String, String), (Instant, String, u32)>,
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
            let ours = p.owned;
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
                self.last_ok.insert(key.clone(), (now, agent, p.pid));
            } else {
                // same key but a different root process = a new pane reusing
                // the id (fresh tmux server) — forget, don't revive
                let stale = self.last_ok.get(&key).is_some_and(|e| e.2 != p.pid);
                if stale {
                    self.last_ok.remove(&key);
                } else if let Some(e) = self.last_ok.get_mut(&key) {
                    e.0 = now; // refresh last-seen for sticky entries
                }
            }
            // Sticky: once a pane has been an agent, it stays one while the
            // pane exists — pane_current_command reads bash/python for the
            // whole duration of a tool call (which can far outlast any grace
            // window), and dropping the mirror mid-call would blank the card
            // and steal typing focus at the worst possible moment.
            if let Some((_, agent, pid)) = self.last_ok.get(&key)
                && *pid == p.pid
            {
                out.push(AgentPane {
                    session: p.session.clone(),
                    pane: p.pane.clone(),
                    pid: p.pid,
                    cwd: p.cwd.clone(),
                    agent: agent.clone(),
                    ours,
                    anchor: p.anchor.clone(),
                });
            }
        }
        // forget identities only once the pane itself is gone past the
        // (scan-flicker) grace
        self.last_ok.retain(|k, (t, _, _)| {
            now.duration_since(*t) <= GRACE
                || panes.iter().any(|p| p.session == k.0 && p.pane == k.1)
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn lifecycle_commands_time_out() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 1"]);
        let error = lifecycle_output_with_timeout(&mut command, Duration::from_millis(25))
            .expect_err("the sleeping command should exceed its deadline");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn parse_filters_to_vault() {
        let text = "work\t%1\t100\tclaude\t\t\t1787141160\t/v/notes\n\
                    other\t%2\t200\tclaude\t\t\t1787141160\t/elsewhere\n\
                    tg_pi_1\t%3\t300\tpi\ttext-graph\t\t\t/v/notes/topics\n\
                    tg_edit\t%4\t400\thx\ttext-graph\tnotes/a.md\t1787141199\t/v/notes\n\
                    bad-line\n";
        let panes = parse_scan(text, Path::new("/v/notes"));
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].session, "work");
        assert_eq!(panes[0].anchor, None, "unset @tg_anchor reads empty");
        assert!(!panes[0].owned);
        assert_eq!(panes[1].pane, "%3");
        assert!(panes[1].owned);
        assert_eq!(panes[2].anchor.as_deref(), Some("notes/a.md"));
        assert_eq!(panes[0].activity, Some(1787141160));
        assert_eq!(panes[2].activity, Some(1787141199));
        assert_eq!(
            panes[1].activity, None,
            "a blank #{{window_activity}} is absence, never a zero timestamp"
        );
    }

    #[test]
    fn parse_survives_tabs_in_paths_and_drops_tabbed_names_safely() {
        // path is the last field, so an embedded tab stays part of the path
        let text = "work\t%1\t100\tclaude\t\t\t1787141160\t/v/notes/weird\tdir\n";
        let panes = parse_scan(text, Path::new("/v/notes"));
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].cwd, PathBuf::from("/v/notes/weird\tdir"));
        assert_eq!(panes[0].command, "claude");
        // a tab inside a session name shifts the pid field; the numeric
        // parse fails and the record is DROPPED — never mis-assigned
        let sheared = "we\tird\t%1\t100\tclaude\t\t\t1787141160\t/v/notes\n\
                       work\t%2\t200\tclaude\t\t\t1787141160\t/v/notes\n";
        let panes = parse_scan(sheared, Path::new("/v/notes"));
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].session, "work");
    }

    fn pane(session: &str, command: &str) -> PaneInfo {
        pane_pid(session, command, 42)
    }

    fn pane_pid(session: &str, command: &str, pid: u32) -> PaneInfo {
        PaneInfo {
            session: session.into(),
            pane: "%1".into(),
            pid,
            cwd: PathBuf::from("/v"),
            command: command.into(),
            owned: false,
            anchor: None,
            activity: None,
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
    fn pane_id_reuse_on_a_new_server_does_not_revive_identity() {
        let allow = vec!["claude".to_string()];
        let mut tr = Tracker::new();
        let t0 = Instant::now();
        assert_eq!(
            tr.update(&[pane_pid("work", "claude", 42)], &allow, t0)
                .len(),
            1
        );

        // tmux server restarted: same session name, pane ids start over at
        // %1, but the root process differs — the remembered identity must
        // not attach to this unrelated pane
        let t1 = t0 + Duration::from_secs(2); // well inside GRACE
        let active = tr.update(&[pane_pid("work", "vim", 999)], &allow, t1);
        assert!(active.is_empty(), "revived onto a new server's pane");
    }

    #[test]
    fn no_server_stderr_is_not_a_failure() {
        assert!(no_server(
            "error connecting to /tmp/tmux-1000/default (No such file or directory)"
        ));
        assert!(no_server("no server running on /tmp/tmux-1000/default"));
        assert!(!no_server("server exited unexpectedly"));
        assert!(!no_server("protocol version mismatch"));
    }

    #[test]
    fn merge_paths_dedups_in_order() {
        assert_eq!(
            merge_paths(["/a", "", "/b", "/a", "/c", "/b"].into_iter()),
            "/a:/b:/c"
        );
        assert_eq!(merge_paths([].into_iter()), "");
    }

    #[test]
    fn path_cmd_wraps_only_with_a_known_path_and_escapes_quotes() {
        assert_eq!(path_cmd(None, OsStr::new("claude")), "claude");
        assert_eq!(
            path_cmd(Some("/a/bin:/b/bin"), OsStr::new("claude")),
            "PATH='/a/bin:/b/bin' exec claude"
        );
        // a single quote in a PATH component must not break out of the
        // quoting (pathological, but this runs through `sh -c`)
        assert_eq!(
            path_cmd(Some("/we'ird"), OsStr::new("sh")),
            r"PATH='/we'\''ird' exec sh"
        );
    }

    #[test]
    fn only_marked_sessions_get_owned_behavior() {
        let mut lookalike = pane("tg_codex_2", "bash");
        let mut tr = Tracker::new();
        assert!(
            tr.update(&[lookalike.clone()], &[], Instant::now())
                .is_empty(),
            "a user-chosen tg_ name is not proof of ownership"
        );

        lookalike.owned = true;
        let active = tr.update(&[lookalike], &[], Instant::now());
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "codex");
        assert!(active[0].ours);
    }

    #[cfg(unix)]
    #[test]
    fn tmux_helpers_preserve_non_utf8_unix_paths() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let file = PathBuf::from(OsString::from_vec(b"/v/raw#-\x80'file.md".to_vec()));
        assert_eq!(
            tmux_start_directory(&file).as_bytes(),
            b"/v/raw##-\x80'file.md"
        );
        assert_eq!(
            edit_command("nvim --clean", &file).as_bytes(),
            b"env COLORFGBG='15;0' nvim --clean '/v/raw#-\x80'\\''file.md'"
        );

        let cwd = PathBuf::from(OsString::from_vec(b"/v/notes-\x81".to_vec()));
        let mut output = b"work\t%1\t100\tclaude\t\t\t1787141160\t".to_vec();
        output.extend_from_slice(cwd.as_os_str().as_bytes());
        output.push(b'\n');
        let panes = parse_scan_bytes(&output, Path::new("/v"));
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].cwd, cwd);
        assert_eq!(path_from_bytes(b"/v/notes-\\201"), cwd);
    }
}
