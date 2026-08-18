//! Force-directed layout simulation (Obsidian-style).
//!
//! Seeded from the deterministic radial layout and integrated with no
//! randomness, so the same graph settles into the same picture every run.
//! Springs act along Contains edges (short: file→dir, longer: dir→dir) and
//! WikiLinks; all nodes repel; weak gravity keeps components together.
//! Ghosts participate too, seeded next to the first node that references
//! them.
//!
//! Repulsion is exact O(n²) up to [`BH_MIN_NODES`] (bit-identical to the
//! original integration, and faster than building a tree down there) and
//! Barnes–Hut above it: a quadtree of charge-weighted centers of mass,
//! rebuilt each iteration, with far cells acting as single points
//! ([`BH_THETA`]) and near neighbours interacting exactly through leaf
//! buckets — the coincident-node nudge included, index-derived as ever.
//! The tree build partitions by stable sort in node-index order and the
//! traversal order is fixed, so the approximation is exactly as
//! deterministic as the brute loop it replaces (the probe put the brute
//! loop at 44 ms per frame at 2k nodes and 730 ms at 10k — the layout
//! settle was the one thing that made big vaults feel broken).

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
const CHARGE_WEB: f32 = 600.0;
/// External (note → URL) springs are shorter than wikilinks — citations
/// hug their citers.
const LEN_EXTERNAL: f32 = 90.0;

/// Above this many nodes, repulsion goes through the Barnes–Hut quadtree;
/// at or below it, the exact pairwise loop wins (no tree to build, and
/// small vaults keep bit-identical layouts to the pre-BH sim).
const BH_MIN_NODES: usize = 256;
/// Opening criterion: a cell whose width/distance ratio is under θ acts
/// as one charge-weighted point. 0.8 sits on the accurate side of the
/// usual 0.5–1.2 range; the match-the-brute-loop test pins the error.
const BH_THETA: f32 = 0.8;
/// Leaf bucket size — slices this small interact exactly, pairwise.
const BH_LEAF: usize = 16;
/// Splitting stops here regardless: coincident and near-coincident points
/// end up sharing a leaf bucket (where the exact path, nudge included,
/// handles them) instead of splitting forever.
const BH_MAX_DEPTH: u32 = 24;

/// One quadtree cell: its size, the charge-weighted center of mass of
/// everything inside, and either four children or a leaf slice of
/// `order`. (The region's center only matters while building — it is not
/// stored.)
struct BhCell {
    half: f32,
    comx: f32,
    comy: f32,
    charge: f32,
    /// Child cell indices ([`BH_NO_CELL`] = empty quadrant); a leaf when
    /// all four are empty.
    kids: [u32; 4],
    /// Leaf slice into `order` (empty for interior cells).
    start: u32,
    len: u32,
}

const BH_NO_CELL: u32 = u32::MAX;

