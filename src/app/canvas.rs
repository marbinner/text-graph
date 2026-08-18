//! The canvas: one frame of the graph view, as a pipeline of named
//! stages. `canvas()` is the order — read it top to bottom and you have
//! the frame: camera, sim, workers, input, cull, hover/gestures, scene,
//! then paint. PAINT ORDER IS STACKING ORDER (later stages draw on top,
//! and hit-testing follows last-rect-wins), so the sequence in
//! `canvas()` is load-bearing: edges under nodes, nodes under labels,
//! the active label last on its backdrop, terminal cards on top of the
//! graph, popups above those. The tether slot is the one exception that
//! proves the rule — tethers belong among the edges but their endpoints
//! are only known once the cards are laid out, so `paint_edges` reserves
//! a slot that `paint_terminals` fills.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Vec2};
use text_graph::filetype;
use text_graph::graph::{LinkKind, NodeId, NodeKind};

use super::terminals::{ResizeDrag, resize_handle};
use super::{GLYPH_MIN_R, ICON_MIN_R, Viewer, icon_font, images, label_lod, shade, terminals};

/// What this frame is about: the active node, its neighborhood, and the
/// lit mask — computed once between input and paint, read by every paint
/// stage.
struct Scene {
    /// Hover, else selection — the node the frame revolves around.
    active: Option<NodeId>,
    /// Wikilink partners of the active node (rings + full labels).
    partners: HashSet<NodeId>,
    /// Per node: paints at full strength. Search matches while a query is
    /// live; else the active neighborhood; else everyone.
    lit: Vec<bool>,
    /// A finder query is live — search matches own the lit mask.
    searching: bool,
    /// The finder row the cursor is on (ring + label reveal).
    cursor_node: Option<NodeId>,
}

impl Viewer {
    pub(super) fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.frame_camera(ui, rect);
        self.step_sim(ui);
        self.pump_workers(ui);
        let over_card = self.pointer_input(ui, rect, &response);
        // the cull margin: nodes just off-screen still paint, so edges to
        // them don't pop while panning
        let view = rect.expand(60.0);
        let visible = self.cull(rect, view);
        let active = self.hover_and_gestures(ui, rect, &response, &visible, &over_card);
        let scene = self.scene(active);

        let painter = ui.painter_at(rect);
        let tether_slot = self.paint_edges(&painter, rect, view, &scene);
        self.paint_nodes(ui, &painter, &visible, &scene);
        self.paint_labels(&painter, &response, &visible, &over_card, &scene);
        // terminal cards, on top of the graph (their tethers fill the
        // reserved under-node slot)
        self.paint_terminals(&painter, rect, view, tether_slot);

