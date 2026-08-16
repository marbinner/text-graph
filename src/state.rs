//! Per-vault view-state persistence: camera and terminal-card arrangement.
//!
//! Lives at `<vault>/.text-graph/view`. The dot-dir is hidden, so the vault
//! walker and the live-reload watcher never see it — writes here can't cause
//! reload loops. Plain tab-separated lines with the session name LAST (a tab
//! inside a session name can't shear the record). Unknown line KINDS are
//! carried through load→save verbatim, so opening a vault with an older
//! binary can't erase a newer version's settings (forward compatibility);
//! corrupt lines of a known kind are simply dropped.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

/// View state is a compact list of coordinates and pane identities. A
/// megabyte leaves room for thousands of cards while preventing a planted
/// file (or an endless device reached through a symlink) from exhausting
/// memory during startup.
const MAX_VIEW_STATE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewState {
    /// (center_x, center_y, zoom)
    pub camera: Option<(f32, f32, f32)>,
    pub cards: Vec<CardPos>,
    /// (session, pane) cards pinned open — expanded at any zoom.
    pub pins: Vec<(String, String)>,
    /// Side-pane width in points, as the user last dragged it. `None` =
    /// never set, so it opens at its share of the window.
    pub pane_width: Option<f32>,
    /// Web nodes hidden by the `w` toggle. Stored inverted so the derived
    /// Default (false) means the default view: webs visible.
    pub hide_web: bool,
    /// Light theme, as written by builds before preferences moved to the
    /// per-user config. READ ONLY: parsed so `config::load_or_migrate` can
    /// seed from it, never written back — `config.rs` owns it now. `None`
    /// means the file didn't mention it, which is NOT the same as dark:
    /// migration must not write a preference the vault never expressed.
    pub light: Option<bool>,
    /// Default agent from those same older builds — see [`ViewState::light`].
    pub default_agent: Option<String>,
    /// Line kinds this version doesn't understand, verbatim in file order.
    /// Loaded so a save can write them back — the forward-compat promise.
    pub unknown: Vec<String>,
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
    if let Some(w) = s.pane_width {
        out.push_str(&format!("pane_w\t{w}\n"));
    }
    if s.hide_web {
        out.push_str("hide_web\n");
    }
    // `light` and `agent` are deliberately NOT written: they moved to the
    // per-user config, and a save here would keep resurrecting the old
    // per-vault copy after the migration read it.
    for l in &s.unknown {
        out.push_str(l);
        out.push('\n');
    }
    out
}

pub fn from_text(text: &str) -> ViewState {
    let mut s = ViewState::default();
    // Borrow keys from `text`: the sets exist only while parsing, and avoid
    // both quadratic Vec scans and a second owned copy of every identity.
    let mut seen_cards: HashSet<(&str, &str)> = HashSet::new();
    let mut seen_pins: HashSet<(&str, &str)> = HashSet::new();
    for line in text.lines() {
        let (kind, fields) = line.split_once('\t').unwrap_or((line, ""));
        match kind {
            "camera" => {
                let mut f = fields.split('\t');
                if let (Some(x), Some(y), Some(z)) = (num(f.next()), num(f.next()), num(f.next()))
                    && z > 0.0
                {
                    s.camera = Some((x, y, z));
                }
            }
            "card" => {
                let mut f = fields.splitn(4, '\t');
                if let (Some(dx), Some(dy), Some(pane), Some(session)) =
                    (f.next(), f.next(), f.next(), f.next())
                    && let (Some(dx), Some(dy)) = (num(Some(dx)), num(Some(dy)))
                    // first occurrence wins: a duplicated line (git merge,
                    // hand edit) would otherwise ride claim()'s fallback
                    // pass onto a pane the user never arranged
                    && seen_cards.insert((session, pane))
                {
                    s.cards.push(CardPos {
                        session: session.to_string(),
                        pane: pane.to_string(),
                        dx,
                        dy,
                    });
                }
            }
            // `pane` (v1) is deliberately dropped: those values were
            // SEEDED by a bug rather than chosen — the pane wrote its own
            // computed default on the first frame, when the window is
            // still eframe's 1280 — and a width nobody picked must not
            // outlive the fix. `pane_w` is only ever written by a drag.
            "pane" => {}
            "pane_w" => {
                // clamped on the way in like the camera: a corrupt width
                // could park the pane over the whole window
                s.pane_width = num(Some(fields)).map(|w| w.clamp(120.0, 4000.0));
            }
            "hide_web" => s.hide_web = true,
            "light" => s.light = Some(true),
            "agent" => {
                if !fields.is_empty() {
                    s.default_agent = Some(fields.to_string());
                }
            }
            "pin" => {
                let mut f = fields.splitn(2, '\t');
                if let (Some(pane), Some(session)) = (f.next(), f.next())
                    // dedup like cards: a doubled pin line spontaneously
                    // pins a second pane of the session via claim()
                    && seen_pins.insert((session, pane))
                {
                    s.pins.push((session.to_string(), pane.to_string()));
                }
            }
            _ => {
                // keep unknown line KINDS for the next save (a newer
                // version's settings must survive an older binary); the
                // header is ours and empty lines are noise
                if !line.trim().is_empty() && !line.starts_with("text-graph view") {
                    s.unknown.push(line.to_string());
                }
            }
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
    load_path(&vault.join(".text-graph/view")).unwrap_or_default()
}

fn load_path(path: &Path) -> io::Result<ViewState> {
    let file = std::fs::File::open(path)?;
    let mut text = String::new();
    file.take(MAX_VIEW_STATE_BYTES as u64 + 1)
        .read_to_string(&mut text)?;
    if text.len() > MAX_VIEW_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("view state exceeds {MAX_VIEW_STATE_BYTES} bytes"),
        ));
    }
    Ok(from_text(&text))
}

