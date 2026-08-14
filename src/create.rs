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

/// Materialize a ghost: the file whose STEM must equal the ghost's target.
/// `.md` is appended unconditionally — a ghost from `[[x.md.md]]` has
/// target `x.md` (resolution strips one suffix), so the note that resolves
/// it is `x.md.md`, not `x.md`.
pub fn ghost_rel_path(target: &str) -> Result<String> {
    Ok(format!("{}.md", clean_rel("", target)?))
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
    if input.contains(':') {
        // on Windows, root.join("C:x") REPLACES the base path entirely —
        // a vault escape. ':' in names breaks tmux targets anyway.
        bail!("':' is not allowed in names");
    }
    let mut parts = Vec::new();
    for part in input.split('/') {
        // the trimmed form is what gets created — checking one string and
        // writing another would let "a /b" produce a dir literally named
        // "a " whose notes ([[b]] vs " b") never resolve
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
        parts.push(part);
    }
    let rel = parts.join("/");
    Ok(if dir.is_empty() { rel } else { format!("{dir}/{rel}") })
}

/// Refuse to create through a symlinked path component: a linked dir (or
/// leaf) inside the vault could redirect the write outside it. The walker
/// doesn't follow links, so nothing behind one is part of the graph anyway.
fn reject_symlink_components(root: &Path, rel: &str) -> Result<()> {
    let mut cur = root.to_path_buf();
    for part in rel.split('/') {
        cur.push(part);
        if let Ok(m) = std::fs::symlink_metadata(&cur)
            && m.file_type().is_symlink()
        {
            bail!("{rel}: refusing to create through a symlink");
        }
    }
    Ok(())
}

/// Create an empty note at `rel` (creating parent folders), refusing to
/// overwrite anything. Returns the absolute path.
pub fn write_note(root: &Path, rel: &str) -> Result<PathBuf> {
    reject_symlink_components(root, rel)?;
    let abs = root.join(rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // O_CREAT|O_EXCL, not exists()-then-write: a racing writer (an agent
    // saving x.md at the same moment) must not be truncated, and a dangling
    // symlink must not be followed to create a file outside the vault.
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&abs) {
        Ok(_) => Ok(abs),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("{rel} already exists")
        }
        Err(e) => Err(e.into()),
    }
}

/// Create a folder at `rel` (and parents). Idempotent.
pub fn make_folder(root: &Path, rel: &str) -> Result<PathBuf> {
    reject_symlink_components(root, rel)?;
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
    fn rejects_empty_dots_hidden_backslash_and_colon() {
        for bad in
            ["", "  ", "..", "a/../b", ".", "a//b", ".hidden", "a/.b", "a\\b", "C:x", "C:/x"]
        {
            assert!(note_rel_path("", bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn components_are_trimmed_in_the_result_too() {
        // the checked form and the created form must be the same string
        assert_eq!(note_rel_path("", "a / b").unwrap(), "a/b.md");
        assert_eq!(folder_rel_path("d", " x / y ").unwrap(), "d/x/y");
    }

    #[test]
    fn ghost_rel_path_appends_md_unconditionally() {
        // a ghost from [[x.md.md]] has target "x.md"; the file that
        // resolves it is x.md.md (stem "x.md") — NOT x.md (stem "x")
        assert_eq!(ghost_rel_path("x.md").unwrap(), "x.md.md");
        assert_eq!(ghost_rel_path("missing-note").unwrap(), "missing-note.md");
        assert_eq!(ghost_rel_path("deep/ghost").unwrap(), "deep/ghost.md");
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("tg-create-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_note_refuses_to_overwrite() {
        let root = scratch();
        write_note(&root, "a/n.md").unwrap();
        std::fs::write(root.join("a/n.md"), "agent wrote this").unwrap();
        assert!(write_note(&root, "a/n.md").is_err(), "must not clobber");
        assert_eq!(
            std::fs::read_to_string(root.join("a/n.md")).unwrap(),
            "agent wrote this",
            "existing content must survive the refused create"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn write_note_does_not_follow_dangling_symlinks() {
        let root = scratch();
        let outside = root.join("outside-target");
        std::os::unix::fs::symlink(&outside, root.join("link.md")).unwrap();
        assert!(write_note(&root, "link.md").is_err(), "dangling symlink must not be followed");
        assert!(!outside.exists(), "nothing may be created at the symlink target");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn creation_refuses_symlinked_directories() {
        let root = scratch();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("sub")).unwrap();
        assert!(write_note(&root, "sub/x.md").is_err(), "symlinked dir must be rejected");
        assert!(!elsewhere.join("x.md").exists());
        assert!(make_folder(&root, "sub/deeper").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
