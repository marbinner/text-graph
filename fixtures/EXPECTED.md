# fixtures/vault — expected parse results

Hand-counted ground truth for `fixtures/vault/`. The `stats` integration test asserts
these exact numbers. If you edit the vault, re-count and update this file in the same
commit.

## Design decisions this vault pins

| Behavior | Fixture that exercises it |
|---|---|
| `.obsidian/` and `.trash/` skipped entirely | `.trash/deleted-note.md` contains `[[index]]` — must not appear anywhere |
| Image files are Image nodes, with their dir chain | `assets/diagram.png` → Image node, `assets/` → Dir node |
| Every other file type is an Asset node, with its dir chain | `misc/data.csv` → Asset node, `misc/` → Dir node |
| Build/dependency dirs are skipped like dotdirs | `node_modules/`, `target/`, `__pycache__/` (no fixture — unit-tested via the watcher filter) |
| Wikilink to an existing image resolves | `[[diagram.png]]` in index.md → `assets/diagram.png` |
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
| External http(s) URLs become Web nodes + External edges (identity = normalized URL) | `projects/ideas.md` cites 2 (md-link + bare URL, trailing `.` trimmed) |
| Footnote-style citations `[^path]` are display-only (mdview), never edges | index.md cites `[^notes/readme.md]` and `[^readme]`; `[^1]` + its definition stay real footnotes |

## Expected node counts

| Kind | Count | Members |
|---|---|---|
| File | 13 | index, empty, frontmatter-only, bom, projects/{rust-app, ideas}, notes/{readme, scratch}, notes/daily/{2026-08-13, 2026-08-14}, topics/{rust, grafér}, languages/rust |
| Dir | 8 | vault root, assets, misc, projects, notes, notes/daily, topics, languages |
| Image | 1 | assets/diagram.png |
| Asset | 1 | misc/data.csv |
| Web | 2 | `https://docs.rs/notify`, `https://example.com/spec` (no tree parent, like ghosts) |
| Ghost | 2 | `missing-note`, `nonexistent/deep/ghost` |
| **Total** | **27** | |

## Expected edge counts

| Kind | Count | Notes |
|---|---|---|
| Contains | 22 | 13 files + 7 non-root dirs + 1 image + 1 asset, one parent each |
| WikiLink | 19 | 16 resolved to Files + 1 to Images + 2 to Ghosts |
| External | 2 | ideas.md → its two Web nodes |

### WikiLink edges by source

| Source file | Resolved targets | Ghost targets |
|---|---|---|
| index.md | projects/rust-app, notes/readme, notes/daily/2026-08-14, assets/diagram.png | missing-note |
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

Per-source totals: 4+5+1+1+1+2+1+1+1 = 17 resolved edges (16 to files, 1 to
the image); 2 ghost edges.

## Other expected stats

- Parse warnings: **1** (scratch.md frontmatter)
- Ambiguous resolutions: **1** (`[[rust]]`)
- Raw `[[` occurrences in countable files: **24** = 19 edges + 2 code traps
  + 2 embeds + 1 dropped self-link (useful cross-check when editing the vault)
- Depth histogram — dirs: d0×1 (root), d1×6, d2×1 (daily);
  files: d1×4, d2×7, d3×2; images: d2×1 (assets/diagram.png);
  assets: d2×1 (misc/data.csv)
- External edges: **2**, both from projects/ideas.md
  (`https://docs.rs/notify`, `https://example.com/spec` — already in
  canonical form, so normalization is a no-op here)
- Web node labels: `the docs` (from the `[the docs](…)` link text) and
  `spec` (slug mined from the bare URL); hosts stay in `name`

## Stress vault

`./gen-stress.sh [N]` (default 500) generates `stress-vault/flat/` — N+1 files in one
directory, each linking to its successor and to `hub.md`. Gitignored; deterministic.
Expected: N+1 File nodes, 2 Dir nodes, 0 ghosts, 2N WikiLink edges, hub in-degree N.
