//! Agent-to-agent messaging: the model behind the `roster`, `send`, `peek`
//! and `protocol` subcommands.
//!
//! The design pins, in the order they constrain the code:
//!
//! - **Bus-free.** There is no queue, no mail directory, no watch list and
//!   no digest. A message is typed into another agent's terminal; anything
//!   worth keeping is written into the vault as a note. Relevance-routing
//!   is the reader's job — an LLM judges "does this concern me" better than
//!   any router we would write, and every layer stays inspectable as either
//!   a terminal or a file.
//! - **One-shot tmux, never control mode.** The CLI is a short-lived
//!   process invoked from inside an agent's pane. It runs `list-panes`,
//!   `capture-pane`, `load-buffer`/`paste-buffer` and exits; the viewer's
//!   control-mode client (`tmux.rs`) is for live mirrors and is irrelevant
//!   here. The viewer need not be running at all.
//! - **The caller is an agent's shell**, so every tmux call carries a
//!   deadline (`agents::lifecycle_output_with_timeout`): a wedged server
//!   must never wedge the agent that asked a question.
//! - **The viewer never sends.** Only agents and the human type into panes.
//!   Nothing in `app/` may call into the send path.
//! - **Deterministic**: the roster is sorted by (session, pane), never by
//!   map iteration, so two agents reading it see the same list in the same
//!   order.
//!
//! This module stays egui-free and tmux-free: it is the join, the ordering,
//! the addressing rules and the rendering. The subcommands in `main.rs` are
//! what actually shells out.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::agents::{AgentPane, PaneInfo};

/// The conventions `text-graph protocol` prints. This is the whole of the
/// protocol: there is no schema and no framing, because the participants
/// are language models reading a terminal. The first ping teaches a
/// newcomer by pointing here.
pub const PROTOCOL: &str = "\
text-graph — how agents talk in this vault

You are one agent among several working in the same vault. Three commands:

  text-graph roster              who else is live, and how long they've been quiet
  text-graph send <agent> <msg>  type a message into another agent's terminal
  text-graph peek <agent> [-n N] read the last N lines of their screen

Conventions, most important first:

1. Chatter goes in terminals; conclusions go in the vault. A message is a
   nudge, not a record. Anything that should outlive the session is a note,
   linked from the notes it concerns — that is the shared memory.
2. Address a note, not an inbox. For anything substantial, write the note
   first and send the link:
     text-graph send pi \"see [[topics/api-shape]] — need your read on §2\"
3. Messages arrive as if typed at the other agent's prompt, and their
   harness queues them while it is busy. You do not have to wait for idle,
   and you should not re-send because a reply hasn't appeared: peek first.
4. Judge relevance yourself. Nothing here routes, filters or summarizes —
   you are the router. If a message doesn't concern you, say so in one line
   or ignore it.
5. Keep messages short. Anything over 8 KiB is refused: that is a note.
6. Reply the way you were reached — the sender's name is in the prefix of
   the message you received.
";

/// What a pane is, which decides whether it can be *sent* to. Owned
/// sessions carry their role in the name (`tg_term`, `tg_edit`), so the
/// tag [`crate::agents::Tracker`] derives is the whole signal.
///
/// Shells and editors are listed and peekable but never send targets: a
/// message pasted into a shell would *execute*, so a mis-addressed ping
/// has to fail loudly instead of running something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Shell,
    Editor,
}

impl Kind {
    /// Owned `tg_` sessions name their role; everything else is an agent
    /// by allowlist (that is how discovery found it in the first place).
    fn of(agent: &str, ours: bool) -> Self {
        match (ours, agent) {
            (true, "term") => Kind::Shell,
            (true, "edit") => Kind::Editor,
            _ => Kind::Agent,
        }
    }

    pub fn addressable(self) -> bool {
        self == Kind::Agent
    }
}

/// One line of the roster: a live pane, where it is working, and how long
/// it has been quiet.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub session: String,
    pub pane: String,
    /// Harness tag — `claude`, `pi`, … — the name you address it by.
    pub agent: String,
    pub kind: Kind,
    pub ours: bool,
    /// Where this pane is working, vault-relative: the session's
    /// `@tg_anchor` when it has one (edit cards pin to their file), else
    /// its cwd. `.` is the vault root.
    pub place: String,
    /// Epoch seconds of the pane window's last output; see
    /// [`PaneInfo::activity`].
    pub activity: Option<u64>,
    /// Last non-blank line of the pane. The model can't fetch it — the
    /// `roster` subcommand fills it in by capturing each pane.
    pub tail: Option<String>,
}

