//! tmux control-mode client (`tmux -C attach-session`).
//!
//! The viewer never owns a PTY: tmux is the terminal the agent talks to (and
//! the thing answering its queries); we attach as a control-mode client and
//! receive each pane's display stream as `%output` events, plus FIFO-ordered
//! reply blocks for commands we send. Killing the client only detaches — the
//! session, and the agent inside it, live on.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// tmux pane identifier as the protocol spells it, e.g. `%3`.
pub type PaneId = String;

#[derive(Debug, PartialEq)]
pub enum TmuxEvent {
    /// Raw bytes a pane emitted (octal escapes already decoded).
    Output { pane: PaneId, bytes: Vec<u8> },
    /// One `%begin`…`%end`/`%error` block. Replies arrive in the order
    /// commands were sent; the attach handshake emits one unsolicited block
    /// first (the "banner") that consumers must skip.
    Reply { lines: Vec<String>, error: bool },
    /// Window/pane layout changed — re-list panes.
    Changed,
    /// The control client ended (session killed, server gone, or detached).
    Exit,
}

pub struct TmuxClient {
    child: Child,
    stdin: Arc<Mutex<std::process::ChildStdin>>,
}

impl TmuxClient {
    /// Attach to `session` in control mode. `socket` selects a private server
    /// (`tmux -L`) — used by tests; `None` is the user's default server.
    /// Events arrive on the returned receiver from a reader thread; `wake` is
    /// invoked after each event lands (e.g. to request a UI repaint).
    pub fn attach(
        session: &str,
        socket: Option<&str>,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<(TmuxClient, Receiver<TmuxEvent>)> {
        let mut cmd = Command::new("tmux");
        if let Some(s) = socket {
            cmd.args(["-L", s]);
        }
        cmd.args(["-C", "attach-session", "-t", session])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("stdin piped")));
        let (tx, rx) = channel();
        std::thread::spawn(move || reader_loop(stdout, tx, wake));
        Ok((TmuxClient { child, stdin }, rx))
    }

    /// Queue a tmux command. Its `Reply` arrives on the event channel, FIFO
    /// relative to other commands.
    pub fn command(&self, cmd: &str) -> std::io::Result<()> {
        let mut w = self.stdin.lock().unwrap();
        w.write_all(cmd.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()
    }
}

impl Drop for TmuxClient {
    fn drop(&mut self) {
        // Terminating the control client merely detaches it; the tmux session
        // (and any agent running inside) is untouched.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct ReaderState {
    /// Accumulating lines of an open `%begin` block.
    reply: Option<Vec<String>>,
}

fn reader_loop(stdout: ChildStdout, tx: Sender<TmuxEvent>, wake: impl Fn()) {
    let mut reader = BufReader::new(stdout);
    let mut state = ReaderState::default();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some(ev) = parse_line(line.trim_end_matches(['\r', '\n']), &mut state) {
            if tx.send(ev).is_err() {
                break;
            }
            wake();
        }
    }
    let _ = tx.send(TmuxEvent::Exit);
    wake();
}

/// Notifications that mean "pane/window layout changed — re-list".
const CHANGED: &[&str] = &[
    "%window-add",
    "%window-close",
    "%unlinked-window-add",
    "%unlinked-window-close",
    "%window-renamed",
    "%layout-change",
    "%window-pane-changed",
    "%session-window-changed",
    "%sessions-changed",
    "%pane-mode-changed",
];

/// One protocol line → at most one event. Command output blocks are atomic
/// (tmux does not interleave notifications inside `%begin`…`%end`).
fn parse_line(l: &str, state: &mut ReaderState) -> Option<TmuxEvent> {
    if let Some(rest) = l.strip_prefix("%output ") {
        let (pane, data) = rest.split_once(' ').unwrap_or((rest, ""));
        return Some(TmuxEvent::Output {
            pane: pane.to_string(),
            bytes: unescape_octal(data),
        });
    }
    if l.starts_with("%begin ") {
        state.reply = Some(Vec::new());
        return None;
    }
    if l.starts_with("%end ") {
        return state.reply.take().map(|lines| TmuxEvent::Reply { lines, error: false });
    }
    if l.starts_with("%error ") {
        return state.reply.take().map(|lines| TmuxEvent::Reply { lines, error: true });
    }
    if let Some(lines) = state.reply.as_mut() {
        lines.push(l.to_string());
        return None;
    }
    if l == "%exit" || l.starts_with("%exit ") {
        return Some(TmuxEvent::Exit);
    }
    if CHANGED.iter().any(|m| l.starts_with(m)) {
        return Some(TmuxEvent::Changed);
    }
    None
}

/// tmux escapes control bytes and backslash in `%output` data as `\ooo`.
pub fn unescape_octal(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let oct = |c: u8| (b'0'..=b'7').contains(&c);
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && oct(b[i + 1]) && oct(b[i + 2]) && oct(b[i + 3]) {
            let v = (b[i + 1] - b'0') as u32 * 64
                + (b[i + 2] - b'0') as u32 * 8
                + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape() {
        assert_eq!(unescape_octal("A\\033[31mB"), b"A\x1b[31mB");
        assert_eq!(unescape_octal("back\\134slash"), b"back\\slash");
        assert_eq!(unescape_octal("plain"), b"plain");
        assert_eq!(unescape_octal("dangling\\03"), b"dangling\\03");
    }

    #[test]
    fn protocol_stream() {
        let lines = [
            "%begin 1 0 0",
            "%end 1 0 0", // attach banner
            "%session-changed $0 t1",
            "%begin 1 1 0",
            "%1,80,24",
            "%end 1 1 0",
            "%output %1 hi\\015\\012",
            "%begin 1 2 0",
            "oops",
            "%error 1 2 0",
            "%window-add @2",
            "%exit",
        ];
        let mut st = ReaderState::default();
        let evs: Vec<_> = lines.iter().filter_map(|l| parse_line(l, &mut st)).collect();
        assert_eq!(
            evs,
            vec![
                TmuxEvent::Reply { lines: vec![], error: false },
                TmuxEvent::Reply { lines: vec!["%1,80,24".into()], error: false },
                TmuxEvent::Output { pane: "%1".into(), bytes: b"hi\r\n".to_vec() },
                TmuxEvent::Reply { lines: vec!["oops".into()], error: true },
                TmuxEvent::Changed,
                TmuxEvent::Exit,
            ]
        );
    }
}
