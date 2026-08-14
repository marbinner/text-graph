//! Force-directed layout simulation (Obsidian-style).
//!
//! Seeded from the deterministic radial layout and integrated with no
//! randomness, so the same graph settles into the same picture every run.
//! Springs act along Contains edges (short: file→dir, longer: dir→dir) and
//! WikiLinks; all nodes repel; weak gravity keeps components together.
//! Ghosts participate too, seeded next to the first node that references
//! them. O(n²) repulsion — fine into the low thousands; Barnes–Hut is the
//! milestone-D upgrade if profiling ever demands it.

use crate::graph::{Graph, NodeKind};
use crate::layout;

const ALPHA_DECAY: f32 = 0.025;
const ALPHA_MIN: f32 = 0.02;
const ALPHA_REHEAT: f32 = 0.4;
const DAMPING: f32 = 0.55;
const MAX_SPEED: f32 = 30.0;
const GRAVITY: f32 = 0.04;
const SPRING_K: f32 = 0.15;
const LEN_CONTAINS_FILE: f32 = 55.0;
const LEN_CONTAINS_DIR: f32 = 150.0;
const LEN_WIKI: f32 = 120.0;
const CHARGE_FILE: f32 = 1500.0;
const CHARGE_DIR: f32 = 1500.0;
const CHARGE_GHOST: f32 = 800.0;

pub struct Sim {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    vx: Vec<f32>,
    vy: Vec<f32>,
    /// (a, b, rest length)
    springs: Vec<(u32, u32, f32)>,
    charge: Vec<f32>,
    /// Node pinned to a world position while the user drags it.
    pinned: Option<(u32, f32, f32)>,
    alpha: f32,
}

impl Sim {
    pub fn new(g: &Graph) -> Sim {
        let n = g.nodes.len();
        let seed = layout::radial(g);
        let mut x = vec![0.0f32; n];
        let mut y = vec![0.0f32; n];
        for (i, p) in seed.iter().enumerate() {
            if let Some(p) = p {
                x[i] = p.x;
                y[i] = p.y;
            }
        }
        // Ghosts have no tree position: seed near their first referencer, at
        // a deterministic golden-angle offset per node index.
        for (i, node) in g.nodes.iter().enumerate() {
            if node.kind != NodeKind::Ghost {
                continue;
            }
            let src = g
                .links
                .iter()
                .find(|l| l.to.0 as usize == i)
                .map(|l| l.from.0 as usize);
            let (bx, by) = src.map_or((0.0, 0.0), |s| (x[s], y[s]));
            let ang = i as f32 * 2.399_963; // golden angle
            x[i] = bx + 90.0 * ang.cos();
            y[i] = by + 90.0 * ang.sin();
        }

        let mut springs = Vec::new();
        for (i, node) in g.nodes.iter().enumerate() {
            if let Some(p) = node.parent {
                let len = if node.kind == NodeKind::Dir {
                    LEN_CONTAINS_DIR
                } else {
                    LEN_CONTAINS_FILE
                };
                springs.push((p.0, i as u32, len));
            }
        }
        for l in &g.links {
            springs.push((l.from.0, l.to.0, LEN_WIKI));
        }

        let charge = g
            .nodes
            .iter()
            .map(|n| match n.kind {
                NodeKind::Dir => CHARGE_DIR,
                NodeKind::File => CHARGE_FILE,
                NodeKind::Ghost => CHARGE_GHOST,
            })
            .collect();

        Sim {
            x,
            y,
            vx: vec![0.0; n],
            vy: vec![0.0; n],
            springs,
            charge,
            pinned: None,
            alpha: 1.0,
        }
    }

    pub fn active(&self) -> bool {
        self.alpha > ALPHA_MIN
    }

    pub fn reheat(&mut self) {
        self.alpha = self.alpha.max(ALPHA_REHEAT);
    }

    /// Cap the starting energy — used after a live-reload rebuild where most
    /// positions were carried over, so the graph ripples instead of
    /// re-settling from scratch.
    pub fn calm(&mut self) {
        self.alpha = self.alpha.min(ALPHA_REHEAT);
    }

    /// Pin a node to a world position (user drag). Reheats the simulation so
    /// the rest of the graph responds.
    pub fn pin(&mut self, id: u32, px: f32, py: f32) {
        self.pinned = Some((id, px, py));
        self.reheat();
    }

    pub fn unpin(&mut self) {
        self.pinned = None;
    }