        // full-content hover preview (dwell to open; tooltip layer)
        self.hover_preview_ui(ui);
        // terminal peek: dwell on a compact card shows its full screen
        self.hover_peek_ui(ui);
        self.status_line(&painter, rect, &scene);
    }

    /// Camera per-frame step: rect-change compensation, the glide
    /// animation, and the initial whole-graph fit.
    fn frame_camera(&mut self, ui: &egui::Ui, rect: Rect) {
        // rect moved (panel toggled, window resized) → keep every world
        // point at its screen position; the no-slide rule lives in camera.rs
        self.cam.compensate(rect);
        if let Some((from, id, t0)) = self.cam.anim {
            if (id.0 as usize) < self.g.nodes.len() {
                // glide duration is a setting; 0 means jump
                let t = if self.cfg.glide <= 0.0 {
                    1.0
                } else {
                    (t0.elapsed().as_secs_f32() / self.cfg.glide).min(1.0)
                };
                let e = 1.0 - (1.0 - t) * (1.0 - t); // ease-out
                self.cam.center = from.lerp(self.frame_target(id.0 as usize, rect), e);
                if t >= 1.0 {
                    self.cam.anim = None;
                }
                ui.ctx().request_repaint();
            } else {
                self.cam.anim = None; // graph swapped underneath
            }
        }
        if !self.cam.fitted {
            self.fit(rect);
            self.cam.fitted = true;
        }
    }

    fn step_sim(&mut self, ui: &egui::Ui) {
        self.sim.configure(self.cfg.spread, self.cfg.freeze);
        if self.sim.active() {
            self.sim.tick(3);
            ui.ctx().request_repaint();
        }
    }

    /// Agent terminals (discovery snapshot, mirrors, screen caches) and
    /// the thumbnail decode worker.
    fn pump_workers(&mut self, ui: &egui::Ui) {
        let ctx = ui.ctx().clone();
        self.sync_terminals(&ctx);
        self.thumbs.pump(&ctx);
    }

    /// Pointer interaction with the world: card drag/resize, node drag,
    /// empty-space pan, scroll/pinch zoom. Returns the card under the
    /// cursor — cards sit on top and win pointer contention (over_card
    /// and hover use last frame's geometry: standard immediate-mode lag,
    /// imperceptible at interactive frame rates).
    fn pointer_input(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
    ) -> Option<(String, String)> {
        // Cards sit on top and win pointer contention. over_card and hover
        // use last frame's geometry — standard immediate-mode lag,
        // imperceptible at interactive frame rates.
        let over_card: Option<(String, String)> = response.hover_pos().and_then(|c| {
            // reverse: cards are painted in order, so the LAST rect containing
            // the cursor is the one visibly on top
            self.terms
                .rects
                .iter()
                .rev()
                .find(|(_, _, r)| r.contains(c))
                .map(|(s, p, _)| (s.clone(), p.clone()))
        });
        if response.drag_started() {
            if let Some(t) = over_card.clone() {
                // corner grip on explicitly owned sessions = native resize; the
                // rest of the card moves it. Foreign sessions never get
                // resized from here — that would reflow someone's real
                // terminal view.
                let on_handle = response
                    .hover_pos()
                    .zip(
                        self.terms
                            .rects
                            .iter()
                            .rev()
                            .find(|(s, p, _)| (s, p) == (&t.0, &t.1)),
                    )
                    .is_some_and(|(pos, (_, _, r))| resize_handle(*r).contains(pos));
                let ours = self
                    .terms
                    .panes
                    .iter()
                    .find(|a| a.session == t.0 && a.pane == t.1)
                    .is_some_and(|a| a.ours);
                // no resize from compact LOD: the fixed summary box gives no
                // feedback and its ~1.5px cell advance makes a twitch reflow
                // the real session by dozens of columns. Cursor/focused cards
                // render expanded at any zoom, so they're never compact.
                let expanded = self.terms.is_expanded(&t);
                let compact = (6.0 * self.cam.zoom).clamp(2.5, 16.0) < 5.0 && !expanded;
                if on_handle
                    && ours
                    && !compact
                    && let Some(c) = self.terms.cache.get(&t)
                    && let Some(pos) = response.hover_pos()
                {
                    // cell metrics must match what the card is rendered at
                    let mut f = (6.0 * self.cam.zoom).clamp(2.5, 16.0);
                    if expanded {
                        f = f.max(terminals::EXPAND_FONT);
                    }
                    let probe = ui.painter().layout_no_wrap(
                        "M".into(),
                        FontId::monospace(f),
                        Color32::WHITE,
                    );
                    let cur = (c.cols, c.total_rows);
                    self.terms.resize = Some(ResizeDrag {
                        key: t,
                        cols0: cur.0 as f32,
                        rows0: cur.1 as f32,
                        start: pos,
                        adv: probe.size().x.max(1.0),
                        line_h: probe.size().y.max(1.0),
                        want: cur,
                        sent: cur,
                        last_sent: Instant::now(),
                    });
                } else {
                    // seed the override from where the card currently is, so
                    // the first dragged frame doesn't jump
                    let cur_center = self
                        .terms
                        .rects
                        .iter()
                        .find(|(s, p, _)| (s, p) == (&t.0, &t.1))
                        .map(|(_, _, r)| r.center());
                    let anchor_s = self
                        .terms
                        .panes
                        .iter()
                        .find(|a| a.session == t.0 && a.pane == t.1)
                        .map(|a| {
                            let id = self.card_anchor(a);
                            self.cam.to_screen(rect, self.world_pos(id.0 as usize))
                        });
                    if let (Some(center), Some(anchor_s)) = (cur_center, anchor_s) {
                        self.terms
                            .offsets
                            .insert(t.clone(), (center - anchor_s) / self.cam.zoom);
                    }
                    self.terms.drag_card = Some(t);
                }
            } else {
                self.drag_node = self.hover;
            }
        }
        if response.dragged() {
            if self.terms.resize.is_some() {
                if let (Some(rz), Some(cur)) =
                    (self.terms.resize.as_mut(), response.interact_pointer_pos())
                {
                    let cols = (rz.cols0 + (cur.x - rz.start.x) / rz.adv).round();
                    let rows = (rz.rows0 + (cur.y - rz.start.y) / rz.line_h).round();
                    rz.want = (cols.clamp(20.0, 220.0) as u16, rows.clamp(5.0, 80.0) as u16);
                    if rz.want != rz.sent && rz.last_sent.elapsed() > Duration::from_millis(90) {
                        let (cols, rows) = rz.want;
                        let cmd = format!("resize-window -t {} -x {cols} -y {rows}", rz.key.1);
                        let sess = rz.key.0.clone();
                        rz.sent = rz.want;
                        rz.last_sent = Instant::now();
                        if let Some(m) = self.terms.mirrors.get_mut(&sess) {
                            m.command(&cmd);
                        }
                    }
                }
                ui.ctx().request_repaint();
            } else if let Some(t) = self.terms.drag_card.clone() {
                if let Some(off) = self.terms.offsets.get_mut(&t) {
                    *off += response.drag_delta() / self.cam.zoom;
                }
                ui.ctx().request_repaint();
            } else if let (Some(id), Some(cur)) = (self.drag_node, response.interact_pointer_pos())
            {
                let w = self.cam.to_world(rect, cur);
                self.sim.pin(id.0, w.x, w.y);
                ui.ctx().request_repaint();
            } else {
                self.cam.cancel_glide(); // manual pan wins over a glide
                self.cam.center -= response.drag_delta() / self.cam.zoom;
            }
        }
        if response.drag_stopped() {
            self.sim.unpin();
            self.drag_node = None;
            self.terms.drag_card = None;
            if let Some(rz) = self.terms.resize.take()
                && rz.want != rz.sent
                && let Some(m) = self.terms.mirrors.get_mut(&rz.key.0)
            {
                // flush the debounced tail so the final size always lands
                m.command(&format!(
                    "resize-window -t {} -x {} -y {}",
                    rz.key.1, rz.want.0, rz.want.1
                ));
            }
        }
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = pinch * (scroll * 0.0025 * self.cfg.zoom_speed).exp();
        if factor != 1.0
            && let Some(cursor) = response.hover_pos()
        {
            // keep the world point under the cursor fixed while zooming
            self.cam.cancel_glide();
            let anchor = self.cam.to_world(rect, cursor);
            self.cam.zoom = (self.cam.zoom * factor).clamp(0.02, 50.0);
            self.cam.center = anchor - (cursor - rect.center()) / self.cam.zoom;
        }
        over_card
    }

    /// Every node inside `view`, with its screen position and radius.
    fn cull(&self, rect: Rect, view: Rect) -> Vec<(NodeId, Pos2, f32)> {
        let mut visible: Vec<(NodeId, Pos2, f32)> = Vec::new();
        for i in 0..self.g.nodes.len() {
            // hidden web nodes are skipped at render/hit-test only — the
            // sim keeps simulating them, so toggling never reflows
            if !self.show_web && self.g.nodes[i].kind == NodeKind::Web {
                continue;
            }
            let s = self.cam.to_screen(rect, self.world_pos(i));
            // images render as thumbnails, so their "radius" is the box
            // half-height with a much larger cap — hover targets, rings, and
            // label anchors all follow it. Their cull test must cover the
            // box extent (half-width ≤ 1.5r), or a picture straddling the
            // viewport edge pops in and out while panning.
            let r = if self.g.nodes[i].kind == NodeKind::Image {
                let r = (self.radius[i] * 2.0 * self.cam.zoom).clamp(1.5, 110.0);
                if !view.expand2(Vec2::new(r * 1.5, r)).contains(s) {
                    continue;
                }
                r
            } else {
                if !view.contains(s) {
                    continue;
                }
                (self.radius[i] * self.cam.zoom).clamp(1.5, 16.0)
            };
            visible.push((NodeId(i as u32), s, r));
        }
        visible
    }

    /// Hover pick, dwell tracking, and the click/double/right-click
    /// gestures. Returns the active node (hover, else selection).
    fn hover_and_gestures(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        visible: &[(NodeId, Pos2, f32)],
        over_card: &Option<(String, String)>,
    ) -> Option<NodeId> {
        if let Some(id) = self.drag_node {
            self.hover = Some(id);
        } else {
            let prev = self.hover;
            self.hover = None;
            if over_card.is_none()
                && let Some(cursor) = response.hover_pos()
            {
                let mut best = f32::INFINITY;
                for &(id, s, r) in visible {
                    let d = s.distance(cursor);
                    // an expanded node (thumbnail / preview card) is wider
                    // than its nominal radius — hover anywhere on it, not a
                    // circle around its center (the mismatch made hover
                    // flicker at the corners of wide boxes). The previously
                    // hovered node gets hysteresis slack: while the sim
                    // drifts (settling, reload ripples) a node must not
                    // escape the stationary cursor mid-dwell.
                    let hit = if let Some(bx) = self.node_box(id, s, r) {
                        bx.expand(4.0).contains(cursor)
                    } else {
                        d < r + 4.0 || (prev == Some(id) && d < r + 16.0)
                    };
                    if hit && d < best {
                        best = d;
                        self.hover = Some(id);
                    }
                }
            }
        }
        // dwell tracking for the full hover preview: the anchor freezes at
        // dwell start so the popup doesn't chase the pointer; any drag
        // (pan, node, card) resets it
        if response.dragged() {
            self.hover_since = None;
            self.terms.hover_since = None;
        } else {
            match (self.hover, self.hover_since) {
                (Some(h), Some((id, ..))) if id == h => {}
                (Some(h), _) => {
                    let a = response.hover_pos().unwrap_or_else(|| rect.center());
                    self.hover_since = Some((h, Instant::now(), a));
                }
                (None, _) => self.hover_since = None,
            }
            // same dwell tracking for terminal cards (drives the peek popup)
            match (over_card, &self.terms.hover_since) {
                (Some(k), Some((hk, ..))) if hk == k => {}
                (Some(k), _) => {
                    let a = response.hover_pos().unwrap_or_else(|| rect.center());
                    self.terms.hover_since = Some((k.clone(), Instant::now(), a));
                }
                (None, _) => self.terms.hover_since = None,
            }
        }
        if response.clicked() {
            if let Some(t) = over_card.clone() {
                if ui.input(|i| i.modifiers.command) {
                    // Ctrl+click pins the card open (expanded at any zoom,
                    // several at once) without touching keyboard focus;
                    // Ctrl+click again unpins
                    self.terms.toggle_pin(t);
                } else {
                    self.terms.cursor = Some(t.clone());
                    self.terms.focused = Some(t);
                    // the pane owns the keyboard now — the picker's prompt
                    // would swallow every keystroke meant for it
                    self.picker.close();
                }
            } else {
                // click-away releases terminal focus AND lands as a normal
                // graph click in the same gesture — selecting a node must
                // never cost a second click. But a release-click on EMPTY
                // space only releases: it must not also close the navigator
                // pane the selection holds open.
                let was_focused = self.terms.focused.take().is_some();
                self.terms.cursor = None;
                if self.hover.is_some() || !was_focused {
                    self.selected = self.hover;
                }
            }
        }
        if response.double_clicked() {
            if let Some(t) = over_card.clone() {
                self.fly_to_card(t);
            } else if let Some(h) = self.hover {
                self.open_in_editor(h);
            }
        }
        if response.secondary_clicked() {
            // right-click on a card targets its anchor dir; on a node, that
            // node; on empty space, the vault root (ctx_node = None)
            self.ctx_card = over_card.clone();
            self.ctx_node = if let Some(t) = over_card {
                self.terms
                    .panes
                    .iter()
                    .find(|a| a.session == t.0 && a.pane == t.1)
                    .map(|a| self.card_anchor(a))
            } else {
                self.hover
            };
        }
        response.context_menu(|ui| self.context_menu_ui(ui));
        self.hover.or(self.selected)
    }

    fn scene(&self, active: Option<NodeId>) -> Scene {
        // Neighborhood of the active node: tree parent + children + wikilink
        // partners. Everything else dims (the Obsidian hover effect).
        let mut neighbors: HashSet<NodeId> = HashSet::new();
        let mut partners: HashSet<NodeId> = HashSet::new();
        if let Some(a) = active {
            neighbors.insert(a);
            let node = self.g.node(a);
            if let Some(p) = node.parent {
                neighbors.insert(p);
            }
            for c in &node.children {
                neighbors.insert(*c);
            }
            for l in &self.g.links {
                if l.from == a {
                    partners.insert(l.to);
                    neighbors.insert(l.to);
                } else if l.to == a {
                    partners.insert(l.from);
                    neighbors.insert(l.from);
                }
            }
        }

        // ---- lit mask: search matches win; else the active neighborhood ----
        let searching = self.picker.searching();
        let cursor_node = self.picker.cursor_node();
        let n_nodes = self.g.nodes.len();
        let lit: Vec<bool> = if searching {
            self.picker
                .node_scores
                .iter()
                .map(Option::is_some)
                .collect()
        } else if active.is_some() {
            (0..n_nodes)
                .map(|i| neighbors.contains(&NodeId(i as u32)))
                .collect()
        } else {
            vec![true; n_nodes]
        };
        Scene {
            active,
            partners,
            lit,
            searching,
            cursor_node,
        }
    }

    /// Tree edges and wikilink curves, under everything — and the
    /// reserved tether slot (returned) that `paint_terminals` fills.
    fn paint_edges(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        view: Rect,
        scene: &Scene,
    ) -> egui::layers::ShapeIdx {
        // contains edges (under everything)
        for (i, node) in self.g.nodes.iter().enumerate() {
            let Some(parent) = node.parent else { continue };
            let sa = self.cam.to_screen(rect, self.world_pos(i));
            let sb = self.cam.to_screen(rect, self.world_pos(parent.0 as usize));
            // bbox test, not endpoint containment: a long edge crossing the
            // viewport must not vanish when both ends are off-screen
            if !view.intersects(Rect::from_two_pos(sa, sb)) {
                continue;
            }
            let on = scene.lit[i] && scene.lit[parent.0 as usize];
            let color = if on {
                self.theme.edge_tree
            } else {
                self.theme.edge_tree.gamma_multiply(self.cfg.focus_fade)
            };
            // sa = child, sb = parent — the wedge thins toward the child
            paint_tree_edge(painter, sb, sa, color);
        }

        // wikilink edges — always visible as faint curves, bright when they
        // touch the active node
        for l in &self.g.links {
            if l.kind == LinkKind::External && !self.show_web {
                continue;
            }
            let sa = self.cam.to_screen(rect, self.world_pos(l.from.0 as usize));
            let sb = self.cam.to_screen(rect, self.world_pos(l.to.0 as usize));
            if !view.intersects(Rect::from_two_pos(sa, sb)) {
                continue;
            }
            let bright = scene.active == Some(l.from) || scene.active == Some(l.to);
            let on = scene.lit[l.from.0 as usize] && scene.lit[l.to.0 as usize];
            // external (citation) edges: cyan and fainter — context, not
            // structure
            let (hue, rest_a) = match l.kind {
                LinkKind::WikiLink => (self.theme.wiki, 0.35),
                LinkKind::External => (self.theme.web, 0.22),
            };
            let (color, width) = if bright {
                (hue, 1.8)
            } else if on {
                (hue.gamma_multiply(rest_a), 1.0)
            } else {
                (hue.gamma_multiply(self.cfg.focus_fade), 1.0)
            };
            let mid = sa.lerp(sb, 0.5);
            let d = sb - sa;
            let ctrl = mid + Vec2::new(-d.y, d.x) * 0.18;
            painter.add(egui::epaint::QuadraticBezierShape::from_points_stroke(
                [sa, ctrl, sb],
                false,
                Color32::TRANSPARENT,
                Stroke::new(width, color),
            ));
            // arrowhead at the target end, pulled back to the node's rim so
            // the icon painted on top doesn't swallow it
            let tr = (self.radius[l.to.0 as usize] * self.cam.zoom).clamp(1.5, 16.0) + 4.0;
            let tangent = sb - ctrl;
            if (sb - sa).length() > tr + 14.0 && tangent.length() > 0.5 {
                let tip = sb - tangent.normalized() * tr;
                paint_arrowhead(painter, tip, tangent, width * 2.2 + 3.0, color);
            }
        }

        // Card tethers belong with the edges — UNDER node icons — but their
        // endpoints are only known once paint_terminals lays the cards out.
        // Reserve a slot here; paint_terminals fills it.
        painter.add(egui::Shape::Noop)
    }

    /// The nodes: glyphs/discs, preview cards, image thumbnails, and the
    /// selection/hover/cursor/partner rings (rings follow the painted
    /// shape).
    fn paint_nodes(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        visible: &[(NodeId, Pos2, f32)],
        scene: &Scene,
    ) {
        // What the finder is highlighting opens up wherever it sits: the
        // preview card for a note, the picture for an image. You are
        // looking at it in the list; the graph should show you the thing,
        // not a dot you have to zoom into.
        let opened = self.highlighted_node();

        // nodes
        for &(id, s, r) in visible {
            let node = self.g.node(id);
            let on = scene.lit[id.0 as usize];
            let fade = self.cfg.focus_fade;
            let dimmed = |c: Color32| if on { c } else { c.gamma_multiply(fade) };
            // Big enough to read, a node paints as its file-type glyph
            // (Nerd Font icons — python is the python logo, css the css
            // shield); below that, a colored disc. Ghosts stay hollow.
            let glyph = r >= GLYPH_MIN_R;
            match node.kind {
                NodeKind::Web => {
                    // a cited URL — cyan globe once readable, dot below
                    if glyph {
                        paint_glyph_node(
                            painter,
                            s,
                            r,
                            filetype::ICON_WEB.glyph,
                            dimmed(self.theme.web),
                            dimmed(self.theme.bg),
                        );
                    } else {
                        painter.circle_filled(s, r, dimmed(self.theme.web));
                    }
                }
                NodeKind::Ghost => {
                    painter.circle_stroke(s, r, Stroke::new(1.2, dimmed(self.theme.ghost)));
                    if r >= ICON_MIN_R {
                        // an unwritten page, hollow like its node
                        paint_doc_icon(painter, s, r, None, Some(dimmed(self.theme.ghost)));
                    }
                }
                NodeKind::Dir => {
                    let col = dir_depth_color(self.theme.dir, self.depths[id.0 as usize]);
                    if glyph {
                        paint_glyph_node(
                            painter,
                            s,
                            r,
                            filetype::ICON_FOLDER.glyph,
                            dimmed(col),
                            dimmed(self.theme.bg),
                        );
                    } else {
                        painter.circle_filled(s, r, dimmed(col));
                    }
                }
                NodeKind::File | NodeKind::Asset => {
                    let disc = if node.kind == NodeKind::File {
                        self.theme.file
                    } else {
                        self.theme.asset
                    };
                    // zoomed in far enough, a textual leaf opens into a
                    // preview card (the canvas sibling of the detail pane);
                    // presence fades so the disc↔card flip never pops.
                    // Binary assets stay discs at every zoom.
                    let ur = self.radius[id.0 as usize] * self.cam.zoom;
                    let open_here = opened == Some(id);
                    let want = self.cfg.canvas_previews
                        && (ur >= Self::PREVIEW_MIN_R || open_here)
                        && self.previewable(id);
                    let key = node.path_key();
                    let presence = ui.ctx().animate_value_with_time(
                        egui::Id::new(("preview", &key)),
                        if want { 1.0 } else { 0.0 },
                        0.12,
                    );
                    if presence < 0.95 {
                        if glyph {
                            let icon = filetype::icon_of(&node.path);
                            let color = self.icon_color(icon);
                            paint_glyph_node(
                                painter,
                                s,
                                r,
                                icon.glyph,
                                dimmed(color),
                                dimmed(self.theme.bg),
                            );
                        } else {
                            painter.circle_filled(s, r, dimmed(disc));
                        }
                    }
                    if presence >= 0.05 {
                        // dim fades like image tint — a big card snapping
                        // between bright and near-black reads as flicker
                        let dim_a = ui.ctx().animate_value_with_time(
                            egui::Id::new(("preview-dim", &key)),
                            if on { 1.0 } else { fade },
                            0.15,
                        );
                        let a = presence * dim_a;
                        let bx = self.preview_box(id, s, open_here);
                        painter.rect_filled(bx, 3.0, self.theme.panel.gamma_multiply(a));
                        painter.rect_stroke(
                            bx,
                            3.0,
                            Stroke::new(1.0, self.theme.edge.gamma_multiply(a)),
                            egui::StrokeKind::Outside,
                        );
                        let fs = (ur.max(if open_here { Self::OPENED_MIN_R } else { 0.0 }) * 0.22)
                            .clamp(6.5, 12.0);
                        // notes read as prose, assets read as code
                        let font = if node.kind == NodeKind::File {
                            FontId::proportional(fs)
                        } else {
                            FontId::monospace(fs * 0.92)
                        };
                        let Some(path) = node.absolute_path(&self.root) else {
                            continue;
                        };
                        let text = self
                            .previews
                            .get_or_load(&key, &path, node.kind == NodeKind::File)
                            .to_string();
                        let galley = painter.layout(
                            text,
                            font,
                            self.theme.text.gamma_multiply(a),
                            bx.width() - 12.0,
                        );
                        painter.with_clip_rect(bx.shrink(2.0)).galley(
                            bx.min + Vec2::new(6.0, 5.0),
                            galley,
                            self.theme.text,
                        );
                    }
                }
                NodeKind::Image => {
                    let open_here = opened == Some(id);
                    if (r < Self::IMG_BOX_MIN_R && !open_here) || !self.cfg.thumbnails {
                        painter.circle_filled(s, r, dimmed(self.theme.img));
                    } else {
                        // decode on demand, draw the thumbnail once it lands;
                        // a framed placeholder with the photo glyph meanwhile
                        let key = node.path_key();
                        let Some(path) = node.absolute_path(&self.root) else {
                            continue;
                        };
                        self.thumbs.request(ui.ctx(), &key, path);
                        let bx = self.image_box(
                            id,
                            s,
                            if open_here {
                                r.max(Self::OPENED_MIN_R)
                            } else {
                                r
                            },
                        );
                        // dim/undim FADES for pictures: the neighborhood
                        // dim snapping a photo between bright and near-black
                        // on every hover change read as flicker
                        let target = if on { 1.0 } else { fade };
                        let t = ui.ctx().animate_value_with_time(
                            egui::Id::new(("img-tint", &key)),
                            target,
                            0.15,
                        );
                        let tint = Color32::WHITE.gamma_multiply(t);
                        match self.thumbs.cache.get(&key) {
                            Some(images::ThumbState::Ready { tex, .. }) => {
                                painter.image(
                                    tex.id(),
                                    bx,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    tint,
                                );
                            }
                            _ => {
                                painter.rect_filled(bx, 2.0, dimmed(self.theme.panel));
                                paint_img_icon(
                                    painter,
                                    s,
                                    r.min(14.0),
                                    dimmed(self.theme.img),
                                    dimmed(self.theme.panel),
                                );
                            }
                        }
                        painter.rect_stroke(
                            bx,
                            2.0,
                            Stroke::new(1.0, dimmed(self.theme.edge)),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }
            // rings follow the painted shape: expanded boxes (thumbnails,
            // preview cards) get a rect ring, everything else the circle
            let boxed = self.node_box(id, s, r);
            let ring = |color: Color32, width: f32| {
                if let Some(b) = boxed {
                    painter.rect_stroke(
                        b.expand(3.0),
                        2.0,
                        Stroke::new(width, color),
                        egui::StrokeKind::Outside,
                    );
                } else {
                    painter.circle_stroke(s, r + 3.0, Stroke::new(width, color));
                }
            };
            if scene.active == Some(id) {
                let color = if self.selected == Some(id) {
                    self.theme.select
                } else {
                    self.theme.hover
                };
                ring(color, 2.0);
            } else if scene.cursor_node == Some(id) {
                ring(self.theme.hover, 2.0);
            } else if scene.partners.contains(&id) {
                ring(self.theme.wiki, 1.5);
            }
        }
    }

    /// Node labels (LOD ramp + cursor flashlight), then the active
    /// node's label last of all, on a backdrop.
    fn paint_labels(
        &self,
        painter: &egui::Painter,
        response: &Response,
        visible: &[(NodeId, Pos2, f32)],
        over_card: &Option<(String, String)>,
        scene: &Scene,
    ) {
        // labels — LOD by screen radius; always for the active neighborhood;
        // plus the cursor flashlight: nodes near the pointer reveal their
        // names (distance-faded) even when zoomed out below the LOD cutoff
        let reveal: HashMap<NodeId, f32> = match response.hover_pos() {
            Some(c) if over_card.is_none() => reveal_near_cursor(c, visible, &scene.lit)
                .into_iter()
                .collect(),
            _ => HashMap::new(),
        };
        for &(id, s, r) in visible {
            if scene.active == Some(id) {
                continue; // painted LAST, on a backdrop — see below
            }
            let node = self.g.node(id);
            // full strength for the active/partner/search cases; otherwise
            // the LOD ramp and the cursor flashlight compete — whichever
            // reveals harder wins
            let full = if scene.searching {
                scene.lit[id.0 as usize] && (r >= 3.0 || scene.cursor_node == Some(id))
            } else {
                scene.active == Some(id) || scene.partners.contains(&id)
            };
            let lod = if !scene.searching && scene.lit[id.0 as usize] {
                label_lod(node.kind, r, self.cfg.label_density)
            } else {
                0.0
            };
            let fade = reveal.get(&id).copied().unwrap_or(0.0);
            let strength = if full { 1.0 } else { lod.max(fade) };
            if strength <= 0.0 {
                continue;
            }
            // folder labels are blue and scale with the node — parent
            // folders read as large bright anchors in the label soup
            let is_dir = node.kind == NodeKind::Dir;
            let color = if scene.active == Some(id) {
                self.theme.hover
            } else if is_dir {
                dir_depth_color(self.theme.dir, self.depths[id.0 as usize])
                    .gamma_multiply(0.45 + 0.55 * strength)
            } else {
                self.theme.text.gamma_multiply(0.35 + 0.65 * strength)
            };
            let font = if is_dir {
                FontId::proportional((r * 1.15).clamp(11.5, 16.5))
            } else {
                FontId::proportional(11.5)
            };
            // an expanded box is wider than r — hang the label off its
            // right edge, not the nominal radius
            let anchor = if let Some(b) = self.node_box(id, s, r) {
                Pos2::new(b.max.x + 5.0, s.y)
            } else {
                s + Vec2::new(r + 5.0, 0.0)
            };
            painter.text(
                anchor,
                Align2::LEFT_CENTER,
                node.display_name(),
                font,
                color,
            );
        }

        // The hovered/selected node's label paints last of all, over a
        // dark backdrop — whatever the cursor is on must be readable, not
        // buried in the label soup or under a neighbor's preview box.
        if let Some(aid) = scene.active
            && let Some(&(_, s, r)) = visible.iter().find(|(id, ..)| *id == aid)
        {
            let node = self.g.node(aid);
            let font = if node.kind == NodeKind::Dir {
                FontId::proportional((r * 1.15).clamp(11.5, 16.5))
            } else {
                FontId::proportional(11.5)
            };
            let anchor = if let Some(b) = self.node_box(aid, s, r) {
                Pos2::new(b.max.x + 5.0, s.y)
            } else {
                s + Vec2::new(r + 5.0, 0.0)
            };
            let galley = painter.layout_no_wrap(node.display_name().into(), font, self.theme.hover);
            let pos = Pos2::new(anchor.x, anchor.y - galley.size().y * 0.5);
            let back = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(4.0, 2.0));
            painter.rect_filled(back, 4.0, self.theme.bg.gamma_multiply(0.88));
            painter.galley(pos, galley, self.theme.hover);
        }
    }

    /// The status line: flash message, focused/cursor terminal hints,
    /// search tally, active node path, or the vault summary.
    fn status_line(&mut self, painter: &egui::Painter, rect: Rect, scene: &Scene) {
        if self
            .flash
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(6))
        {
            self.flash = None;
        }
        let status = if let Some((msg, _)) = &self.flash {
            msg.clone()
        } else if let Some((s, p)) = &self.terms.focused {
            format!("typing into {s} {p} — Ctrl+Q or click away releases")
        } else if scene.searching {
            let count = self.picker.node_scores.iter().flatten().count();
            let tcount = self.terms.scores.iter().flatten().count();
            let terms = if tcount > 0 {
                format!(" + {tcount} terminals")
            } else {
                String::new()
            };
            format!(
                "{count} node{}{terms} lit — the highlighted result is framed here",
                if count == 1 { "" } else { "s" }
            )
        } else if scene.active.is_none()
            && let Some((s, p)) = &self.terms.cursor
        {
            format!("{s} {p} — Enter types into it · Ctrl+click pins open · Esc dismisses")
        } else {
            match scene.active.map(|id| self.g.node(id)) {
                Some(n) => {
                    let what = if n.path.is_empty() { &n.name } else { &n.path };
                    if n.kind == NodeKind::Ghost {
                        format!("[[{what}]] — not written yet")
                    } else {
                        what.clone()
                    }
                }
                None => format!(
                    "{} files · {} dirs{}{}{} · {} links{}   |   f find · b browse · hjkl move · s/d zoom · z center · w web · 0 reset  |  on selection: e edit · t terminal · a agent",
                    self.n_files,
                    self.n_dirs,
                    if self.n_images > 0 {
                        format!(" · {} images", self.n_images)
                    } else {
                        String::new()
                    },
                    if self.n_assets > 0 {
                        format!(" · {} assets", self.n_assets)
                    } else {
                        String::new()
                    },
                    if self.n_webs > 0 {
                        format!(" · {} web", self.n_webs)
                    } else {
                        String::new()
                    },
                    self.g.links.len(),
                    if self.sim.active() {
                        " · settling…"
                    } else {
                        ""
                    },
                ),
            }
        };
        painter.text(
            rect.left_top() + Vec2::new(10.0, 8.0),
            Align2::LEFT_TOP,
            status,
            FontId::proportional(12.0),
            self.theme.text,
        );
    }
}

