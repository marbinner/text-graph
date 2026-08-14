//! Integration tests asserting the exact hand-counted numbers in
//! fixtures/EXPECTED.md. If you edit the fixture vault, re-count there and
//! update both in the same commit.

use std::path::PathBuf;

use text_graph::graph::{Graph, LinkKind, NodeId, NodeKind};
use text_graph::{graph, stats, vault};

fn build_fixture() -> Graph {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let scan = vault::scan(&root).expect("fixture vault must scan");
    graph::build(scan)
}

fn find(g: &Graph, path: &str) -> NodeId {
    g.nodes
        .iter()
        .position(|n| n.path == path)
        .map(|i| NodeId(i as u32))
        .unwrap_or_else(|| panic!("no node with path {path:?}"))
}

fn has_link(g: &Graph, from: &str, to: &str) -> bool {
    let (f, t) = (find(g, from), find(g, to));
    g.links
        .iter()
        .any(|l| l.from == f && l.to == t && l.kind == LinkKind::WikiLink)
}

#[test]
fn expected_counts() {
    let g = build_fixture();
    let s = stats::compute(&g);
    assert_eq!(s.files, 13, "file nodes");
    assert_eq!(s.dirs, 6, "dir nodes (assets pruned)");
    assert_eq!(s.ghosts, 2, "ghost nodes");
    assert_eq!(s.contains_edges, 18);
    assert_eq!(s.wiki_to_files, 16);
    assert_eq!(s.wiki_to_ghosts, 2);
    assert_eq!(s.warnings, 1, "scratch.md frontmatter");
    assert_eq!(s.errors, 0);
    assert_eq!(s.ambiguous, 1, "[[rust]]");
    assert_eq!(s.self_links_dropped, 1, "[[readme]] in readme.md");
}

#[test]
fn depth_histogram() {
    let g = build_fixture();
    let s = stats::compute(&g);
    let h: Vec<(usize, (usize, usize))> = s.depth_hist.into_iter().collect();
    assert_eq!(h, vec![(0, (1, 0)), (1, (4, 4)), (2, (1, 7)), (3, (0, 2))]);
}

#[test]
fn ambiguous_rust_resolves_to_lex_smallest_and_is_flagged() {
    let g = build_fixture();
    assert!(has_link(&g, "projects/rust-app.md", "languages/rust.md"));
    let a = &g.ambiguities[0];
    assert_eq!(a.target, "rust");
    assert_eq!(g.node(a.chosen).path, "languages/rust.md");
    assert_eq!(
        a.rejected.iter().map(|r| g.node(*r).path.as_str()).collect::<Vec<_>>(),
        ["topics/rust.md"]
    );
    // the explicit-path link still reaches the other one
    assert!(has_link(&g, "projects/rust-app.md", "topics/rust.md"));
}

#[test]
fn case_insensitive_and_unicode_resolution() {
    let g = build_fixture();
    assert!(has_link(&g, "index.md", "notes/readme.md")); // [[Readme]]
    assert!(has_link(&g, "notes/readme.md", "topics/grafér.md")); // [[grafér]]
}

#[test]
fn alias_heading_and_block_suffixes_resolve_to_files() {
    let g = build_fixture();
    assert!(has_link(&g, "projects/rust-app.md", "index.md")); // [[index#Heading One]]
    assert!(has_link(&g, "projects/rust-app.md", "notes/scratch.md")); // [[scratch#^abc123]]
    assert!(has_link(&g, "projects/rust-app.md", "projects/ideas.md")); // [[ideas|my ideas]]
}

#[test]
fn crlf_and_bom_files_extract_links() {
    let g = build_fixture();
    assert!(has_link(&g, "notes/daily/2026-08-14.md", "notes/daily/2026-08-13.md"));
    assert!(has_link(&g, "notes/daily/2026-08-14.md", "projects/rust-app.md"));
    assert!(has_link(&g, "bom.md", "empty.md"));
}

#[test]
fn garbage_frontmatter_warns_but_body_links_survive() {
    let g = build_fixture();
    assert!(has_link(&g, "notes/scratch.md", "index.md"));
    assert!(g.warnings.iter().any(|(p, _)| p == "notes/scratch.md"));
}

