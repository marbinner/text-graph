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
//! This module stays egui-free: it owns the join, the ordering, the
//! addressing rules, the rendering and the tmux calls the trio needs.
//! `main.rs` only parses arguments and prints what comes back.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::agents::{self, AgentPane, PaneInfo};

/// Deadline for the CLI's own tmux calls. Longer than discovery's 1s
/// (a capture of deep history is more work than listing panes), short
/// enough that a wedged server never holds an agent's shell for long.
const CLI_TIMEOUT: Duration = Duration::from_secs(3);

/// How many lines `peek` shows without being asked, and the ceiling on
/// asking: a capture is text an agent must then read, so an unbounded
/// `-n` spends someone else's context.
pub const PEEK_DEFAULT: usize = 40;
pub const PEEK_MAX: usize = 2000;

/// Roster tails are a glance, not a read — `peek` is the read.
const TAIL_CHARS: usize = 60;

/// The biggest message that may be typed into another agent's prompt.
/// Not a buffer limit — a design one: past this you are writing a
/// document at someone, and the vault is where documents go. Refusing is
/// how the "chatter in terminals, conclusions in notes" rule gets teeth.
pub const MAX_MESSAGE: usize = 8 * 1024;

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

/// Who is live in `vault` right now, sorted and ready to address.
///
/// The [`agents::Tracker`] is fresh every run, which one-shot processes
/// can't avoid: its stickiness is memory across scans. The consequence is
/// worth knowing — a FOREIGN pane (no `tg_` owner marker) is only visible
/// while its foreground command is still the agent binary, so one that is
/// deep in a tool call reads as absent. Sessions the graph launched carry
/// the owner marker and are unaffected.
pub fn live(socket: Option<&str>, vault: &Path) -> Result<Vec<Entry>, String> {
    let panes = agents::scan(socket, vault)?;
    let allowlist = agents::default_allowlist();
    let active = agents::Tracker::new().update(&panes, &allowlist, Instant::now());
    Ok(roster(vault, &panes, &active))
}

/// The last `lines` lines of a pane that carry anything, oldest first.
///
/// `-S -n` starts n lines up in the history and runs to the bottom of the
/// screen, so the capture is longer than asked; trimming the blank tail a
/// screen always has and then keeping the last `lines` gives "the last N
/// lines that say something", which is what a reader wanted.
pub fn capture(socket: Option<&str>, pane: &str, lines: usize) -> Result<Vec<String>, String> {
    let start = format!("-{}", lines.min(PEEK_MAX));
    let text = capture_raw(socket, pane, &start)?;
    let mut out: Vec<String> = text.lines().map(str::to_string).collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let keep = lines.min(PEEK_MAX);
    if out.len() > keep {
        out.drain(..out.len() - keep);
    }
    Ok(out)
}

/// Fill in each entry's [`Entry::tail`] — one capture per pane, best
/// effort: a pane that dies between the scan and the capture costs its
/// last line, never the roster.
pub fn fill_tails(socket: Option<&str>, entries: &mut [Entry]) {
    for entry in entries.iter_mut() {
        entry.tail = capture_raw(socket, &entry.pane, "0")
            .ok()
            .and_then(|text| last_non_blank(&text));
    }
}

