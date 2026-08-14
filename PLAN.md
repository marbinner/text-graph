# text-graph — Plan

*Last updated 2026-08-14 (v0.2.0, 108 tests). Where to pick up: milestone F
(agents talk) is fully spec'd below — design converged, NO code yet; start at
slice F1 (CLI trio). Still queued behind it: jump history (Ctrl+O/I), Phase 2
step 0, the audit backlog under D.*

**Phase 1 (current):** a fast native GUI over a markdown vault. Point it at a folder of
`.md` files and it renders an interactive graph. One static Rust binary — egui, no
webview, no JS, no runtime.

```
text-graph ~/notes          # open the graph window
text-graph stats ~/notes    # headless: counts, edge sources, unresolved links
```

The LLM layer from the original concept is deferred, not dropped — see Phase 2 at the
bottom. Phase-1 architecture is chosen so Phase 2 bolts on without a rewrite.

---

## Data model

Nodes are directories and files; edges are typed and come from multiple sources that
coexist rather than competing:

```rust
enum NodeKind { Dir, File, Image, Ghost } // Ghost = referenced but nonexistent target;
                                          // Image = raster file, rendered as a thumbnail
enum EdgeKind { Contains, WikiLink }      // later: MdLink, Tag, Embed, Frontmatter

struct Edge { from: NodeId, to: NodeId, kind: EdgeKind }
```

- **`Contains`** — from the filesystem. A true tree *by construction* (every file has
  exactly one parent dir, no cycles, total coverage). This is the layout spine; no
  cycle-breaking or provenance heuristics needed to trust it.
- **`WikiLink`** — `[[...]]` references, an overlay on top of the spine. Rendered only
  for the hovered/selected node.
- **Memory:** nodes store metadata + extracted links only. Bodies are parsed and
  discarded at ingest; the detail pane (C) reads the selected file on demand.

### Extract → resolve, two phases

1. **Extract** — per-file, parallel, no global state. Emits edges whose targets are
   still strings (`[[foo/bar]]`).
2. **Resolve** — build the name index once, map every target string to a `NodeId`.
   Sequential, O(edges).

A new edge source later = one extractor function + one match arm in the renderer.

Resolution implements Obsidian semantics and gets its own module + tests: `[[note]]`,
`[[note|alias]]`, `[[note#heading]]`, `[[note#^block]]`, `[[dir/note]]`;
shortest-unique-path matching; case-insensitive. Parsing goes through `pulldown-cmark`
events so a `[[link]]` inside a code fence never becomes an edge. Unresolved targets
become `Ghost` nodes — in the model from A, rendered from C.

### v1 scope cuts (decisions, not accidents)

- Only `.md` files, raster images (as `Image` nodes with thumbnails — a v0.2
  addition that reversed the original "only .md" cut), and directories become
  nodes; other files are skipped.
- `![[embeds]]` are skipped. A plain `[[pic.png]]` link resolves to the Image
  node when the image exists; links to *missing* assets are skipped, not
  ghosted.
- A flat vault (everything in one folder) degrades to a single ring around the root —
  acceptable, not a bug; hover cross-links still make it useful.

### Robustness on real vaults

Skip `.obsidian/` and `.trash/` explicitly (the walker knows `.gitignore`, not Obsidian
conventions). Tolerate per-file read/parse errors without aborting the walk. Handle
BOM and CRLF. Don't follow symlinks (walker default).

---

## Determinism — a requirement, not a nicety

"Same vault → same layout, every run" is a core promise, and the parallel walker breaks
it by default (nondeterministic yield order → nondeterministic NodeIds). Rules from
commit one:

- Sort walked paths before assigning NodeIds.
- Sort children by name (dirs first, then files).
- Every iterated/serialized collection has a defined order. No bare `HashMap` iteration
  feeds layout or output.

---

## Layout

**Force-directed (Obsidian-style), seeded by the deterministic radial layout.** The
radial tree (root at center, sector per subtree ∝ leaf count — pure function in
`layout/`, O(n), tested without a window) provides initial positions; `sim/` then runs
a d3-style force simulation: springs on Contains + WikiLink edges, pairwise repulsion,
weak gravity, alpha cooling. No randomness anywhere, so the same vault still settles
into the same picture every run — the seed is what buys reproducibility back from a
force layout.

- Animated settling; sim stops at alpha-min, after which egui idles at ~0% CPU.
- Dragging a node pins it and reheats the sim (the graph responds live).
- Hover dims everything outside the active node's neighborhood; node radius scales
  with degree; ghosts participate, seeded next to their first referencer.
- Wikilinks always visible as faint curves, bright when touching the active node.
- Repulsion is O(n²) — fine into the low thousands; Barnes–Hut quadtree is the
  milestone-D upgrade if profiling demands it.