/// Write through a private, exclusively-created temporary file, then rename.
/// Concurrent viewers can all finish; the last rename wins with one complete
/// snapshot. On Unix every mutation is relative to a no-follow directory
/// descriptor, closing the check-then-use symlink race in a shared vault.
pub fn save(vault: &Path, s: &ViewState) -> io::Result<()> {
    save_impl(vault, &to_text(s))
}

static NEXT_VIEW_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_view_temp_name() -> String {
    let sequence = NEXT_VIEW_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("view.tmp.{}.{sequence}", std::process::id())
}

#[cfg(unix)]
fn save_impl(vault: &Path, text: &str) -> io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags};

    let dir_path = vault.join(".text-graph");
    match std::fs::create_dir(&dir_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }

    // O_NOFOLLOW rejects a planted .text-graph symlink. Once opened, the
    // descriptor keeps all later operations in this exact directory even if
    // another process renames or replaces its path.
    let dir = rustix::fs::open(
        &dir_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| {
        let error: io::Error = error.into();
        io::Error::new(
            error.kind(),
            format!(
                "cannot safely open view-state directory {}: {error}",
                dir_path.display()
            ),
        )
    })?;

    let (temp_name, temp_fd) = loop {
        let name = next_view_temp_name();
        match rustix::fs::openat(
            &dir,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => break (name, fd),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        }
    };

    let mut temp = std::fs::File::from(temp_fd);
    if let Err(error) = temp.write_all(text.as_bytes()) {
        drop(temp);
        let _ = rustix::fs::unlinkat(&dir, temp_name.as_str(), AtFlags::empty());
        return Err(error);
    }
    drop(temp);

    if let Err(error) = rustix::fs::renameat(&dir, temp_name.as_str(), &dir, "view") {
        let _ = rustix::fs::unlinkat(&dir, temp_name.as_str(), AtFlags::empty());
        return Err(error.into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn save_impl(vault: &Path, text: &str) -> io::Result<()> {
    let dir = vault.join(".text-graph");
    if std::fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.is_symlink()) {
        return Err(io::Error::other(
            ".text-graph is a symlink — refusing to write view state through it",
        ));
    }
    std::fs::create_dir_all(&dir)?;

    let (temp_path, mut temp) = loop {
        let path = dir.join(next_view_temp_name());
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };

    if let Err(error) = temp.write_all(text.as_bytes()) {
        drop(temp);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(temp);

    if let Err(error) = std::fs::rename(&temp_path, dir.join("view")) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
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
            pane_width: Some(412.5),
            hide_web: true,
            light: None,
            default_agent: None,
            unknown: vec!["future-thing\tdata".to_string()],
        };
        assert_eq!(
            from_text(&to_text(&s)),
            s,
            "unknown line kinds round-trip too"
        );
    }

    #[test]
    fn preferences_are_read_for_migration_but_never_written_back() {
        let old = "text-graph view v1\nlight\nagent\tclaude\ncamera\t1\t2\t3\n";
        let s = from_text(old);
        assert_eq!(
            s.light,
            Some(true),
            "an older build's theme is still readable"
        );
        assert_eq!(s.default_agent.as_deref(), Some("claude"));
        let out = to_text(&s);
        assert!(
            !out.contains("light"),
            "theme moved to the user config: {out}"
        );
        assert!(
            !out.contains("agent"),
            "agent moved to the user config: {out}"
        );
        assert!(out.contains("camera\t1\t2\t3"), "view state still saved");
    }

    #[test]
    fn session_names_with_tabs_survive() {
        let s = ViewState {
            camera: None,
            cards: vec![card("weird\tname", "%1", 1.0, 2.0)],
            pins: vec![("weird\tname".to_string(), "%1".to_string())],
            pane_width: None,
            hide_web: false,
            light: None,
            default_agent: None,
            unknown: Vec::new(),
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
        assert_eq!(
            s.unknown,
            vec!["nonsense".to_string(), "future-thing\tdata".to_string()],
            "unknown KINDS are kept for the next save; corrupt lines of \
             known kinds and the header are not"
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
    fn oversized_view_state_is_rejected() {
        let base = std::env::temp_dir().join(format!("tg-state-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("view");
        std::fs::write(&path, vec![b'x'; MAX_VIEW_STATE_BYTES + 1]).unwrap();

        let error = load_path(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("view state exceeds"),
            "unexpected error: {error}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_or_missing_is_default() {
        assert_eq!(from_text(""), ViewState::default());
        assert_eq!(
            load(Path::new("/nonexistent-vault-path")),
            ViewState::default()
        );
    }

    /// Saving must refuse to follow planted symlinks — it fires ~3s after
    /// opening a vault with no interaction, so a hostile vault could
    /// otherwise truncate an arbitrary user-writable file.
    #[test]
    fn save_refuses_symlinked_state_dir_and_ignores_legacy_tmp() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("tg-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        // case 1: .text-graph itself is a symlink out of the vault
        let v1 = base.join("v1");
        std::fs::create_dir_all(&v1).unwrap();
        symlink(&outside, v1.join(".text-graph")).unwrap();
        assert!(save(&v1, &ViewState::default()).is_err());
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing may be written through the link"
        );

        // case 2: a planted view.tmp symlink targeting a victim file
        let v2 = base.join("v2");
        std::fs::create_dir_all(v2.join(".text-graph")).unwrap();
        let victim = base.join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        symlink(&victim, v2.join(".text-graph/view.tmp")).unwrap();
        let expected = ViewState {
            camera: Some((1.0, 2.0, 3.0)),
            ..ViewState::default()
        };
        save(&v2, &expected).unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "precious",
            "the victim file must not be truncated"
        );
        assert_eq!(load(&v2), expected);
        assert!(
            std::fs::symlink_metadata(v2.join(".text-graph/view.tmp"))
                .unwrap()
                .is_symlink(),
            "a legacy planted name is neither followed nor reused"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn concurrent_saves_each_use_a_private_temp_file() {
        let base = std::env::temp_dir().join(format!("tg-save-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let vault = base.join("vault");
        std::fs::create_dir_all(&vault).unwrap();

        let writers = 12;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(writers));
        let mut threads = Vec::new();
        for i in 0..writers {
            let barrier = barrier.clone();
            let vault = vault.clone();
            threads.push(std::thread::spawn(move || {
                let state = ViewState {
                    camera: Some((i as f32, 2.0, 3.0)),
                    ..ViewState::default()
                };
                barrier.wait();
                save(&vault, &state)
            }));
        }
        for thread in threads {
            thread.join().unwrap().unwrap();
        }

        let x = load(&vault).camera.unwrap().0;
        assert!((0.0..writers as f32).contains(&x));
        let names: Vec<_> = std::fs::read_dir(vault.join(".text-graph"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("view")]);

        let _ = std::fs::remove_dir_all(&base);
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