    pub fn tick(&mut self, iters: usize) {
        let n = self.x.len();
        for _ in 0..iters {
            if !self.active() {
                return;
            }
            self.alpha *= 1.0 - ALPHA_DECAY;
            let a = self.alpha;

            // pairwise repulsion (symmetric)
            for i in 0..n {
                for j in (i + 1)..n {
                    let mut dx = self.x[i] - self.x[j];
                    let mut dy = self.y[i] - self.y[j];
                    if dx * dx + dy * dy < 1e-6 {
                        // exactly coincident nodes have no direction to repel
                        // along and would stay stuck — nudge them apart on a
                        // deterministic index-derived angle
                        let ang = (i * 31 + j) as f32;
                        dx = ang.cos();
                        dy = ang.sin();
                    }
                    let d2 = (dx * dx + dy * dy).max(64.0);
                    let inv = a / d2;
                    self.vx[i] += dx * inv * self.charge[j];
                    self.vy[i] += dy * inv * self.charge[j];
                    self.vx[j] -= dx * inv * self.charge[i];
                    self.vy[j] -= dy * inv * self.charge[i];
                }
            }

            // springs
            for &(pa, pb, rest) in &self.springs {
                let (i, j) = (pa as usize, pb as usize);
                let dx = self.x[j] - self.x[i];
                let dy = self.y[j] - self.y[i];
                let d = (dx * dx + dy * dy).sqrt().max(1.0);
                let f = (d - rest) * SPRING_K * a / d;
                self.vx[i] += dx * f;
                self.vy[i] += dy * f;
                self.vx[j] -= dx * f;
                self.vy[j] -= dy * f;
            }

            // gravity, damping, speed cap, integrate
            for i in 0..n {
                self.vx[i] -= self.x[i] * GRAVITY * a;
                self.vy[i] -= self.y[i] * GRAVITY * a;
                self.vx[i] *= DAMPING;
                self.vy[i] *= DAMPING;
                let sp2 = self.vx[i] * self.vx[i] + self.vy[i] * self.vy[i];
                if sp2 > MAX_SPEED * MAX_SPEED {
                    let s = MAX_SPEED / sp2.sqrt();
                    self.vx[i] *= s;
                    self.vy[i] *= s;
                }
                self.x[i] += self.vx[i];
                self.y[i] += self.vy[i];
            }

            if let Some((id, px, py)) = self.pinned {
                let i = id as usize;
                self.x[i] = px;
                self.y[i] = py;
                self.vx[i] = 0.0;
                self.vy[i] = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Link, LinkKind, Node, NodeId, NodeKind};

    /// root dir with two files; file a wikilinks a ghost.
    fn synth() -> Graph {
        let mut g = Graph {
            nodes: Vec::new(),
            links: Vec::new(),
            root: NodeId(0),
            warnings: Vec::new(),
            errors: Vec::new(),
            ambiguities: Vec::new(),
            self_links_dropped: 0,
        };
        let mk = |kind, path: &str, name: &str, parent, children: Vec<NodeId>| Node {
            kind,
            path: path.into(),
            name: name.into(),
            title: None,
            aliases: Vec::new(),
            parent,
            children,
        };
        g.nodes.push(mk(NodeKind::Dir, "", "r", None, vec![NodeId(1), NodeId(2)]));
        g.nodes.push(mk(NodeKind::File, "a.md", "a", Some(NodeId(0)), vec![]));
        g.nodes.push(mk(NodeKind::File, "b.md", "b", Some(NodeId(0)), vec![]));
        g.nodes.push(mk(NodeKind::Ghost, "gh", "gh", None, vec![]));
        g.links.push(Link { from: NodeId(1), to: NodeId(3), kind: LinkKind::WikiLink });
        g
    }

    #[test]
    fn deterministic_and_finite_and_settles() {
        let g = synth();
        let mut s1 = Sim::new(&g);
        let mut s2 = Sim::new(&g);
        s1.tick(300);
        s2.tick(300);
        assert_eq!(s1.x, s2.x);
        assert_eq!(s1.y, s2.y);
        assert!(s1.x.iter().chain(&s1.y).all(|v| v.is_finite()));
        assert!(!s1.active(), "should settle within 300 ticks");
    }

    #[test]
    fn ghost_ends_up_near_its_referencer() {
        let g = synth();
        let mut s = Sim::new(&g);
        s.tick(300);
        let d = |i: usize, j: usize| {
            ((s.x[i] - s.x[j]).powi(2) + (s.y[i] - s.y[j]).powi(2)).sqrt()
        };
        // a→ghost are spring-linked; b→ghost are not
        assert!(d(1, 3) < d(2, 3), "linked pair should sit closer");
    }

    #[test]
    fn coincident_nodes_separate() {
        let g = synth();
        let mut s = Sim::new(&g);
        s.x[2] = s.x[1]; // force exact overlap
        s.y[2] = s.y[1];
        s.tick(100);
        let d = ((s.x[1] - s.x[2]).powi(2) + (s.y[1] - s.y[2]).powi(2)).sqrt();
        assert!(d > 10.0, "overlapping nodes must separate, got {d}");
    }
}
