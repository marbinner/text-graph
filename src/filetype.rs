//! File-type classification by extension — shared by the viewer (previews,
//! icons, colors), open actions (editor vs system opener), and anything
//! else that needs to know what a leaf *is*. Headless: no egui types;
//! colors are RGB tuples and glyphs are chars from `assets/icons.ttf`
//! (a Nerd Font subset — regenerate with `assets/gen-icons-font.sh`,
//! keeping its codepoint list in sync with the table here).

/// Lowercase-compared extensions considered textual: safe to excerpt in
/// previews and sensible to open in $EDITOR. An allowlist, not a heuristic
/// — misclassifying a binary as text costs a mojibake preview.
const TEXT_EXTS: &[&str] = &[
    "md",
    "txt",
    "text",
    "rst",
    "org",
    "adoc", // prose
    "rs",
    "py",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "hh",
    "go",
    "java",
    "rb",
    "lua",
    "sh",
    "bash",
    "zsh",
    "fish",
    "pl",
    "php",
    "swift",
    "kt",
    "scala",
    "hs",
    "ml",
    "ex",
    "exs",
    "clj",
    "el",
    "vim",
    "zig",
    "nim",
    "cs",
    "sql", // code
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "svg",
    "xml",
    "svelte",
    "vue", // markup
    "json",
    "jsonc",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    "editorconfig", // config
    "csv",
    "tsv",
    "log",
    "lock",
    "diff",
    "patch", // data / misc
];

/// Extensionless files that are conventionally text (hidden dotfiles never
/// become nodes, so they don't need covering).
const TEXT_NAMES: &[&str] = &[
    "makefile",
    "dockerfile",
    "justfile",
    "rakefile",
    "gemfile",
    "vagrantfile",
    "procfile",
    "license",
    "notice",
    "copying",
    "changelog",
    "authors",
    "codeowners",
    "readme",
    "todo",
    "version",
];

/// The file name (final component) of a relative, forward-slash path.
fn file_name(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map_or(rel_path, |(_, f)| f)
}

/// The extension of a relative path, if any — the part after the last dot
/// of the final component (empty for trailing-dot names, None for
/// extensionless ones).
pub fn ext_of(rel_path: &str) -> Option<&str> {
    file_name(rel_path).rsplit_once('.').map(|(_, e)| e)
}

/// Is this file textual — previewable as an excerpt and editable in $EDITOR?
pub fn is_text(rel_path: &str) -> bool {
    match ext_of(rel_path) {
        Some(ext) => TEXT_EXTS.iter().any(|t| ext.eq_ignore_ascii_case(t)),
        None => {
            let name = file_name(rel_path);
            TEXT_NAMES.iter().any(|t| name.eq_ignore_ascii_case(t))
        }
    }
}

/// Canvas icon for a leaf file: a Nerd Font glyph plus its color (RGB,
/// tuned for the dark canvas). Every glyph here must be in the subset
/// `assets/gen-icons-font.sh` produces.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FileIcon {
    pub glyph: char,
    pub color: (u8, u8, u8),
}

/// Neutral gray for text-ish files without a stronger identity.
const PLAIN: (u8, u8, u8) = (0x9a, 0xa0, 0xac);
/// Fallback for unknown extensions — matches the Asset disc color.
const UNKNOWN: (u8, u8, u8) = (0x8b, 0x92, 0x9f);

/// Glyphs used directly by the viewer (not tied to an extension).
pub const ICON_FOLDER: FileIcon = FileIcon {
    glyph: '\u{e5ff}',
    color: (0x7a, 0xa2, 0xf7),
};
pub const ICON_IMAGE: FileIcon = FileIcon {
    glyph: '\u{f03e}',
    color: (0x9e, 0xce, 0x6a),
};
/// Globe, for external web nodes.
pub const ICON_WEB: FileIcon = FileIcon {
    glyph: '\u{f0ac}',
    color: (0x56, 0xb6, 0xc2),
};

