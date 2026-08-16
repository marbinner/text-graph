use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;

use text_graph::{graph, stats, vault};

#[cfg(feature = "gui")]
mod app;

#[cfg(feature = "gui")]
const USAGE: &str = "\
text-graph — markdown vault graph viewer

usage:
  text-graph <vault-path>         open the graph window
  text-graph stats <vault-path>   headless vault statistics
  text-graph --help | --version
";

#[cfg(not(feature = "gui"))]
const USAGE: &str = "\
text-graph — markdown vault graph tools

usage:
  text-graph stats <vault-path>   headless vault statistics
  text-graph --help | --version
";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [flag]
            if flag.as_os_str() == OsStr::new("--help") || flag.as_os_str() == OsStr::new("-h") =>
        {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        [flag]
            if flag.as_os_str() == OsStr::new("--version")
                || flag.as_os_str() == OsStr::new("-V") =>
        {
            println!("text-graph {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [command, p] if command.as_os_str() == OsStr::new("stats") => match run_stats(Path::new(p))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
        #[cfg(feature = "gui")]
        [p] if !p.to_string_lossy().starts_with('-') => app::run(Path::new(p)),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
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