/// Build the cell for `order[start..start+len]` and recurse. Partitioning
/// uses a STABLE sort keyed by quadrant, so node-index order is preserved
/// within every cell and the whole tree is a pure function of positions.
#[allow(clippy::too_many_arguments)]
fn bh_build(
    cells: &mut Vec<BhCell>,
    order: &mut [u32],
    start: u32,
    x: &[f32],
    y: &[f32],
    charge: &[f32],
    cx: f32,
    cy: f32,
    half: f32,
    depth: u32,
) -> u32 {
    let mut comx = 0.0f32;
    let mut comy = 0.0f32;
    let mut q = 0.0f32;
    for &i in order.iter() {
        let c = charge[i as usize];
        comx += x[i as usize] * c;
        comy += y[i as usize] * c;
        q += c;
    }
    if q > 0.0 {
        comx /= q;
        comy /= q;
    }
    let idx = cells.len() as u32;
    cells.push(BhCell {
        half,
        comx,
        comy,
        charge: q,
        kids: [BH_NO_CELL; 4],
        start,
        len: 0,
    });
    if order.len() <= BH_LEAF || depth >= BH_MAX_DEPTH {
        cells[idx as usize].len = order.len() as u32;
        return idx;
    }
    let quadrant = |i: u32| -> usize {
        usize::from(x[i as usize] > cx) | (usize::from(y[i as usize] > cy) << 1)
    };
    order.sort_by_key(|&i| quadrant(i));
    let h = half * 0.5;
    let mut lo = 0usize;
    for k in 0..4 {
        let hi = lo
            + order[lo..]
                .iter()
                .take_while(|&&i| quadrant(i) == k)
                .count();
        if hi > lo {
            let ccx = cx + if k & 1 == 1 { h } else { -h };
            let ccy = cy + if k & 2 == 2 { h } else { -h };
            let child = bh_build(
                cells,
                &mut order[lo..hi],
                start + lo as u32,
                x,
                y,
                charge,
                ccx,
                ccy,
                h,
                depth + 1,
            );
            cells[idx as usize].kids[k] = child;
        }
        lo = hi;
    }
    idx
}

/// The exact pairwise contribution to `i` from `j` — including the
/// index-derived nudge for coincident nodes, with the SAME angle and the
/// same opposing signs the symmetric brute loop produces.
#[inline]
fn bh_pair(i: usize, j: usize, x: &[f32], y: &[f32], charge: &[f32], a_cs: f32) -> (f32, f32) {
    let mut dx = x[i] - x[j];
    let mut dy = y[i] - y[j];
    if dx * dx + dy * dy < 1e-6 {
        let (p, q) = if i < j { (i, j) } else { (j, i) };
        let ang = (p * 31 + q) as f32;
        let (sin, cos) = ang.sin_cos();
        let sign = if i == p { 1.0 } else { -1.0 };
        dx = cos * sign;
        dy = sin * sign;
    }
    let d2 = (dx * dx + dy * dy).max(64.0);
    let inv = a_cs / d2;
    (dx * inv * charge[j], dy * inv * charge[j])
}

