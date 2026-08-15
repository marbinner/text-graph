//! Obsidian-style wikilink resolution.
//!
//! Rules: casefolded matching; a bare name matches files by stem first, then
//! by frontmatter `aliases:`; a target containing `/` matches by
//! path-component suffix (so any unambiguous suffix of a path works).
//! Ambiguity resolves to the first candidate in sorted path order and is
//! flagged. Unresolved targets become Ghost nodes.
//! Self-links resolve and are then dropped. Duplicate (from, to) edges are
//! deduplicated.

use std::collections::{HashMap, HashSet};

use crate::graph::{Ambiguity, Graph, Link, LinkKind, Node, NodeId, NodeKind};
use crate::vault::RawLink;

/// Extensions treated as assets: UNRESOLVED links to these are skipped
/// rather than ghosted. Images that exist in the vault are Image nodes and
/// resolve normally (by full filename or path suffix) before this list is
/// consulted. An allowlist, not "any extension" — note names like `v1.2`
/// must not be mistaken for assets.
const ASSET_EXTS: &[&str] = &[
    "png",
    "jpg",
    "jpeg",
    "gif",
    "svg",
    "webp",
    "bmp",
    "ico",
    "pdf",
    "mp3",
    "wav",
    "ogg",
    "m4a",
    "mp4",
    "mov",
    "avi",
    "mkv",
    "zip",
    "excalidraw",
    "canvas",
];

pub fn resolve(g: &mut Graph, file_links: &[(NodeId, Vec<RawLink>)]) {
    // Index all File, Image, and Asset nodes. Leaves are indexed in NodeId
    // order, which is sorted relative-path (string) order, so the first
    // candidate in any bucket is the deterministic ambiguity winner. Image
    // and Asset names keep their extension, so `[[pic.png]]`/`[[data.csv]]`
    // hit by_stem while `[[pic]]` never matches either.
    let mut by_stem: HashMap<String, Vec<NodeId>> = HashMap::new();
    let mut by_alias: HashMap<String, Vec<NodeId>> = HashMap::new();
    let mut comp_paths: Vec<(NodeId, Vec<String>)> = Vec::new();
    for (idx, node) in g.nodes.iter().enumerate() {
        if !matches!(
            node.kind,
            NodeKind::File | NodeKind::Image | NodeKind::Asset
        ) {
            continue;
        }
        let id = NodeId(idx as u32);
        by_stem.entry(casefold(&node.name)).or_default().push(id);
        for alias in &node.aliases {
            let bucket = by_alias.entry(casefold(alias)).or_default();
            // dedupe: repeated or case-variant aliases on one file must not
            // make a link to it look ambiguous
            if !bucket.contains(&id) {
                bucket.push(id);
            }
        }
        comp_paths.push((id, strip_md(&node.path).split('/').map(casefold).collect()));
    }

    let mut ghosts: HashMap<String, NodeId> = HashMap::new();
    let mut seen: HashSet<(NodeId, NodeId)> = HashSet::new();

    for (src, links) in file_links {
        for link in links {
            let target = normalize(&link.target);
            if target.is_empty() {
                continue;
            }

            let candidates: Vec<NodeId> = if target.contains('/') {
                let want: Vec<String> = target.split('/').map(casefold).collect();
                comp_paths
                    .iter()
                    .filter(|(_, comps)| {
                        comps.len() >= want.len() && comps[comps.len() - want.len()..] == want[..]
                    })
                    .map(|(id, _)| *id)
                    .collect()
            } else {
                let key = casefold(&target);
                // filename (stem) matches take precedence over alias matches
                match by_stem.get(&key) {
                    Some(v) => v.clone(),
                    None => by_alias.get(&key).cloned().unwrap_or_default(),
                }
            };

            let to = match candidates.len() {
                0 => {
                    // The asset skip applies only to UNRESOLVED targets — a
                    // real note named pic.png.md (or an Obsidian-Excalidraw
                    // Drawing.excalidraw.md) stays linkable by its stem.
                    if is_asset(&target) {
                        continue;
                    }
                    *ghosts.entry(casefold(&target)).or_insert_with(|| {
                        g.push_node(Node {
                            kind: NodeKind::Ghost,
                            path: target.clone(),
                            name: target.clone(),
                            title: None,
                            aliases: Vec::new(),
                            externals: Vec::new(),
                            parent: None,
                            children: Vec::new(),
                        })
                    })
                }
                1 => candidates[0],
                _ => {
                    let chosen = candidates[0];
                    g.ambiguities.push(Ambiguity {
                        source: *src,
                        target: target.clone(),
                        chosen,
                        rejected: candidates[1..].to_vec(),
                    });
                    chosen
                }
            };

            if to == *src {
                g.self_links_dropped += 1;
                continue;
            }
            if seen.insert((*src, to)) {
                g.links.push(Link {
                    from: *src,
                    to,
                    kind: LinkKind::WikiLink,
                    offset: link.offset,
                });
            }
        }
    }
}

