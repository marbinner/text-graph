//! Terminal cards: discovery sync, live mirrors, painting, keyboard
//! forwarding, gestures (fly-to, native resize), and session lifecycle
//! (launch / kill / external attach).

use super::actions::{detached, new_terminal_window};
use super::*;

/// All terminal-card state, grouped: discovery, mirrors, screen caches,
/// focus/cursor, arrangement, gestures, and search scores.
pub(super) struct Terminals {
    /// Written by the scanner thread, snapshotted each frame.
    pub(super) seen: Arc<Mutex<Vec<AgentPane>>>,
    pub(super) panes: Vec<AgentPane>,
    pub(super) mirrors: HashMap<String, SessionMirror>,
    /// Sessions whose last mirror attach failed, with the failure time —
    /// retried after a cooldown instead of at frame rate.
    pub(super) attach_backoff: HashMap<String, Instant>,
    /// (session, pane) → converted screen; refreshed per mirror generation.
    pub(super) cache: HashMap<(String, String), CachedPane>,
    pub(super) mirror_gen: HashMap<String, u64>,
    pub(super) activity: HashMap<String, Instant>,
    /// Keyboard goes to this pane instead of the graph.
    pub(super) focused: Option<(String, String)>,
    /// Card hit-boxes from the last paint (screen space).
    pub(super) rects: Vec<(String, String, Rect)>,
    /// User-arranged card positions: world-space offset of the card's min
    /// corner from its anchor node. Absent = automatic outward placement.
    pub(super) offsets: HashMap<(String, String), Vec2>,
    /// Parked arrangements (from disk, or from sessions that went away),
    /// keyed by session name: reclaimed when the session reappears.
    pub(super) parked: HashMap<String, Vec<(String, Vec2)>>,
    /// Card currently being dragged.
    pub(super) drag_card: Option<(String, String)>,
    /// Corner-grip resize in progress (tg_ sessions only — native resize).
    pub(super) resize: Option<ResizeDrag>,
    /// Set on double-click / t: next paint recenters the view on this card.
    pub(super) fly_to: Option<(String, String)>,
    /// Position of the t (cycle terminals) key, modulo the pane count.
    pub(super) cycle: usize,
    /// The card the terminal cursor is on: highlighted, Enter focuses it.
    pub(super) cursor: Option<(String, String)>,
    /// (card, dwell start, screen anchor) — drives the hover peek popup on
    /// compact cards, like `Viewer::hover_since` does for nodes.
    pub(super) hover_since: Option<((String, String), Instant, Pos2)>,
    /// Cards pinned open by Ctrl+click: expanded at any zoom, independent
    /// of focus/cursor — several agents watchable at once. Value-less map
    /// (not a set) so state.rs park/claim shepherd pins across tmux
    /// restarts exactly like arrangements.
    pub(super) pinned: HashMap<(String, String), ()>,
    /// Parked pins, keyed by session name (see `parked`).
    pub(super) parked_pins: HashMap<String, Vec<(String, ())>>,
    /// Flash messages from background threads (launch watchdogs), drained
    /// into the status line each frame.
    pub(super) bg_flash: Arc<Mutex<Vec<String>>>,
    /// Fuzzy-search scores for panes (aligned with `panes`).
    pub(super) scores: Vec<Option<u32>>,
    /// Best terminal hit: index at scoring time plus its key.
    pub(super) best: Option<(usize, (String, String))>,
    /// tmux presence, probed once at startup.
    pub(super) tmux_ok: bool,
}

impl Terminals {
    pub(super) fn new(
        parked: HashMap<String, Vec<(String, Vec2)>>,
        parked_pins: HashMap<String, Vec<(String, ())>>,
    ) -> Self {
        Terminals {
            seen: Arc::new(Mutex::new(Vec::new())),
            panes: Vec::new(),
            mirrors: HashMap::new(),
            attach_backoff: HashMap::new(),
            cache: HashMap::new(),
            mirror_gen: HashMap::new(),
            activity: HashMap::new(),
            focused: None,
            rects: Vec::new(),
            offsets: HashMap::new(),
            parked,
            drag_card: None,
            resize: None,
            fly_to: None,
            cycle: 0,
            cursor: None,
            hover_since: None,
            pinned: HashMap::new(),
            parked_pins,
            bg_flash: Arc::new(Mutex::new(Vec::new())),
            scores: Vec::new(),
            best: None,
            tmux_ok: std::process::Command::new("tmux")
                .arg("-V")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
        }
    }

