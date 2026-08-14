//! Creating notes and folders from the GUI — the one place text-graph
//! writes to the vault, and only ever NEW files (existing notes are never
//! touched; editing stays in the user's editor).
//!
//! Validation is pure and unit-tested; the fs writes are thin wrappers.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Normalize a user-typed name into a vault-relative `.md` path under `dir`
/// (`dir` is vault-relative with `/` separators; `""` = vault root). The
/// input may itself contain `/` — intermediate folders are implied.
pub fn note_rel_path(dir: &str, input: &str) -> Result<String> {
    let rel = clean_rel(dir, input)?;
    // case-insensitive so "Note.MD" doesn't become "Note.MD.md"
    let has_md = rel
        .len()
        .checked_sub(3)
        .and_then(|i| rel.get(i..))
        .is_some_and(|s| s.eq_ignore_ascii_case(".md"));
    Ok(if has_md { rel } else { format!("{rel}.md") })
}

/// Same, for a folder (no extension handling).
pub fn folder_rel_path(dir: &str, input: &str) -> Result<String> {
    clean_rel(dir, input)
}

fn clean_rel(dir: &str, input: &str) -> Result<String> {
    let input = input.trim().trim_matches('/');
    if input.is_empty() {
        bail!("name is empty");
    }
    if input.contains('\\') {
        bail!("backslashes are not allowed");
    }
    for part in input.split('/') {
        let part = part.trim();
        if part.is_empty() {
            bail!("empty path component");
        }
        if part == "." || part == ".." {
            bail!("'.' and '..' are not allowed");
        }
        if part.starts_with('.') {
            bail!("hidden names (leading '.') would be invisible to the graph");
        }
    }
    Ok(if dir.is_empty() { input.to_string() } else { format!("{dir}/{input}") })
}

/// Create an empty note at `rel` (creating parent folders), refusing to
/// overwrite anything. Returns the absolute path.
pub fn write_note(root: &Path, rel: &str) -> Result<PathBuf> {
    let abs = root.join(rel);
    if abs.exists() {
        bail!("{rel} already exists");
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&abs, "")?;
    Ok(abs)
}

/// Create a folder at `rel` (and parents). Idempotent.
pub fn make_folder(root: &Path, rel: &str) -> Result<PathBuf> {
    let abs = root.join(rel);
    if abs.is_file() {
        bail!("{rel} exists and is a file");
    }
    std::fs::create_dir_all(&abs)?;
    Ok(abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_gets_md_suffix() {
        assert_eq!(note_rel_path("", "ideas").unwrap(), "ideas.md");
        assert_eq!(note_rel_path("notes", "ideas").unwrap(), "notes/ideas.md");
    }

    #[test]
    fn existing_md_suffix_is_kept_case_insensitively() {
        assert_eq!(note_rel_path("", "ideas.md").unwrap(), "ideas.md");
        assert_eq!(note_rel_path("", "ideas.MD").unwrap(), "ideas.MD");
        assert_eq!(note_rel_path("", "мир").unwrap(), "мир.md"); // multibyte-safe
    }

    #[test]
    fn slashes_imply_folders_and_are_trimmed() {
        assert_eq!(note_rel_path("notes", "daily/2026-08-15").unwrap(), "notes/daily/2026-08-15.md");
        assert_eq!(note_rel_path("", "/ideas/").unwrap(), "ideas.md");
        assert_eq!(folder_rel_path("notes", "sub").unwrap(), "notes/sub");
    }

    #[test]
    fn rejects_empty_dots_hidden_and_backslash() {
        for bad in ["", "  ", "..", "a/../b", ".", "a//b", ".hidden", "a/.b", "a\\b"] {
            assert!(note_rel_path("", bad).is_err(), "should reject {bad:?}");
        }
    }
}