fn casefold(s: &str) -> String {
    // NFC first: macOS-created vaults store filenames in NFD while link text
    // is typically NFC — without normalization such notes are unlinkable.
    use unicode_normalization::UnicodeNormalization as _;
    s.nfc().collect::<String>().to_lowercase()
}

fn strip_md(path: &str) -> &str {
    // Byte-boundary-safe: get() returns None when len-3 would split a
    // multibyte char (targets like `мир`, `éé`, or an emoji), where direct
    // slicing panics — and this runs on every link target in the vault.
    match path.len().checked_sub(3).and_then(|i| path.get(i..)) {
        Some(tail) if path.len() > 3 && tail.eq_ignore_ascii_case(".md") => &path[..path.len() - 3],
        _ => path,
    }
}

/// Normalize a raw wikilink target: trimmed, forward slashes, no leading or
/// trailing `/`, `.md` suffix removed.
fn normalize(target: &str) -> String {
    let t = target.trim().replace('\\', "/");
    strip_md(t.trim_matches('/')).to_string()
}

fn is_asset(target: &str) -> bool {
    let last = target.rsplit_once('/').map_or(target, |(_, f)| f);
    match last.rsplit_once('.') {
        Some((_, ext)) => ASSET_EXTS.iter().any(|a| ext.eq_ignore_ascii_case(a)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_md_and_slashes() {
        assert_eq!(normalize(" Note.md "), "Note");
        assert_eq!(normalize("dir\\sub\\note"), "dir/sub/note");
        assert_eq!(normalize("/notes/x/"), "notes/x");
    }

    #[test]
    fn normalize_survives_multibyte_targets() {
        // regression: these used to panic on a byte-slice off a char boundary
        assert_eq!(normalize("мир"), "мир");
        assert_eq!(normalize("éé"), "éé");
        assert_eq!(normalize("💥"), "💥");
        assert_eq!(normalize("日a"), "日a");
        assert_eq!(normalize("тест.md"), "тест");
        assert_eq!(normalize("ab"), "ab");
    }

    #[test]
    fn asset_allowlist() {
        assert!(is_asset("diagram.png"));
        assert!(is_asset("dir/pic.JPG"));
        assert!(!is_asset("v1.2"));
        assert!(!is_asset("2026-08-14"));
        assert!(!is_asset("plain"));
    }

    #[test]
    fn casefold_bridges_nfc_and_nfd() {
        assert_eq!(casefold("grafe\u{301}r"), casefold("graf\u{e9}r"));
    }

    #[test]
    fn asset_skip_applies_only_to_unresolved_targets() {
        use crate::graph::{Graph, Node, NodeId, NodeKind};
        use crate::vault::RawLink;
        let node = |kind, path: &str, name: &str, parent| Node {
            kind,
            path: path.into(),
            name: name.into(),
            title: None,
            aliases: Vec::new(),
            externals: Vec::new(),
            parent,
            children: Vec::new(),
        };
        let mut g = Graph::empty();
        g.nodes = vec![
            node(NodeKind::Dir, "", "r", None),
            node(NodeKind::File, "pic.png.md", "pic.png", Some(NodeId(0))),
            node(NodeKind::File, "b.md", "b", Some(NodeId(0))),
        ];
        let links = vec![(
            NodeId(2),
            vec![
                RawLink {
                    target: "pic.png".into(),
                    offset: 0,
                },
                RawLink {
                    target: "missing.png".into(),
                    offset: 1,
                },
            ],
        )];
        resolve(&mut g, &links);
        assert_eq!(
            g.links.len(),
            1,
            "a note with an asset-like stem is linkable"
        );
        assert_eq!(g.links[0].to, NodeId(1));
        assert_eq!(
            g.nodes.len(),
            3,
            "an unresolved asset target must not ghost"
        );
    }

    #[test]
    fn duplicate_aliases_on_one_file_do_not_fake_ambiguity() {
        use crate::graph::{Graph, Node, NodeId, NodeKind};
        use crate::vault::RawLink;
        let node = |kind, path: &str, name: &str, aliases: Vec<String>, parent| Node {
            kind,
            path: path.into(),
            name: name.into(),
            title: None,
            aliases,
            externals: Vec::new(),
            parent,
            children: Vec::new(),
        };
        let mut g = Graph::empty();
        g.nodes = vec![
            node(NodeKind::Dir, "", "r", vec![], None),
            node(
                NodeKind::File,
                "a.md",
                "a",
                vec!["Same".into(), "same".into()],
                Some(NodeId(0)),
            ),
            node(NodeKind::File, "b.md", "b", vec![], Some(NodeId(0))),
        ];
        let links = vec![(
            NodeId(2),
            vec![RawLink {
                target: "same".into(),
                offset: 0,
            }],
        )];
        resolve(&mut g, &links);
        assert_eq!(g.links.len(), 1);
        assert!(
            g.ambiguities.is_empty(),
            "case-variant duplicate aliases flagged ambiguity"
        );
    }
}