/// Sum the repulsion on node `i`: far cells as their center of mass,
/// near cells opened, leaf buckets exactly.
#[allow(clippy::too_many_arguments)]
fn bh_accumulate(
    cells: &[BhCell],
    order: &[u32],
    stack: &mut Vec<u32>,
    i: usize,
    x: &[f32],
    y: &[f32],
    charge: &[f32],
    a_cs: f32,
    theta2: f32,
) -> (f32, f32) {
    let (mut fx, mut fy) = (0.0f32, 0.0f32);
    stack.clear();
    stack.push(0);
    while let Some(c) = stack.pop() {
        let cell = &cells[c as usize];
        if cell.charge <= 0.0 {
            continue;
        }
        if cell.len > 0 {
            for &j in &order[cell.start as usize..(cell.start + cell.len) as usize] {
                if j as usize != i {
                    let (px, py) = bh_pair(i, j as usize, x, y, charge, a_cs);
                    fx += px;
                    fy += py;
                }
            }
            continue;
        }
        let dx = x[i] - cell.comx;
        let dy = y[i] - cell.comy;
        let d2 = dx * dx + dy * dy;
        let s = cell.half * 2.0;
        if s * s < theta2 * d2 {
            let inv = a_cs / d2.max(64.0);
            fx += dx * inv * cell.charge;
            fy += dy * inv * cell.charge;
        } else {
            for &k in cell.kids.iter().rev() {
                if k != BH_NO_CELL {
                    stack.push(k);
                }
            }
        }
    }
    (fx, fy)
}

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
    /// Layout spread (a setting): scales spring rest lengths, and charge by
    /// its SQUARE — repulsion falls off as 1/d², so both sides of the force
    /// balance have to scale together or the graph changes shape instead of
    /// size. Applied at integration time rather than baked into the spring
    /// table, so changing it never rebuilds anything.
    spread: f32,
    /// Layout paused (a setting). A frozen sim still honours the pin, so a
    /// dragged node goes exactly where it is dropped and stays there.
    frozen: bool,
    // Barnes–Hut scratch, reused across iterations so the per-iteration
    // tree rebuild allocates nothing in steady state.
    bh_cells: Vec<BhCell>,
    bh_order: Vec<u32>,
    bh_stack: Vec<u32>,
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
        // Ghosts and web nodes have no tree position: seed near their first
        // referencer, at a deterministic golden-angle offset per node index.
        for (i, node) in g.nodes.iter().enumerate() {
            if !matches!(node.kind, NodeKind::Ghost | NodeKind::Web) {
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
            let len = match l.kind {
                crate::graph::LinkKind::WikiLink => LEN_WIKI,
                crate::graph::LinkKind::External => LEN_EXTERNAL,
            };
            springs.push((l.from.0, l.to.0, len));
        }

        let charge = g
            .nodes
            .iter()
            .map(|n| match n.kind {
                NodeKind::Dir => CHARGE_DIR,
                NodeKind::File | NodeKind::Image | NodeKind::Asset => CHARGE_FILE,
                NodeKind::Ghost => CHARGE_GHOST,
                NodeKind::Web => CHARGE_WEB,
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
            spread: 1.0,
            frozen: false,
            bh_cells: Vec::new(),
            bh_order: Vec::new(),
            bh_stack: Vec::new(),
        }
    }

    /// Carry the live settings into a sim (a fresh one, or after a reload
    /// rebuilds it). Reheats when the spread actually moves, since the
    /// graph has to relax into the new equilibrium to show the change.
    pub fn configure(&mut self, spread: f32, frozen: bool) {
        let spread = spread.clamp(0.1, 10.0);
        if (spread - self.spread).abs() > f32::EPSILON {
            self.spread = spread;
            self.reheat();
        }
        self.frozen = frozen;
    }

    pub fn active(&self) -> bool {
        !self.frozen && self.alpha > ALPHA_MIN
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
        if self.frozen {
            // a frozen layout still follows the hand: the drag pin is the
            // one thing allowed to move a node
            self.apply_pin();
            return;
        }
        let charge_scale = self.spread * self.spread;
        for _ in 0..iters {
            if !self.active() {
                return;
            }
            self.alpha *= 1.0 - ALPHA_DECAY;
            let a = self.alpha;

            // repulsion: exact pairwise up to BH_MIN_NODES, Barnes–Hut
            // above (see the module docs — bit-identical below the line,
            // tree-approximated and ~n log n above it)
            if n > BH_MIN_NODES {
                self.bh_repulsion(a * charge_scale);
            } else {
                for i in 0..n {
                    for j in (i + 1)..n {
                        let mut dx = self.x[i] - self.x[j];
                        let mut dy = self.y[i] - self.y[j];
                        if dx * dx + dy * dy < 1e-6 {
                            // exactly coincident nodes have no direction to
                            // repel along and would stay stuck — nudge them
                            // apart on a deterministic index-derived angle
                            let ang = (i * 31 + j) as f32;
                            dx = ang.cos();
                            dy = ang.sin();
                        }
                        let d2 = (dx * dx + dy * dy).max(64.0);
                        let inv = a / d2 * charge_scale;
                        self.vx[i] += dx * inv * self.charge[j];
                        self.vy[i] += dy * inv * self.charge[j];
                        self.vx[j] -= dx * inv * self.charge[i];
                        self.vy[j] -= dy * inv * self.charge[i];
                    }
                }
            }

            // springs
            for &(pa, pb, rest) in &self.springs {
                let rest = rest * self.spread;
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

            self.apply_pin();
        }
    }

    /// One Barnes–Hut repulsion pass at the current positions: rebuild the
    /// quadtree (positions moved last iteration), then accumulate per node.
    fn bh_repulsion(&mut self, a_cs: f32) {
        let n = self.x.len();
        let mut cells = std::mem::take(&mut self.bh_cells);
        let mut order = std::mem::take(&mut self.bh_order);
        let mut stack = std::mem::take(&mut self.bh_stack);
        cells.clear();
        order.clear();
        order.extend(0..n as u32);
        let mut minx = f32::INFINITY;
        let mut miny = f32::INFINITY;
        let mut maxx = f32::NEG_INFINITY;
        let mut maxy = f32::NEG_INFINITY;
        for k in 0..n {
            minx = minx.min(self.x[k]);
            maxx = maxx.max(self.x[k]);
            miny = miny.min(self.y[k]);
            maxy = maxy.max(self.y[k]);
        }
        let half = ((maxx - minx).max(maxy - miny) * 0.5).max(1.0);
        bh_build(
            &mut cells,
            &mut order,
            0,
            &self.x,
            &self.y,
            &self.charge,
            (minx + maxx) * 0.5,
            (miny + maxy) * 0.5,
            half,
            0,
        );
        for i in 0..n {
            let (fx, fy) = bh_accumulate(
                &cells,
                &order,
                &mut stack,
                i,
                &self.x,
                &self.y,
                &self.charge,
                a_cs,
                BH_THETA * BH_THETA,
            );
            self.vx[i] += fx;
            self.vy[i] += fy;
        }
        self.bh_cells = cells;
        self.bh_order = order;
        self.bh_stack = stack;
    }

    fn apply_pin(&mut self) {
        if let Some((id, px, py)) = self.pinned {
            let i = id as usize;
            // bounds-guarded: a stale pin held across a graph swap must
            // degrade, not panic
            if i < self.x.len() {
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
        let mut g = Graph::empty();
        let mk = |kind, path: &str, name: &str, parent, children: Vec<NodeId>| Node {
            kind,
            path: path.into(),
            os_path: (!matches!(kind, NodeKind::Ghost | NodeKind::Web))
                .then(|| std::path::PathBuf::from(path)),
            name: name.into(),
            title: None,
            aliases: Vec::new(),
            parent,
            children,
        };
        g.nodes
            .push(mk(NodeKind::Dir, "", "r", None, vec![NodeId(1), NodeId(2)]));
        g.nodes
            .push(mk(NodeKind::File, "a.md", "a", Some(NodeId(0)), vec![]));
        g.nodes
            .push(mk(NodeKind::File, "b.md", "b", Some(NodeId(0)), vec![]));
        g.nodes.push(mk(NodeKind::Ghost, "gh", "gh", None, vec![]));
        g.links.push(Link {
            from: NodeId(1),
            to: NodeId(3),
            kind: LinkKind::WikiLink,
            offset: 0,
        });
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
        let d = |i: usize, j: usize| ((s.x[i] - s.x[j]).powi(2) + (s.y[i] - s.y[j]).powi(2)).sqrt();
        // a→ghost are spring-linked; b→ghost are not
        assert!(d(1, 3) < d(2, 3), "linked pair should sit closer");
    }

    #[test]
    fn stale_pin_out_of_range_does_not_panic() {
        let g = synth();
        let mut s = Sim::new(&g);
        s.pin(999, 0.0, 0.0);
        s.tick(10);
    }

    /// Spread has to scale the picture, not redraw it: springs and charge
    /// move together (charge by the square, since repulsion falls off as
    /// 1/d²), so a wider setting spaces the same layout out.
    #[test]
    fn spread_scales_the_layout() {
        let g = synth();
        let extent = |spread: f32| {
            let mut s = Sim::new(&g);
            s.configure(spread, false);
            s.tick(600);
            let n = s.x.len() as f32;
            let (cx, cy) = (s.x.iter().sum::<f32>() / n, s.y.iter().sum::<f32>() / n);
            s.x.iter()
                .zip(&s.y)
                .map(|(x, y)| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt())
                .sum::<f32>()
                / n
        };
        let (tight, wide) = (extent(1.0), extent(1.6));
        assert!(
            wide > tight * 1.2 && wide < tight * 2.2,
            "1.6x spread should space the graph out by roughly that much: \
             {tight} -> {wide}"
        );
        // still deterministic at a non-default setting
        let mut a = Sim::new(&g);
        let mut b = Sim::new(&g);
        a.configure(1.6, false);
        b.configure(1.6, false);
        a.tick(200);
        b.tick(200);
        assert_eq!((a.x, a.y), (b.x, b.y));
    }

    #[test]
    fn a_frozen_layout_holds_still_but_still_follows_the_hand() {
        let g = synth();
        let mut s = Sim::new(&g);
        s.tick(50);
        s.configure(1.0, true);
        let (x, y) = (s.x.clone(), s.y.clone());
        s.tick(300);
        assert_eq!(
            (s.x.clone(), s.y.clone()),
            (x.clone(), y),
            "frozen means frozen"
        );
        assert!(!s.active(), "and it stops asking for frames");
        // dragging is the one thing that may still move a node
        s.pin(2, 500.0, -500.0);
        s.tick(1);
        assert_eq!((s.x[2], s.y[2]), (500.0, -500.0));
        assert_eq!(s.x[1], x[1], "…and only that node");
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

    /// A flat dir with `n` files chained by wikilinks — big enough to put
    /// the sim on the Barnes–Hut path (n > BH_MIN_NODES).
    fn synth_flat(n: usize) -> Graph {
        let mut g = Graph::empty();
        let mk = |kind, path: &str, name: &str, parent, children: Vec<NodeId>| Node {
            kind,
            path: path.into(),
            os_path: Some(std::path::PathBuf::from(path)),
            name: name.into(),
            title: None,
            aliases: Vec::new(),
            parent,
            children,
        };
        g.nodes.push(mk(
            NodeKind::Dir,
            "",
            "r",
            None,
            (1..=n).map(|i| NodeId(i as u32)).collect(),
        ));
        for i in 1..=n {
            g.nodes.push(mk(
                NodeKind::File,
                &format!("f{i}.md"),
                &format!("f{i}"),
                Some(NodeId(0)),
                vec![],
            ));
        }
        for i in 1..n {
            g.links.push(Link {
                from: NodeId(i as u32),
                to: NodeId(i as u32 + 1),
                kind: LinkKind::WikiLink,
                offset: 0,
            });
        }
        g
    }

    /// The exact pairwise loop, restated as the reference the tree must
    /// track — plus each node's GROSS force (the sum of contribution
    /// magnitudes). Duplicating the formula here is deliberate: a change
    /// to either side that drifts from the other fails this test.
    fn exact_forces(
        x: &[f32],
        y: &[f32],
        charge: &[f32],
        a_cs: f32,
    ) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let n = x.len();
        let mut fx = vec![0.0f32; n];
        let mut fy = vec![0.0f32; n];
        let mut gross = vec![0.0f32; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let mut dx = x[i] - x[j];
                let mut dy = y[i] - y[j];
                if dx * dx + dy * dy < 1e-6 {
                    let ang = (i * 31 + j) as f32;
                    dx = ang.cos();
                    dy = ang.sin();
                }
                let d2 = (dx * dx + dy * dy).max(64.0);
                let inv = a_cs / d2;
                fx[i] += dx * inv * charge[j];
                fy[i] += dy * inv * charge[j];
                fx[j] -= dx * inv * charge[i];
                fy[j] -= dy * inv * charge[i];
                let mag = (dx * dx + dy * dy).sqrt() * inv;
                gross[i] += mag * charge[j];
                gross[j] += mag * charge[i];
            }
        }
        (fx, fy, gross)
    }

    #[test]
    fn barnes_hut_tracks_the_exact_forces() {
        let g = synth_flat(600);
        let s = Sim::new(&g); // radial seed = a realistic spread of positions
        let (ex, ey, gross) = exact_forces(&s.x, &s.y, &s.charge, 1.0);

        let n = s.x.len();
        let mut cells = Vec::new();
        let mut order: Vec<u32> = (0..n as u32).collect();
        let (mut minx, mut miny) = (f32::INFINITY, f32::INFINITY);
        let (mut maxx, mut maxy) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for k in 0..n {
            minx = minx.min(s.x[k]);
            maxx = maxx.max(s.x[k]);
            miny = miny.min(s.y[k]);
            maxy = maxy.max(s.y[k]);
        }
        let half = ((maxx - minx).max(maxy - miny) * 0.5).max(1.0);
        bh_build(
            &mut cells,
            &mut order,
            0,
            &s.x,
            &s.y,
            &s.charge,
            (minx + maxx) * 0.5,
            (miny + maxy) * 0.5,
            half,
            0,
        );
        // Error is normalized per node by its GROSS force (the sum of
        // contribution magnitudes) — the standard Barnes–Hut quality
        // metric. Net-relative error is meaningless on this seed: the
        // radial layout is symmetric, so most nodes' contributions nearly
        // cancel and any approximation of the large gross terms reads as
        // a huge fraction of the tiny net.
        // θ = 0 forces full descent: every interaction goes through a leaf
        // bucket, exactly once, with the exact pair formula — so the tree
        // must reproduce the reference to float-reordering noise. This is
        // the correctness proof (a missed or double-counted pair fails
        // loudly); the production-θ pass below then only measures
        // approximation quality.
        let mut stack = Vec::new();
        for i in 0..n {
            let (fx, fy) = bh_accumulate(
                &cells, &order, &mut stack, i, &s.x, &s.y, &s.charge, 1.0, 0.0,
            );
            let err = ((fx - ex[i]).powi(2) + (fy - ey[i]).powi(2)).sqrt() / gross[i].max(1e-6);
            assert!(
                err < 1e-4,
                "θ=0 must match the exact loop; node {i} err {err}"
            );
        }
        let mut worst = 0.0f32;
        let mut mean = 0.0f64;
        for i in 0..n {
            let (fx, fy) = bh_accumulate(
                &cells,
                &order,
                &mut stack,
                i,
                &s.x,
                &s.y,
                &s.charge,
                1.0,
                BH_THETA * BH_THETA,
            );
            let err = ((fx - ex[i]).powi(2) + (fy - ey[i]).powi(2)).sqrt() / gross[i].max(1e-6);
            worst = worst.max(err);
            mean += err as f64;
        }
        mean /= n as f64;
        assert!(worst < 0.05, "worst force error {worst} ≥ 5% of gross");
        assert!(mean < 0.025, "mean force error {mean} ≥ 2.5% of gross");
    }

    #[test]
    fn a_big_graph_settles_deterministically_on_the_tree_path() {
        let g = synth_flat(400); // > BH_MIN_NODES
        let mut s1 = Sim::new(&g);
        let mut s2 = Sim::new(&g);
        s1.tick(300);
        s2.tick(300);
        assert_eq!(s1.x, s2.x);
        assert_eq!(s1.y, s2.y);
        assert!(s1.x.iter().chain(&s1.y).all(|v| v.is_finite()));
        assert!(!s1.active(), "settles within 300 ticks");
    }

    #[test]
    fn coincident_nodes_separate_on_the_tree_path() {
        let g = synth_flat(300);
        let mut s = Sim::new(&g);
        s.x[20] = s.x[10]; // exact overlap, deep in a shared leaf bucket
        s.y[20] = s.y[10];
        s.tick(100);
        let d = ((s.x[10] - s.x[20]).powi(2) + (s.y[10] - s.y[20]).powi(2)).sqrt();
        assert!(d > 10.0, "overlapping nodes must separate, got {d}");
    }
}
