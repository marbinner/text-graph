//! The in-memory graph: an arena of nodes with a Contains tree (from the
//! filesystem — a real tree by construction) plus typed overlay links.

use std::collections::HashMap;

use crate::resolve;
use crate::vault::{RawLink, VaultScan};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Dir,
    File,
    /// A wikilink target that doesn't exist (yet).
    Ghost,
}

#[derive(Debug)]
pub struct Node {
    pub kind: NodeKind,
    /// Files/dirs: path relative to the vault root ("" for the root itself).
    /// Ghosts: the normalized target text as written.
    pub path: String,
    /// Display name: file stem, dir name, or ghost target.
    pub name: String,
    /// `title:` from frontmatter, files only.
    pub title: Option<String>,
    /// `aliases:` from frontmatter, files only — alternate link names.
    pub aliases: Vec<String>,
    /// Contains-parent. None for the root and for ghosts.
    pub parent: Option<NodeId>,
    /// Sorted: dirs first, then by name.
    pub children: Vec<NodeId>,
}

impl Node {
    /// Human-facing label: explicit title, else first alias, else stem/name.
    /// (Vaults with timestamped filenames typically carry the human name in
    /// `aliases:` — labels must not show raw stems there.)
    pub fn display_name(&self) -> &str {
        self.title
            .as_deref()
            .or_else(|| self.aliases.first().map(String::as_str))
            .unwrap_or(&self.name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkKind {
    WikiLink,
    // later: MdLink, Tag, Embed, FrontmatterParent
}

#[derive(Debug)]
pub struct Link {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: LinkKind,
}

/// A basename link that matched several files; resolution picked the
/// lexicographically smallest path and recorded the rest.
#[derive(Debug)]
pub struct Ambiguity {
    pub source: NodeId,
    pub target: String,
    pub chosen: NodeId,
    pub rejected: Vec<NodeId>,
}

#[derive(Debug)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub root: NodeId,
    /// (rel_path, message) — non-fatal per-file problems.
    pub warnings: Vec<(String, String)>,
    /// (rel_path, message) — unreadable files.
    pub errors: Vec<(String, String)>,
    pub ambiguities: Vec<Ambiguity>,
    pub self_links_dropped: usize,
}

impl Graph {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    pub fn depth(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = self.node(id).parent;
        while let Some(p) = cur {
            d += 1;
            cur = self.node(p).parent;
        }
        d
    }

    pub(crate) fn push_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }
}

/// Build the full graph from a scan: Contains tree, then link resolution.
pub fn build(scan: VaultScan) -> Graph {
    let root_name = scan
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());

    let mut g = Graph {
        nodes: Vec::new(),
        links: Vec::new(),
        root: NodeId(0),
        warnings: Vec::new(),
        errors: scan.errors.into_iter().map(|e| (e.rel_path, e.message)).collect(),
        ambiguities: Vec::new(),
        self_links_dropped: 0,
    };
    g.push_node(Node {
        kind: NodeKind::Dir,
        path: String::new(),
        name: root_name,
        title: None,
        aliases: Vec::new(),
        parent: None,
        children: Vec::new(),
    });

    // Dir nodes are created only as ancestors of markdown files, so
    // directories with no markdown descendants are pruned for free.
    let mut dir_ids: HashMap<String, NodeId> = HashMap::from([(String::new(), g.root)]);
    let mut file_links: Vec<(NodeId, Vec<RawLink>)> = Vec::new();

    for file in scan.files {
        let parent = ensure_dirs(&mut g, &mut dir_ids, &file.rel_path);
        let name = file_stem(&file.rel_path);
        let id = g.push_node(Node {
            kind: NodeKind::File,
            path: file.rel_path.clone(),
            name,
            title: file.title,
            aliases: file.aliases,
            parent: Some(parent),
            children: Vec::new(),
        });
        g.node_mut(parent).children.push(id);
        if let Some(w) = file.warning {
            g.warnings.push((file.rel_path, w));
        }
        file_links.push((id, file.links));
    }

    sort_children(&mut g);
    resolve::resolve(&mut g, &file_links);
    g
}

/// Get-or-create the Dir node chain for a file's parent directories; returns
/// the immediate parent.
fn ensure_dirs(g: &mut Graph, dir_ids: &mut HashMap<String, NodeId>, rel_path: &str) -> NodeId {
    let Some((dir_path, _file)) = rel_path.rsplit_once('/') else {
        return g.root;
    };
    let mut parent = g.root;
    let mut acc = String::new();
    for comp in dir_path.split('/') {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(comp);
        parent = match dir_ids.get(&acc).copied() {
            Some(id) => id,
            None => {
                let id = g.push_node(Node {
                    kind: NodeKind::Dir,
                    path: acc.clone(),
                    name: comp.to_string(),
                    title: None,
                    aliases: Vec::new(),
                    parent: Some(parent),
                    children: Vec::new(),
                });
                g.node_mut(parent).children.push(id);
                dir_ids.insert(acc.clone(), id);
                id
            }
        };
    }
    parent
}

fn file_stem(rel_path: &str) -> String {
    let file = rel_path.rsplit_once('/').map_or(rel_path, |(_, f)| f);
    if file.len() > 3 && file[file.len() - 3..].eq_ignore_ascii_case(".md") {
        file[..file.len() - 3].to_string()
    } else {
        file.to_string()
    }
}

/// Deterministic child ordering: dirs first, then by name — regardless of
/// walk or creation order.
fn sort_children(g: &mut Graph) {
    let keys: Vec<(u8, String)> = g
        .nodes
        .iter()
        .map(|n| (matches!(n.kind, NodeKind::File) as u8, n.name.clone()))
        .collect();
    for node in &mut g.nodes {
        node.children
            .sort_by(|a, b| keys[a.0 as usize].cmp(&keys[b.0 as usize]));
    }
}
