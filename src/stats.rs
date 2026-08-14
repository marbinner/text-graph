//! Vault statistics: computed from the graph, rendered as text. The
//! integration test asserts these numbers against fixtures/EXPECTED.md.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::graph::{Graph, LinkKind, NodeId, NodeKind};

#[derive(Debug)]
pub struct Stats {
    pub files: usize,
    pub dirs: usize,
    pub ghosts: usize,
    pub contains_edges: usize,
    pub wiki_to_files: usize,
    pub wiki_to_ghosts: usize,
    pub warnings: usize,
    pub errors: usize,
    pub ambiguous: usize,
    pub self_links_dropped: usize,
    /// depth -> (dirs, files); ghosts have no tree position and are excluded.
    pub depth_hist: BTreeMap<usize, (usize, usize)>,
}

pub fn compute(g: &Graph) -> Stats {
    let mut s = Stats {
        files: 0,
        dirs: 0,
        ghosts: 0,
        contains_edges: 0,
        wiki_to_files: 0,
        wiki_to_ghosts: 0,
        warnings: g.warnings.len(),
        errors: g.errors.len(),
        ambiguous: g.ambiguities.len(),
        self_links_dropped: g.self_links_dropped,
        depth_hist: BTreeMap::new(),
    };
    for (idx, node) in g.nodes.iter().enumerate() {
        match node.kind {
            NodeKind::File => s.files += 1,
            NodeKind::Dir => s.dirs += 1,
            NodeKind::Ghost => s.ghosts += 1,
        }
        if node.parent.is_some() {
            s.contains_edges += 1;
        }
        if node.kind != NodeKind::Ghost {
            let d = g.depth(NodeId(idx as u32));
            let e = s.depth_hist.entry(d).or_default();
            match node.kind {
                NodeKind::Dir => e.0 += 1,
                NodeKind::File => e.1 += 1,
                NodeKind::Ghost => {}
            }
        }
    }
    for l in &g.links {
        match (l.kind, g.node(l.to).kind) {
            (LinkKind::WikiLink, NodeKind::Ghost) => s.wiki_to_ghosts += 1,
            (LinkKind::WikiLink, _) => s.wiki_to_files += 1,
        }
    }
    s
}

pub fn render(g: &Graph, s: &Stats) -> String {
    let mut out = String::new();
    let w = &mut out;
    let _ = writeln!(w, "vault: {}", g.node(g.root).name);
    let _ = writeln!(
        w,
        "nodes: {} total = {} files + {} dirs + {} ghosts",
        s.files + s.dirs + s.ghosts,
        s.files,
        s.dirs,
        s.ghosts
    );
    let _ = writeln!(
        w,
        "edges: {} contains, {} wikilinks ({} -> files, {} -> ghosts)",
        s.contains_edges,
        s.wiki_to_files + s.wiki_to_ghosts,
        s.wiki_to_files,
        s.wiki_to_ghosts
    );

    let depth_line: Vec<String> = s
        .depth_hist
        .iter()
        .map(|(d, (dirs, files))| {
            let mut parts = Vec::new();
            if *dirs > 0 {
                parts.push(format!("{dirs} dir{}", plural(*dirs)));
            }
            if *files > 0 {
                parts.push(format!("{files} file{}", plural(*files)));
            }
            format!("d{d}: {}", parts.join(" + "))
        })
        .collect();
    let _ = writeln!(w, "depth: {}", depth_line.join(" | "));

    let mut dir_counts: Vec<(usize, &str)> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Dir)
        .map(|n| {
            let count = n
                .children
                .iter()
                .filter(|c| g.node(**c).kind == NodeKind::File)
                .count();
            (count, if n.path.is_empty() { "<root>" } else { n.path.as_str() })
        })
        .filter(|(c, _)| *c > 0)
        .collect();
    dir_counts.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let _ = writeln!(w, "largest dirs (direct md files):");
    for (count, path) in dir_counts.iter().take(6) {
        let _ = writeln!(w, "  {count:3}  {path}");
    }

    if !g.ambiguities.is_empty() {
        let _ = writeln!(w, "ambiguous links ({}):", g.ambiguities.len());
        for a in &g.ambiguities {
            let rejected: Vec<&str> =
                a.rejected.iter().map(|r| g.node(*r).path.as_str()).collect();
            let _ = writeln!(
                w,
                "  {}: [[{}]] -> {}  (not: {})",
                g.node(a.source).path,
                a.target,
                g.node(a.chosen).path,
                rejected.join(", ")
            );
        }
    }

    let ghost_ids: Vec<NodeId> = g
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == NodeKind::Ghost)
        .map(|(i, _)| NodeId(i as u32))
        .collect();
    if !ghost_ids.is_empty() {
        let _ = writeln!(w, "ghosts ({}):", ghost_ids.len());
        for gid in ghost_ids {
            let sources: Vec<&str> = g
                .links
                .iter()
                .filter(|l| l.to == gid)
                .map(|l| g.node(l.from).path.as_str())
                .collect();
            let _ = writeln!(w, "  [[{}]]  <- {}", g.node(gid).path, sources.join(", "));
        }
    }

    if s.self_links_dropped > 0 {
        let _ = writeln!(w, "self-links dropped: {}", s.self_links_dropped);
    }
    if !g.warnings.is_empty() {
        let _ = writeln!(w, "warnings ({}):", g.warnings.len());
        for (path, msg) in &g.warnings {
            let _ = writeln!(w, "  {path}: {msg}");
        }
    }
    if !g.errors.is_empty() {
        let _ = writeln!(w, "errors ({}):", g.errors.len());
        for (path, msg) in &g.errors {
            let _ = writeln!(w, "  {path}: {msg}");
        }
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
