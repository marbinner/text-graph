//! Per-vault view-state persistence: camera and terminal-card arrangement.
//!
//! Lives at `<vault>/.text-graph/view`. The dot-dir is hidden, so the vault
//! walker and the live-reload watcher never see it — writes here can't cause
//! reload loops. Plain tab-separated lines with the session name LAST (a tab
//! inside a session name can't shear the record); unknown lines are ignored
//! for forward compatibility, and any unparsable line is simply dropped.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewState {
    /// (center_x, center_y, zoom)
    pub camera: Option<(f32, f32, f32)>,
    pub cards: Vec<CardPos>,
    /// (session, pane) cards pinned open — expanded at any zoom.
    pub pins: Vec<(String, String)>,
    /// Web nodes hidden by the `w` toggle. Stored inverted so the derived
    /// Default (false) means the default view: webs visible.
    pub hide_web: bool,
    /// Light theme on (default false = dark).
    pub light: bool,
    /// Agent the one-click "Launch <agent>" menu button starts; None =
    /// the built-in default (pi).
    pub default_agent: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardPos {
    pub session: String,
    pub pane: String,
    /// World-space offset of the card's CENTER from its anchor node — the
    /// center is the placement reference, so compact↔expanded flips grow
    /// the card symmetrically around where the user put it.
    pub dx: f32,
    pub dy: f32,
}

pub fn to_text(s: &ViewState) -> String {
    let mut out = String::from("text-graph view v1\n");
    if let Some((x, y, z)) = s.camera {
        out.push_str(&format!("camera\t{x}\t{y}\t{z}\n"));
    }
    for c in &s.cards {
        out.push_str(&format!(
            "card\t{}\t{}\t{}\t{}\n",
            c.dx, c.dy, c.pane, c.session
        ));
    }
    for (session, pane) in &s.pins {
        out.push_str(&format!("pin\t{pane}\t{session}\n"));
    }
    if s.hide_web {
        out.push_str("hide_web\n");
    }
    if s.light {
        out.push_str("light\n");
    }
    if let Some(a) = &s.default_agent {
        out.push_str(&format!("agent\t{a}\n"));
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
                    // first occurrence wins: a duplicated line (git merge,
                    // hand edit) would otherwise ride claim()'s fallback
                    // pass onto a pane the user never arranged
                    && !s.cards.iter().any(|c| c.session == session && c.pane == pane)
                {
                    s.cards.push(CardPos {
                        session: session.to_string(),
                        pane: pane.to_string(),
                        dx,
                        dy,
                    });
                }
            }
            Some("hide_web") => s.hide_web = true,
            Some("light") => s.light = true,
            Some("agent") => {
                if let Some((_, a)) = line.split_once('\t')
                    && !a.is_empty()
                {
                    s.default_agent = Some(a.to_string());
                }
            }
            Some("pin") => {
                let fields: Vec<&str> = line.splitn(3, '\t').collect();
                if let [_, pane, session] = fields[..]
                    // dedup like cards: a doubled pin line spontaneously
                    // pins a second pane of the session via claim()
                    && !s.pins.iter().any(|(se, p)| se == session && p == pane)
                {
                    s.pins.push((session.to_string(), pane.to_string()));
                }
            }
            _ => {} // header / unknown line kinds
        }
    }
    s
}

fn num(v: Option<&str>) -> Option<f32> {
    v.and_then(|t| t.parse::<f32>().ok())
        .filter(|f| f.is_finite())
}

/// Move every arrangement whose session is absent into the parking map,
/// keyed by session name. Arrangements are never dropped — a session that
/// reappears (even after a tmux server restart) reclaims its spot.
pub fn park_absent<V: Copy>(
    offsets: &mut HashMap<(String, String), V>,
    parked: &mut HashMap<String, Vec<(String, V)>>,
    live_sessions: &HashSet<String>,
) {
    let mut gone: Vec<((String, String), V)> = offsets
        .iter()
        .filter(|((s, _), _)| !live_sessions.contains(s))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    gone.sort_by(|a, b| a.0.cmp(&b.0)); // parking order independent of map order
    for ((s, p), v) in gone {
        offsets.remove(&(s.clone(), p.clone()));
        parked.entry(s).or_default().push((p, v));
    }
}

