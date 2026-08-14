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
    /// An image file — a leaf like File, but with no body to parse. Its
    /// `name` keeps the extension (that's how links address it).
    Image,
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

    /// Cross-reload identity key. A ghost's `path` is raw target text, which
    /// can collide with a real dir path (ghost `[[notes]]` vs dir `notes`) —
    /// ghosts get their own namespace so position/selection carry-over never
    /// confuses the two.
    pub fn ident(&self) -> String {
        match self.kind {
            NodeKind::Ghost => format!("[[{}]]", self.path),
            _ => self.path.clone(),
        }
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
    /// Byte offset of the first `[[` occurrence of this edge in the source
    /// file's BODY — the text `vault::read_body` returns (BOM and
    /// frontmatter stripped), not the raw file. Duplicate occurrences are
    /// deduplicated; the first wins.
    pub offset: usize,
}

/// A basename link that matched several files; resolution picked the first
/// in sorted path order and recorded the rest.
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
    /// Per-node indices into `links`: outgoing / incoming, in link order
    /// (deterministic). Built by `finish_indexes` after resolution.
    links_out: Vec<Vec<u32>>,
    links_in: Vec<Vec<u32>>,
    /// `Node::ident()` → id. Idents are unique (ghosts are namespaced).
    ident_index: HashMap<String, NodeId>,
}

impl Graph {
    /// An empty shell (tests, synthetic builders). Populate nodes/links,
    /// then call `finish_indexes` if the query layer will be used.
    pub fn empty() -> Self {
        Graph {
            nodes: Vec::new(),
            links: Vec::new(),
            root: NodeId(0),
            warnings: Vec::new(),
            errors: Vec::new(),
            ambiguities: Vec::new(),
            self_links_dropped: 0,
            links_out: Vec::new(),
            links_in: Vec::new(),
            ident_index: HashMap::new(),
        }
    }

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

    // ---- ranger-style tree walking (keyboard navigation) ----
    // All in the deterministic sorted child order, so the navigator list,
    // the graph layout, and the key stepping always agree.

    /// (parent, index of `id` within its children). None for root/ghosts.
    fn sibling_index(&self, id: NodeId) -> Option<(NodeId, usize)> {
        let parent = self.node(id).parent?;
        let i = self.node(parent).children.iter().position(|c| *c == id)?;
        Some((parent, i))
    }

    /// The sibling `delta` steps away, clamped at the ends; None when the
    /// move goes nowhere (already at an end, or no parent).
    pub fn nav_sibling(&self, id: NodeId, delta: isize) -> Option<NodeId> {
        let (parent, i) = self.sibling_index(id)?;
        let sibs = &self.node(parent).children;
        let j = (i as isize + delta).clamp(0, sibs.len() as isize - 1) as usize;
        (j != i).then(|| sibs[j])
    }

    /// First or last sibling (vim gg / G).
    pub fn nav_sibling_end(&self, id: NodeId, last: bool) -> Option<NodeId> {
        let (parent, _) = self.sibling_index(id)?;
        let sibs = &self.node(parent).children;
        if last {
            sibs.last().copied()
        } else {
            sibs.first().copied()
        }
    }

    /// Enter a directory: its first child (ranger `l`).
    pub fn nav_enter(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).children.first().copied()
    }

    // ---- query layer: link adjacency and stable lookups ----

    /// Wikilinks FROM this node, in body order.
    pub fn outlinks(&self, id: NodeId) -> impl Iterator<Item = &Link> {
        self.links_out[id.0 as usize]
            .iter()
            .map(|i| &self.links[*i as usize])
    }

    /// Wikilinks TO this node, in source (NodeId) order.
    pub fn backlinks(&self, id: NodeId) -> impl Iterator<Item = &Link> {
        self.links_in[id.0 as usize]
            .iter()
            .map(|i| &self.links[*i as usize])
    }

    /// Look a node up by vault-relative path (files/dirs; "" = root).
    /// Reload-stable where NodeIds are not.
    pub fn by_path(&self, path: &str) -> Option<NodeId> {
        // ghost idents are namespaced "[[...]]", so plain paths only ever
        // hit real nodes here
        self.ident_index.get(path).copied()
    }

    /// Look a node up by its `Node::ident()` (includes ghosts).
    pub fn by_ident(&self, ident: &str) -> Option<NodeId> {
        self.ident_index.get(ident).copied()
    }

    /// Build the adjacency and ident indexes. Called once at the end of
    /// `build`, after resolution has added ghosts and links.
    pub(crate) fn finish_indexes(&mut self) {
        self.links_out = vec![Vec::new(); self.nodes.len()];
        self.links_in = vec![Vec::new(); self.nodes.len()];
        for (i, l) in self.links.iter().enumerate() {
            self.links_out[l.from.0 as usize].push(i as u32);
            self.links_in[l.to.0 as usize].push(i as u32);
        }
        self.ident_index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.ident(), NodeId(i as u32)))
            .collect();
    }
}

