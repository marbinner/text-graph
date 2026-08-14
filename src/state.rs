//! Per-vault view-state persistence: camera and terminal-card arrangement.
//!
//! Lives at `<vault>/.text-graph/view`. The dot-dir is hidden, so the vault
//! walker and the live-reload watcher never see it — writes here can't cause
//! reload loops. Plain tab-separated lines with the session name LAST (a tab
//! inside a session name can't shear the record); unknown lines are ignored
//! for forward compatibility, and any unparsable line is simply dropped.

use std::io;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewState {
    /// (center_x, center_y, zoom)
    pub camera: Option<(f32, f32, f32)>,
    pub cards: Vec<CardPos>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardPos {
    pub session: String,
    pub pane: String,
    /// World-space offset of the card's min corner from its anchor node.
    pub dx: f32,
    pub dy: f32,
}

pub fn to_text(s: &ViewState) -> String {
    let mut out = String::from("text-graph view v1\n");
    if let Some((x, y, z)) = s.camera {
        out.push_str(&format!("camera\t{x}\t{y}\t{z}\n"));
    }
    for c in &s.cards {
        out.push_str(&format!("card\t{}\t{}\t{}\t{}\n", c.dx, c.dy, c.pane, c.session));
    }
    out
}

pub fn from_text(text: &str) -> ViewState {
    let mut s = ViewState::default();
    for line in text.lines() {
        match line.split('\t').next() {
            Some("camera") => {
                let mut f = line.split('\t').skip(1);
                if let (Some(x), Some(y), Some(z)) = (num(f.next()), num(f.next()), num(f.next()))
                    && z > 0.0
                {
                    s.camera = Some((x, y, z));
                }
            }
            Some("card") => {
                let fields: Vec<&str> = line.splitn(5, '\t').collect();
                if let [_, dx, dy, pane, session] = fields[..]
                    && let (Some(dx), Some(dy)) = (num(Some(dx)), num(Some(dy)))
                {
                    s.cards.push(CardPos {
                        session: session.to_string(),
                        pane: pane.to_string(),
                        dx,
                        dy,
                    });
                }
            }
            _ => {} // header / unknown line kinds
        }
    }
    s
}

fn num(v: Option<&str>) -> Option<f32> {
    v.and_then(|t| t.parse::<f32>().ok()).filter(|f| f.is_finite())
}

pub fn load(vault: &Path) -> ViewState {
    std::fs::read_to_string(vault.join(".text-graph/view"))
        .map(|t| from_text(&t))
        .unwrap_or_default()
}

/// Write-temp-then-rename, so a crash mid-write can't leave a torn file.
pub fn save(vault: &Path, s: &ViewState) -> io::Result<()> {
    let dir = vault.join(".text-graph");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("view.tmp");
    std::fs::write(&tmp, to_text(s))?;
    std::fs::rename(&tmp, dir.join("view"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(session: &str, pane: &str, dx: f32, dy: f32) -> CardPos {
        CardPos { session: session.into(), pane: pane.into(), dx, dy }
    }

    #[test]
    fn round_trips_camera_and_cards() {
        let s = ViewState {
            camera: Some((12.5, -3.0, 2.2)),
            cards: vec![card("tg_claude", "%4", -80.0, 12.0), card("work", "%0", 5.5, 0.0)],
        };
        assert_eq!(from_text(&to_text(&s)), s);
    }

    #[test]
    fn session_names_with_tabs_survive() {
        let s = ViewState { camera: None, cards: vec![card("weird\tname", "%1", 1.0, 2.0)] };
        assert_eq!(from_text(&to_text(&s)), s);
    }

    #[test]
    fn garbage_and_unknown_lines_are_ignored() {
        let text = "text-graph view v99\nnonsense\ncamera\tx\ty\tz\ncamera\t1\t2\t0\n\
                    card\tnan\t2\t%1\ts\ncard\t1\t2\t%1\tok\nfuture-thing\tdata\n";
        let s = from_text(text);
        assert_eq!(s.camera, None, "non-numeric / non-positive-zoom cameras dropped");
        assert_eq!(s.cards, vec![card("ok", "%1", 1.0, 2.0)]);
    }

    #[test]
    fn empty_or_missing_is_default() {
        assert_eq!(from_text(""), ViewState::default());
        assert_eq!(load(Path::new("/nonexistent-vault-path")), ViewState::default());
    }
}