#[test]
fn alias_resolution_works_and_stem_beats_alias() {
    let g = build_fixture();
    // [[rustlang]] resolves via frontmatter aliases, not by filename
    assert!(has_link(&g, "notes/daily/2026-08-13.md", "languages/rust.md"));
    // languages/rust.md also carries alias "empty", but the stem empty.md
    // wins — silently, with no ambiguity recorded
    assert!(has_link(&g, "bom.md", "empty.md"));
    assert_eq!(g.ambiguities.len(), 1, "only [[rust]] is ambiguous");
}

#[test]
fn traps_embeds_and_skip_dirs_leave_no_trace() {
    let g = build_fixture();
    // code-fence / inline-code links would surface as ghost nodes if extracted
    assert!(!g.nodes.iter().any(|n| n.path.contains("trap")));
    // the embed target and the asset dir must not exist as nodes
    assert!(!g.nodes.iter().any(|n| n.path.contains("diagram")));
    assert!(!g.nodes.iter().any(|n| n.path == "assets"));
    // .trash canary: its [[index]] must not be counted anywhere
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.path.contains(".trash") || n.path.contains(".obsidian"))
    );
}

#[test]
fn ghosts_are_recorded_in_encounter_order() {
    let g = build_fixture();
    let ghost_paths: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Ghost)
        .map(|n| n.path.as_str())
        .collect();
    assert_eq!(ghost_paths, ["missing-note", "nonexistent/deep/ghost"]);
}

#[test]
fn deterministic_across_builds() {
    let a = build_fixture();
    let b = build_fixture();
    let paths = |g: &Graph| g.nodes.iter().map(|n| n.path.clone()).collect::<Vec<_>>();
    let links = |g: &Graph| g.links.iter().map(|l| (l.from, l.to)).collect::<Vec<_>>();
    let children = |g: &Graph| g.nodes.iter().map(|n| n.children.clone()).collect::<Vec<_>>();
    assert_eq!(paths(&a), paths(&b));
    assert_eq!(links(&a), links(&b));
    assert_eq!(children(&a), children(&b));
}

#[test]
fn radial_layout_places_every_tree_node_and_no_ghosts() {
    let g = build_fixture();
    let pos = text_graph::layout::radial(&g);
    for (i, node) in g.nodes.iter().enumerate() {
        match node.kind {
            NodeKind::Ghost => assert!(pos[i].is_none(), "ghost placed: {}", node.path),
            _ => {
                let p = pos[i].unwrap_or_else(|| panic!("unplaced node: {}", node.path));
                assert!(p.x.is_finite() && p.y.is_finite(), "non-finite: {}", node.path);
            }
        }
    }
    let r = pos[g.root.0 as usize].unwrap();
    assert_eq!((r.x, r.y), (0.0, 0.0), "root at origin");
}

#[test]
fn radial_layout_is_deterministic_and_siblings_are_distinct() {
    let g = build_fixture();
    let a = text_graph::layout::radial(&g);
    let b = text_graph::layout::radial(&g);
    assert_eq!(a, b);
    for node in &g.nodes {
        for (i, &c1) in node.children.iter().enumerate() {
            for &c2 in &node.children[i + 1..] {
                let (p1, p2) = (a[c1.0 as usize].unwrap(), a[c2.0 as usize].unwrap());
                let d = ((p1.x - p2.x).powi(2) + (p1.y - p2.y).powi(2)).sqrt();
                assert!(
                    d > 1.0,
                    "siblings overlap: {} vs {}",
                    g.node(c1).path,
                    g.node(c2).path
                );
            }
        }
    }
}

#[test]
fn frontmatter_titles_load() {
    let g = build_fixture();
    let idx = find(&g, "index.md");
    assert_eq!(g.node(idx).title.as_deref(), Some("Index"));
    let ideas = find(&g, "projects/ideas.md");
    assert_eq!(g.node(ideas).title, None); // deliberately has no frontmatter
}