/// `capture-pane -p` without `-e`: plain text, since the reader is a
/// language model and escape sequences are noise it would have to parse.
fn capture_raw(socket: Option<&str>, pane: &str, start: &str) -> Result<String, String> {
    let mut command =
        agents::tmux_command(socket, &["capture-pane", "-p", "-t", pane, "-S", start]);
    let out = agents::lifecycle_output_with_timeout(&mut command, CLI_TIMEOUT).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "tmux is not installed".to_string()
        } else {
            e.to_string()
        }
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(if agents::no_server(&stderr) {
            "no tmux server is running".to_string()
        } else {
            stderr.trim().to_string()
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn last_non_blank(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(tidy_tail)
}

/// One roster cell's worth of a terminal line: control characters gone
/// (a capture can carry them even without `-e`) and clipped, so a wide
/// TUI can't push the table apart.
fn tidy_tail(line: &str) -> String {
    let cleaned: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.chars().count() <= TAIL_CHARS {
        return cleaned.to_string();
    }
    cleaned.chars().take(TAIL_CHARS - 1).collect::<String>() + "…"
}

/// Who a message is from, as the receiving agent will read it.
#[derive(Clone, Debug, PartialEq)]
pub enum Sender {
    /// Another pane on the roster: it can be replied to by name.
    Agent {
        name: String,
        /// Session name — the address that can never be ambiguous.
        address: String,
        place: String,
    },
    /// No `$TMUX_PANE`: someone typed this at an ordinary terminal.
    Human,
    /// In tmux, but not a pane discovery lists (outside the vault, or a
    /// window this scan didn't accept). Attribution degrades to the pane
    /// id rather than lying about who spoke.
    Unlisted { pane: String },
}

/// Identify the caller from `$TMUX_PANE`. The environment variable is
/// tmux's own, set in every pane it spawns, so it needs no cooperation
/// from the agent — which matters, because attribution a sender can
/// forge by argument is attribution nobody can trust.
pub fn sender_from(entries: &[Entry], tmux_pane: Option<&str>) -> Sender {
    let Some(pane) = tmux_pane else {
        return Sender::Human;
    };
    match entries.iter().find(|e| e.pane == pane) {
        Some(me) => Sender::Agent {
            name: me.agent.clone(),
            address: me.session.clone(),
            place: me.place.clone(),
        },
        None => Sender::Unlisted {
            pane: pane.to_string(),
        },
    }
}

/// What actually gets typed into the target's prompt: one attribution
/// line, the message verbatim, and (from an agent) one line telling the
/// reader how to answer.
///
/// The reply hint is stateless and unconditional — tracking "have these
/// two spoken before" would need a state file, which is the mail
/// directory this design refuses. Whether it earns its line is a question
/// for the F2 experiment, when two real agents have used it.
pub fn compose(sender: &Sender, message: &str) -> String {
    let (attribution, footer) = match sender {
        Sender::Agent {
            name,
            address,
            place,
        } => (
            format!("[tg] from {name} · at {place}"),
            Some(format!(
                "(reply: text-graph send {address} \"…\" · then: text-graph protocol)"
            )),
        ),
        Sender::Human => (
            "[tg] from the human · answer in your own terminal".to_string(),
            None,
        ),
        Sender::Unlisted { pane } => (
            format!("[tg] from tmux pane {pane}"),
            Some("(conventions: text-graph protocol)".to_string()),
        ),
    };
    let mut out = format!("{attribution}\n{}", message.trim_end());
    if let Some(footer) = footer {
        out.push('\n');
        out.push_str(&footer);
    }
    out
}

/// Why a message wasn't delivered. Every variant names what to do instead:
/// the sender is a model that will try again immediately.
#[derive(Clone, Debug, PartialEq)]
pub enum SendError {
    Empty,
    TooLong(usize),
    NotAnAgent { session: String, kind: Kind },
    Yourself { session: String },
    Tmux(String),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::Empty => write!(f, "nothing to send"),
            SendError::TooLong(bytes) => write!(
                f,
                "message is {bytes} bytes, over the {MAX_MESSAGE}-byte limit — write it as a note and send the link instead"
            ),
            SendError::NotAnAgent { session, kind } => {
                let what = match kind {
                    Kind::Shell => "a shell: a message pasted there would RUN as a command",
                    Kind::Editor => "an editor: a message pasted there would land in a file",
                    Kind::Agent => "not an agent",
                };
                write!(f, "{session} is {what} (peek at it instead)")
            }
            SendError::Yourself { session } => {
                write!(f, "{session} is your own pane — talk to yourself elsewhere")
            }
            SendError::Tmux(message) => write!(f, "{message}"),
        }
    }
}

/// Type `message` into `target`'s pane and submit it.
///
/// The delivery path is tmux's own paste machinery, for the reason
/// documented on [`crate::keys::paste_cmds`]: `paste-buffer -p` wraps the
/// text in bracketed-paste markers only if the receiving application asked
/// for them, and only the SERVER knows that. Deciding it here would
/// silently submit a multi-line message one line at a time. The buffer is
/// filled from stdin (`load-buffer -`) so no quoting question arises, and
/// named by our pid so two senders can't overwrite each other between
/// filling and pasting.
pub fn send(
    socket: Option<&str>,
    target: &Entry,
    sender: &Sender,
    message: &str,
) -> Result<(), SendError> {
    if message.trim().is_empty() {
        return Err(SendError::Empty);
    }
    if message.len() > MAX_MESSAGE {
        return Err(SendError::TooLong(message.len()));
    }
    if !target.kind.addressable() {
        return Err(SendError::NotAnAgent {
            session: target.session.clone(),
            kind: target.kind,
        });
    }
    if let Sender::Agent { address, .. } = sender
        && address == &target.session
    {
        return Err(SendError::Yourself {
            session: target.session.clone(),
        });
    }

    let buffer = format!("tg_send_{}", std::process::id());
    let text = compose(sender, message);
    let mut load = agents::tmux_command(socket, &["load-buffer", "-b", &buffer, "-"]);
    run(
        agents::output_with_stdin(&mut load, text.as_bytes(), CLI_TIMEOUT),
        "load-buffer",
    )?;
    // -d drops the one-shot buffer as it pastes
    let mut paste = agents::tmux_command(
        socket,
        &["paste-buffer", "-dp", "-b", &buffer, "-t", &target.pane],
    );
    run(
        agents::lifecycle_output_with_timeout(&mut paste, CLI_TIMEOUT),
        "paste-buffer",
    )?;
    // separate from the paste on purpose: inside the bracketed markers a
    // newline is text, so the submit has to arrive after them
    let mut enter = agents::tmux_command(socket, &["send-keys", "-t", &target.pane, "Enter"]);
    run(
        agents::lifecycle_output_with_timeout(&mut enter, CLI_TIMEOUT),
        "send-keys",
    )
}

