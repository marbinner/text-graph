# text-graph — Plan

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
enum NodeKind { Dir, File, Ghost }        // Ghost = referenced but nonexistent target
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

- Only `.md` files and directories become nodes; other files are skipped.
- `![[embeds]]` and links to non-`.md` targets are skipped.
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

**A — Headless core.** Fixture vault; walk, parse, extract, resolve; typed-edge arena;
deterministic ordering; `stats` command (node/edge counts by kind, unresolved links,
depth histogram, biggest dirs). Fully testable with no window and no API key.

**B — The window.** Radial layout, pan/zoom-to-cursor, culling, nodes+edges painted,
dirs vs files rendered distinctly, hover highlight + label, click select, wikilink
overlay for the active node. The "it works" moment.

**C — Usable.** Detail pane (`egui_commonmark`, body read on demand), fuzzy search
(`nucleo-matcher`) dimming non-matches, `f` frames selection, open-in-`$EDITOR`,
`notify` live reload (reparse just the changed file), ghost nodes rendered.

**D — Scale & polish.** Label LOD tuning, collapse/expand subtrees (default-collapse
huge siblings), focus mode (k-hop neighborhood), mesh batching if profiling demands,
persist viewport per vault.

---

## Phase 2 (deferred): the LLM layer

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
