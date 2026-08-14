//! Debug harness: drive SessionMirror exactly like the failing test.
//! Usage: cargo run --example tmux_debug <socket> <session>

use std::time::{Duration, Instant};

use text_graph::keys::{self, Mods, Special};
use text_graph::mirror::SessionMirror;

fn main() {
    let socket = std::env::args().nth(1).expect("socket");
    let session = std::env::args().nth(2).expect("session");
    let mut m = SessionMirror::attach(&session, Some(&socket), None, || {}).expect("attach");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut pane = None;
    while Instant::now() < deadline && pane.is_none() {
        m.pump();
        pane = m.grids().first().map(|(p, _)| p.clone());
        std::thread::sleep(Duration::from_millis(50));
    }
    let pane = pane.expect("pane discovered");
    eprintln!("pane = {pane:?}");

    let c1 = keys::hex_cmd(&pane, b"hello graph");
    let c2 = keys::special_cmd(&pane, Special::Enter, Mods::default());
    eprintln!("sending: {c1:?}");
    eprintln!("sending: {c2:?}");
    m.command(&c1);
    m.command(&c2);

    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if m.pump() {
            let grids = m.grids();
            if let Some((_, g)) = grids.first() {
                let row0: String = g.cells[..g.cols as usize].iter().map(|c| c.ch).collect();
                eprintln!("gen {} row0: {:?}", m.generation(), row0.trim_end());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
