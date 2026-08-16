//! Session mirror: turns control-mode events into per-pane styled screens.
//!
//! Each pane gets a `vt100` parser fed by `%output` bytes; the initial screen
//! is replayed via `capture-pane -e`. The `TermGrid` facade keeps vt100 an
//! implementation detail — swappable for `alacritty_terminal` later if
//! fidelity demands, without touching callers.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Receiver;

use crate::tmux::{PaneId, TmuxClient, TmuxEvent};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TermCell {
    pub ch: char,
    /// None = the terminal's default foreground.
    pub fg: Option<(u8, u8, u8)>,
    /// None = the terminal's default background.
    pub bg: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Debug)]
pub struct TermGrid {
    pub cols: u16,
    pub rows: u16,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<TermCell>,
    /// (row, col); None while the application hides the cursor.
    pub cursor: Option<(u16, u16)>,
}

// NOTE: TermGrid deliberately does NOT expose terminal modes (bracketed
// paste etc.): every capture replay rebuilds the parser from screen
// CONTENT, which carries no mode state, so any parser-derived mode flag
// silently reads false after attach or resize. Behavior that depends on a
// pane's live modes must ask tmux (pastes go via `paste-buffer -p`, the
// cursor-visibility query rides the post-replay cursor restore).

enum Pending {
    Ignore,
    ListPanes,
    Capture(PaneId),
    /// Restore the true cursor after a capture replay — the replay itself
    /// leaves the parser's cursor at the bottom of the fed content, while
    /// the real pane's cursor may be anywhere (found via the failing
    /// `typed_input_round_trips` test: echo landed on the last row).
    Cursor(PaneId),
}

/// The command channel back to tmux. Tests swap in a recorder so `pump`'s
/// reply-correlation logic runs headless.
enum Transport {
    Tmux(TmuxClient),
    #[cfg(test)]
    Recorded(Vec<String>),
}

impl Transport {
    fn command(&mut self, cmd: &str) -> std::io::Result<()> {
        match self {
            Transport::Tmux(c) => c.command(cmd),
            #[cfg(test)]
            Transport::Recorded(v) => {
                v.push(cmd.to_string());
                Ok(())
            }
        }
    }
}

pub struct SessionMirror {
    client: Transport,
    rx: Receiver<TmuxEvent>,
    panes: HashMap<PaneId, vt100::Parser>,
    /// FIFO tags correlating our sent commands with incoming Reply blocks.
    pending: VecDeque<Pending>,
    /// The attach handshake emits one unsolicited reply block first.
    saw_banner: bool,
    pub exited: bool,
    generation: u64,
}

impl SessionMirror {
    /// `set_size`: declare a client size, which lets tmux resize the window
    /// to us. Only pass Some for explicitly marked sessions we own —
    /// sizing a session
    /// the user is viewing in a real terminal would reflow it under them.
    pub fn attach(
        session: &str,
        socket: Option<&str>,
        set_size: Option<(u16, u16)>,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let (client, rx) = TmuxClient::attach(session, socket, wake)?;
        let mut m = SessionMirror {
            client: Transport::Tmux(client),
            rx,
            panes: HashMap::new(),
            pending: VecDeque::new(),
            saw_banner: false,
            exited: false,
            generation: 0,
        };
        if let Some((w, h)) = set_size {
            let cmd = format!("refresh-client -C {w}x{h}");
            m.send(Pending::Ignore, &cmd);
        }
        m.list_panes();
        Ok(m)
    }

    fn send(&mut self, tag: Pending, cmd: &str) {
        // Tag only what was actually written: a failed write produces no
        // reply, and a tag with no reply would desync the FIFO for good.
        match self.client.command(cmd) {
            Ok(()) => self.pending.push_back(tag),
            Err(_) => self.exited = true, // client gone; reader's Exit follows
        }
    }

    fn list_panes(&mut self) {
        // -s: every pane in the SESSION. Bare list-panes covers only the
        // current window, which dropped background-window agents and blanked
        // parsers whenever the user switched windows.
        self.send(
            Pending::ListPanes,
            "list-panes -s -F '#{pane_id},#{pane_width},#{pane_height}'",
        );
    }

