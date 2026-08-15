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
    assert_eq!(s.dirs, 8, "dir nodes (misc appears via its csv asset)");
    assert_eq!(s.images, 1, "image nodes");
    assert_eq!(s.assets, 1, "asset nodes (misc/data.csv)");
    assert_eq!(s.ghosts, 2, "ghost nodes");
    assert_eq!(s.contains_edges, 22);
    assert_eq!(s.wiki_to_files, 16);
    assert_eq!(s.wiki_to_images, 1, "[[diagram.png]] in index.md");
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
    let h: Vec<(usize, (usize, usize, usize, usize))> = s.depth_hist.into_iter().collect();
    assert_eq!(
        h,
        vec![
            (0, (1, 0, 0, 0)),
            (1, (6, 4, 0, 0)),
            (2, (1, 7, 1, 1)),
            (3, (0, 2, 0, 0))
        ]
    );
}

#[test]
fn ambiguous_rust_resolves_to_lex_smallest_and_is_flagged() {
    let g = build_fixture();
    assert!(has_link(&g, "projects/rust-app.md", "languages/rust.md"));
    let a = &g.ambiguities[0];
    assert_eq!(a.target, "rust");
    assert_eq!(g.node(a.chosen).path, "languages/rust.md");
    assert_eq!(
        a.rejected
            .iter()
            .map(|r| g.node(*r).path.as_str())
            .collect::<Vec<_>>(),
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
    assert!(has_link(
        &g,
        "notes/daily/2026-08-14.md",
        "notes/daily/2026-08-13.md"
    ));
    assert!(has_link(
        &g,
        "notes/daily/2026-08-14.md",
        "projects/rust-app.md"
    ));
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
    assert!(has_link(
        &g,
        "notes/daily/2026-08-13.md",
        "languages/rust.md"
    ));
    // languages/rust.md also carries alias "empty", but the stem empty.md
    // wins — silently, with no ambiguity recorded
    assert!(has_link(&g, "bom.md", "empty.md"));
    assert_eq!(g.ambiguities.len(), 1, "only [[rust]] is ambiguous");
}

#[test]
fn traps_embeds_and_skip_dirs_leave_no_trace() {
    let g = build_fixture();
    // code-fence / inline-code links and the md-resolvable embed would all
    // surface as ghost nodes if extraction regressed
    assert!(!g.nodes.iter().any(|n| n.path.contains("trap")));
    assert!(!g.nodes.iter().any(|n| n.path.contains("embedded-note")));
    // .trash canary: its [[index]] must not be counted anywhere
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.path.contains(".trash") || n.path.contains(".obsidian"))
    );
}

/// The markdown-preview preprocessing: wikilinks become tg:// links using
/// the graph's own resolution, image embeds become file:// images, note
/// embeds degrade, relative md links map to nodes, code stays untouched.
#[test]
fn mdview_prepare_renders_obsidian_flavor() {
    let g = build_fixture();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault");
    let idx = find(&g, "index.md");
    let body = vault::read_body(&root.join("index.md")).unwrap();
    let out = text_graph::mdview::prepare(&g, &root, idx, &body);

    let tg = |p: &str| format!("(tg://{})", find(&g, p).0);
    assert!(out.contains(&format!(
        "[projects/rust-app]{}",
        tg("projects/rust-app.md")
    )));
    assert!(out.contains(&format!("[Readme]{}", tg("notes/readme.md"))));
    // a plain wikilink to an image links to its Image node
    assert!(out.contains(&format!("[diagram.png]{}", tg("assets/diagram.png"))));
    // a ghost target links to the ghost node
    let ghost = g.by_ident("[[missing-note]]").expect("ghost");
    assert!(out.contains(&format!("[missing-note](tg://{})", ghost.0)));
    // the image embed becomes an inline file:// image
    assert!(out.contains("![](<file://"), "embed rewritten: {out}");
    assert!(out.contains("assets/diagram.png>)"));
    // an embed of a nonexistent note stays literal
    assert!(out.contains("![[embedded-note-trap]]"));
    // a relative markdown link to a vault note becomes a node link
    assert!(out.contains(&format!("[ideas]{}", tg("projects/ideas.md"))));
    // code traps stay byte-for-byte
    assert!(out.contains("[[trap-link]]"));
    assert!(out.contains("`[[inline-trap]]`"));
    // footnote-style citations link to the cited note (by path or name),
    // showing its display name; real footnotes stay untouched
    let readme = find(&g, "notes/readme.md");
    let cite = format!("[^{}](tg://{})", g.node(readme).display_name(), readme.0);
    assert_eq!(
        out.matches(&cite).count(),
        2,
        "both [^notes/readme.md] and [^readme] link: {out}"
    );
    assert!(out.contains("[^1] stays a footnote"));
    assert!(out.contains("[^1]: plain footnotes are untouched."));
}

#[test]
fn external_urls_are_metadata_and_subtree_stats_add_up() {
    let g = build_fixture();
    let ideas = find(&g, "projects/ideas.md");
    assert_eq!(
        g.node(ideas).externals,
        ["https://docs.rs/notify", "https://example.com/spec"],
        "md-link and bare URL extracted, trailing period trimmed"
    );
    // externals are never edges: ideas.md still has exactly its ghost link
    assert_eq!(g.outlinks(ideas).count(), 1);

    // the whole vault under the root
    let s = g.subtree_stats(g.root);
    assert_eq!(
        (s.files, s.dirs, s.images, s.assets),
        (13, 7, 1, 1),
        "recursive counts exclude the root itself"
    );
    assert_eq!(s.wiki_out, 19, "all wikilink edges originate in files");
    assert_eq!(s.external_out, 2);

    // a leaf dir
    let misc = g.subtree_stats(find(&g, "misc"));
    assert_eq!((misc.files, misc.assets, misc.wiki_out), (0, 1, 0));
}

