use std::path::Path;
use std::process::ExitCode;

use text_graph::{graph, stats, vault};

mod app;

const USAGE: &str = "\
text-graph — markdown vault graph viewer

usage:
  text-graph <vault-path>         open the graph window
  text-graph stats <vault-path>   headless vault statistics
  text-graph --help | --version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    match strs[..] {
        ["--help" | "-h"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["--version" | "-V"] => {
            println!("text-graph {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["stats", p] => match run_stats(Path::new(p)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
        [p] if !p.starts_with('-') => app::run(Path::new(p)),
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