fn run(result: std::io::Result<std::process::Output>, what: &str) -> Result<(), SendError> {
    let out = result.map_err(|e| {
        SendError::Tmux(if e.kind() == std::io::ErrorKind::NotFound {
            "tmux is not installed".to_string()
        } else {
            format!("{what}: {e}")
        })
    })?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(SendError::Tmux(if stderr.trim().is_empty() {
        format!("{what} failed")
    } else {
        format!("{what}: {}", stderr.trim())
    }))
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
    fn tails_are_one_glance_wide_and_free_of_control_bytes() {
        assert_eq!(
            last_non_blank("first\nlast line here\n\n   \n").as_deref(),
            Some("last line here"),
            "the blank tail every screen has is not the last line"
        );
        assert_eq!(last_non_blank("   \n\n"), None);
        let noisy = format!("busy{}spinner", '\u{7}');
        assert_eq!(tidy_tail(&noisy), "busy spinner");
        let long = "x".repeat(TAIL_CHARS + 20);
        let clipped = tidy_tail(&long);
        assert_eq!(clipped.chars().count(), TAIL_CHARS);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn attribution_comes_from_the_pane_the_caller_is_in() {
        let vault = Path::new("/v");
        let entries = roster(
            vault,
            &[],
            &[
                agent_pane("tg_claude", "%1", "claude", true),
                agent_pane("tg_pi", "%2", "pi", true),
            ],
        );
        assert_eq!(
            sender_from(&entries, Some("%2")),
            Sender::Agent {
                name: "pi".into(),
                address: "tg_pi".into(),
                place: ".".into()
            }
        );
        assert_eq!(sender_from(&entries, None), Sender::Human);
        assert_eq!(
            sender_from(&entries, Some("%9")),
            Sender::Unlisted { pane: "%9".into() },
            "an unknown pane is named, never attributed to someone else"
        );
    }

    #[test]
    fn composed_messages_carry_a_reply_route() {
        let from = Sender::Agent {
            name: "pi".into(),
            address: "tg_pi".into(),
            place: "topics".into(),
        };
        let text = compose(&from, "does §2 still hold?\n");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "[tg] from pi · at topics");
        assert_eq!(lines[1], "does §2 still hold?", "the message is verbatim");
        assert_eq!(
            lines[2], "(reply: text-graph send tg_pi \"…\" · then: text-graph protocol)",
            "exact, because this text is typed into someone's prompt"
        );
        assert_eq!(lines.len(), 3, "three lines, no padding: {text:?}");

        let human = compose(&Sender::Human, "stop and look at the build");
        assert!(human.starts_with("[tg] from the human"));
        assert!(
            !human.contains("reply: text-graph send"),
            "a human at a terminal has no roster address to reply to"
        );
    }

    #[test]
    fn refusals_that_protect_the_receiver() {
        let vault = Path::new("/v");
        let entries = roster(
            vault,
            &[],
            &[
                agent_pane("tg_term", "%1", "term", true),
                agent_pane("tg_pi", "%2", "pi", true),
            ],
        );
        let shell = entries.iter().find(|e| e.session == "tg_term").unwrap();
        let pi = entries.iter().find(|e| e.session == "tg_pi").unwrap();
        let me = sender_from(&entries, Some("%2"));

        // no tmux is reached in any of these: the guards come first
        let error = send(None, shell, &me, "hello").unwrap_err();
        assert!(error.to_string().contains("RUN as a command"), "{error}");
        let error = send(None, pi, &me, "hello").unwrap_err();
        assert_eq!(
            error,
            SendError::Yourself {
                session: "tg_pi".into()
            }
        );
        let stranger = Sender::Human;
        assert_eq!(
            send(None, pi, &stranger, "   ").unwrap_err(),
            SendError::Empty
        );
        let huge = "x".repeat(MAX_MESSAGE + 1);
        let error = send(None, pi, &stranger, &huge).unwrap_err();
        assert!(
            error.to_string().contains("write it as a note"),
            "the limit has to teach the convention: {error}"
        );
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