/// A file-type glyph centered on the node, over a canvas-colored backing
/// disc — edges crossing behind a thin glyph would shred it, and the old
/// solid discs occluded them the same way.
fn paint_glyph_node(
    p: &egui::Painter,
    c: Pos2,
    r: f32,
    glyph: char,
    color: Color32,
    backing: Color32,
) {
    p.circle_filled(c, r, backing);
    p.text(c, Align2::CENTER_CENTER, glyph, icon_font(r * 2.3), color);
}

/// Cursor flashlight: labels within this screen distance of the pointer are
/// revealed even when the zoom LOD would hide them.
const REVEAL_R: f32 = 130.0;
/// Cap on flashlight labels — a dense zoomed-out cluster would otherwise
/// dissolve into text soup.
const REVEAL_MAX: usize = 12;

/// The nearest lit nodes within `REVEAL_R` of the cursor, each with a 0..=1
/// proximity weight (1 at the cursor). Capped at `REVEAL_MAX`, nearest
/// first, ties broken by node index so the pick is deterministic.
fn reveal_near_cursor(
    cursor: Pos2,
    visible: &[(NodeId, Pos2, f32)],
    lit: &[bool],
) -> Vec<(NodeId, f32)> {
    let mut near: Vec<(f32, NodeId)> = visible
        .iter()
        .filter(|(id, _, _)| lit[id.0 as usize])
        .filter_map(|&(id, s, _)| {
            let d = s.distance(cursor);
            (d < REVEAL_R).then_some((d, id))
        })
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    near.truncate(REVEAL_MAX);
    near.into_iter()
        .map(|(d, id)| (id, 1.0 - d / REVEAL_R))
        .collect()
}