/// Hand parked arrangements to live panes. Two passes: exact
/// (session, pane) matches claim their own spot first — only then may a
/// leftover pane of the same session take any remaining spot (pane ids
/// change across tmux server restarts). A single greedy pass would let an
/// earlier unrelated pane steal a later pane's exact match.
pub fn claim<V: Copy>(
    offsets: &mut HashMap<(String, String), V>,
    parked: &mut HashMap<String, Vec<(String, V)>>,
    panes: &[(String, String)],
) {
    for key in panes {
        if offsets.contains_key(key) {
            continue;
        }
        if let Some(list) = parked.get_mut(&key.0)
            && let Some(i) = list.iter().position(|(p, _)| p == &key.1)
        {
            let (_, v) = list.remove(i);
            offsets.insert(key.clone(), v);
            if list.is_empty() {
                parked.remove(&key.0);
            }
        }
    }
    for key in panes {
        if offsets.contains_key(key) {
            continue;
        }
        if let Some(list) = parked.get_mut(&key.0) {
            let (_, v) = list.remove(0);
            offsets.insert(key.clone(), v);
            if list.is_empty() {
                parked.remove(&key.0);
            }
        }
    }
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
        CardPos {
            session: session.into(),
            pane: pane.into(),
            dx,
            dy,
        }
    }

    #[test]
    fn round_trips_camera_cards_and_pins() {
        let s = ViewState {
            camera: Some((12.5, -3.0, 2.2)),
            cards: vec![
                card("tg_claude", "%4", -80.0, 12.0),
                card("work", "%0", 5.5, 0.0),
            ],
            pins: vec![
                ("tg_claude".to_string(), "%4".to_string()),
                ("work".to_string(), "%0".to_string()),
            ],
            hide_web: true,
            light: true,
            default_agent: Some("claude".into()),
        };
        assert_eq!(from_text(&to_text(&s)), s);
    }

    #[test]
    fn session_names_with_tabs_survive() {
        let s = ViewState {
            camera: None,
            cards: vec![card("weird\tname", "%1", 1.0, 2.0)],
            pins: vec![("weird\tname".to_string(), "%1".to_string())],
            hide_web: false,
            light: false,
            default_agent: None,
        };
        assert_eq!(from_text(&to_text(&s)), s);
    }

    #[test]
    fn garbage_and_unknown_lines_are_ignored() {
        let text = "text-graph view v99\nnonsense\ncamera\tx\ty\tz\ncamera\t1\t2\t0\n\
                    card\tnan\t2\t%1\ts\ncard\t1\t2\t%1\tok\npin\t%9\npin\t%2\ts\n\
                    future-thing\tdata\n";
        let s = from_text(text);
        assert_eq!(
            s.camera, None,
            "non-numeric / non-positive-zoom cameras dropped"
        );
        assert_eq!(s.cards, vec![card("ok", "%1", 1.0, 2.0)]);
        assert_eq!(
            s.pins,
            vec![("s".to_string(), "%2".to_string())],
            "truncated pin line dropped, valid one kept"
        );
    }

    /// A duplicated card/pin line (git merge, hand edit) must collapse to
    /// one on load — the copies fed claim()'s fallback pass, which pinned
    /// or stacked panes the user never touched, and re-saving perpetuated
    /// the corruption.
    #[test]
    fn duplicate_card_and_pin_lines_collapse_on_load() {
        let text = "card\t1\t2\t%1\ts\ncard\t9\t9\t%1\ts\ncard\t3\t4\t%2\ts\n\
                    pin\t%1\ts\npin\t%1\ts\n";
        let s = from_text(text);
        assert_eq!(
            s.cards,
            vec![card("s", "%1", 1.0, 2.0), card("s", "%2", 3.0, 4.0)],
            "first occurrence wins; distinct panes all load"
        );
        assert_eq!(s.pins, vec![("s".to_string(), "%1".to_string())]);
    }

    #[test]
    fn empty_or_missing_is_default() {
        assert_eq!(from_text(""), ViewState::default());
        assert_eq!(
            load(Path::new("/nonexistent-vault-path")),
            ViewState::default()
        );
    }

    fn k(s: &str, p: &str) -> (String, String) {
        (s.to_string(), p.to_string())
    }

    #[test]
    fn claim_prefers_exact_pane_matches_over_scan_order() {
        // session with panes %1 and %2; only %2 was ever arranged. %1 comes
        // first in scan order and must NOT steal %2's spot.
        let mut offsets: HashMap<(String, String), (f32, f32)> = HashMap::new();
        let mut parked = HashMap::new();
        parked.insert("work".to_string(), vec![("%2".to_string(), (7.0, 9.0))]);
        claim(
            &mut offsets,
            &mut parked,
            &[k("work", "%1"), k("work", "%2")],
        );
        assert_eq!(offsets.get(&k("work", "%2")), Some(&(7.0, 9.0)));
        assert!(!offsets.contains_key(&k("work", "%1")));
        assert!(parked.is_empty());
    }

    #[test]
    fn claim_falls_back_across_pane_id_changes() {
        // server restart: saved pane %9 no longer exists; the session's new
        // pane %1 inherits the arrangement
        let mut offsets: HashMap<(String, String), (f32, f32)> = HashMap::new();
        let mut parked = HashMap::new();
        parked.insert(
            "tg_claude".to_string(),
            vec![("%9".to_string(), (1.0, 2.0))],
        );
        claim(&mut offsets, &mut parked, &[k("tg_claude", "%1")]);
        assert_eq!(offsets.get(&k("tg_claude", "%1")), Some(&(1.0, 2.0)));
        assert!(parked.is_empty());
    }

    #[test]
    fn park_then_claim_round_trips() {
        let mut offsets: HashMap<(String, String), (f32, f32)> = HashMap::new();
        offsets.insert(k("a", "%1"), (3.0, 4.0));
        offsets.insert(k("b", "%2"), (5.0, 6.0));
        let mut parked = HashMap::new();
        // session b vanished
        let live: HashSet<String> = ["a".to_string()].into();
        park_absent(&mut offsets, &mut parked, &live);
        assert_eq!(offsets.len(), 1, "a stays live");
        assert_eq!(parked["b"], vec![("%2".to_string(), (5.0, 6.0))]);
        // b comes back
        claim(&mut offsets, &mut parked, &[k("b", "%2")]);
        assert_eq!(offsets.get(&k("b", "%2")), Some(&(5.0, 6.0)));
        assert!(parked.is_empty());
    }
}
