use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use text_graph::{comm, graph, stats, vault};

#[cfg(feature = "gui")]
mod app;

/// The subcommands agents run from inside their own tmux pane. Listed in
/// both builds: messaging needs no window, and the GUI-free binary is
/// exactly what a headless box would install.
const COMM_USAGE: &str = "\
  text-graph roster               who else is live in this vault
  text-graph peek <agent> [-n N]  the last N lines of an agent's screen
  text-graph protocol             how agents talk here
";

#[cfg(feature = "gui")]
const USAGE_HEAD: &str = "\
text-graph — markdown vault graph viewer

usage:
  text-graph <vault-path>         open the graph window
  text-graph stats <vault-path>   headless vault statistics
";

#[cfg(not(feature = "gui"))]
const USAGE_HEAD: &str = "\
text-graph — markdown vault graph tools

usage:
  text-graph stats <vault-path>   headless vault statistics
";

const USAGE_TAIL: &str = "  text-graph --help | --version\n";

fn usage() -> String {
    format!("{USAGE_HEAD}{COMM_USAGE}{USAGE_TAIL}")
}

/// Why a subcommand stopped, which is also its exit code: a malformed
/// command line is the caller's mistake (2), anything else is a failure to
/// carry out a well-formed request (1).
enum CliError {
    Usage(String),
    Failed(String),
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let is = |arg: &OsString, name: &str| arg.as_os_str() == OsStr::new(name);
    match args.as_slice() {
        [flag] if is(flag, "--help") || is(flag, "-h") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        [flag] if is(flag, "--version") || is(flag, "-V") => {
            println!("text-graph {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [command, p] if is(command, "stats") => match run_stats(Path::new(p)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
        [command, rest @ ..] if is(command, "roster") => report(run_roster(rest)),
        [command, rest @ ..] if is(command, "peek") => report(run_peek(rest)),
        [command] if is(command, "protocol") => {
            print!("{}", comm::PROTOCOL);
            ExitCode::SUCCESS
        }
        #[cfg(feature = "gui")]
        [p] if !p.to_string_lossy().starts_with('-') => app::run(Path::new(p)),
        _ => {
            eprint!("{}", usage());
            ExitCode::from(2)
        }
    }
}

fn report(result: Result<(), CliError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(message)) => {
            eprintln!("usage: {message}");
            ExitCode::from(2)
        }
        Err(CliError::Failed(message)) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_stats(path: &Path) -> anyhow::Result<()> {
    let t0 = std::time::Instant::now();
    let scan = vault::scan(path)?;
    let g = graph::build(scan);
    let s = stats::compute(&g);
    print!("{}", stats::render(&g, &s));
    eprintln!("({} nodes in {:.1?})", g.nodes.len(), t0.elapsed());
    Ok(())
}

/// Flags shared by the messaging subcommands, hand-parsed like the rest of
/// this CLI. `-L` picks a private tmux server, which is how the integration
/// test stays off the user's own.
struct CommArgs {
    positional: Vec<String>,
    socket: Option<String>,
    lines: Option<usize>,
}

fn parse_comm(args: &[OsString]) -> Result<CommArgs, CliError> {
    let mut out = CommArgs {
        positional: Vec::new(),
        socket: None,
        lines: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy().into_owned();
        let mut value = |flag: &str| {
            args.next()
                .map(|v| v.to_string_lossy().into_owned())
                .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))
        };
        match arg.as_str() {
            "-L" => out.socket = Some(value("-L")?),
            "-n" => {
                let raw = value("-n")?;
                let lines: usize = raw
                    .parse()
                    .map_err(|_| CliError::Usage(format!("-n takes a line count, not {raw:?}")))?;
                if lines == 0 {
                    return Err(CliError::Usage("-n must be at least 1".into()));
                }
                out.lines = Some(lines);
            }
            // "-" is a value (send reads the message from stdin), not a flag
            flag if flag.starts_with('-') && flag != "-" => {
                return Err(CliError::Usage(format!("unknown flag {flag}")));
            }
            _ => out.positional.push(arg),
        }
    }
    Ok(out)
}

/// The vault the calling agent is speaking from — its cwd's root. Never an
/// argument: an agent already stands inside the vault, and asking it to
/// name one invites naming the wrong one.
fn here() -> Result<std::path::PathBuf, CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Failed(format!("cannot read the working directory: {e}")))?;
    Ok(comm::vault_root(&cwd))
}

fn run_roster(args: &[OsString]) -> Result<(), CliError> {
    let args = parse_comm(args)?;
    if !args.positional.is_empty() {
        return Err(CliError::Usage("roster takes no arguments".into()));
    }
    let vault = here()?;
    let socket = args.socket.as_deref();
    let mut entries = comm::live(socket, &vault).map_err(CliError::Failed)?;
    if entries.is_empty() {
        println!("no live agents in {}", vault.display());
        return Ok(());
    }
    comm::fill_tails(socket, &mut entries);
    let self_pane = std::env::var("TMUX_PANE").ok();
    print!(
        "{}",
        comm::render_roster(&entries, comm::epoch_now(), self_pane.as_deref())
    );
    Ok(())
}

fn run_peek(args: &[OsString]) -> Result<(), CliError> {
    let args = parse_comm(args)?;
    let [target] = args.positional.as_slice() else {
        return Err(CliError::Usage("peek <agent> [-n lines]".into()));
    };
    let vault = here()?;
    let socket = args.socket.as_deref();
    let entries = comm::live(socket, &vault).map_err(CliError::Failed)?;
    let entry = comm::resolve(&entries, target).map_err(|e| CliError::Failed(e.to_string()))?;
    let lines = args.lines.unwrap_or(comm::PEEK_DEFAULT);
    let captured = comm::capture(socket, &entry.pane, lines).map_err(CliError::Failed)?;
    // a header, because a screen dump out of context reads as your own
    // terminal's output
    let quiet = match entry.idle(comm::epoch_now()) {
        Some(secs) => format!("quiet {secs}s"),
        None => "quiet unknown".to_string(),
    };
    println!(
        "# {} ({}) — {}, at {}",
        entry.session, entry.agent, quiet, entry.place
    );
    for line in captured {
        println!("{line}");
    }
    Ok(())
}
