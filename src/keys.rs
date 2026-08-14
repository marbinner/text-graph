//! Keyboard input → tmux `send-keys` commands.
//!
//! Two channels, chosen to dodge the two classic failure modes:
//! - **Raw hex** (`send-keys -H`) for typed text and Ctrl/Alt chords — no
//!   command-line quoting problems, ever.
//! - **tmux key names** (`send-keys Up`, `BTab`, …) for special keys — tmux
//!   translates them according to the pane's current terminal modes
//!   (application cursor keys etc.), so we never have to guess encodings.
//!
//! egui-free: the app maps its events into these types; everything here is
//! headless-testable.

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Special {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
}

/// `send-keys -t <pane> -H aa bb …` — literal bytes, quoting-proof.
pub fn hex_cmd(pane: &str, bytes: &[u8]) -> String {
    let mut cmd = format!("send-keys -t {pane} -H");
    for b in bytes {
        cmd.push_str(&format!(" {b:02x}"));
    }
    cmd
}

/// A special key by tmux name, with C-/M-/S- modifier prefixes. Shift+Tab is
/// its own name (BTab); shift on Enter/Backspace/Escape has no terminal
/// encoding through this path and is dropped to the plain key.
pub fn special_cmd(pane: &str, key: Special, mods: Mods) -> String {
    use Special::*;
    let (name, shiftable): (String, bool) = match key {
        Enter => ("Enter".into(), false),
        Tab if mods.shift => ("BTab".into(), false),
        Tab => ("Tab".into(), false),
        Backspace => ("BSpace".into(), false),
        Escape => ("Escape".into(), false),
        Up => ("Up".into(), true),
        Down => ("Down".into(), true),
        Left => ("Left".into(), true),
        Right => ("Right".into(), true),
        Home => ("Home".into(), true),
        End => ("End".into(), true),
        PageUp => ("PageUp".into(), true),
        PageDown => ("PageDown".into(), true),
        Delete => ("DC".into(), true),
        Insert => ("IC".into(), true),
        // tmux's key table defines F1–F12 only; an unknown name would be
        // sent as literal characters ("F13" would type F, 1, 3)
        F(n) => (format!("F{}", n.clamp(1, 12)), true),
    };
    let mut prefixed = String::new();
    if mods.ctrl {
        prefixed.push_str("C-");
    }
    if mods.alt {
        prefixed.push_str("M-");
    }
    if mods.shift && shiftable {
        prefixed.push_str("S-");
    }
    prefixed.push_str(&name);
    format!("send-keys -t {pane} {prefixed}")
}

/// Ctrl/Alt chords on printable characters, as raw bytes: Ctrl+letter is the
/// ASCII control byte, Alt prefixes ESC. Returns None without Ctrl/Alt —
/// plain characters arrive as text events, not through here.
pub fn chord_cmd(pane: &str, c: char, mods: Mods) -> Option<String> {
    if !mods.ctrl && !mods.alt {
        return None;
    }
    let mut bytes = Vec::new();
    if mods.alt {
        bytes.push(0x1b);
    }
    if mods.ctrl {
        let b = match c {
            'a'..='z' => c as u8 & 0x1f,
            'A'..='Z' => c.to_ascii_lowercase() as u8 & 0x1f,
            ' ' | '@' => 0x00,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' => 0x1e,
            '_' | '/' => 0x1f,
            // xterm-family Ctrl+digit: 2..8 map onto the control bytes
            // NUL ESC FS GS RS US DEL; 1, 9, 0 pass through unchanged
            '2' => 0x00,
            '3' => 0x1b,
            '4' => 0x1c,
            '5' => 0x1d,
            '6' => 0x1e,
            '7' => 0x1f,
            '8' | '?' => 0x7f,
            '0' | '1' | '9' => c as u8,
            _ => return None,
        };
        bytes.push(b);
    } else {
        let mut buf = [0u8; 4];
        bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(hex_cmd(pane, &bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_literal_and_quote_proof() {
        assert_eq!(hex_cmd("%3", b"hi"), "send-keys -t %3 -H 68 69");
        assert_eq!(hex_cmd("%3", b"'\";\n"), "send-keys -t %3 -H 27 22 3b 0a");
    }

    #[test]
    fn special_names_and_modifiers() {
        let none = Mods::default();
        assert_eq!(
            special_cmd("%1", Special::Enter, none),
            "send-keys -t %1 Enter"
        );
        assert_eq!(
            special_cmd(
                "%1",
                Special::Tab,
                Mods {
                    shift: true,
                    ..none
                }
            ),
            "send-keys -t %1 BTab"
        );
        assert_eq!(
            special_cmd(
                "%1",
                Special::Up,
                Mods {
                    ctrl: true,
                    shift: true,
                    ..none
                }
            ),
            "send-keys -t %1 C-S-Up"
        );
        assert_eq!(
            special_cmd(
                "%1",
                Special::Enter,
                Mods {
                    shift: true,
                    ..none
                }
            ),
            "send-keys -t %1 Enter",
            "shift on enter drops to plain"
        );
        assert_eq!(special_cmd("%1", Special::F(5), none), "send-keys -t %1 F5");
        assert_eq!(
            special_cmd("%1", Special::F(13), none),
            "send-keys -t %1 F12",
            "clamped"
        );
    }

    #[test]
    fn chords_are_bytes() {
        let ctrl = Mods {
            ctrl: true,
            ..Default::default()
        };
        let alt = Mods {
            alt: true,
            ..Default::default()
        };
        let both = Mods {
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert_eq!(
            chord_cmd("%1", 'c', ctrl),
            Some("send-keys -t %1 -H 03".into())
        );
        assert_eq!(
            chord_cmd("%1", 'x', alt),
            Some("send-keys -t %1 -H 1b 78".into())
        );
        assert_eq!(
            chord_cmd("%1", 'b', both),
            Some("send-keys -t %1 -H 1b 02".into())
        );
        assert_eq!(
            chord_cmd("%1", ' ', ctrl),
            Some("send-keys -t %1 -H 00".into())
        );
        // xterm Ctrl+digit control bytes (Ctrl+6 = RS is vim's alternate-file)
        assert_eq!(
            chord_cmd("%1", '6', ctrl),
            Some("send-keys -t %1 -H 1e".into())
        );
        assert_eq!(
            chord_cmd("%1", '8', ctrl),
            Some("send-keys -t %1 -H 7f".into())
        );
        assert_eq!(
            chord_cmd("%1", '1', ctrl),
            Some("send-keys -t %1 -H 31".into())
        );
        assert_eq!(
            chord_cmd("%1", 'a', Mods::default()),
            None,
            "plain chars go via text"
        );
    }
}