/// Every other file type is an Asset node (with its dir chain), addressable
/// like an image — by full filename.
#[test]
fn other_files_are_asset_nodes() {
    let g = build_fixture();
    let csv = find(&g, "misc/data.csv");
    assert_eq!(g.node(csv).kind, NodeKind::Asset);
    assert_eq!(g.node(csv).name, "data.csv", "name keeps the extension");
    let misc = find(&g, "misc");
    assert_eq!(g.node(misc).kind, NodeKind::Dir);
    assert_eq!(g.node(csv).parent, Some(misc));
}

#[test]
fn image_becomes_a_node_and_its_link_resolves() {
    let g = build_fixture();
    let img = find(&g, "assets/diagram.png");
    assert_eq!(g.node(img).kind, NodeKind::Image);
    assert_eq!(g.node(img).name, "diagram.png", "name keeps the extension");
    // the image's dir chain exists and parents it
    let assets = find(&g, "assets");
    assert_eq!(g.node(assets).kind, NodeKind::Dir);
    assert_eq!(g.node(img).parent, Some(assets));
    // the non-embed [[diagram.png]] in index.md resolves to the Image node —
    // and must NOT also leave a ghost behind
    assert!(has_link(&g, "index.md", "assets/diagram.png"));
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Ghost && n.path.contains("diagram")),
        "resolved image target must not ghost"
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
    let children = |g: &Graph| {
        g.nodes
            .iter()
            .map(|n| n.children.clone())
            .collect::<Vec<_>>()
    };
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
                assert!(
                    p.x.is_finite() && p.y.is_finite(),
                    "non-finite: {}",
                    node.path
                );
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

/// Pins the rendered report's stable prefix (everything up to the warnings
/// section, whose YAML error text may vary across serde_yaml_ng versions).
#[test]
fn stats_render_snapshot() {
    let g = build_fixture();
    let s = stats::compute(&g);
    let text = stats::render(&g, &s);
    let expected = "\
vault: vault
nodes: 25 total = 13 files + 8 dirs + 1 image + 1 asset + 2 ghosts
edges: 22 contains, 19 wikilinks (16 -> files, 1 -> images, 2 -> ghosts)
depth: d0: 1 dir | d1: 6 dirs + 4 files | d2: 1 dir + 7 files + 1 image + 1 asset | d3: 2 files
largest dirs (direct md files):
    4  <root>
    2  notes
    2  notes/daily
    2  projects
    2  topics
    1  languages
ambiguous links (1):
  projects/rust-app.md: [[rust]] -> languages/rust.md  (not: topics/rust.md)
ghosts (2):
  [[missing-note]]  <- index.md
  [[nonexistent/deep/ghost]]  <- projects/ideas.md
self-links dropped: 1
warnings (1):
";
    assert!(
        text.starts_with(expected),
        "stats render drifted; got:\n{text}"
    );
}

/// The query layer: adjacency indexes, path lookup, preserved offsets.
#[test]
fn query_layer_backlinks_outlinks_paths_offsets() {
    let g = build_fixture();
    // languages/rust.md is linked from rust-app ([[rust]] ambiguous winner),
    // 2026-08-13 (alias [[rustlang]]), and topics/rust ([[languages/rust]])
    let rust = g.by_path("languages/rust.md").expect("by_path");
    let mut sources: Vec<&str> = g
        .backlinks(rust)
        .map(|l| g.node(l.from).path.as_str())
        .collect();
    sources.sort_unstable();
    assert_eq!(
        sources,
        [
            "notes/daily/2026-08-13.md",
            "projects/rust-app.md",
            "topics/rust.md"
        ]
    );
    // index.md links out to 3 files + 1 ghost + 1 image, in body order
    let index = g.by_path("index.md").unwrap();
    let outs: Vec<&str> = g
        .outlinks(index)
        .map(|l| g.node(l.to).path.as_str())
        .collect();
    assert_eq!(
        outs,
        [
            "projects/rust-app.md",
            "notes/readme.md",
            "notes/daily/2026-08-14.md",
            "missing-note",
            "assets/diagram.png"
        ]
    );
    // offsets index into the BODY (read_body strips BOM + frontmatter),
    // which is exactly what the preview pane renders
    let body = vault::read_body(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/vault/index.md"),
    )
    .unwrap();
    for l in g.outlinks(index) {
        assert_eq!(
            &body[l.offset..l.offset + 2],
            "[[",
            "offset {} is not a link",
            l.offset
        );
    }
    // ghosts are reachable by ident, not by bare path
    assert!(g.by_ident("[[missing-note]]").is_some());
    assert_eq!(g.by_path(""), Some(g.root));
}

#[test]
fn frontmatter_titles_load() {
    let g = build_fixture();
    let idx = find(&g, "index.md");
    assert_eq!(g.node(idx).title.as_deref(), Some("Index"));
    let ideas = find(&g, "projects/ideas.md");
    assert_eq!(g.node(ideas).title, None); // deliberately has no frontmatter
}