- Known stress case: the 800-sibling folder becomes a large disc around its dir hub.
  Collapse-by-default above N children remains the milestone-D fix.

---

## Rendering & interaction (v1)

- Pan = drag; **zoom toward the cursor**, not viewport center (small code, huge feel).
- One world→screen transform owned in one place; all input + paint goes through it.
- Hover: highlight node + incident edges, show label. Click: select, persist highlight.
- Perf, in order, measure before escalating:
  1. Viewport culling (in B — ~10 lines, everything downstream gets cheaper)
  2. Label LOD — text only above a screen-radius threshold (galley construction is the
     real cost, not circles)
  3. `Shape::circle_filled` per node until profiling says hand-built `Mesh`; paint code
     structured so that's a one-function swap
  4. Hit test = linear scan over *culled* nodes; no spatial index until a frame-time
     number demands one
- Idle cost ~0% CPU for free (egui repaints on input only).

---

## Crates

Milestone A (headless core) — keep it to exactly this:

```toml
[dependencies]
anyhow         = "1"
ignore         = "0.4"     # ripgrep's ignore-aware walker
pulldown-cmark = { version = "0.13", default-features = false }
serde_yaml_ng  = "0.10"    # frontmatter (serde_yaml is unmaintained)
```

Added at the milestone that needs them, not before: `eframe`/`egui` 0.36 (B),
`egui_commonmark` (C — verify egui compat then), `nucleo-matcher` (C), `notify` pinned
to stable 8.x (C — 9.x is RC). `serde` derive returns when something needs serializing
— frontmatter reads go through `serde_yaml_ng::Value`, so real vaults' numeric titles
and arbitrary extra fields don't warn. No `rayon` (`ignore` walks in parallel itself).
No clap yet — two CLI forms hand-parse; add it if the CLI grows.

---

## Repo layout

```
src/
  vault/     walk + frontmatter/markdown parse + wikilink extraction (string targets)
  resolve/   Obsidian link resolution: name index, unique-path, case rules. Own tests.
  graph/     arena, typed edges, queries. Clean public API (Phase 2 depends on it).
  layout/    pure: graph -> positions. No egui dependency. Snapshot tests.
  app/       eframe shell: transform, input, painting, panels.
  main.rs
fixtures/
  vault/     synthetic test vault (see below)
```

**Fixture vault is the first commit of milestone A.** It covers: every wikilink variant,
a `[[link]]` inside a code fence (must not edge), unresolved targets, CRLF/BOM/unicode
files, deep nesting, and a script-generated big flat directory. Drives extraction tests
and layout snapshots.

---

## Milestones

**A — Headless core. ✓ done.** Fixture vault; walk, parse, extract, resolve; typed-edge
arena; deterministic ordering; `stats` command. Fully tested with no window, no API key.

**B — The window. ✓ done.** Force-directed Obsidian-style layout seeded by the radial
tree (deterministic), node drag with live sim response, hover-dim of non-neighborhood,
degree-scaled node sizes, alias resolution, ghosts rendered hollow.

**C — Daily driver. ✓ done.** One commit per step, in order:
1. Fuzzy search (`nucleo-matcher`): `/` opens bar; matches name+path+aliases; live
   dims non-matches; Enter frames + selects best hit; Esc clears.
2. Detail pane (`egui_commonmark`): selection opens right panel with rendered
   markdown, body read from disk on demand. Ghost selected → list of referencers.
3. Open in `$EDITOR` on Enter / double-click. Viewer stays read-only.
4. Live reload (`notify` 8.x, debounced): full re-scan on change (cheap), sim
   positions carried over by path + gentle reheat so edits ripple instead of
   re-settling; selection/camera survive by path.
5. Navigation: `f` frames selection, `Home` fits all, `Esc` deselects.

Checkpoint after C: daily-drive it against the real vault; annoyances set D's order.

**D — Scale & polish (as needed, not speculatively).** Collapse/expand subtrees
(default-collapse huge sibling sets), Barnes–Hut repulsion only if a >2k-node vault
shows up, sim-constant tuning pass, label collision avoidance if labels annoy.
Camera + card-arrangement persistence under `.text-graph/` ✓ done (state.rs).

Audit backlog (external audit, 2026-08):
- Background rebuild worker ✓ done (generation-numbered, stale discarded).
- In-GUI diagnostics ✓ done (⚠ badge + health window, app/diag.rs).
- Query layer core ✓ done (outlinks/backlinks indexes, by_path/by_ident,
  Link.offset preserved) — JSON/headless output still deferred to Phase 2.
- app decomposition ✓ done: app/{mod,terminals,navigator,actions,reload,diag}.rs,
  terminal state grouped in terminals::Terminals. mod.rs ~1,200 lines (canvas +
  keys + search).
- Perf budgets against the stress vault (scan/build/settle/reload at 0.5k/2k/10k).
- License + release packaging (user decision on license first).

