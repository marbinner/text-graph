# fixtures/vault — expected parse results

Hand-counted ground truth for `fixtures/vault/`. The `stats` integration test asserts
these exact numbers. If you edit the vault, re-count and update this file in the same
commit.

## Design decisions this vault pins

| Behavior | Fixture that exercises it |
|---|---|
| `.obsidian/` and `.trash/` skipped entirely | `.trash/deleted-note.md` contains `[[index]]` — must not appear anywhere |
| Non-md files are not nodes | `assets/diagram.png` |
| Dirs with no md descendants are pruned | `assets/` must not become a Dir node |
| Embeds skipped in v1 | `![[diagram.png]]` and `![[embedded-note-trap]]` in index.md — the second is md-resolvable, so an extraction regression surfaces as a ghost |
| Markdown-style links not edges in v1 | `[ideas](projects/ideas.md)` in index.md |
| Links in fenced code blocks are not edges | ```` ```text [[trap-link]] ``` ```` in index.md |
| Links in inline code are not edges | `` `[[inline-trap]]` `` in index.md |
| Case-insensitive resolution | `[[Readme]]` → `notes/readme.md` |
| Unique-basename resolution across dirs | `[[2026-08-14]]`, `[[ideas]]`, `[[grafér]]`, `[[empty]]` |
| Explicit-path resolution | `[[projects/rust-app]]`, `[[topics/rust]]`, `[[languages/rust]]` |
| **Ambiguous basename** → first in sorted path order, flagged | `[[rust]]` → `languages/rust.md` (not `topics/rust.md`); ambiguous count = 1 |
| Alias syntax | `[[ideas\|my ideas]]`, `[[rust-app\|the app]]`, `[[languages/rust\|other rust]]` |
| Heading suffix stripped for resolution | `[[index#Heading One]]` → index.md (heading not validated in v1) |
| Block suffix stripped for resolution | `[[scratch#^abc123]]` → scratch.md |
| Unresolved target → Ghost node + edge | `[[missing-note]]`, `[[nonexistent/deep/ghost]]` |
| Self-links resolve but are dropped (no self-loop edge) | `[[readme]]` inside `notes/readme.md` |
| Alias resolution (frontmatter `aliases:`) | `[[rustlang]]` in 2026-08-13.md → `languages/rust.md` |
| Stem beats alias, silently | alias `empty` on languages/rust.md must not capture `[[empty]]`, no ambiguity flagged |
| Garbage frontmatter → warning, body still parsed | `notes/scratch.md` (its `[[index]]` link must still be extracted) |
| Missing frontmatter is fine | `projects/ideas.md`, daily notes |
| Frontmatter-only file, empty file | `frontmatter-only.md`, `empty.md` |
| UTF-8 BOM tolerated | `bom.md` (BOM must not corrupt frontmatter detection or first link) |
| CRLF tolerated | `notes/daily/2026-08-14.md` (both its links must extract) |
| Unicode filenames | `topics/grafér.md` |

## Expected node counts

| Kind | Count | Members |
|---|---|---|
| File | 13 | index, empty, frontmatter-only, bom, projects/{rust-app, ideas}, notes/{readme, scratch}, notes/daily/{2026-08-13, 2026-08-14}, topics/{rust, grafér}, languages/rust |
| Dir | 6 | vault root, projects, notes, notes/daily, topics, languages (assets pruned) |
| Ghost | 2 | `missing-note`, `nonexistent/deep/ghost` |
| **Total** | **21** | |

## Expected edge counts

| Kind | Count | Notes |
|---|---|---|
| Contains | 18 | 13 files + 5 non-root dirs, one parent each |
| WikiLink | 18 | 16 resolved to Files + 2 to Ghosts |

### WikiLink edges by source

| Source file | Resolved targets | Ghost targets |
|---|---|---|
| index.md | projects/rust-app, notes/readme, notes/daily/2026-08-14 | missing-note |
| projects/rust-app.md | languages/rust (ambiguous), topics/rust, projects/ideas, index, notes/scratch | |
| projects/ideas.md | | nonexistent/deep/ghost |
| notes/readme.md | topics/grafér | |
| notes/scratch.md | index | |
| notes/daily/2026-08-13.md | languages/rust (via alias `rustlang`) | |
| notes/daily/2026-08-14.md | notes/daily/2026-08-13, projects/rust-app | |
| topics/rust.md | languages/rust | |
| topics/grafér.md | index | |
| bom.md | empty | |
| (all others) | — | — |

Per-source totals: 3+5+1+1+1+2+1+1+1 = 16 file edges; 2 ghost edges.

## Other expected stats

- Parse warnings: **1** (scratch.md frontmatter)
- Ambiguous resolutions: **1** (`[[rust]]`)
- Raw `[[` occurrences in countable files: **23** = 18 edges + 2 code traps
  + 2 embeds + 1 dropped self-link (useful cross-check when editing the vault)
- Depth histogram — dirs: d0×1 (root), d1×4, d2×1 (daily);
  files: d1×4, d2×7, d3×2

## Stress vault

`./gen-stress.sh [N]` (default 500) generates `stress-vault/flat/` — N+1 files in one
directory, each linking to its successor and to `hub.md`. Gitignored; deterministic.
Expected: N+1 File nodes, 2 Dir nodes, 0 ghosts, 2N WikiLink edges, hub in-degree N.