/// The icon for a leaf, by extension (or well-known extensionless name).
pub fn icon_of(rel_path: &str) -> FileIcon {
    let icon = |glyph: char, color: (u8, u8, u8)| FileIcon { glyph, color };
    let Some(ext) = ext_of(rel_path) else {
        // Makefile, Dockerfile, LICENSE, … — a gear for build-ish names,
        // plain text page for the rest, generic file for true unknowns
        let name = file_name(rel_path);
        return if name.eq_ignore_ascii_case("dockerfile") {
            icon('\u{e7b0}', (0x66, 0xb8, 0xe8))
        } else if ["makefile", "justfile", "rakefile"]
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n))
        {
            icon('\u{f013}', PLAIN)
        } else if is_text(rel_path) {
            icon('\u{f0f6}', PLAIN)
        } else {
            icon('\u{f016}', UNKNOWN)
        };
    };
    let e = ext.to_ascii_lowercase();
    match e.as_str() {
        "md" => icon('\u{e73e}', (0xb8, 0xbc, 0xc8)),
        "txt" | "text" | "rst" | "org" | "adoc" | "log" => icon('\u{f0f6}', PLAIN),
        "py" => icon('\u{e73c}', (0x6f, 0xa8, 0xdc)),
        "rs" => icon('\u{e7a8}', (0xde, 0xa5, 0x84)),
        "js" | "mjs" | "cjs" => icon('\u{e74e}', (0xf1, 0xe0, 0x5a)),
        "ts" => icon('\u{e628}', (0x5a, 0x9c, 0xe0)),
        "jsx" | "tsx" => icon('\u{e7ba}', (0x67, 0xd8, 0xef)),
        "html" | "htm" => icon('\u{e736}', (0xe3, 0x6d, 0x44)),
        "css" | "scss" | "sass" | "less" => icon('\u{e749}', (0x9b, 0x7c, 0xd6)),
        "json" | "jsonc" => icon('\u{e60b}', (0xcb, 0xcb, 0x41)),
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" | "properties"
        | "editorconfig" => icon('\u{e615}', (0x9a, 0xb8, 0x73)),
        "sh" | "bash" | "zsh" | "fish" => icon('\u{e795}', (0x7f, 0xba, 0x63)),
        "c" | "h" => icon('\u{e61e}', (0x9a, 0xb0, 0xc9)),
        "cpp" | "hpp" | "cc" | "hh" => icon('\u{e61d}', (0x6f, 0xa8, 0xdc)),
        "go" => icon('\u{e626}', (0x6a, 0xd7, 0xe5)),
        "java" | "kt" => icon('\u{e738}', (0xd9, 0x8e, 0x48)),
        "rb" => icon('\u{e739}', (0xd6, 0x6a, 0x6a)),
        "php" => icon('\u{e73d}', (0x8f, 0x93, 0xc9)),
        "lua" => icon('\u{e620}', (0x7a, 0x8b, 0xd8)),
        "swift" => icon('\u{e755}', (0xe8, 0x9a, 0x5e)),
        "sql" => icon('\u{e706}', (0xc9, 0xa2, 0x6a)),
        "csv" | "tsv" => icon('\u{f0ce}', (0x8a, 0xc4, 0x8a)),
        "svg" => icon('\u{f03e}', (0x9e, 0xce, 0x6a)),
        "pdf" => icon('\u{f1c1}', (0xe0, 0x6c, 0x75)),
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" => icon('\u{f1c6}', (0xc4, 0xa1, 0x5c)),
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => icon('\u{f1c7}', (0xc9, 0x82, 0xc9)),
        "mp4" | "mkv" | "mov" | "avi" | "webm" => icon('\u{f1c8}', (0xc9, 0x82, 0x82)),
        "lock" => icon('\u{f023}', PLAIN),
        _ if is_text(rel_path) => icon('\u{f0f6}', PLAIN),
        _ => icon('\u{f016}', UNKNOWN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_is_last_component_only() {
        assert_eq!(ext_of("a/b/c.tar.gz"), Some("gz"));
        assert_eq!(ext_of("dir.d/plain"), None);
        assert_eq!(ext_of("x.PY"), Some("PY"));
    }

    #[test]
    fn icons_resolve_by_extension_name_and_fallback() {
        assert_eq!(icon_of("a/b.py").glyph, '\u{e73c}');
        assert_eq!(icon_of("style.CSS").glyph, '\u{e749}');
        assert_eq!(icon_of("notes/x.md").glyph, '\u{e73e}');
        assert_eq!(icon_of("Dockerfile").glyph, '\u{e7b0}');
        assert_eq!(icon_of("LICENSE").glyph, '\u{f0f6}', "known text name");
        assert_eq!(icon_of("blob.bin").glyph, '\u{f016}', "unknown ext");
        assert_eq!(icon_of("noext").glyph, '\u{f016}', "unknown name");
    }

    #[test]
    fn text_classification() {
        assert!(is_text("src/main.rs"));
        assert!(is_text("style.CSS"));
        // module JS got the JS icon but was classed binary — no preview/edit
        assert!(is_text("lib/util.mjs"));
        assert!(is_text("lib/util.cjs"));
        assert!(is_text("misc/data.csv"));
        assert!(is_text("Makefile"));
        assert!(is_text("docs/LICENSE"));
        assert!(!is_text("a.png"));
        assert!(!is_text("blob.bin"));
        assert!(!is_text("archive.tar"));
        assert!(!is_text("plain"));
    }
}