impl Entry {
    /// Seconds since this pane last produced output, or None when tmux
    /// gave no timestamp. Deliberately a raw age rather than a
    /// busy/idle verdict: a harness that animates a spinner looks busy and
    /// one that thinks silently looks idle, so the reader judges.
    pub fn idle(&self, now: u64) -> Option<u64> {
        self.activity.map(|t| now.saturating_sub(t))
    }
}

/// Join a scan against the panes discovery accepted, in a stable order.
///
/// The activity timestamp rides [`PaneInfo`] alone (see the note on
/// [`AgentPane`]), so this is where the two halves meet.
pub fn roster(vault: &Path, panes: &[PaneInfo], active: &[AgentPane]) -> Vec<Entry> {
    let mut out: Vec<Entry> = active
        .iter()
        .map(|a| {
            let activity = panes
                .iter()
                .find(|p| p.session == a.session && p.pane == a.pane)
                .and_then(|p| p.activity);
            Entry {
                session: a.session.clone(),
                pane: a.pane.clone(),
                agent: a.agent.clone(),
                kind: Kind::of(&a.agent, a.ours),
                ours: a.ours,
                place: place_of(vault, a),
                activity,
                tail: None,
            }
        })
        .collect();
    // (session, pane) with the pane compared NUMERICALLY: "%10" sorts
    // before "%2" as text, and the roster is something two agents compare
    // notes about.
    out.sort_by(|a, b| {
        (&a.session, pane_number(&a.pane), &a.pane).cmp(&(
            &b.session,
            pane_number(&b.pane),
            &b.pane,
        ))
    });
    out
}

fn pane_number(pane: &str) -> u64 {
    pane.trim_start_matches('%').parse().unwrap_or(u64::MAX)
}

fn place_of(vault: &Path, a: &AgentPane) -> String {
    if let Some(anchor) = &a.anchor {
        return anchor.clone();
    }
    match a.cwd.strip_prefix(vault) {
        Ok(rest) if rest.as_os_str().is_empty() => ".".to_string(),
        Ok(rest) => rest.to_string_lossy().into_owned(),
        // outside the vault can't happen (the scan filters on it), but a
        // display string is never worth a panic
        Err(_) => a.cwd.to_string_lossy().into_owned(),
    }
}

/// Why an address didn't resolve. Both variants carry what to try instead:
/// the caller is a language model that will retry, so a failure that
/// doesn't name the alternatives just buys another wrong guess.
#[derive(Clone, Debug, PartialEq)]
pub enum Address {
    Unknown {
        query: String,
        suggestions: Vec<String>,
    },
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Address::Unknown { query, suggestions } if suggestions.is_empty() => {
                write!(f, "no agent named {query:?}, and nothing else is live")
            }
            Address::Unknown { query, suggestions } => write!(
                f,
                "no agent named {query:?} — live now: {}",
                suggestions.join(", ")
            ),
            Address::Ambiguous { query, candidates } => write!(
                f,
                "{query:?} matches {} sessions: {} — address one by session name",
                candidates.len(),
                candidates.join(", ")
            ),
        }
    }
}

/// Resolve an address: a pane id (`%3`), a session name (`tg_claude`), or
/// a harness tag (`claude`) when only one session runs it. Case-insensitive,
/// because the sender is a model writing prose.
pub fn resolve<'a>(entries: &'a [Entry], query: &str) -> Result<&'a Entry, Address> {
    let q = query.trim();
    if let Some(hit) = entries.iter().find(|e| e.pane == q) {
        return Ok(hit);
    }
    if let Some(hit) = entries.iter().find(|e| e.session.eq_ignore_ascii_case(q)) {
        return Ok(hit);
    }
    let by_tag: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.agent.eq_ignore_ascii_case(q))
        .collect();
    match by_tag.as_slice() {
        [only] => return Ok(only),
        [] => {}
        many => {
            return Err(Address::Ambiguous {
                query: q.to_string(),
                candidates: many.iter().map(|e| e.session.clone()).collect(),
            });
        }
    }
    Err(Address::Unknown {
        query: q.to_string(),
        suggestions: entries
            .iter()
            .map(|e| format!("{} ({})", e.agent, e.session))
            .collect(),
    })
}

