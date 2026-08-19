use std::path::PathBuf;
use std::process::{Command, Output};

fn text_graph() -> Command {
    Command::new(env!("CARGO_BIN_EXE_text-graph"))
}

fn fixture_vault() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault")
}

fn output(command: &mut Command) -> Output {
    command.output().expect("run text-graph")
}

#[test]
fn help_and_version_are_successful() {
    let help = output(text_graph().arg("--help"));
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("text-graph stats <vault-path>"));
    assert!(help.stderr.is_empty());

    let version = output(text_graph().arg("--version"));
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("text-graph {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(version.stderr.is_empty());
}

#[test]
fn stats_scans_the_fixture_vault_through_the_binary() {
    let stats = output(text_graph().arg("stats").arg(fixture_vault()));
    assert!(stats.status.success());

    let stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(stdout.contains("nodes: 27 total = 13 files"));
    assert!(stdout.contains("ambiguous links (1):"));
    assert!(stdout.contains("ghosts (2):"));

    let stderr = String::from_utf8_lossy(&stats.stderr);
    assert!(stderr.contains("(27 nodes in "));
}

#[test]
fn bad_invocation_uses_the_documented_usage_exit() {
    let bad = output(text_graph().arg("--not-an-option"));
    assert_eq!(bad.status.code(), Some(2));
    assert!(bad.stdout.is_empty());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("usage:"));
}

#[cfg(unix)]
#[test]
fn stats_accepts_a_non_utf8_vault_path() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let mut component = format!("text-graph-cli-{}-", std::process::id()).into_bytes();
    component.push(0xff);
    let root = std::env::temp_dir().join(OsString::from_vec(component));
    std::fs::create_dir(&root).expect("create non-UTF-8 vault");
    std::fs::write(root.join("note.md"), "# note\n").expect("write note");

    let stats = output(text_graph().arg("stats").arg(&root));
    let cleanup = std::fs::remove_dir_all(&root);

    assert!(
        stats.status.success(),
        "{}",
        String::from_utf8_lossy(&stats.stderr)
    );
    assert!(String::from_utf8_lossy(&stats.stdout).contains("1 files"));
    cleanup.expect("remove non-UTF-8 vault");
}

/// A scratch directory to run the messaging commands from — never the
/// repo, where the user's own tmux may well have a pane whose cwd is
/// inside it and would show up in the roster.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tg-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A socket name no server listens on: tmux says so, and "nobody is here"
/// is an answer, not a failure.
fn dead_socket() -> String {
    format!("tg-cli-absent-{}", std::process::id())
}

#[test]
fn protocol_prints_the_conventions() {
    let out = output(text_graph().arg("protocol"));
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("text-graph send <agent> <message>"));
    assert!(stdout.contains("conclusions go in the vault"));
    assert!(
        !stdout.starts_with("---"),
        "the skill's frontmatter is for the harness, not the reader"
    );
}

#[test]
fn help_lists_the_messaging_commands() {
    let help = output(text_graph().arg("--help"));
    let stdout = String::from_utf8_lossy(&help.stdout);
    for command in [
        "text-graph roster",
        "text-graph peek",
        "text-graph protocol",
    ] {
        assert!(stdout.contains(command), "usage forgot {command}: {stdout}");
    }
}

#[test]
fn an_empty_roster_is_success_not_an_error() {
    let dir = scratch("roster");
    let out = output(
        text_graph()
            .current_dir(&dir)
            .args(["roster", "-L", &dead_socket()]),
    );
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("no live agents in"));
}

#[test]
fn malformed_messaging_commands_exit_two() {
    let dir = scratch("usage");
    let cases: [&[&str]; 7] = [
        &["peek"],
        &["peek", "someone", "-n", "zero"],
        &["peek", "someone", "-n"],
        &["roster", "--bogus"],
        &["send"],
        &["send", "someone"],
        &["protocol", "--user"],
    ];
    for case in cases {
        let out = output(text_graph().current_dir(&dir).args(case));
        assert_eq!(
            out.status.code(),
            Some(2),
            "{case:?} should be a usage error: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty(), "{case:?} printed to stdout");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn installing_the_skill_puts_it_where_a_harness_looks() {
    let dir = scratch("install");
    let run = || {
        output(
            text_graph()
                .current_dir(&dir)
                .args(["protocol", "--install"]),
        )
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let skill = dir.join(".claude/skills/text-graph/SKILL.md");
    let installed = std::fs::read_to_string(&skill).expect("installed skill");
    assert!(
        installed.starts_with("---\nname: text-graph\n"),
        "no frontmatter"
    );
    assert!(installed.contains("text-graph roster"));

    // harnesses without skills read AGENTS.md, which we create if absent
    let agents = std::fs::read_to_string(dir.join("AGENTS.md")).expect("pointer");
    assert!(agents.contains("text-graph protocol"), "{agents}");

    run();
    assert_eq!(
        std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
        agents,
        "installing twice must not stack blocks"
    );

    // --user follows the person, not the vault
    let home = scratch("install-home");
    let user = output(text_graph().current_dir(&dir).env("HOME", &home).args([
        "protocol",
        "--install",
        "--user",
    ]));
    assert!(
        user.status.success(),
        "{}",
        String::from_utf8_lossy(&user.stderr)
    );
    assert!(home.join(".claude/skills/text-graph/SKILL.md").is_file());
    assert!(
        !home.join("AGENTS.md").exists(),
        "a user install has no vault to leave a pointer in"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}