    /// Cards that render their full screen at a readable size regardless of
    /// zoom: focused, cursor-selected, or pinned open.
    pub(super) fn is_expanded(&self, key: &(String, String)) -> bool {
        self.focused.as_ref() == Some(key)
            || self.cursor.as_ref() == Some(key)
            || self.pinned.contains_key(key)
    }
}

/// Zoom level a double-clicked card flies to — full styled screen, readable.
pub(super) const CARD_ZOOM: f32 = 2.2;

/// Font a cursor-selected/focused card expands to regardless of zoom.
pub(super) const EXPAND_FONT: f32 = 13.0;

/// Border/title color for the card that owns the keyboard.
const TERM_FOCUS: Color32 = Color32::from_rgb(0x56, 0xd4, 0xdd);

/// Bottom-right corner zone of a card that resizes instead of moving.
pub(super) fn resize_handle(card: Rect) -> Rect {
    Rect::from_min_max(card.max - Vec2::splat(16.0), card.max)
}

/// Two diagonal grip lines marking the resize corner.
pub(super) fn paint_resize_grip(p: &egui::Painter, card: Rect, color: Color32) {
    let m = card.max;
    p.line_segment(
        [
            Pos2::new(m.x - 11.0, m.y - 3.0),
            Pos2::new(m.x - 3.0, m.y - 11.0),
        ],
        Stroke::new(1.5, color),
    );
    p.line_segment(
        [
            Pos2::new(m.x - 6.0, m.y - 3.0),
            Pos2::new(m.x - 3.0, m.y - 6.0),
        ],
        Stroke::new(1.5, color),
    );
}

/// In-flight native resize of a graph-launched session via its corner grip:
/// the drag maps to `resize-window` on the real tmux pane (debounced), and
/// the card follows through the normal mirror resize path.
pub(super) struct ResizeDrag {
    pub(super) key: (String, String),
    pub(super) cols0: f32,
    pub(super) rows0: f32,
    pub(super) start: Pos2,
    pub(super) adv: f32,
    pub(super) line_h: f32,
    pub(super) want: (u16, u16),
    pub(super) sent: (u16, u16),
    pub(super) last_sent: Instant,
}

// ---- terminal cards ----
pub(super) const AGENT: Color32 = Color32::from_rgb(0x4e, 0xc9, 0x8b);

pub(super) const TERM_BG: Color32 = Color32::from_rgb(0x10, 0x13, 0x19);

pub(super) const TERM_BORDER: Color32 = Color32::from_rgb(0x3a, 0x40, 0x4d);

pub(super) const TERM_FG_T: (u8, u8, u8) = (0xc9, 0xd1, 0xd9);

pub(super) const TERM_BG_T: (u8, u8, u8) = (0x10, 0x13, 0x19);

/// One style-run of a terminal row, precomputed at cache time so painting is
/// a straight pass.
pub(super) struct Run {
    pub(super) start_col: u16,
    pub(super) text: String,
    pub(super) fg: Color32,
    pub(super) bg: Option<Color32>,
    pub(super) italic: bool,
    pub(super) underline: bool,
}

/// A pane's screen, converted once per mirror generation.
pub(super) struct CachedPane {
    pub(super) cols: u16,
    /// Real pane height (rows) — the resize gesture's baseline.
    pub(super) total_rows: u16,
    pub(super) rows: Vec<Vec<Run>>, // trailing blank rows trimmed
    pub(super) cursor: Option<(u16, u16)>,
    /// Pane app requested bracketed paste — pastes get ESC[200~ markers.
    pub(super) bracketed_paste: bool,
    /// The last screen line with real content — a TUI's status line, a
    /// shell's last output. Shown on compact cards as "what it's doing".
    pub(super) summary: Option<String>,
}

pub(super) fn brighten(c: Color32) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(45),
        c.g().saturating_add(45),
        c.b().saturating_add(45),
    )
}

