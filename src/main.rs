use std::path::Path;
use std::process::ExitCode;

use text_graph::{graph, stats, vault};

const USAGE: &str = "\
text-graph — markdown vault graph viewer

usage:
  text-graph stats <vault-path>   headless vault statistics
  text-graph <vault-path>         (GUI arrives in milestone B; runs stats)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    let path = match strs[..] {
        ["stats", p] => p,
        [p] if !p.starts_with('-') => {
            eprintln!("(GUI arrives in milestone B — showing stats)\n");
            p
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run_stats(Path::new(path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
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
