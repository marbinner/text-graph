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