pub(super) fn build_cached(grid: &TermGrid) -> CachedPane {
    let cols = grid.cols as usize;
    let blank_row = |r: usize| {
        grid.cells[r * cols..(r + 1) * cols]
            .iter()
            .all(|c| c.ch == ' ' && c.bg.is_none() && !c.inverse)
    };
    let mut shown = 0;
    for r in 0..grid.rows as usize {
        if !blank_row(r) {
            shown = r + 1;
        }
    }
    if let Some((cr, _)) = grid.cursor {
        shown = shown.max(cr as usize + 1);
    }
    // never exceed the real row count — a 1-row pane must not slice past the
    // cells vec (was a reachable panic)
    let max_rows = grid.rows.max(1) as usize;
    let shown = shown.max(2).min(max_rows);

    let mut rows = Vec::with_capacity(shown);
    for r in 0..shown {
        let mut runs: Vec<Run> = Vec::new();
        for (ci, cell) in grid.cells[r * cols..(r + 1) * cols].iter().enumerate() {
            let (mut fg_t, mut bg_t) = (cell.fg.unwrap_or(TERM_FG_T), cell.bg);
            if cell.inverse {
                let old = fg_t;
                fg_t = bg_t.unwrap_or(TERM_BG_T);
                bg_t = Some(old);
            }
            let mut fg = Color32::from_rgb(fg_t.0, fg_t.1, fg_t.2);
            if cell.bold {
                fg = brighten(fg);
            }
            let bg = bg_t.map(|(r, g, b)| Color32::from_rgb(r, g, b));
            match runs.last_mut() {
                Some(run)
                    if run.fg == fg
                        && run.bg == bg
                        && run.italic == cell.italic
                        && run.underline == cell.underline =>
                {
                    run.text.push(cell.ch);
                }
                _ => runs.push(Run {
                    start_col: ci as u16,
                    text: cell.ch.to_string(),
                    fg,
                    bg,
                    italic: cell.italic,
                    underline: cell.underline,
                }),
            }
        }
        rows.push(runs);
    }
    let summary = text_graph::mirror::summary_line(grid);
    CachedPane {
        cols: grid.cols,
        total_rows: grid.rows,
        rows,
        cursor: grid.cursor,
        bracketed_paste: grid.bracketed_paste,
        summary,
    }
}

/// Paint a cached pane's rows (and cursor block) at `origin` with the
/// given monospace metrics — shared by the cards and the hover peek popup.
pub(super) fn paint_pane_rows(
    p: &egui::Painter,
    origin: Pos2,
    c: &CachedPane,
    font: &FontId,
    adv: f32,
    line_h: f32,
) {
    for (r, row) in c.rows.iter().enumerate() {
        let y = origin.y + r as f32 * line_h;
        let mut job = egui::text::LayoutJob::default();
        for run in row {
            if let Some(bg) = run.bg {
                let x0 = origin.x + run.start_col as f32 * adv;
                let w = run.text.chars().count() as f32 * adv;
                p.rect_filled(
                    Rect::from_min_size(Pos2::new(x0, y), Vec2::new(w, line_h)),
                    0.0,
                    bg,
                );
            }
            let mut fmt = egui::TextFormat::simple(font.clone(), run.fg);
            fmt.italics = run.italic;
            if run.underline {
                fmt.underline = Stroke::new(1.0, run.fg);
            }
            job.append(&run.text, 0.0, fmt);
        }
        let galley = p.layout_job(job);
        p.galley(Pos2::new(origin.x, y), galley, Color32::WHITE);
    }
    if let Some((cr, cc)) = c.cursor
        && (cr as usize) < c.rows.len()
    {
        let p0 = Pos2::new(origin.x + cc as f32 * adv, origin.y + cr as f32 * line_h);
        p.rect_filled(
            Rect::from_min_size(p0, Vec2::new(adv.max(2.0), line_h)),
            0.0,
            HOVER.gamma_multiply(0.55),
        );
    }
}