/// The roster as aligned columns with a header — read by agents *and* by
/// the human running the command, so it stays a table rather than a
/// machine format. Empty in, empty out: the caller says what "nobody is
/// here" means in its own words.
pub fn render_roster(entries: &[Entry], now: u64, self_pane: Option<&str>) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let rows: Vec<[String; 4]> = entries
        .iter()
        .map(|e| {
            let name = match e.kind {
                Kind::Agent => e.agent.clone(),
                Kind::Shell => format!("{} (shell)", e.agent),
                Kind::Editor => format!("{} (editor)", e.agent),
            };
            let session = match self_pane {
                Some(p) if p == e.pane => format!("{} (you)", e.session),
                _ => e.session.clone(),
            };
            [
                name,
                session,
                e.idle(now).map(idle_label).unwrap_or("-".to_string()),
                e.place.clone(),
            ]
        })
        .collect();
    let head = ["AGENT", "SESSION", "QUIET", "WHERE"];
    let width = |col: usize| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .chain(std::iter::once(head[col].len()))
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..4).map(width).collect();

    let mut out = String::new();
    let mut line = |cells: [&str; 4], tail: Option<&str>| {
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(cell);
            out.push_str(&" ".repeat(widths[i].saturating_sub(cell.chars().count()) + 2));
        }
        if let Some(tail) = tail {
            out.push_str(tail);
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    };
    line(head, Some("LAST LINE"));
    for (row, entry) in rows.iter().zip(entries) {
        let cells = [
            row[0].as_str(),
            row[1].as_str(),
            row[2].as_str(),
            row[3].as_str(),
        ];
        line(cells, entry.tail.as_deref());
    }
    out
}

/// Coarse ages: the difference between 3s and 4s never matters, the
/// difference between seconds and hours always does.
fn idle_label(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

/// The vault an agent is speaking from: the nearest ancestor of `cwd` (or
/// `cwd` itself) holding a `.text-graph/` directory, else `cwd`. Found
/// git-style rather than passed as an argument, because the caller is an
/// agent in a pane that may have `cd`'d anywhere below the root — and a
/// vault the viewer has never opened has no marker, so plain `cwd` has to
/// remain a working answer.
pub fn vault_root(cwd: &Path) -> PathBuf {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".text-graph").is_dir() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    cwd.to_path_buf()
}