/// The folder color at a given tree depth: the root's folders are the
/// brightest blue on the canvas, each level a shade darker.
fn dir_depth_color(base: Color32, depth: u8) -> Color32 {
    shade(base, 1.0 - depth.min(4) as f32 * 0.10)
}

/// Tapered tree edge: thick at the parent, thin at the child — hierarchy
/// and direction readable without an arrowhead per edge.
fn paint_tree_edge(p: &egui::Painter, parent: Pos2, child: Pos2, color: Color32) {
    let d = child - parent;
    let len = d.length();
    if len < 1.0 {
        return;
    }
    let perp = Vec2::new(-d.y, d.x) / len;
    let (wp, wc) = (2.1, 0.35);
    p.add(egui::Shape::convex_polygon(
        vec![
            parent + perp * wp,
            parent - perp * wp,
            child - perp * wc,
            child + perp * wc,
        ],
        color,
        Stroke::NONE,
    ));
}

/// Filled arrowhead with its TIP at `tip`, pointing along `dir`.
fn paint_arrowhead(p: &egui::Painter, tip: Pos2, dir: Vec2, size: f32, color: Color32) {
    let len = dir.length();
    if len < 0.5 {
        return;
    }
    let d = dir / len;
    let perp = Vec2::new(-d.y, d.x);
    p.add(egui::Shape::convex_polygon(
        vec![
            tip,
            tip - d * size + perp * (size * 0.45),
            tip - d * size - perp * (size * 0.45),
        ],
        color,
        Stroke::NONE,
    ));
}