pub(super) fn hash_angle(session: &str, pane: &str, index: usize) -> f32 {
    let mut h: u32 = 17;
    for b in session.bytes().chain(pane.bytes()) {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    h = h.wrapping_add(index as u32 * 97);
    (h % 628) as f32 / 100.0
}

pub(super) fn map_key(key: Key) -> Option<Special> {
    Some(match key {
        Key::Enter => Special::Enter,
        Key::Tab => Special::Tab,
        Key::Backspace => Special::Backspace,
        Key::Escape => Special::Escape,
        Key::ArrowUp => Special::Up,
        Key::ArrowDown => Special::Down,
        Key::ArrowLeft => Special::Left,
        Key::ArrowRight => Special::Right,
        Key::Home => Special::Home,
        Key::End => Special::End,
        Key::PageUp => Special::PageUp,
        Key::PageDown => Special::PageDown,
        Key::Delete => Special::Delete,
        Key::Insert => Special::Insert,
        Key::F1 => Special::F(1),
        Key::F2 => Special::F(2),
        Key::F3 => Special::F(3),
        Key::F4 => Special::F(4),
        Key::F5 => Special::F(5),
        Key::F6 => Special::F(6),
        Key::F7 => Special::F(7),
        Key::F8 => Special::F(8),
        Key::F9 => Special::F(9),
        Key::F10 => Special::F(10),
        Key::F11 => Special::F(11),
        Key::F12 => Special::F(12),
        _ => return None,
    })
}

/// Characters reachable for Ctrl chords (plain characters arrive as text).
pub(super) fn key_char(key: Key) -> Option<char> {
    use Key::*;
    Some(match key {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        Num0 => '0',
        Num1 => '1',
        Num2 => '2',
        Num3 => '3',
        Num4 => '4',
        Num5 => '5',
        Num6 => '6',
        Num7 => '7',
        Num8 => '8',
        Num9 => '9',
        Space => ' ',
        OpenBracket => '[',
        CloseBracket => ']',
        Backslash => '\\',
        Slash => '/',
        Minus => '-',
        _ => return None,
    })
}

impl Viewer {
    /// Forward this frame's keyboard input to the focused pane, DRAINING the
    /// keyboard events so egui's own focus/shortcut machinery never sees
    /// them — otherwise Tab moves widget focus to a detail-pane button and
    /// Enter fake-clicks it (spawning an editor) while the user is just
    /// typing into a terminal. Pointer/window events are put back untouched.
    ///
    /// Text and Ctrl-chords go as raw hex; special keys go as tmux key names
    /// so tmux applies the pane's terminal modes. Ctrl+C/X arrive from
    /// egui-winit as clipboard events with NO Key event, so they're handled
    /// as Copy/Cut — without that, an agent can't be interrupted.
    pub(super) fn forward_input(&mut self, ui: &egui::Ui) {
        let Some((session, pane)) = self.terms.focused.clone() else {
            return;
        };
        if !self.terms.mirrors.contains_key(&session) {
            self.terms.focused = None;
            return;
        }
        let frame_mods = ui.ctx().input(|i| i.modifiers);
        let events = ui.ctx().input_mut(|i| std::mem::take(&mut i.events));
        let mut keep: Vec<egui::Event> = Vec::with_capacity(events.len());
        let mut cmds: Vec<String> = Vec::new();
        for ev in events {
            match ev {
                egui::Event::Text(ref t) if !t.is_empty() => {
                    // Alt+letter chords (readline word motion) arrive as BOTH
                    // a Text event and a Key event on X11; the Key arm sends
                    // the ESC-prefixed chord, so drop the bare text — but
                    // only for ASCII alphanumerics, so AltGr-composed chars
                    // (€, ñ — non-ASCII) still type normally.
                    let alt_chord = frame_mods.alt
                        && !frame_mods.ctrl
                        && t.chars().all(|c| c.is_ascii_alphanumeric());
                    if !alt_chord {
                        cmds.push(keys::hex_cmd(&pane, t.as_bytes()));
                    }
                }
                egui::Event::Paste(ref t) if !t.is_empty() => {
                    // Bracketed paste when the pane app asked for it: without
                    // the markers, every newline in a multiline paste reads
                    // as Enter — a REPL or agent prompt submits mid-paste.
                    let bracketed = self
                        .terms
                        .cache
                        .get(&(session.clone(), pane.clone()))
                        .is_some_and(|c| c.bracketed_paste);
                    if bracketed {
                        let mut bytes = b"\x1b[200~".to_vec();
                        bytes.extend_from_slice(t.as_bytes());
                        bytes.extend_from_slice(b"\x1b[201~");
                        cmds.push(keys::hex_cmd(&pane, &bytes));
                    } else {
                        cmds.push(keys::hex_cmd(&pane, t.as_bytes()));
                    }
                }
                egui::Event::Copy => cmds.push(keys::hex_cmd(&pane, &[0x03])), // Ctrl+C
                egui::Event::Cut => cmds.push(keys::hex_cmd(&pane, &[0x18])),  // Ctrl+X
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let mods = Mods {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                    };
                    if let Some(sp) = map_key(key) {
                        cmds.push(keys::special_cmd(&pane, sp, mods));
                    } else if (mods.ctrl || mods.alt)
                        && let Some(c) = key_char(key)
                        && let Some(cmd) = keys::chord_cmd(&pane, c, mods)
                    {
                        cmds.push(cmd);
                    }
                }
                egui::Event::Key { .. } => {} // key releases: swallow
                other => keep.push(other),
            }
        }
        ui.ctx().input_mut(|i| {
            let mut late = std::mem::take(&mut i.events);
            keep.append(&mut late);
            i.events = keep;
        });
        if !cmds.is_empty()
            && let Some(m) = self.terms.mirrors.get_mut(&session)
        {
            for c in &cmds {
                m.command(c);
            }
        }
    }

    /// Poll tmux for agent panes anchored in this vault (default server),
    /// publish diffs, wake the UI.
    pub(super) fn start_agent_scan(&self, ctx: egui::Context) {
        let shared = self.terms.seen.clone();
        let root = self.root.clone();
        std::thread::spawn(move || {
            let allow = agents::default_allowlist();
            let mut tracker = agents::Tracker::new();
            loop {
                let panes = agents::scan(&root);
                let active = tracker.update(&panes, &allow, Instant::now());
                {
                    let mut s = shared.lock().unwrap();
                    if *s != active {
                        *s = active;
                        ctx.request_repaint();
                    }
                }
                std::thread::sleep(Duration::from_millis(1500));
            }
        });
    }

    /// Snapshot discovery, keep mirrors in sync with it, pump events, and
    /// refresh converted screens per mirror generation.
    pub(super) fn sync_terminals(&mut self, ctx: &egui::Context) {
        let bg: Vec<String> = std::mem::take(&mut *self.terms.bg_flash.lock().unwrap());
        for msg in bg {
            self.set_flash(msg);
        }
        self.terms.panes = self.terms.seen.lock().unwrap().clone();

        let sessions: HashSet<String> =
            self.terms.panes.iter().map(|a| a.session.clone()).collect();
        for s in &sessions {
            if !self.terms.mirrors.contains_key(s) {
                // failed attaches back off — retrying at frame rate would
                // spawn tmux processes 60 times a second
                if self
                    .terms
                    .attach_backoff
                    .get(s)
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
                {
                    continue;
                }
                let c = ctx.clone();
                // Never pass a size for discovered sessions — the user may be
                // viewing them in a real terminal.
                match SessionMirror::attach(s, None, None, move || c.request_repaint()) {
                    Ok(m) => {
                        self.terms.mirrors.insert(s.clone(), m);
                        self.terms.attach_backoff.remove(s);
                    }
                    Err(_) => {
                        self.terms.attach_backoff.insert(s.clone(), Instant::now());
                    }
                }
            }
        }
        self.terms
            .attach_backoff
            .retain(|s, _| sessions.contains(s));
        self.terms
            .mirrors
            .retain(|s, m| !m.exited && sessions.contains(s));
        self.terms.cache.retain(|(s, _), _| sessions.contains(s));
        self.terms.mirror_gen.retain(|s, _| sessions.contains(s));
        // Arrangements are never dropped, only parked by session name and
        // reclaimed when a session with that name reappears (exact pane
        // first, then any spot) — including across viewer restarts via
        // .text-graph/view. Logic lives in state.rs, where it's unit-tested.
        state::park_absent(&mut self.terms.offsets, &mut self.terms.parked, &sessions);
        state::park_absent(
            &mut self.terms.pinned,
            &mut self.terms.parked_pins,
            &sessions,
        );
        let pane_keys: Vec<(String, String)> = self
            .terms
            .panes
            .iter()
            .map(|a| (a.session.clone(), a.pane.clone()))
            .collect();
        state::claim(&mut self.terms.offsets, &mut self.terms.parked, &pane_keys);
        state::claim(
            &mut self.terms.pinned,
            &mut self.terms.parked_pins,
            &pane_keys,
        );
        let focus_dead = self
            .terms
            .focused
            .as_ref()
            .is_some_and(|(s, _)| !self.terms.mirrors.contains_key(s));
        if focus_dead {
            self.terms.focused = None;
        }
        if self.terms.cursor.as_ref().is_some_and(|(s, p)| {
            !self
                .terms
                .panes
                .iter()
                .any(|a| &a.session == s && &a.pane == p)
        }) {
            self.terms.cursor = None;
        }
        // A focused pane killed externally (session survives) must release
        // the keyboard — otherwise every keystroke drains into a dead
        // target while all graph keybinds stay suspended.
        if self.terms.focused.as_ref().is_some_and(|(s, p)| {
            !self
                .terms
                .panes
                .iter()
                .any(|a| &a.session == s && &a.pane == p)
        }) {
            self.terms.focused = None;
        }

        for (s, m) in &mut self.terms.mirrors {
            if m.pump() {
                self.terms.activity.insert(s.clone(), Instant::now());
            }
            let mgen = m.generation();
            if self.terms.mirror_gen.get(s).copied() != Some(mgen) {
                self.terms.mirror_gen.insert(s.clone(), mgen);
                let grids = m.grids();
                self.terms
                    .cache
                    .retain(|(cs, cp), _| cs != s || grids.iter().any(|(p, _)| p == cp));
                for (pane, grid) in grids {
                    self.terms
                        .cache
                        .insert((s.clone(), pane), build_cached(&grid));
                }
            }
        }

        // glow fades need a few follow-up frames
        if self
            .terms
            .activity
            .values()
            .any(|t| t.elapsed() < Duration::from_secs(2))
        {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    pub(super) fn paint_terminals(&mut self, painter: &egui::Painter, rect: Rect, view: Rect) {
        self.terms.rects.clear();
        // one shot per double-click, whether or not the card still exists
        let recenter = self.terms.fly_to.take();
        if self.terms.panes.is_empty() {
            return;
        }
        let f_base = (6.0 * self.zoom).clamp(2.5, 16.0);
        let compact_base = f_base < 5.0;
        let font_base = FontId::monospace(f_base);
        // measured monospace advance keeps columns honest at any zoom
        let probe = painter.layout_no_wrap("M".into(), font_base.clone(), Color32::WHITE);
        let (adv_base, line_h_base) = (probe.size().x, probe.size().y);
        let title_h = 16.0;
        let pad = 6.0;

        for (i, a) in self.terms.panes.iter().enumerate() {
            let key = (a.session.clone(), a.pane.clone());
            let Some(c) = self.terms.cache.get(&key) else {
                continue;
            };
            let focused = self.terms.focused.as_ref() == Some(&key);
            let cursor = self.terms.cursor.as_ref() == Some(&key);
            // The cursor-selected, focused, or pinned card always shows its
            // full screen at a readable size, however far out the camera is
            // — click through the cards while zoomed out to inspect agents,
            // Ctrl+click to keep several open at once.
            let expanded = self.terms.is_expanded(&key);
            let (font, adv, line_h) = if expanded && f_base < EXPAND_FONT {
                let font = FontId::monospace(EXPAND_FONT);
                let p = painter.layout_no_wrap("M".into(), font.clone(), Color32::WHITE);
                (font, p.size().x, p.size().y)
            } else {
                (font_base.clone(), adv_base, line_h_base)
            };
            let compact = compact_base && !expanded;
            let anchor = self.anchor_for(&a.cwd);
            let anchor_w = self.world_pos(anchor.0 as usize);
            let anchor_s = self.to_screen(rect, anchor_w);
            let size = if compact {
                Vec2::new(230.0, 54.0)
            } else {
                Vec2::new(
                    c.cols as f32 * adv + pad * 2.0,
                    c.rows.len() as f32 * line_h + title_h + pad * 2.0,
                )
            };
            // User-arranged cards keep their world-space offset from the
            // anchor; otherwise place outward from the graph center relative
            // to the anchor (jittered so several cards fan out), past the
            // node's radius in screen space — the tether points inward and
            // the card never sits on top of the cluster it's attached to.
            let (card, tether_to) = if let Some(off) = self.terms.offsets.get(&key) {
                let card = Rect::from_min_size(anchor_s + *off * self.zoom, size);
                (card, card.center())
            } else {
                let jitter = (hash_angle(&a.session, &a.pane, i) - std::f32::consts::PI) * 0.25;
                let base = if anchor_w.to_vec2().length() > 1.0 {
                    anchor_w.to_vec2().angle()
                } else {
                    hash_angle(&a.session, &a.pane, i)
                };
                let dir = Vec2::angled(base + jitter);
                let anchor_r = (self.radius[anchor.0 as usize] * self.zoom).clamp(1.5, 16.0);
                let p = anchor_s + dir * (anchor_r + 22.0);
                let min = Pos2::new(
                    if dir.x >= 0.0 { p.x } else { p.x - size.x },
                    if dir.y >= 0.0 { p.y } else { p.y - size.y },
                );
                (Rect::from_min_size(min, size), p)
            };
            // Double-click fly-in: the zoom jump already happened in the
            // input pass; now that the card's rect is known at the new zoom,
            // shift the view so the card sits centered.
            if recenter.as_ref() == Some(&key) {
                self.center += (card.center() - rect.center()) / self.zoom;
                painter.ctx().request_repaint();
            }
            if !view.intersects(card) {
                continue;
            }
            self.terms
                .rects
                .push((a.session.clone(), a.pane.clone(), card));

            // drawn before the card, so it vanishes cleanly behind its edge
            painter.extend(egui::Shape::dashed_line(
                &[anchor_s, tether_to],
                Stroke::new(1.0, EDGE),
                6.0,
                4.0,
            ));

            let hot = self
                .terms
                .activity
                .get(&a.session)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
            let searching = self.search_open && !self.query.is_empty();
            let smatch = searching && self.terms.scores.get(i).copied().flatten().is_some();
            let sbest = searching && self.terms.best.as_ref().is_some_and(|(_, bk)| bk == &key);
            // typing focus = thick cyan (unmistakable); the t-cursor =
            // orange like node selection; streaming = green
            let (border, bw) = if focused {
                (TERM_FOCUS, 3.0)
            } else if sbest {
                (HOVER, 2.5)
            } else if smatch {
                (WIKI, 2.0)
            } else if cursor {
                (SELECT, 2.2)
            } else if hot {
                (AGENT, 1.5)
            } else {
                (TERM_BORDER, 1.5)
            };
            painter.rect_filled(card, 4.0, TERM_BG);
            painter.rect_stroke(card, 4.0, Stroke::new(bw, border), egui::StrokeKind::Inside);
            if sbest {
                // same ring the best node hit gets — Enter jumps here
                painter.rect_stroke(
                    card.expand(3.0),
                    6.0,
                    Stroke::new(1.5, HOVER),
                    egui::StrokeKind::Outside,
                );
            }
            // everything inside the card is clipped to it — no overflow, ever
            let cp = painter.with_clip_rect(card);
            // 📌 marks a Ctrl+click pin — the reader must be able to tell
            // WHY a card is expanded, and that Ctrl+click undoes it
            let pin = if self.terms.pinned.contains_key(&key) {
                "📌 "
            } else {
                ""
            };
            let title = if focused {
                format!("⌨ {pin}{} · {} {}", a.agent, a.session, a.pane)
            } else {
                format!("{pin}{} · {} {}", a.agent, a.session, a.pane)
            };
            cp.text(
                card.left_top() + Vec2::new(pad, 2.0),
                Align2::LEFT_TOP,
                title,
                FontId::proportional(11.0),
                if focused {
                    TERM_FOCUS
                } else if cursor {
                    SELECT
                } else if hot {
                    AGENT
                } else {
                    TEXT
                },
            );

            if compact {
                // metadata beats a TUI's bottom status bar for glanceability
                let state = match self.terms.activity.get(&a.session) {
                    Some(t) if t.elapsed() < Duration::from_secs(3) => "active".to_string(),
                    Some(t) => {
                        let s = t.elapsed().as_secs();
                        if s < 60 {
                            format!("idle {s}s")
                        } else {
                            format!("idle {}m", s / 60)
                        }
                    }
                    None => "quiet".to_string(),
                };
                let meta = format!("in {}/ · {}", self.g.node(anchor).display_name(), state);
                cp.text(
                    card.left_top() + Vec2::new(pad, 20.0),
                    Align2::LEFT_TOP,
                    meta,
                    FontId::proportional(10.5),
                    Color32::from_rgb(TERM_FG_T.0, TERM_FG_T.1, TERM_FG_T.2),
                );
                // the pane's own last status/output line: what it's doing
                if let Some(s) = &c.summary {
                    cp.text(
                        card.left_top() + Vec2::new(pad, 35.0),
                        Align2::LEFT_TOP,
                        s,
                        FontId::monospace(9.5),
                        WIKI,
                    );
                }
                continue; // compact cards: no grip — resize is gated off too
            }

            let origin = card.left_top() + Vec2::new(pad, title_h + pad);
            paint_pane_rows(&cp, origin, c, &font, adv, line_h);
            if a.ours {
                paint_resize_grip(&cp, card, border);
            }
        }
    }

    /// Peek popup: dwell on a COMPACT card and its full screen renders at a
    /// readable size on the tooltip layer — inspect what an agent is doing
    /// without zooming in, focusing, or pinning. Expanded cards already
    /// show everything, so they never peek.
    pub(super) fn hover_peek_ui(&self, ui: &egui::Ui) {
        let Some((key, since, anchor)) = self.terms.hover_since.clone() else {
            return;
        };
        let compact = (6.0 * self.zoom).clamp(2.5, 16.0) < 5.0 && !self.terms.is_expanded(&key);
        if !compact {
            return;
        }
        let elapsed = since.elapsed();
        if elapsed < super::previews::HOVER_DELAY {
            ui.ctx()
                .request_repaint_after(super::previews::HOVER_DELAY - elapsed);
            return;
        }
        let Some(c) = self.terms.cache.get(&key) else {
            return;
        };
        let title = self
            .terms
            .panes
            .iter()
            .find(|a| a.session == key.0 && a.pane == key.1)
            .map(|a| format!("{} · {} {}", a.agent, a.session, a.pane))
            .unwrap_or_else(|| format!("{} {}", key.0, key.1));
        let screen = ui.ctx().content_rect();
        // fit wide panes: shrink the font until the full width fits
        let mut f = 11.5;
        let probe = |ui: &egui::Ui, f: f32| {
            let g = ui
                .painter()
                .layout_no_wrap("M".into(), FontId::monospace(f), Color32::WHITE);
            (g.size().x, g.size().y)
        };
        let (mut adv, mut line_h) = probe(ui, f);
        let max_w = screen.width() * 0.85;
        if c.cols as f32 * adv > max_w {
            f *= max_w / (c.cols as f32 * adv);
            (adv, line_h) = probe(ui, f);
        }
        let font = FontId::monospace(f);
        let size = Vec2::new(c.cols as f32 * adv, c.rows.len() as f32 * line_h);
        let pivot = super::previews::popup_pivot(anchor, screen);
        let off = Vec2::new(
            if pivot.0[0] == egui::Align::Min {
                16.0
            } else {
                -16.0
            },
            if pivot.0[1] == egui::Align::Min {
                14.0
            } else {
                -14.0
            },
        );
        egui::Area::new(egui::Id::new("term-peek"))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .pivot(pivot)
            .fixed_pos(anchor + off)
            .constrain_to(screen.shrink(6.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(title).strong());
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    let p = ui.painter_at(rect.expand(2.0));
                    p.rect_filled(rect.expand(2.0), 2.0, TERM_BG);
                    paint_pane_rows(&p, rect.min, c, &font, adv, line_h);
                });
            });
    }

    /// Jump the view into a card: readable zoom now, exact centering on the
    /// next paint (which knows the card's rect at the new zoom).
    pub(super) fn fly_to_card(&mut self, t: (String, String)) {
        if let Some(id) = self
            .terms
            .panes
            .iter()
            .find(|a| a.session == t.0 && a.pane == t.1)
            .map(|a| self.anchor_for(&a.cwd))
        {
            self.center = self.world_pos(id.0 as usize);
        }
        self.zoom = CARD_ZOOM;
        self.terms.fly_to = Some(t);
    }

    /// Open a real terminal window attached to the card's session, landed on
    /// its pane. `;` is tmux's command separator — it goes through argv
    /// unshelled, so no quoting games.
    pub(super) fn attach_external(&mut self, session: &str, pane: &str) {
        let Some(mut cmd) = new_terminal_window() else {
            self.set_flash("no terminal emulator found — set $TERMINAL".into());
            return;
        };
        cmd.args(["tmux", "attach-session", "-t", &format!("={session}")]);
        cmd.args([
            ";",
            "select-window",
            "-t",
            pane,
            ";",
            "select-pane",
            "-t",
            pane,
        ]);
        match detached(&mut cmd) {
            Ok(()) => self.set_flash(format!("attaching {session} in a new terminal")),
            Err(e) => self.set_flash(format!("attach failed: {e}")),
        }
    }

    /// Kill just this pane (pane ids are server-global). tmux ends the
    /// session with its last pane, which removes the card via discovery.
    pub(super) fn kill_pane(&mut self, session: &str, pane: &str) {
        let ok = std::process::Command::new("tmux")
            .args(["kill-pane", "-t", pane])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            if self
                .terms
                .focused
                .as_ref()
                .is_some_and(|(fs, fp)| fs == session && fp == pane)
            {
                self.terms.focused = None;
            }
            self.set_flash(format!("killed {session} {pane}"));
        } else {
            self.set_flash(format!("kill failed for {session} {pane}"));
        }
    }

    pub(super) fn launch_agent(&mut self, ctx: &egui::Context, dir: &str, agent: &str) {
        let path = self.ctx_path(dir);
        match agents::launch(None, &path, agent) {
            Ok(name) => {
                self.set_flash(format!("launched {agent} — session {name}"));
                // Instant-death watchdog: a command that exits immediately
                // (binary not on the pane's PATH, bad flags) takes its
                // session down before discovery's next scan — without this
                // the success flash is the only, false, feedback.
                let sink = self.terms.bg_flash.clone();
                let ctx = ctx.clone();
                let agent = agent.to_string();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(2500));
                    let alive = std::process::Command::new("tmux")
                        .args(["has-session", "-t", &format!("={name}")])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false);
                    if !alive {
                        sink.lock().unwrap().push(format!(
                            "{agent} exited immediately — session {name} is gone (is `{agent}` installed?)"
                        ));
                        ctx.request_repaint();
                    }
                });
            }
            Err(e) => self.set_flash(format!("launch failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_cards_count_as_expanded() {
        let mut t = Terminals::new(HashMap::new(), HashMap::new());
        let key = ("tg_a".to_string(), "%1".to_string());
        assert!(!t.is_expanded(&key));
        t.pinned.insert(key.clone(), ());
        assert!(t.is_expanded(&key), "pin expands without focus/cursor");
        t.pinned.remove(&key);
        t.cursor = Some(key.clone());
        assert!(t.is_expanded(&key));
        t.cursor = None;
        t.focused = Some(key.clone());
        assert!(t.is_expanded(&key));
    }
}
