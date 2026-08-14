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

enum Pending {
    Ignore,
    ListPanes,
    Capture(PaneId),
}

pub struct SessionMirror {
    client: TmuxClient,
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
    pub fn attach(
        session: &str,
        socket: Option<&str>,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let (client, rx) = TmuxClient::attach(session, socket, wake)?;
        let mut m = SessionMirror {
            client,
            rx,
            panes: HashMap::new(),
            pending: VecDeque::new(),
            saw_banner: false,
            exited: false,
            generation: 0,
        };
        // Declare a workable client size (best effort — an old-syntax %error
        // lands on an Ignore tag and is dropped).
        m.send(Pending::Ignore, "refresh-client -C 120x40");
        m.list_panes();
        Ok(m)
    }

    fn send(&mut self, tag: Pending, cmd: &str) {
        self.pending.push_back(tag);
        let _ = self.client.command(cmd);
    }

    fn list_panes(&mut self) {
        self.send(
            Pending::ListPanes,
            "list-panes -F '#{pane_id},#{pane_width},#{pane_height}'",
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
                                let (Some(id), Some(w), Some(h)) = (it.next(), it.next(), it.next())
                                else {
                                    continue;
                                };
                                let (Ok(w), Ok(h)) = (w.parse::<u16>(), h.parse::<u16>()) else {
                                    continue;
                                };
                                if !self.panes.contains_key(id) {
                                    self.panes.insert(id.to_string(), vt100::Parser::new(h, w, 0));
                                    let cmd = format!("capture-pane -peq -t {id}");
                                    self.send(Pending::Capture(id.to_string()), &cmd);
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
        cursor: if s.hide_cursor() { None } else { Some(s.cursor_position()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn palette_cube_and_gray() {
        assert_eq!(indexed_rgb(16), (0, 0, 0));
        assert_eq!(indexed_rgb(21), (0, 0, 255));
        assert_eq!(indexed_rgb(196), (255, 0, 0));
        assert_eq!(indexed_rgb(232), (8, 8, 8));
        assert_eq!(indexed_rgb(255), (238, 238, 238));
    }
}
