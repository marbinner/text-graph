//! File-type classification by extension — shared by the viewer (previews,
//! icons, colors), open actions (editor vs system opener), and anything
//! else that needs to know what a leaf *is*. Headless: no egui types.

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
    fn text_classification() {
        assert!(is_text("src/main.rs"));
        assert!(is_text("style.CSS"));
        assert!(is_text("misc/data.csv"));
        assert!(is_text("Makefile"));
        assert!(is_text("docs/LICENSE"));
        assert!(!is_text("a.png"));
        assert!(!is_text("blob.bin"));
        assert!(!is_text("archive.tar"));
        assert!(!is_text("plain"));
    }
}
