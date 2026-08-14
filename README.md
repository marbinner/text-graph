# text-graph

A fast, native graph viewer for a folder of markdown notes. Point it at a
vault (an Obsidian vault works as-is) and get an interactive, Obsidian-style
force-directed graph — one static Rust binary, no Electron, no webview, no
runtime.

```
cargo run --release -- ~/notes          # open the graph window
cargo run --release -- stats ~/notes    # headless statistics
```

The vault is the database: nodes are your `.md` files and directories, edges
come from the directory structure and `[[wikilinks]]`. text-graph never
writes to your notes — editing happens in your own editor, and the graph
live-reloads when you save.

## The graph model

| Element | Source |
|---|---|
| **File node** | every `.md` file (`.obsidian/`, `.trash/`, hidden dirs skipped; git-ignore semantics deliberately off) |
| **Dir node** | every directory with markdown somewhere beneath it (empty ones pruned) |
| **Ghost node** | a `[[target]]` that resolves to nothing — a note you've referenced but not written; drawn hollow |
| **Contains edge** | filesystem parent → child; a true tree by construction, and the layout's skeleton |
| **WikiLink edge** | `[[...]]` in note bodies; drawn as faint curves, bright on the hovered node |

### Link resolution

Casefolded throughout; `.md` suffix optional; alias (`[[x|shown]]`) and
heading/block (`[[x#h]]`, `[[x#^id]]`) suffixes are stripped before resolving.

1. A target containing `/` matches by **path-component suffix** —
   `[[daily/2026-08-14]]` finds `notes/daily/2026-08-14.md`.
2. A bare name matches **file stems** first…
3. …then frontmatter **`aliases:`** (or singular `alias:`) — so a note named
   `2025-0803-1746-transcoders.md` with `aliases: [SAE]` is reachable as
   `[[SAE]]`, and the alias becomes its label in the graph.
4. Several candidates → the **first in sorted path order** wins and the link
   is flagged ambiguous (visible in `stats`).
5. Not edges at all: `![[embeds]]`, links to asset extensions (png/pdf/…),
   anything inside code fences or inline code, and self-links.
6. Still unresolved → a ghost node.

Labels prefer frontmatter `title:`, then the first alias, then the file stem.

## Controls

| Input | Action |
|---|---|
| drag empty space | pan |
| mouse wheel | zoom toward the cursor |
| drag a node | move it — pins to the cursor, the simulation responds live |
| hover | highlight the node's neighborhood, dim everything else |
| click | select (opens the detail pane with rendered markdown) |
| `Enter` / double-click | open the file in `$VISUAL`/`$EDITOR` (terminal editors get a **new terminal window**; set `$TERMINAL` to choose which); dirs open in the file manager |
| `/` or `Ctrl+F` | fuzzy search over names, aliases, and paths; matches stay lit, `Enter` jumps to the ringed best hit, `Esc` closes |
| `f` | frame the selection |
| `Home` | fit the whole graph |
| `Esc` | close search, else deselect |

Edit any file in the vault and the graph updates ~300ms after you save — new
links, files, and ghosts appear in place, and existing nodes keep their
positions (the layout ripples instead of re-settling).

## Determinism

Force-directed layouts are usually non-reproducible; this one isn't. The
simulation is seeded from a deterministic radial layout of the directory tree
and integrates with zero randomness, so the same vault settles into the same
picture every launch — spatial memory works. Node order, child order, and
resolution results are all deterministic too (a build-twice test enforces it).

## Feel

The layout lives in ~10 constants at the top of `src/sim.rs` (repulsion,
spring stiffness, rest lengths, gravity, damping). If clusters look too tight
or too spread for your vault, those are the dials.

## Project layout

```
src/
  vault.rs    walk + frontmatter/wikilink extraction (per-file, no global state)
  resolve.rs  Obsidian-style link resolution (stems → aliases → ghosts)
  graph.rs    arena: typed nodes, Contains tree, overlay links
  layout.rs   pure radial layout — the simulation's deterministic seed
  sim.rs      force simulation (springs, repulsion, gravity, cooling)
  stats.rs    headless statistics (`stats` subcommand)
  app.rs      egui shell: transform, input, painting, search, detail pane, reload
fixtures/
  vault/      synthetic test vault — every link variant and trap
  EXPECTED.md hand-counted ground truth the integration tests assert exactly
  gen-stress.sh generates a large flat vault (gitignored) for stress testing
```

`PLAN.md` holds the roadmap, including the deferred Phase 2 (MCP server so
agents can read/write the graph, then LLM-assisted ingest).

## Development

```
cargo test      # unit + integration; integration asserts fixtures/EXPECTED.md
cargo clippy --all-targets
```

House rules: if you touch `fixtures/vault/`, re-count `fixtures/EXPECTED.md`
and update the tests in the same commit — the numbers are asserted exactly.
One commit per change. See `CLAUDE.md` for the details that bite.
