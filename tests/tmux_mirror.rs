//! Integration: mirror a scripted tmux session on a private socket.
//!
//! Skips (passes) when tmux isn't installed. Uses `tmux -L tg-test-<pid>` so
//! the user's default server is never touched, and kills only that server.

use std::process::Command;
use std::time::{Duration, Instant};

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
    assert!(
        tmux(&socket, &[
            "new-session",
            "-d",
            "-s",
            "t1",
            "-x",
            "80",
            "-y",
            "24",
            "printf 'plain \\033[1;31mRED\\033[0m text'; sleep 30",
        ]),
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
            if let Some(r_at) = last_row.find("RED") {
                let cell = g.cells[r_at];
                success = last_row.contains("plain")
                    && cell.bold
                    && cell.fg == Some(indexed_rgb(1));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    tmux(&socket, &["kill-server"]);
    assert!(success, "expected styled 'plain RED text'; row 0 was {last_row:?}");
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
    assert!(
        tmux(&socket, &["new-session", "-d", "-s", "t2", "-x", "80", "-y", "24", "cat"]),
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
        tmux(&socket, &["kill-server"]);
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
    tmux(&socket, &["kill-server"]);
    assert!(success, "typed text never echoed back; screen was:\n{seen}");
}
