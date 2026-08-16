//! Radial tree layout over the Contains spine.
//!
//! Pure: graph in, positions out — no egui types, testable without a window.
//! Root at the origin, one ring per depth, each subtree's angular sector
//! proportional to its leaf count. Deterministic because child order is
//! (graph.rs sorts children dirs-first-then-name).

use crate::graph::{Graph, NodeId};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

/// World-space distance between depth rings.
pub const RING_SPACING: f32 = 180.0;

/// Positions indexed by NodeId. Ghosts (and anything else outside the
/// Contains tree) get `None` — v1 doesn't place them.
pub fn radial(g: &Graph) -> Vec<Option<Pos>> {
    let n = g.nodes.len();
    if n == 0 {
        return Vec::new(); // graph::build always makes a root, but the pub
        // API must not panic on a hand-built empty graph
    }
    let mut pos = vec![None; n];
    let mut weight = vec![0f32; n];
    subtree_weight(g, g.root, &mut weight);
    pos[g.root.0 as usize] = Some(Pos { x: 0.0, y: 0.0 });
    assign(g, g.root, 0.0, std::f32::consts::TAU, 1, &weight, &mut pos);
    pos
}

/// Post-order: a leaf weighs 1; an inner node weighs the sum of its children.
fn subtree_weight(g: &Graph, id: NodeId, out: &mut [f32]) -> f32 {
    let node = g.node(id);
    let w = if node.children.is_empty() {
        1.0
    } else {
        node.children
            .iter()
            .map(|c| subtree_weight(g, *c, out))
            .sum()
    };
    out[id.0 as usize] = w;
    w
}

/// Pre-order: split the parent's angular interval among children by weight;
/// place each child at its sector's midpoint on the depth ring.
fn assign(
    g: &Graph,
    id: NodeId,
    a0: f32,
    a1: f32,
    depth: u32,
    weight: &[f32],
    pos: &mut [Option<Pos>],
) {
    let children = &g.node(id).children;
    if children.is_empty() {
        return;
    }
    let total: f32 = children.iter().map(|c| weight[c.0 as usize]).sum();
    let r = depth as f32 * RING_SPACING;
    let mut a = a0;
    for &c in children {
        let span = (a1 - a0) * weight[c.0 as usize] / total;
        let mid = a + span * 0.5;
        pos[c.0 as usize] = Some(Pos {
            x: r * mid.cos(),
            y: r * mid.sin(),
        });
        assign(g, c, a, a + span, depth + 1, weight, pos);
        a += span;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Node, NodeId, NodeKind};

    /// Root with two file children.
    fn tiny() -> Graph {
        let mut g = Graph::empty();
        g.nodes.push(Node {
            kind: NodeKind::Dir,
            path: String::new(),
            os_path: Some(std::path::PathBuf::new()),
            name: "r".into(),
            title: None,
            aliases: Vec::new(),
            parent: None,
            children: vec![NodeId(1), NodeId(2)],
        });
        for (i, n) in ["a", "b"].iter().enumerate() {
            g.nodes.push(Node {
                kind: NodeKind::File,
                path: format!("{n}.md"),
                os_path: Some(std::path::PathBuf::from(format!("{n}.md"))),
                name: (*n).into(),
                title: None,
                aliases: Vec::new(),
                parent: Some(NodeId(0)),
                children: Vec::new(),
            });
            let _ = i;
        }
        g
    }

    #[test]
    fn two_children_sit_on_ring_one_at_opposite_angles() {
        let pos = radial(&tiny());
        let a = pos[1].unwrap();
        let b = pos[2].unwrap();
        assert!((a.x.hypot(a.y) - RING_SPACING).abs() < 1e-3);
        assert!((b.x.hypot(b.y) - RING_SPACING).abs() < 1e-3);
        // equal weights → opposite sector midpoints → positions cancel
        assert!((a.x + b.x).abs() < 1e-3);
        assert!((a.y + b.y).abs() < 1e-3);
    }

    #[test]
    fn root_is_at_origin() {
        let pos = radial(&tiny());
        assert_eq!(pos[0], Some(Pos { x: 0.0, y: 0.0 }));
    }

    #[test]
    fn empty_graph_yields_no_positions() {
        let g = Graph::empty();
        assert!(radial(&g).is_empty());
    }
}
