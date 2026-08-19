//! Ad-hoc probe: what would discovery + mirrors show for a vault?
use std::time::{Duration, Instant};

fn main() {
    let vault = std::path::PathBuf::from(std::env::args().nth(1).expect("vault path"));
    // optional second arg: a private `-L` socket, so the probe can inspect
    // a scratch server instead of the user's own
    let socket = std::env::args().nth(2);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let allow = text_graph::agents::default_allowlist();
    let mut tr = text_graph::agents::Tracker::new();
    let panes = match text_graph::agents::scan(socket.as_deref(), &vault) {
        Ok(p) => p,
        Err(e) => {
            println!("scan FAILED (server present but list-panes errored): {e}");
            return;
        }
    };
    println!("scan: {} panes with cwd in vault", panes.len());
    for p in &panes {
        let idle = match p.activity {
            Some(t) => format!("{}s", now.saturating_sub(t)),
            None => "?".to_string(),
        };
        println!(
            "  {} {} pid={} cmd={} idle={} cwd={}",
            p.session,
            p.pane,
            p.pid,
            p.command,
            idle,
            p.cwd.display()
        );
    }
    let active = tr.update(&panes, &allow, Instant::now());
    println!("active agents: {}", active.len());
    for a in &active {
        println!(
            "  {} {} agent={} ours={}",
            a.session, a.pane, a.agent, a.ours
        );
    }
    if let Some(a) = active.first() {
        let mut m = text_graph::mirror::SessionMirror::attach(&a.session, None, None, || {})
            .expect("attach");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            m.pump();
            std::thread::sleep(Duration::from_millis(100));
        }
        let grids = m.grids();
        println!(
            "mirror '{}': {} grids, exited={}",
            a.session,
            grids.len(),
            m.exited
        );
        for (id, g) in &grids {
            let row0: String = g.cells[..g.cols as usize]
                .iter()
                .map(|c| c.text.as_str())
                .collect();
            println!("  {id} {}x{} row0={:?}", g.cols, g.rows, row0.trim_end());
        }
    }
}