**E — Terminals in the graph (done, except the procfs fallback).** Agent TUIs (claude, codex, pi, any harness)
live in tmux sessions; the viewer renders them as live terminal cards anchored to the
node they were opened at, and you type into them in place. Architecture: the viewer
attaches as a **tmux control-mode client** (`tmux -C attach`) — the iTerm2 approach —
so sessions persist beyond the viewer, external `tmux attach` keeps working, and tmux
answers every terminal query the TUI makes; we only ever render display streams.

Status: E1–E3 ✓ done (fidelity decision point passed — a live claude TUI mirrors
faithfully on vt100; capture-replay cursor restore was the one real bug found).
Card drag-to-arrange, agent/terminal launch (right-click a node → Launch agent /
New terminal → `agents::launch{,_shell}` spawns a sized `tg_*` session), kill
(card right-click, confirm submenu, `kill-pane`), and external attach (card
right-click → new terminal window running `tmux attach ; select-window/pane`)
are done; note/folder creation and ghost materialization landed alongside
(right-click menu, `create.rs`). Remaining E4: procfs fallback for non-tmux
agents.

Since then (post-E polish wave): ranger navigator (sibling column + preview +
connections strip, modal hjkl / gg / G / f / ]-[ link walking, camera follow),
query-layer core (out/backlink indexes, by_path, preserved offsets),
background reload worker, ⚠ health badge, per-vault persistence, native card
resize, bracketed paste, Alt chords, node type glyphs, focus-vs-cursor card
states with any-zoom expansion, compact-card live status lines, CI +
rustfmt, and the app/ module decomposition.

Second polish wave (2026-08-14): images as first-class nodes (scan →
resolve → stats → live reload; canvas thumbnails via a background decode
worker, lib thumb.rs); zoom-in text preview cards on File nodes; full-body
hover preview popup on dwell (markdown-rendered, images large); cursor
flashlight labels; Ctrl+click card pinning (several expanded at once,
persisted); one-gesture terminal release that keeps the selection; and the
anti-flicker rules — file-backed caches evict by (mtime, len) stamp, boxes
cull by extent, dims fade. Next: milestone F below; then jump history
(Ctrl+O / Ctrl+I) and Phase 2 step 0 (query JSON + read-only MCP).

1. **E1 — control-mode client + screen mirror** (lib, egui-free). Protocol parsing
   (`%begin/%end/%error` reply blocks FIFO-correlated, `%output` octal-unescaped,
   layout-change notifications), per-pane screens via `vt100` behind our own
   `TermGrid` facade (parser swappable for `alacritty_terminal` if fidelity demands),
   initial paint via `capture-pane -e`. Integration-tested against a scripted real
   tmux on a private `-L` socket; skipped gracefully where tmux is absent.
2. **E2 — read-only live mirrors in the graph.** Terminal cards anchored to dir
   nodes; LOD (zoomed out: name + activity glow + last line; zoomed in: the live
   styled grid); discovery of vault sessions (`tg_*` ours by name, foreign panes by
   cwd-in-vault + agent-command allowlist; identity is sticky for the pane's
   lifetime — pinned to the pane's root pid — so tool calls of any length
   survive, with a short grace only for panes that vanish from a scan).
   — **Decision point: mirror a real claude session and judge fidelity before E3.**
3. **E3 — input.** Click-to-focus; egui key/text events → xterm byte sequences →
   `send-keys -H`; all graph keybinds suspend while a terminal is focused; release
   by clicking empty space or Ctrl+Q. Known long tail: bracketed paste,
   Shift+Enter, extended keys — expect a punch-list from real use. Mouse-into-
   terminal and in-graph scrollback are explicitly v2 (attach externally for those).
4. **E4 — lifecycle.** Launch-from-node buttons (`tmux new-session -d -s tg_… -c
   <dir> <agent>`), kill, external attach in a new terminal window (reuses the
   editor-window machinery), card drag-to-arrange, and the procfs fallback tier so
   agents running in a bare terminal (no tmux) still show as presence badges.

**F — Agents talk (current).** Multi-agent communication, designed from first
principles and deliberately bus-free: agents ping each other **directly
through tmux** (which we already speak), remember **through the vault**
(conclusions written as notes), and each agent **is a text node** — live body
= its terminal, persistent body = its transcript, which outlives the session
as its ghost. Relevance-routing is the reader's job (LLMs judge "what
matters" better than any router we'd write), so there is no watch list, no
event queue, no digest machinery, no `mail/` directory, and the viewer never
types into a pane on its own — only agents and the human do.