/// Wall-clock seconds, for comparing against [`PaneInfo::activity`].
pub fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_pane(session: &str, pane: &str, agent: &str, ours: bool) -> AgentPane {
        AgentPane {
            session: session.into(),
            pane: pane.into(),
            pid: 100,
            cwd: PathBuf::from("/v"),
            agent: agent.into(),
            ours,
            anchor: None,
        }
    }

    fn pane_info(session: &str, pane: &str, activity: Option<u64>) -> PaneInfo {
        PaneInfo {
            session: session.into(),
            pane: pane.into(),
            pid: 100,
            cwd: PathBuf::from("/v"),
            command: "claude".into(),
            owned: true,
            anchor: None,
            activity,
        }
    }

    #[test]
    fn roster_joins_activity_and_orders_deterministically() {
        let vault = Path::new("/v");
        let panes = vec![
            pane_info("tg_pi", "%10", Some(1000)),
            pane_info("tg_claude", "%2", Some(900)),
            pane_info("tg_pi", "%2", None),
        ];
        let active = vec![
            agent_pane("tg_pi", "%2", "pi", true),
            agent_pane("tg_claude", "%2", "claude", true),
            agent_pane("tg_pi", "%10", "pi", true),
        ];
        let entries = roster(vault, &panes, &active);
        let order: Vec<_> = entries
            .iter()
            .map(|e| (e.session.as_str(), e.pane.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![("tg_claude", "%2"), ("tg_pi", "%2"), ("tg_pi", "%10")],
            "sessions alphabetical, panes numeric — %10 after %2"
        );
        assert_eq!(entries[0].activity, Some(900));
        assert_eq!(entries[0].idle(1000), Some(100));
        assert_eq!(entries[1].idle(1000), None, "no timestamp is not age zero");
    }

    #[test]
    fn owned_session_tags_decide_what_can_be_sent_to() {
        let vault = Path::new("/v");
        let active = vec![
            agent_pane("tg_term", "%1", "term", true),
            agent_pane("tg_edit", "%2", "edit", true),
            agent_pane("tg_claude", "%3", "claude", true),
            agent_pane("work", "%4", "claude", false),
        ];
        let kinds: Vec<_> = roster(vault, &[], &active).iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![Kind::Agent, Kind::Editor, Kind::Shell, Kind::Agent],
            "sorted: tg_claude, tg_edit, tg_term, work"
        );
        assert!(!Kind::Shell.addressable(), "a paste into a shell RUNS");
        assert!(!Kind::Editor.addressable());
        assert!(Kind::Agent.addressable());
    }

    #[test]
    fn place_prefers_the_anchor_then_the_relative_cwd() {
        let vault = Path::new("/v");
        let mut anchored = agent_pane("tg_edit", "%1", "edit", true);
        anchored.anchor = Some("notes/a.md".into());
        let mut deep = agent_pane("tg_claude", "%2", "claude", true);
        deep.cwd = PathBuf::from("/v/topics/api");
        let root = agent_pane("tg_pi", "%3", "pi", true);

        let entries = roster(vault, &[], &[anchored, deep, root]);
        assert_eq!(entries[0].place, "topics/api");
        assert_eq!(entries[1].place, "notes/a.md");
        assert_eq!(entries[2].place, ".", "the vault root is not an empty cell");
    }

    #[test]
    fn addressing_resolves_by_tag_session_or_pane_and_fails_with_names() {
        let vault = Path::new("/v");
        let entries = roster(
            vault,
            &[],
            &[
                agent_pane("tg_claude", "%1", "claude", true),
                agent_pane("tg_pi", "%2", "pi", true),
            ],
        );
        assert_eq!(resolve(&entries, "pi").unwrap().session, "tg_pi");
        assert_eq!(resolve(&entries, "TG_PI").unwrap().pane, "%2");
        assert_eq!(resolve(&entries, "%1").unwrap().agent, "claude");
        assert_eq!(resolve(&entries, " pi ").unwrap().session, "tg_pi");

        let error = resolve(&entries, "clod").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("claude (tg_claude)"), "unhelpful: {text}");
        assert!(text.contains("pi (tg_pi)"), "unhelpful: {text}");
    }

    #[test]
    fn a_tag_shared_by_two_sessions_is_ambiguous_not_first_wins() {
        let vault = Path::new("/v");
        let entries = roster(
            vault,
            &[],
            &[
                agent_pane("tg_claude", "%1", "claude", true),
                agent_pane("tg_claude_2", "%2", "claude", true),
            ],
        );
        let error = resolve(&entries, "claude").unwrap_err();
        assert!(matches!(error, Address::Ambiguous { .. }));
        let text = error.to_string();
        assert!(text.contains("tg_claude"), "{text}");
        assert!(text.contains("tg_claude_2"), "{text}");
        // …and the exact session name still gets through
        assert_eq!(resolve(&entries, "tg_claude_2").unwrap().pane, "%2");
    }

    #[test]
    fn rendering_aligns_columns_and_marks_the_caller() {
        let vault = Path::new("/v");
        let mut entries = roster(
            vault,
            &[
                pane_info("tg_claude", "%1", Some(940)),
                pane_info("tg_pi", "%2", Some(400)),
            ],
            &[
                agent_pane("tg_claude", "%1", "claude", true),
                agent_pane("tg_pi", "%2", "pi", true),
            ],
        );
        entries[0].tail = Some("waiting for your reply".into());
        let text = render_roster(&entries, 1000, Some("%2"));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("AGENT "), "{:?}", lines[0]);
        assert!(lines[0].ends_with("LAST LINE"));
        assert!(lines[1].starts_with("claude "), "{:?}", lines[1]);
        assert!(
            lines[1].contains("1m"),
            "60s quiet reads as 1m: {:?}",
            lines[1]
        );
        assert!(lines[2].contains("10m"), "{:?}", lines[2]);
        assert!(lines[1].ends_with("waiting for your reply"));
        // columns line up: every row starts its SESSION cell where the
        // header does (the widest cell in the column sets the offset)
        let session_at = |line: &str, cell: &str| line.find(cell).expect(cell);
        assert_eq!(
            session_at(lines[0], "SESSION"),
            session_at(lines[1], "tg_claude")
        );
        assert_eq!(
            session_at(lines[0], "SESSION"),
            session_at(lines[2], "tg_pi")
        );
        assert!(
            lines[2].contains("tg_pi (you)"),
            "the caller's own pane is marked: {:?}",
            lines[2]
        );
        assert!(!lines[2].ends_with(' '), "no trailing padding");
        assert!(render_roster(&[], 1000, None).is_empty());
    }

    #[test]
    fn idle_labels_stay_coarse() {
        assert_eq!(idle_label(0), "0s");
        assert_eq!(idle_label(59), "59s");
        assert_eq!(idle_label(60), "1m");
        assert_eq!(idle_label(3599), "59m");
        assert_eq!(idle_label(3600), "1h");
        assert_eq!(idle_label(86_400), "1d");
    }

    #[test]
    fn vault_root_walks_up_to_the_marker_and_falls_back_to_cwd() {
        let base = std::env::temp_dir().join(format!("tg-comm-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let deep = base.join("vault/topics/api");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(base.join("vault/.text-graph")).unwrap();

        assert_eq!(vault_root(&deep), base.join("vault"));
        assert_eq!(vault_root(&base.join("vault")), base.join("vault"));
        // no marker anywhere above: the cwd is the vault, unchanged
        let unmarked = base.join("elsewhere");
        std::fs::create_dir_all(&unmarked).unwrap();
        assert_eq!(vault_root(&unmarked), unmarked);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_protocol_text_documents_the_commands_it_ships_with() {
        for command in ["text-graph roster", "text-graph send", "text-graph peek"] {
            assert!(PROTOCOL.contains(command), "protocol forgot {command}");
        }
    }
}