    /// Drain pending tmux events. Returns true if any screen changed (the
    /// generation was bumped) — callers cache grid conversions on it.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                TmuxEvent::Output { pane, bytes } => {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        p.process(&bytes);
                        changed = true;
                    }
                }
                TmuxEvent::Reply { lines, error } => {
                    if !self.saw_banner {
                        self.saw_banner = true;
                        continue;
                    }
                    match self.pending.pop_front() {
                        Some(Pending::ListPanes) if !error => {
                            for l in &lines {
                                let mut it = l.splitn(3, ',');
                                let (Some(id), Some(w), Some(h)) =
                                    (it.next(), it.next(), it.next())
                                else {
                                    continue;
                                };
                                let (Ok(w), Ok(h)) = (w.parse::<u16>(), h.parse::<u16>()) else {
                                    continue;
                                };
                                let is_new = !self.panes.contains_key(id);
                                if is_new {
                                    self.panes
                                        .insert(id.to_string(), vt100::Parser::new(h, w, 0));
                                }
                                // Existing panes: apply resizes — tmux formats
                                // subsequent %output for the new geometry, so
                                // a stale parser garbles the screen forever.
                                let resized = !is_new
                                    && self.panes.get_mut(id).is_some_and(|p| {
                                        if p.screen().size() != (h, w) {
                                            p.screen_mut().set_size(h, w);
                                            true
                                        } else {
                                            false
                                        }
                                    });
                                if is_new || resized {
                                    let cmd = format!("capture-pane -peq -t {id}");
                                    self.send(Pending::Capture(id.to_string()), &cmd);
                                    // cursor_flag rides along: the replay
                                    // also resets DECTCEM, and without the
                                    // true visibility a pane that hid its
                                    // cursor gets a phantom block painted
                                    let cmd = format!(
                                        "display-message -p -t {id} \
                                         '#{{cursor_y}},#{{cursor_x}},#{{cursor_flag}}'"
                                    );
                                    self.send(Pending::Cursor(id.to_string()), &cmd);
                                }
                            }
                            let keep: Vec<&str> =
                                lines.iter().filter_map(|l| l.split(',').next()).collect();
                            self.panes.retain(|k, _| keep.contains(&k.as_str()));
                            changed = true;
                        }
                        Some(Pending::Capture(id)) if !error => {
                            if let Some(p) = self.panes.get_mut(&id) {
                                // Rebuild from scratch: any %output that raced
                                // in before this capture is already included
                                // in it, so replaying on a fresh parser avoids
                                // double-application.
                                let (h, w) = p.screen().size();
                                let mut fresh = vt100::Parser::new(h, w, 0);
                                fresh.process(lines.join("\r\n").as_bytes());
                                *p = fresh;
                                changed = true;
                            }
                        }
                        Some(Pending::Cursor(id)) if !error => {
                            if let (Some(p), Some(line)) = (self.panes.get_mut(&id), lines.first())
                            {
                                let mut it = line.split(',');
                                if let (Some(Ok(y)), Some(Ok(x))) = (
                                    it.next().map(str::parse::<u16>),
                                    it.next().map(str::parse::<u16>),
                                ) {
                                    let cup = format!("\x1b[{};{}H", y + 1, x + 1);
                                    p.process(cup.as_bytes());
                                    // restore true cursor visibility too —
                                    // the fresh replay parser defaults to
                                    // visible even when the app hid it
                                    p.process(match it.next() {
                                        Some("0") => b"\x1b[?25l",
                                        _ => b"\x1b[?25h",
                                    });
                                    changed = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                TmuxEvent::Changed => self.list_panes(),
                TmuxEvent::Exit => {
                    self.exited = true;
                    changed = true;
                }
            }
        }
        if changed {
            self.generation += 1;
        }
        changed
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current screens, sorted by pane id for determinism. Convert on
    /// generation change, not per frame.
    pub fn grids(&self) -> Vec<(PaneId, TermGrid)> {
        let mut out: Vec<(PaneId, TermGrid)> = self
            .panes
            .iter()
            .map(|(id, p)| (id.clone(), screen_to_grid(p.screen())))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Raw tmux command escape hatch (reply dropped). Input via
    /// `send-keys -H` arrives with milestone E3.
    pub fn command(&mut self, cmd: &str) {
        self.send(Pending::Ignore, cmd);
    }
}

fn color(c: vt100::Color) -> Option<(u8, u8, u8)> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(indexed_rgb(i)),
        vt100::Color::Rgb(r, g, b) => Some((r, g, b)),
    }
}

/// The standard xterm 256-color palette.
pub fn indexed_rgb(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let i = i - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            (
                steps[(i / 36) as usize],
                steps[((i % 36) / 6) as usize],
                steps[(i % 6) as usize],
            )
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

/// The last `n` screen lines with real content, top-to-bottom — compact
/// cards show them as "what this agent is doing". Box-drawing borders are
/// trimmed so a TUI frame doesn't read as content.
pub fn tail_lines(g: &TermGrid, n: usize) -> Vec<String> {
    let cols = g.cols as usize;
    if cols == 0 || n == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for r in (0..g.rows as usize).rev() {
        let text: String = g.cells[r * cols..(r + 1) * cols]
            .iter()
            .map(|c| c.ch)
            .collect();
        let t = text
            .trim()
            .trim_matches(|ch: char| "│┃┆┇╎╏╰╯╭╮─━┄┈┐└┘┌├┤".contains(ch))
            .trim();
        if t.chars().any(char::is_alphanumeric) {
            out.push(t.to_string());
            if out.len() == n {
                break;
            }
        }
    }
    out.reverse();
    out
}

fn screen_to_grid(s: &vt100::Screen) -> TermGrid {
    let (rows, cols) = s.size();
    let mut cells = Vec::with_capacity(rows as usize * cols as usize);
    let blank = TermCell {
        ch: ' ',
        fg: None,
        bg: None,
        bold: false,
        italic: false,
        underline: false,
        inverse: false,
    };
    for r in 0..rows {
        for c in 0..cols {
            cells.push(match s.cell(r, c) {
                Some(cell) => TermCell {
                    ch: cell.contents().chars().next().unwrap_or(' '),
                    fg: color(cell.fgcolor()),
                    bg: color(cell.bgcolor()),
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                },
                None => blank,
            });
        }
    }
    TermGrid {
        cols,
        rows,
        cells,
        cursor: if s.hide_cursor() {
            None
        } else {
            Some(s.cursor_position())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Sender;

    /// A mirror with a recorded transport and a hand-fed event channel —
    /// the full pump/reply-FIFO path, no tmux required.
    fn test_mirror() -> (SessionMirror, Sender<TmuxEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut m = SessionMirror {
            client: Transport::Recorded(Vec::new()),
            rx,
            panes: HashMap::new(),
            pending: VecDeque::new(),
            saw_banner: false,
            exited: false,
            generation: 0,
        };
        m.list_panes(); // what attach() does after the handshake
        (m, tx)
    }

    fn sent(m: &SessionMirror) -> &[String] {
        match &m.client {
            Transport::Recorded(v) => v,
            Transport::Tmux(_) => unreachable!(),
        }
    }

    fn reply(tx: &Sender<TmuxEvent>, lines: &[&str], error: bool) {
        tx.send(TmuxEvent::Reply {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            error,
        })
        .unwrap();
    }

    #[test]
    fn pump_correlates_replies_and_restores_cursor() {
        let (mut m, tx) = test_mirror();

        // 1. the attach handshake's unsolicited banner must NOT consume the
        //    pending ListPanes tag
        reply(&tx, &[], false);
        // 2. list-panes answer: one 20x4 pane appears, capture + cursor
        //    queries go out
        reply(&tx, &["%5,20,4"], false);
        m.pump();
        assert!(
            sent(&m)[1].contains("capture-pane -peq -t %5"),
            "{:?}",
            sent(&m)
        );
        assert!(sent(&m)[2].contains("-t %5"), "cursor query");

        // 3. capture replay parks the parser cursor at the bottom…
        reply(&tx, &["hello", "world"], false);
        // 4. …and the cursor query must move it back to the true position
        //    (cursor_flag 1 = visible)
        reply(&tx, &["0,5,1"], false);
        // 5. live output then lands at that cursor, not on the last row
        tx.send(TmuxEvent::Output {
            pane: "%5".into(),
            bytes: b"!".to_vec(),
        })
        .unwrap();
        m.pump();
        let grids = m.grids();
        let (id, g) = &grids[0];
        assert_eq!(id, "%5");
        assert_eq!((g.cols, g.rows), (20, 4));
        let row0: String = g.cells[..20].iter().map(|c| c.ch).collect();
        assert_eq!(row0.trim_end(), "hello!");
        assert_eq!(g.cursor, Some((0, 6)));

        // 6. an %error reply still pops its tag — the FIFO stays aligned —
        //    and a failed list-panes must not wipe the panes
        m.command("resize-window -t %5 -x 1 -y 1"); // Pending::Ignore
        m.list_panes();
        reply(&tx, &[], false); // Ignore's reply
        reply(&tx, &["oops"], true); // list-panes failed
        m.pump();
        assert_eq!(m.grids().len(), 1, "error reply must not clear panes");

        // 7. a resize re-captures at the new geometry
        m.list_panes();
        reply(&tx, &["%5,30,10"], false);
        m.pump();
        let g = &m.grids()[0].1;
        assert_eq!((g.cols, g.rows), (30, 10));
        // answer the re-capture + cursor query the resize queued, keeping
        // the reply FIFO aligned like a real tmux would — this pane HID its
        // cursor (cursor_flag 0), which the restore must reapply: the fresh
        // replay parser defaults to visible (phantom-cursor regression)
        reply(&tx, &["resized"], false);
        reply(&tx, &["0,0,0"], false);
        m.pump();
        assert_eq!(
            m.grids()[0].1.cursor,
            None,
            "hidden cursor must survive a capture replay"
        );

        // 8. pane replaced: %5 gone, %7 appears
        tx.send(TmuxEvent::Changed).unwrap();
        m.pump(); // queues another list-panes
        reply(&tx, &["%7,10,5"], false);
        m.pump();
        let grids = m.grids();
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].0, "%7");

        // 9. exit flag
        tx.send(TmuxEvent::Exit).unwrap();
        m.pump();
        assert!(m.exited);
    }

    fn grid(bytes: &[u8]) -> TermGrid {
        let mut p = vt100::Parser::new(4, 20, 0);
        p.process(bytes);
        screen_to_grid(p.screen())
    }

    #[test]
    fn sgr_colors_and_attrs() {
        let g = grid(b"A\x1b[1;31mB\x1b[0m\x1b[48;5;21mC\x1b[0m");
        let a = &g.cells[0];
        assert_eq!((a.ch, a.fg, a.bold), ('A', None, false));
        let b = &g.cells[1];
        assert_eq!(b.ch, 'B');
        assert_eq!(b.fg, Some(indexed_rgb(1)));
        assert!(b.bold);
        let c = &g.cells[2];
        assert_eq!(c.ch, 'C');
        assert_eq!(c.bg, Some(indexed_rgb(21)));
    }

    #[test]
    fn rows_and_cursor() {
        let g = grid(b"hi\r\nthere");
        assert_eq!(g.cells[0].ch, 'h');
        assert_eq!(g.cells[20].ch, 't'); // row 1, col 0 (cols = 20)
        assert_eq!(g.cursor, Some((1, 5)));
    }

    #[test]
    fn tail_skips_the_input_box_and_keeps_the_status_line() {
        // claude-shaped screen: output, a status line, then the input box —
        // the box is pure box-drawing + '>' and must be skipped
        let mut p = vt100::Parser::new(6, 30, 0);
        p.process(
            b"some earlier output\r\n\
              \xE2\x9C\xB3 Deliberating\xE2\x80\xA6 (12s)\r\n\
              \xE2\x95\xAD\xE2\x94\x80\xE2\x94\x80\xE2\x94\x80\xE2\x95\xAE\r\n\
              \xE2\x94\x82 > \xE2\x94\x82\r\n\
              \xE2\x95\xB0\xE2\x94\x80\xE2\x94\x80\xE2\x94\x80\xE2\x95\xAF",
        );
        let g = screen_to_grid(p.screen());
        assert_eq!(tail_lines(&g, 1), vec!["✳ Deliberating… (12s)"]);
        // shells: the last output line; blank/control-only screens: nothing
        let sh = grid(b"$ cargo test\r\nok. 84 passed");
        assert_eq!(tail_lines(&sh, 1), vec!["ok. 84 passed"]);
        assert!(tail_lines(&grid(b""), 1).is_empty());
        assert!(
            tail_lines(&grid(b"\x1b[?2004h"), 1).is_empty(),
            "control-only screen"
        );
    }

    #[test]
    fn tail_lines_are_the_last_content_top_to_bottom() {
        let g = grid(b"one\r\ntwo\r\nthree");
        assert_eq!(tail_lines(&g, 2), vec!["two", "three"]);
        assert_eq!(
            tail_lines(&g, 9),
            vec!["one", "two", "three"],
            "asking for more than exists returns what's there"
        );
        assert!(tail_lines(&grid(b""), 3).is_empty());
        assert!(tail_lines(&g, 0).is_empty());
    }

    #[test]
    fn palette_cube_and_gray() {
        assert_eq!(indexed_rgb(16), (0, 0, 0));
        assert_eq!(indexed_rgb(21), (0, 0, 255));
        assert_eq!(indexed_rgb(196), (255, 0, 0));
        assert_eq!(indexed_rgb(232), (8, 8, 8));
        assert_eq!(indexed_rgb(255), (238, 238, 238));
    }
}
