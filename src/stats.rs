//! Vault statistics: computed from the graph, rendered as text. The
//! integration test asserts these numbers against fixtures/EXPECTED.md.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::graph::{Graph, LinkKind, NodeId, NodeKind};

#[derive(Debug)]
pub struct Stats {
    pub files: usize,
    pub dirs: usize,
    pub images: usize,
    pub assets: usize,
    pub webs: usize,
    pub ghosts: usize,
    pub contains_edges: usize,
    pub wiki_to_files: usize,
    pub wiki_to_images: usize,
    pub wiki_to_assets: usize,
    pub wiki_to_ghosts: usize,
    pub external_edges: usize,
    pub warnings: usize,
    pub errors: usize,
    pub ambiguous: usize,
    pub self_links_dropped: usize,
    /// depth -> (dirs, files, images, assets); ghosts have no tree position
    /// and are excluded.
    pub depth_hist: BTreeMap<usize, (usize, usize, usize, usize)>,
}

pub fn compute(g: &Graph) -> Stats {
    let mut s = Stats {
        files: 0,
        dirs: 0,
        images: 0,
        assets: 0,
        webs: 0,
        ghosts: 0,
        contains_edges: 0,
        wiki_to_files: 0,
        wiki_to_images: 0,
        wiki_to_assets: 0,
        wiki_to_ghosts: 0,
        external_edges: 0,
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
            NodeKind::Image => s.images += 1,
            NodeKind::Asset => s.assets += 1,
            NodeKind::Web => s.webs += 1,
            NodeKind::Ghost => s.ghosts += 1,
        }
        if node.parent.is_some() {
            s.contains_edges += 1;
        }
        if !matches!(node.kind, NodeKind::Ghost | NodeKind::Web) {
            let d = g.depth(NodeId(idx as u32));
            let e = s.depth_hist.entry(d).or_default();
            match node.kind {
                NodeKind::Dir => e.0 += 1,
                NodeKind::File => e.1 += 1,
                NodeKind::Image => e.2 += 1,
                NodeKind::Asset => e.3 += 1,
                NodeKind::Ghost | NodeKind::Web => {}
            }
        }
    }
    for l in &g.links {
        match (l.kind, g.node(l.to).kind) {
            (LinkKind::WikiLink, NodeKind::Ghost) => s.wiki_to_ghosts += 1,
            (LinkKind::WikiLink, NodeKind::Image) => s.wiki_to_images += 1,
            (LinkKind::WikiLink, NodeKind::Asset) => s.wiki_to_assets += 1,
            (LinkKind::WikiLink, _) => s.wiki_to_files += 1,
            (LinkKind::External, _) => s.external_edges += 1,
        }
    }
    s
}

pub fn render(g: &Graph, s: &Stats) -> String {
    let mut out = String::new();
    let w = &mut out;
    let _ = writeln!(w, "vault: {}", g.node(g.root).name);
    // image segments appear only when a vault has images, so image-free
    // vaults render byte-identically to before images existed
    let img_nodes = if s.images > 0 {
        format!(" + {} image{}", s.images, plural(s.images))
    } else {
        String::new()
    };
    let asset_nodes = if s.assets > 0 {
        format!(" + {} asset{}", s.assets, plural(s.assets))
    } else {
        String::new()
    };
    let web_nodes = if s.webs > 0 {
        format!(" + {} web{}", s.webs, plural(s.webs))
    } else {
        String::new()
    };
    let _ = writeln!(
        w,
        "nodes: {} total = {} files + {} dirs{img_nodes}{asset_nodes}{web_nodes} + {} ghosts",
        s.files + s.dirs + s.images + s.assets + s.webs + s.ghosts,
        s.files,
        s.dirs,
        s.ghosts
    );
    let img_wiki = if s.wiki_to_images > 0 {
        format!("{} -> images, ", s.wiki_to_images)
    } else {
        String::new()
    };
    let asset_wiki = if s.wiki_to_assets > 0 {
        format!("{} -> assets, ", s.wiki_to_assets)
    } else {
        String::new()
    };
    let _ = writeln!(
        w,
        "edges: {} contains, {} wikilinks ({} -> files, {img_wiki}{asset_wiki}{} -> ghosts){}",
        s.contains_edges,
        s.wiki_to_files + s.wiki_to_images + s.wiki_to_assets + s.wiki_to_ghosts,
        s.wiki_to_files,
        s.wiki_to_ghosts,
        if s.external_edges > 0 {
            format!(", {} external", s.external_edges)
        } else {
            String::new()
        }
    );

    let depth_line: Vec<String> = s
        .depth_hist
        .iter()
        .map(|(d, (dirs, files, images, assets))| {
            let mut parts = Vec::new();
            if *dirs > 0 {
                parts.push(format!("{dirs} dir{}", plural(*dirs)));
            }
            if *files > 0 {
                parts.push(format!("{files} file{}", plural(*files)));
            }
            if *images > 0 {
                parts.push(format!("{images} image{}", plural(*images)));
            }
            if *assets > 0 {
                parts.push(format!("{assets} asset{}", plural(*assets)));
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
            (
                count,
                if n.path.is_empty() {
                    "<root>"
                } else {
                    n.path.as_str()
                },
            )
        })
        .filter(|(c, _)| *c > 0)
        .collect();
    dir_counts.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    if !dir_counts.is_empty() {
        let _ = writeln!(w, "largest dirs (direct md files):");
        for (count, path) in dir_counts.iter().take(6) {
            let _ = writeln!(w, "  {count:3}  {path}");
        }
    }

    if !g.ambiguities.is_empty() {
        let _ = writeln!(w, "ambiguous links ({}):", g.ambiguities.len());
        for a in &g.ambiguities {
            let rejected: Vec<&str> = a
                .rejected
                .iter()
                .map(|r| g.node(*r).path.as_str())
                .collect();
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
        for (path, _, message) in &g.warnings {
            let _ = writeln!(w, "  {path}: {message}");
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