/// Build the full graph from a scan: Contains tree, then link resolution.
pub fn build(scan: VaultScan) -> Graph {
    let root_name = scan
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());

    let mut g = Graph::empty();
    g.errors = scan
        .errors
        .into_iter()
        .map(|e| (e.rel_path, e.message))
        .collect();
    g.push_node(Node {
        kind: NodeKind::Dir,
        path: String::new(),
        name: root_name,
        title: None,
        aliases: Vec::new(),
        parent: None,
        children: Vec::new(),
    });

    // Dir nodes are created only as ancestors of markdown/image files, so
    // directories with no such descendants are pruned for free.
    let mut dir_ids: HashMap<String, NodeId> = HashMap::from([(String::new(), g.root)]);
    let mut file_links: Vec<(NodeId, Vec<RawLink>)> = Vec::new();

    // Merge notes and images in sorted rel_path order, so NodeId order keeps
    // meaning "sorted path order" across every leaf — resolution's ambiguity
    // winner and the determinism tests both lean on that.
    let mut files = scan.files.into_iter().peekable();
    let mut images = scan.images.into_iter().peekable();
    loop {
        let take_file = match (files.peek(), images.peek()) {
            (Some(f), Some(i)) => f.rel_path <= *i,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_file {
            let file = files.next().unwrap();
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
        } else {
            let rel = images.next().unwrap();
            let parent = ensure_dirs(&mut g, &mut dir_ids, &rel);
            let name = rel
                .rsplit_once('/')
                .map_or(rel.as_str(), |(_, f)| f)
                .to_string();
            let id = g.push_node(Node {
                kind: NodeKind::Image,
                path: rel,
                name,
                title: None,
                aliases: Vec::new(),
                parent: Some(parent),
                children: Vec::new(),
            });
            g.node_mut(parent).children.push(id);
        }
    }

    sort_children(&mut g);
    resolve::resolve(&mut g, &file_links);
    g.finish_indexes();
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
        .map(|n| (!matches!(n.kind, NodeKind::Dir) as u8, n.name.clone()))
        .collect();
    for node in &mut g.nodes {
        node.children
            .sort_by(|a, b| keys[a.0 as usize].cmp(&keys[b.0 as usize]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, path: &str) -> Node {
        Node {
            kind,
            path: path.to_string(),
            name: path.to_string(),
            title: None,
            aliases: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    /// A tiny hand-wired tree: root -> [a/, z.md], a/ -> [b.md, c.md].
    fn tree() -> Graph {
        let mut g = Graph::empty();
        g.push_node(node(NodeKind::Dir, "")); // 0 root
        g.push_node(node(NodeKind::Dir, "a")); // 1
        g.push_node(node(NodeKind::File, "z.md")); // 2
        g.push_node(node(NodeKind::File, "a/b.md")); // 3
        g.push_node(node(NodeKind::File, "a/c.md")); // 4
        g.nodes[0].children = vec![NodeId(1), NodeId(2)];
        g.nodes[1].parent = Some(NodeId(0));
        g.nodes[2].parent = Some(NodeId(0));
        g.nodes[1].children = vec![NodeId(3), NodeId(4)];
        g.nodes[3].parent = Some(NodeId(1));
        g.nodes[4].parent = Some(NodeId(1));
        g
    }

    #[test]
    fn tree_walk_steps_clamps_and_enters() {
        let g = tree();
        let (a, z, b, c) = (NodeId(1), NodeId(2), NodeId(3), NodeId(4));
        // j/k between siblings, clamped at the ends
        assert_eq!(g.nav_sibling(b, 1), Some(c));
        assert_eq!(g.nav_sibling(c, 1), None, "clamped at last");
        assert_eq!(g.nav_sibling(c, -1), Some(b));
        assert_eq!(g.nav_sibling(b, -1), None, "clamped at first");
        // gg / G
        assert_eq!(g.nav_sibling_end(c, false), Some(b));
        assert_eq!(g.nav_sibling_end(b, true), Some(c));
        // l enters a dir; h is just .parent
        assert_eq!(g.nav_enter(a), Some(b));
        assert_eq!(g.nav_enter(z), None, "files have no children");
        // root has no parent, no siblings
        assert_eq!(g.nav_sibling(g.root, 1), None);
        assert_eq!(g.node(a).parent, Some(g.root));
    }

    /// The cross-reload identity invariant: a ghost whose raw target text
    /// equals a real node's path must NOT share its identity — otherwise
    /// live reload hands the ghost the dir's position/selection (or vice
    /// versa).
    #[test]
    fn ghost_ident_never_collides_with_real_paths() {
        let dir = node(NodeKind::Dir, "notes");
        let file = node(NodeKind::File, "notes");
        let ghost = node(NodeKind::Ghost, "notes");
        assert_eq!(dir.ident(), "notes");
        assert_eq!(file.ident(), "notes");
        assert_ne!(ghost.ident(), dir.ident());
        assert_eq!(ghost.ident(), "[[notes]]");
    }
}