/// Photo silhouette: a punched-out landscape frame with a sun dot and a
/// mountain wedge back in the disc color.
fn paint_img_icon(p: &egui::Painter, c: Pos2, r: f32, punch: Color32, disc: Color32) {
    let w = r * 1.05;
    let h = r * 0.85;
    let frame = Rect::from_center_size(c, Vec2::new(w, h));
    p.rect_filled(frame, r * 0.10, punch);
    p.circle_filled(frame.min + Vec2::new(w * 0.30, h * 0.32), r * 0.11, disc);
    let base = frame.max.y - h * 0.16;
    let pts = vec![
        Pos2::new(frame.min.x + w * 0.14, base),
        Pos2::new(frame.min.x + w * 0.55, frame.min.y + h * 0.40),
        Pos2::new(frame.min.x + w * 0.88, base),
    ];
    p.add(egui::Shape::convex_polygon(pts, disc, Stroke::NONE));
}

/// Dog-eared page. `fill` paints it solid (punch-out on filled discs);
/// `outline` strokes it instead (hollow ghosts).
fn paint_doc_icon(
    p: &egui::Painter,
    c: Pos2,
    r: f32,
    fill: Option<Color32>,
    outline: Option<Color32>,
) {
    let w = r * 0.78;
    let h = r * 1.02;
    let page = Rect::from_center_size(c, Vec2::new(w, h));
    let ear = w * 0.40;
    let pts = vec![
        page.min,
        Pos2::new(page.max.x - ear, page.min.y),
        Pos2::new(page.max.x, page.min.y + ear),
        Pos2::new(page.max.x, page.max.y),
        Pos2::new(page.min.x, page.max.y),
    ];
    if let Some(color) = fill {
        p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else if let Some(color) = outline {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.0, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis(pts: &[(f32, f32)]) -> Vec<(NodeId, Pos2, f32)> {
        pts.iter()
            .enumerate()
            .map(|(i, &(x, y))| (NodeId(i as u32), Pos2::new(x, y), 3.0))
            .collect()
    }

    #[test]
    fn nearer_nodes_weigh_more_and_far_ones_drop() {
        let v = vis(&[(0.0, 0.0), (60.0, 0.0), (500.0, 0.0)]);
        let r = reveal_near_cursor(Pos2::ZERO, &v, &[true; 3]);
        let ids: Vec<u32> = r.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, vec![0, 1]); // 500px is outside the flashlight
        assert!((r[0].1 - 1.0).abs() < 1e-5);
        assert!(r[0].1 > r[1].1);
    }

    #[test]
    fn dimmed_nodes_stay_dark() {
        let v = vis(&[(0.0, 0.0), (10.0, 0.0)]);
        let r = reveal_near_cursor(Pos2::ZERO, &v, &[true, false]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, NodeId(0));
    }

    #[test]
    fn crowd_is_capped_deterministically() {
        // all at the same distance — the cap must keep the lowest indexes
        let pts: Vec<(f32, f32)> = (0..20).map(|_| (50.0, 0.0)).collect();
        let v = vis(&pts);
        let r = reveal_near_cursor(Pos2::ZERO, &v, &[true; 20]);
        assert_eq!(r.len(), REVEAL_MAX);
        let ids: Vec<u32> = r.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, (0..REVEAL_MAX as u32).collect::<Vec<_>>());
    }
}