1. **F1 — CLI trio** (lib `comm.rs`, egui-free; hand-parsed subcommands like
   `stats`): `roster` (live agents: session, anchor, busy/idle via
   `window_activity`, status line), `send <agent> <msg>` (paste-safe via
   `load-buffer` + `paste-buffer -p` + Enter; sender attribution from
   `$TMUX_PANE`; roster-validated addressing that fails loudly with
   suggestions), `peek <agent> [-n]` (capture-pane wrapper), `protocol`
   (prints the conventions: chatter in terminals, conclusions in notes; the
   first ping teaches newcomers). Vault root found git-style: walk up from
   cwd to the nearest `.text-graph/`, else cwd. Private-socket integration
   test; scan-format additions verified against a real server.
2. **F2 — the experiment (decision point).** Two real `tg_` agents on a
   scratch vault negotiate one small artifact purely over `send`/`peek` and a
   shared topic note, no human relay. Ergonomics verdict gates the rest.
3. **F3 — identity pinning.** Mint a UUID per `tg_` launch; `claude
   --session-id`, `pi --session-id` (pi's native store co-located via
   `--session-dir` under `.text-graph/sessions/`); session meta (harness,
   uuid, cwd, started) written to `.text-graph/sessions/<name>/meta`. This is
   what makes resume exact later. Foreign panes degrade gracefully (no pin).
4. **F4 — attention signal.** Per-pane last-output timestamps in the mirror;
   busy→idle transition sets attention (amber card), cleared by focusing;
   `t` cycles attention cards. Also gates transcript sync.
5. **F5 — three-layer transcripts** in `agents/<session>/` (gitignore hint
   for project vaults): the harness's own jsonl stays home and powers
   *native* resume (pointer only, never written by us); adapters normalize
   claude/pi jsonl into generic `events.jsonl` ({ts, role, text, tool});
   `transcript.md` renders from it — a real node whose `[[links]]` become
   edges, so conversations wire themselves into the graph. Screen-scrape is
   the universal fallback tier (bare shells, unknown harnesses, broken
   adapters). Sync on idle transitions + exit flush; `history-limit` bumped
   on `tg_` launches so attach back-fill is deep.
6. **F6 — agent-as-node GUI.** Card ↔ transcript note association;
   detail-pane live tail; on session death the note remains as the agent's
   ghost in place; right-click → Resume (native, from pinned meta) or
   Rebirth from transcript (fresh session in *any* harness, pointed at the
   transcript — cross-harness handoff).

MCP stays deferred to Phase 2 as a thin adapter over the same files. No
sockets, no daemons: message latency is dominated by agent thinking time,
and every layer stays inspectable as plain files.

---

## Phase 2: the intelligence layer

Reframed after milestone E: agents arrive by being *opened in the vault* (terminals
in the graph), not by registering infrastructure. Ordered so each step is useful on
its own:

0. **Query layer first** (audit recommendation, adopted): indexed
   backlinks/outlinks + diagnostics behind a stable path-keyed API in the lib,
   with a headless JSON output mode. The same layer then backs a richer detail
   pane, and the MCP server becomes a thin adapter. Read-only before write.
1. **MCP server — now optional, later.** Graph-aware tools (`search`, `read`,
   `create`, `append`, `link`, `children`) over stdio, for agents that want
   structured access instead of raw file edits. Presence, launching, and live
   observation are milestone E's job and need none of this.
2. **Retrieval** — tantivy BM25, incremental via blake3-gated reindex; powers both the
   MCP `search` tool and, later, the decider's context. Still $0.
3. **The decider** — `text-graph add`: buffer → retrieve → one structured-output call
   returning append/create/link actions → apply → write markdown. Ships together with
   the replay-eval harness (fixture transcripts → graph snapshot → diff).
4. **Maintenance passes** — split oversized nodes, reattach orphans/ghosts, on idle.
5. **Adapters** — URL → readability-extract → same pipeline; later mic → STT.

Decisions worth keeping from the original plan, so Phase 1 stays compatible:

- **`graph/` keeps a clean public API** — it later backs an MCP server (agents read/write
  the graph as shared memory) and the ingest pipeline's apply step.
- **Bootstrap of an existing vault stays $0** — LLM reorganization is an explicit
  command, never a side effect of opening a vault.
- **One decider call does segmentation + placement together** (structured output,
  JSON-schema via `schemars`); strictly serial within a stream, parallel across
  independent documents.
- **Prompt-prefix caching is a design constraint on prompt assembly** — stable prefix
  first, deterministic serialization everywhere, volatile frontier last.
- **Retrieval is local and free** — BM25 (`tantivy`) + recency frontier + 1-hop.
  Embeddings only if an eval harness proves retrieval is the bottleneck.
- **Maintenance passes** (split oversized nodes, reattach orphans) are what keep the
  graph coherent past ~50 notes; they run on idle.
- Frontmatter `parent:` becomes an explicit spine *override* (an edge source outranking
  `Contains` for layout), not a rival heuristic.
