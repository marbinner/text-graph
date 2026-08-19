//! Integration: the messaging trio against a real tmux server.
//!
//! Skips (passes) when tmux isn't installed. Uses `tmux -L tg-test-<pid>` so
//! the user's own server is never touched, and kills only that server.
//!
//! The stand-in agents run `cat`: an owned `tg_` session counts as an agent
//! whatever it runs, and `cat` echoes what was pasted into it — which is
//! how delivery becomes assertable on a machine with no agent installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn tmux(socket: &str, args: &[&str]) -> Output {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .output()
        .expect("run tmux")
}

/// Kills the private server on every exit path — the `cat` panes never
/// exit on their own, so a panic before cleanup would leak a tmux.
struct ServerGuard(String);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = tmux(&self.0, &["kill-server"]);
    }
}

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// An owned `tg_` session at `dir` running `cat`.
fn agent_session(socket: &str, name: &str, dir: &Path) {
    assert!(
        tmux(
            socket,
            &[
                "new-session",
                "-d",
                "-s",
                name,
                "-c",
                &dir.to_string_lossy(),
                "cat",
            ],
        )
        .status
        .success(),
        "create {name}"
    );
    assert!(
        tmux(
            socket,
            &["set-option", "-t", name, "@tg_owner", "text-graph"]
        )
        .status
        .success(),
        "mark {name} as ours"
    );
}

fn pane_of(socket: &str, session: &str) -> String {
    let out = tmux(
        socket,
        &["list-panes", "-a", "-F", "#{session_name}\t#{pane_id}"],
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("{session}\t"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("no pane for {session}"))
}

/// Run the CLI from inside the vault, against the private server. `pane`
/// is the caller's `$TMUX_PANE`; None means a human at an ordinary
/// terminal — and it is *removed*, never merely unset by omission, or a
/// `cargo test` run from inside tmux would leak its own pane in.
fn cli(vault: &Path, socket: &str, pane: Option<&str>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_text-graph"));
    command.current_dir(vault).args(args).args(["-L", socket]);
    match pane {
        Some(pane) => command.env("TMUX_PANE", pane),
        None => command.env_remove("TMUX_PANE"),
    };
    command.output().expect("run text-graph")
}

/// Poll a pane until it shows `needle`, or give up and return what it did
/// show — the assertion that follows is what reports the failure.
fn wait_for(socket: &str, session: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = tmux(socket, &["capture-pane", "-p", "-t", session]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if text.contains(needle) || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn agents_see_each_other_and_a_message_lands_in_a_pane() {
    if !have_tmux() {
        eprintln!("tmux not installed — skipping");
        return;
    }
    let socket = format!("tg-test-comm-{}", std::process::id());
    let _guard = ServerGuard(socket.clone());

    let vault: PathBuf = std::env::temp_dir().join(format!("tg-comm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&vault);
    std::fs::create_dir_all(vault.join(".text-graph")).expect("vault");
    std::fs::create_dir_all(vault.join("topics")).expect("topics");

    agent_session(&socket, "tg_fake", &vault);
    agent_session(&socket, "tg_pi", &vault.join("topics"));
    agent_session(&socket, "tg_term", &vault); // named a shell: not a target
    let pi = pane_of(&socket, "tg_pi");

    // the roster is found from the cwd, so run it from a subdirectory
    let roster = cli(&vault.join("topics"), &socket, Some(&pi), &["roster"]);
    let listing = stdout(&roster);
    assert!(roster.status.success(), "{}", stderr(&roster));
    for expected in [
        "fake",
        "tg_fake",
        "pi",
        "tg_pi (you)",
        "term (shell)",
        "topics",
    ] {
        assert!(
            listing.contains(expected),
            "roster missing {expected}:\n{listing}"
        );
    }

    // …and a message typed by pi arrives in fake's pane, attributed
    let sent = cli(
        &vault,
        &socket,
        Some(&pi),
        &["send", "fake", "does the shape still hold?"],
    );
    assert!(sent.status.success(), "{}", stderr(&sent));
    let screen = wait_for(&socket, "tg_fake", "does the shape still hold?");
    assert!(
        screen.contains("[tg] from pi"),
        "no attribution in:\n{screen}"
    );
    assert!(
        screen.contains("reply: text-graph send tg_pi"),
        "no reply route in:\n{screen}"
    );

    // peek reads it back through the CLI, behind its header
    let peeked = cli(&vault, &socket, Some(&pi), &["peek", "fake", "-n", "20"]);
    assert!(peeked.status.success(), "{}", stderr(&peeked));
    let seen = stdout(&peeked);
    assert!(seen.starts_with("# tg_fake (fake)"), "no header:\n{seen}");
    assert!(seen.contains("does the shape still hold?"), "{seen}");

    // a shell is listed and peekable but never a send target
    let refused = cli(&vault, &socket, Some(&pi), &["send", "term", "ls"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("RUN as a command"),
        "{}",
        stderr(&refused)
    );

    // a wrong name fails loudly, naming who is actually there
    let unknown = cli(&vault, &socket, Some(&pi), &["send", "clod", "hello"]);
    assert_eq!(unknown.status.code(), Some(1));
    let complaint = stderr(&unknown);
    assert!(complaint.contains("no agent named"), "{complaint}");
    assert!(complaint.contains("fake (tg_fake)"), "{complaint}");

    // and the human's message is attributed to the human, not to a pane
    let from_human = cli(&vault, &socket, None, &["send", "fake", "stop and look"]);
    assert!(from_human.status.success(), "{}", stderr(&from_human));
    let screen = wait_for(&socket, "tg_fake", "stop and look");
    assert!(
        screen.contains("[tg] from the human"),
        "no human attribution in:\n{screen}"
    );

    let _ = std::fs::remove_dir_all(&vault);
}
