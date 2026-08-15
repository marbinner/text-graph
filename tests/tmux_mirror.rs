//! Integration: mirror a scripted tmux session on a private socket.
//!
//! Skips (passes) when tmux isn't installed. Uses `tmux -L tg-test-<pid>` so
//! the user's default server is never touched, and kills only that server.

use std::process::Command;
use std::time::{Duration, Instant};

use text_graph::agents;
use text_graph::keys::{self, Mods, Special};
use text_graph::mirror::{SessionMirror, indexed_rgb};

fn tmux(socket: &str, args: &[&str]) -> bool {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kills the private server on every exit path — without this, a panic
/// before cleanup leaks a background tmux (test 2's `cat` never exits).
struct ServerGuard(String);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        tmux(&self.0, &["kill-server"]);
    }
}

#[test]
fn mirrors_a_scripted_session() {
    let have_tmux = Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_tmux {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let socket = format!("tg-test-{}", std::process::id());
    let _guard = ServerGuard(socket.clone());
    assert!(
        tmux(
            &socket,
            &[
                "new-session",
                "-d",
                "-s",
                "t1",
                "-x",
                "80",
                "-y",
                "24",
                "printf 'plain \\033[1;31mRED\\033[0m text'; sleep 30",
            ]
        ),
        "failed to create scripted session"
    );

    let mut m =
        SessionMirror::attach("t1", Some(&socket), None, || {}).expect("attach control client");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut success = false;
    let mut last_row = String::new();
    while Instant::now() < deadline && !success {
        m.pump();
        let grids = m.grids();
        if let Some((_, g)) = grids.first() {
            last_row = g.cells[..g.cols as usize].iter().map(|c| c.ch).collect();
            if let Some(byte_at) = last_row.find("RED") {
                // cells are indexed by CHAR position, not byte offset
                let cell_at = last_row[..byte_at].chars().count();
                let cell = g.cells[cell_at];
                success =
                    last_row.contains("plain") && cell.bold && cell.fg == Some(indexed_rgb(1));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        success,
        "expected styled 'plain RED text'; row 0 was {last_row:?}"
    );
}

/// The native-resize path the corner grip uses: `resize-window` sent over
/// the control client must flow back (%layout-change) into a resized grid.
#[test]
fn resize_window_updates_mirror_grid() {
    let have_tmux = Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_tmux {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let socket = format!("tg-test-rz-{}", std::process::id());
    let _guard = ServerGuard(socket.clone());
    assert!(
        tmux(
            &socket,
            &[
                "new-session",
                "-d",
                "-s",
                "t3",
                "-x",
                "80",
                "-y",
                "24",
                "cat"
            ]
        ),
        "failed to create session"
    );

    let mut m = SessionMirror::attach("t3", Some(&socket), None, || {}).expect("attach");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut pane = None;
    while Instant::now() < deadline && pane.is_none() {
        m.pump();
        pane = m.grids().first().map(|(p, _)| p.clone());
        std::thread::sleep(Duration::from_millis(50));
    }
    let pane = pane.expect("pane never discovered");

    m.command(&format!("resize-window -t {pane} -x 100 -y 30"));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut size = (0, 0);
    while Instant::now() < deadline && size != (100, 30) {
        m.pump();
        if let Some((_, g)) = m.grids().first() {
            size = (g.cols, g.rows);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(size, (100, 30), "grid never took the new size");
}

/// Launching agents from the graph: unique tg_ names, correct cwd.
#[test]
fn launch_creates_uniquely_named_tg_sessions() {
    let have_tmux = Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_tmux {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let socket = format!("tg-test-launch-{}", std::process::id());
    let _guard = ServerGuard(socket.clone());
    let dir = std::env::temp_dir();

    let first = agents::launch(Some(&socket), &dir, "sh").expect("first launch");
    assert_eq!(first, "tg_sh");
    let second = agents::launch(Some(&socket), &dir, "sh").expect("second launch");
    assert_eq!(second, "tg_sh_2", "name probing must skip the live session");
    let shell = agents::launch_shell(Some(&socket), &dir).expect("shell launch");
    assert_eq!(shell, "tg_term");
    // edit sessions carry their node binding IN tmux (@tg_anchor), and the
    // scan format must read it back — verified against a real server, per
    // the format-change house rule
    let edit = agents::launch_edit(Some(&socket), &dir, "cat", "notes/x.md").expect("edit launch");
    assert_eq!(edit, "tg_edit");
    let out = Command::new("tmux")
        .args([
            "-L",
            &socket,
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{@tg_anchor}",
        ])
        .output()
        .expect("list-panes");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.lines().any(|l| l == "tg_edit\tnotes/x.md"),
        "@tg_anchor must round-trip through the format: {text:?}"
    );
    assert!(
        text.lines().any(|l| l == "tg_term\t"),
        "sessions without an anchor read as empty: {text:?}"
    );

    // all sessions exist, cwd'd where we asked
    let out = Command::new("tmux")
        .args([
            "-L",
            &socket,
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{pane_current_path}",
        ])
        .output()
        .expect("list-panes");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for name in [&first, &second, &shell] {
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("{name}\t")))
            .unwrap_or_else(|| panic!("{name} missing from {text:?}"));
        assert_eq!(line.split('\t').nth(1), dir.to_str(), "cwd of {name}");
    }
}

/// Input path end to end: keystrokes sent through the same `keys::` commands
/// the GUI uses must come back through the mirror (tty echo of `cat`).
#[test]
fn typed_input_round_trips() {
    let have_tmux = Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_tmux {
        eprintln!("tmux not installed — skipping");
        return;
    }

    let socket = format!("tg-test-in-{}", std::process::id());
    let _guard = ServerGuard(socket.clone());
    assert!(
        tmux(
            &socket,
            &[
                "new-session",
                "-d",
                "-s",
                "t2",
                "-x",
                "80",
                "-y",
                "24",
                "cat"
            ]
        ),
        "failed to create cat session"
    );

    let mut m = SessionMirror::attach("t2", Some(&socket), None, || {}).expect("attach");

    // wait for the pane to be discovered
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut pane = None;
    while Instant::now() < deadline && pane.is_none() {
        m.pump();
        pane = m.grids().first().map(|(p, _)| p.clone());
        std::thread::sleep(Duration::from_millis(50));
    }
    let Some(pane) = pane else {
        panic!("pane never discovered");
    };

    m.command(&keys::hex_cmd(&pane, b"hello graph"));
    m.command(&keys::special_cmd(&pane, Special::Enter, Mods::default()));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = String::new();
    let mut success = false;
    while Instant::now() < deadline && !success {
        m.pump();
        if let Some((_, g)) = m.grids().first() {
            seen = g
                .cells
                .chunks(g.cols as usize)
                .take(3)
                .map(|row| row.iter().map(|c| c.ch).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            success = seen.contains("hello graph");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(success, "typed text never echoed back; screen was:\n{seen}");
}
