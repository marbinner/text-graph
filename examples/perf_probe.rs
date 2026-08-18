//! Headless performance probe: times the pipeline stages the viewer's
//! responsiveness depends on, against any vault.
//!
//!     cargo run --release --example perf_probe <vault> [content-query]
//!
//! Big synthetic vaults come from `fixtures/gen-stress.sh N` (writes
//! fixtures/stress-vault/flat, gitignored). This covers the headless
//! side — scan, build, the reload ident map, sim seed + settle, and a
//! full content scan; the ⚙ "frame statistics" overlay covers the paint
//! side in the running viewer. "per frame" under settle is what the sim
//! adds to every frame while the layout is still moving (3 ticks).

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use text_graph::config::Config;
use text_graph::graph::NodeKind;
use text_graph::search::{self, Query, ScanFile};
use text_graph::sim::Sim;
use text_graph::{filetype, graph, vault};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: perf_probe <vault> [content-query]");
        eprintln!("       (fixtures/gen-stress.sh N generates a big flat one)");
        std::process::exit(2);
    };
    let query = args.next().unwrap_or_else(|| "the".to_string());

    let t = Instant::now();
    let scan = match vault::scan(Path::new(&root)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    };
    let scan_ms = ms(t);
    let root_path = scan.root.clone();
    let raw_files = scan.files.len();

    let t = Instant::now();
    let g = graph::build(scan);
    let build_ms = ms(t);

    // the carry-over side of a reload rebuilds ident → node maps; this is
    // that cost, isolated
    let t = Instant::now();
    let idents: HashMap<String, u32> = g
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.ident(), i as u32))
        .collect();
    let ident_ms = ms(t);

    let cfg = Config::default();
    let t = Instant::now();
    let mut sim = Sim::new(&g);
    sim.configure(cfg.spread, false);
    let seed_ms = ms(t);

    let t = Instant::now();
    let mut ticks = 0usize;
    while sim.active() && ticks < 30_000 {
        sim.tick(3);
        ticks += 3;
    }
    let settle_ms = ms(t);
    let frames = (ticks / 3).max(1);

    let files: Vec<ScanFile> = g
        .nodes
        .iter()
        .filter(|n| match n.kind {
            NodeKind::File => true,
            NodeKind::Asset => filetype::is_text(&n.path),
            _ => false,
        })
        .filter_map(|n| {
            Some(ScanFile {
                key: n.path_key(),
                path: n.os_path.clone()?,
            })
        })
        .collect();
    let q = Query::parse(&query);
    let t = Instant::now();
    let mut hit_files = 0usize;
    let mut hit_lines = 0usize;
    let outcome = search::scan_files(
        &root_path,
        &q,
        &files,
        search::MAX_FILE_BYTES,
        &|| false,
        &mut |batch| {
            hit_files += batch.len();
            hit_lines += batch.iter().map(|f| f.total).sum::<usize>();
        },
    );
    let content_ms = ms(t);

    println!("vault      {} ({raw_files} files walked)", root_path.display());
    println!(
        "scan     {scan_ms:>9.2} ms   ({} unreadable, {} warnings)",
        g.errors.len(),
        g.warnings.len()
    );
    println!(
        "build    {build_ms:>9.2} ms   ({} nodes, {} links)",
        g.nodes.len(),
        g.links.len()
    );
    println!("ident map{ident_ms:>9.2} ms   ({} idents — reload carry-over proxy)", idents.len());
    println!("sim seed {seed_ms:>9.2} ms");
    println!(
        "settle   {settle_ms:>9.2} ms   ({ticks} ticks · {:.3} ms per frame while settling{})",
        settle_ms / frames as f64,
        if ticks >= 30_000 { " · CAPPED" } else { "" }
    );
    println!(
        "content  {content_ms:>9.2} ms   ({query:?} → {hit_lines} lines in {hit_files} of {} files{}{})",
        outcome.files_read,
        if outcome.truncated { " · truncated" } else { "" },
        if outcome.cancelled { " · cancelled" } else { "" },
    );
}
