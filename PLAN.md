# text-graph — Plan

*Last updated 2026-08-16 (v0.3.0, 268 tests). Where to pick up: milestone F
(agents talk) is fully spec'd below — design converged, NO code yet; start at
slice F1 (CLI trio). Still queued behind it: jump history (Ctrl+O/I), Phase 2
step 0, the audit backlog under D. The 2026-08-15 feature wave: asset
nodes + file-type icons, metadata hover popups everywhere,
Obsidian-flavored previews, milestone G (web nodes), directed/tapered
edges + depth-scaled hierarchy, camera glides, edit-in-graph cards
(@tg_anchor), ⚙ settings (dark/light Theme, default agent), launch
keybinds (e/t/a at the selection, auto-focused cards), and the
launch-PATH (server + client + rescue bin dirs) + busy-agent-hover
fixes. Same evening: a 7-lens adversarial audit — 18 confirmed defects,
14 fixed same-day (tmux `-c` format expansion, server-side paste
bracketing, inotify overflow, keybind focus guard, mdview dest rewrite,
unpin revert, and 8 hardening lows), 4 accepted — see "Audit
2026-08-15" below. The 2026-08-16 three-pass audit then closed every
confirmed finding, including preview confinement, bounded parsing/state/
terminal work, async lifecycle cleanup, lossless native paths, terminal
Unicode fidelity, locked/MSRV CI, a no-GUI core build, and process-level CLI
coverage.*

**Phase 1 (current):** a fast native GUI over a markdown vault. Point it at a folder of
`.md` files and it renders an interactive graph. One native Rust binary — egui, no
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
coexist rather than competing. As shipped (grown from the original
`{Dir, File, Ghost}` / `{Contains, WikiLink}` design):

```rust
enum NodeKind { Dir, File, Image, Asset, Ghost, Web }
// Ghost = referenced but nonexistent target; Image = raster, rendered as
// its picture; Asset = any other visible file; Web = a cited URL
enum LinkKind { WikiLink, External }   // overlay edges; later: Tag, Embed…
// Contains is the tree itself (per-node children), not an overlay edge
```

- **`Contains`** — from the filesystem. A true tree *by construction* (every file has
  exactly one parent dir, no cycles, total coverage). This is the layout spine; no
  cycle-breaking or provenance heuristics needed to trust it.
- **`WikiLink`** — `[[...]]` references, an overlay on top of the spine — always
  visible as faint arrowheaded curves, bright on the hovered node;
  **`External`** (note → cited URL) fainter still.
- **Memory:** nodes store metadata + extracted links only. Bodies are parsed and
  discarded at ingest; the detail pane (C) reads the selected file on demand.

### Extract → resolve, two phases

1. **Extract** — per-file, parallel, no global state. Emits edges whose targets are
   still strings (`[[foo/bar]]`).
2. **Resolve** — build the name index once, map every target string to a `NodeId`.
   Sequential, O(edges).

A new edge source later = one extractor function + one match arm in the renderer.

Resolution implements Obsidian semantics and gets its own module + tests: `[[note]]`,
`[[note|alias]]`, `[[note#heading]]`, `[[note#^block]]`, `[[dir/note]]`
(path-component suffix match); case-insensitive; ambiguity goes to the
first in sorted path order and is flagged. Parsing goes through `pulldown-cmark`
events so a `[[link]]` inside a code fence never becomes an edge. Unresolved targets
become `Ghost` nodes — in the model from A, rendered from C.

### v1 scope cuts (decisions, not accidents)

- ~~Only `.md` files, raster images, and directories become nodes; other
  files are skipped.~~ **Reversed twice**: raster images became `Image`
  nodes in v0.2, and as of 2026-08-15 every other visible file is an
  `Asset` node (code/config/data — the viewer runs over code projects with
  agents, not just notes). Build/dependency dirs (`node_modules/`,
  `target/`, `__pycache__/`) are skipped like dotdirs.
- `![[embeds]]` are skipped *as edges* (previews render them inline). A plain
  `[[pic.png]]` link resolves to the Image node when the image exists; links
  to *missing* assets are skipped, not ghosted.
- A flat vault (everything in one folder) degrades to a single ring around the root —
  acceptable, not a bug; hover cross-links still make it useful.

### Robustness on real vaults

Skip `.obsidian/` and `.trash/` explicitly (the walker's hidden-file rules,
not git's — gitignore semantics are deliberately off). Tolerate per-file read/parse errors without aborting the walk. Handle
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

The rule: dependencies are added at the milestone that needs them, never
before (milestone A shipped on exactly four: anyhow, ignore,
pulldown-cmark, serde_yaml_ng). Cargo.toml is the current list, each
entry annotated with why it's there and what was trimmed (glow instead
of wgpu, loaders-only egui_extras, decoder-scoped image features).
`serde` derive returns only when something needs serializing —
frontmatter reads go through `serde_yaml_ng::Value`, so real vaults'
numeric titles and arbitrary extra fields don't warn. No `rayon`
(`ignore` walks in parallel itself). No clap — the CLI forms hand-parse;
add it if the CLI grows.

---

## Repo layout

README's *Project layout* section is the maintained map of `src/` (the
original five-module sketch lived here until it drifted hopelessly).
The invariant that outlasts any listing: every lib module stays
egui-free and headless-testable; only the bin's `app/` tree touches
egui.

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
   dims non-matches; Enter frames + selects best hit; Esc clears. **Superseded**
   by the picker (below), which kept the lit-mask and the glide-on-jump.
2. Detail pane (`egui_commonmark`): selection opens right panel with rendered
   markdown, body read from disk on demand. Ghost selected → list of referencers.
3. Open in `$EDITOR` on Enter / double-click. Viewer stays read-only.
4. Live reload (`notify` 8.x, debounced): full re-scan on change (cheap), sim
   positions carried over by path + gentle reheat so edits ripple instead of
   re-settling; selection/camera survive by path.
5. Navigation: `f` frames selection, `Home` fits all, `Esc` deselects.

**C+ — The picker. ✓ done.** The search bar grew into a telescope-style finder
(lib `search.rs` + `app/picker.rs`) and then MERGED into the navigator: one side
pane with two modes, browsing (ranger) and searching (names, aliases, paths,
**file contents**, live terminal panes), sharing one preview column. Ranked
results, matches highlighted in context, arrow-key browsing that glides the
camera to the highlighted node, `Ctrl+Enter` to open `$EDITOR` at the matched
line. `f` opens it; the old find-in-directory prompt is gone (one finder, one
key). Content is streamed from disk per query on a cancellable worker (never
indexed — bodies stay out of memory and nothing goes stale under agents writing
notes).

**C++ — Settings, centralized. ✓ done 2026-08-16.** Preferences were two
widgets in a corner window stored per vault, while every other knob was
either a hardcoded constant or an environment variable a GUI-launched
viewer can't see. Now: one registry (`config.rs`) where a setting is a
field plus a `Spec` row, and the file format, the ⚙ window, reset-to-
default and the load-time clamps all derive from it. Per USER
(`~/.config/text-graph/config`), so theme and editor follow the person
while camera/cards/pins stay with the vault; older per-vault values
migrate on first open, and only when the vault actually stored them.
Twenty settings across appearance, motion, previews, search, tools and
agents, applied live, plus a keys tab (`?`) — the first help surface the
app has had. The **tools** section is the one that fixes a real failure
mode: `$VISUAL`/`$EDITOR`/`$TERMINAL` simply aren't there when the viewer
is started from an IDE or a desktop entry, the same class of bug as the
agent-launch PATH rescue.

**C+++ — Navigation centered on the finder. ✓ done 2026-08-16.** The pane
was doing two jobs — choosing a file (sibling column) and looking at one
(preview) — so each chooser grew its own preview and they drifted. Now:
overlays choose, the pane shows. ONE `Preview`, built by
`sync_pane_preview` for whatever the current subject is (the overlay's
highlighted row, else the selection), drawn by one `preview_pane`. The
ranger stopped being a surface and became a SOURCE of the overlay —
`b` lists a folder, typing filters it (the scoped search that was always
meant to be a query mode), `Tab` swaps source, `Enter` descends,
`Backspace` ascends. An empty find prompt lists what changed last, which
under working agents is the question you actually had. `r` switches any
preview between rendered markdown and numbered source. Adding a third
source (links? tags?) means adding a source, not a surface.

**C4 — The reading surface. ✓ done 2026-08-16.** The preview grew up:
syntax colouring for code (new lib `highlight.rs` — syntect spans as
plain RGB, scanned from line one because a highlighter's state is what
knows whether line 400 is inside a string) in both the source view and
fenced code, and Obsidian-flavored rendering (callouts of any case with
titles and fold markers, `==highlights==`, hidden `%%comments%%`, `#tag`
chips, dropped `^block-ids`) — Obsidian in, CommonMark out, because the
renderer only speaks CommonMark. Code previews as source by DEFAULT:
there is nothing to render in a .py, and rendering it strips the
structure a reader is looking for.

Keys settled around the finder at the same time: hjkl pan
unconditionally with `s`/`d` zooming (one hand drives the view), `gg`
refits and `0` resets only the zoom, `p` climbs, `G` frames the
selection's neighbourhood, `Tab`/`Shift+Tab` step the terminal cards in
a stable order, and `Ctrl+Q` lets go of everything — including egui's
widget focus, which is what makes the next `f` reach the graph. Whatever
the finder highlights is drawn OPENED on the canvas, cards included.

Three egui lessons paid for in bugs and written into CLAUDE.md: a
constant in POINTS inside the pane is a floor under it, not a ceiling
(that clipped every preview); egui remembers sizes and only ever
ratchets them DOWN unless you own them (pane width, finder height); and
Tab focus is decided in `Memory::begin_pass`, before any app code runs,
so it has to be taken back rather than prevented.

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
  terminal state grouped in terminals::Terminals, the finder in app/picker.rs
  over lib search.rs. Deepened 2026-08-18: canvas.rs (the frame as named
  stages), camera.rs, keymap.rs (declarative binding table with central
  guards), Derived/Reload/Persist/Menu substructs, kb_tests/ by topic;
  mod.rs ~970 lines, invariants moved from CLAUDE.md prose into module
  docs and tests (CLAUDE.md 434 → ~210 lines).
- Perf budgets against the stress vault ✓ measured 2026-08-18 via
  examples/perf_probe.rs (headless timings) + the ⚙ frame-statistics
  overlay (per-stage frame times, repaint rate). Release-build baseline
  after the Barnes–Hut sim (flat stress vault; sim = ms per 3-tick frame
  while settling, was the O(n²) figure in parens):
  0.5k → scan 8ms · build 2.5ms · sim 1.7ms (was 3.2);
  2k → scan 28ms · build 5.7ms · sim 5.1ms (was 44);
  10k → scan 116ms · build 29ms · sim 26ms (was 729).
  Scan/build run on the reload worker; content scan ~11ms at 2k. The
  remaining settle cost at 10k (~26ms/frame for a few seconds) is the
  next lever if vaults ever get there (θ, leaf size, or fewer iterations
  as alpha decays).
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

**G — external web links as nodes (✓ done 2026-08-15, same day).** URLs cited in
notes become first-class leaf nodes, modeled on ghosts: no tree parent,
seeded near the first referencer, identity = the normalized URL (can never
collide with a path — paths don't contain `://`). Deduplication is the
point: one node per URL however many notes cite it, so shared sources
become visible bridges between notes that never wikilink each other (and
the raw-note convention composes: analysis → citation → raw note →
external edge → source). Decisions made: per-URL granularity (not
per-domain — presentation carries the domain story: small nodes, globe
glyph, host labels — refined post-ship to layered titles: authored link text,
else URL slug, else host; full URL in popup/status); `LinkKind::External`
edges drawn fainter than wikilinks; mild deterministic normalization
(lowercase host, strip fragment + utm_*/fbclid/gclid, trailing slash;
KEEP other query params); on by default with a `w` toggle persisted in
view state — hidden means skipped at render/hit-test only, the sim keeps
simulating so toggling never reflows; click selects (navigator: URL +
open-in-browser + citers via backlinks), Enter/double-click opens the
browser. NO network in v1 (no favicons, no fetched titles — offline and
deterministic); those are a later opt-in.

1. **G1 — normalization.** `weburl::normalize` (pure, tested);
   extraction keeps offsets.
2. **G2 — data model.** `NodeKind::Web` + `LinkKind::External`;
   resolution creates deduped web nodes + edges (encounter order, like
   ghosts); `Node.externals` metadata dissolves into real edges; sim
   seeding/charge/springs; stats; fixture re-count; minimal disc render.
3. **G3 — presentation.** Globe glyph into the font subset; edge style;
   navigator/hover arms; open-in-browser; host labels.
4. **G4 — the `w` toggle**, persisted; kb test.

---

## Audit 2026-08-15 — accepted limitations

A 7-lens adversarially-verified audit found 18 real defects; 14 are fixed
(commits `5c4c168`…`1edf1a0`). Four were judged not worth their fix cost —
recorded here so they read as decisions, not undiscovered bugs:

- **(mtime, len) stamp granularity**: two same-length rewrites of a file
  within one kernel clock tick are indistinguishable, so a canvas
  excerpt/thumbnail captured between them can stay stale until the next
  length- or tick-changing write. Fixing needs content hashing on every
  reload — not worth it for a ≤4ms window.
- **`%output` between Capture and Cursor replies** (mirror.rs): output
  landing in that few-ms window paints at the replay-parked cursor —
  transient bottom-row garbling on attach/resize while a pane streams.
  A fix means buffering output while a capture is pending; the hot-path
  risk outweighs the cosmetic blip.
- **Tab inside an anchored filename** shears the discovery scan record
  (anchor is field 5 of 6) and the tg_edit card never appears. Legal on
  Linux, essentially never occurs in a notes vault.
- **Trailing-edge-only 300ms reload debounce**: a gap-free sub-300ms write
  stream defers the rebuild until it pauses. Real bursts pause constantly,
  so staleness is bounded and self-heals; add a max-latency cap only if a
  real workload shows starvation.

### Second audit (same day, against 77ca418)

An independent external audit added ~30 findings; 14 fixed same-day
(`6c3954d`…`3004988`): allowlist-validated agent restore, symlink-refusing
state saves, Enter repeat-guard + child reaping, mdview source-relative
resolution + path-qualified embeds + footnote-definition protection,
pid-scoped paste buffers, .mjs/.cjs as text, scrollable diag, pruned
walker, single-flight scans, byte-safe `%output` (split UTF-8 survives),
scan failures keep the last pane snapshot, CI `--all-targets` + RustSec.
Triaged and deliberately deferred:

- **Thumbnail LRU/byte budget**: decodes are visibility-driven, so growth
  tracks what was actually viewed. Add budgets (and decoder dimension
  caps) when an image-heavy vault makes it real.
- **Watcher-side dir pruning**: notify can't filter during registration,
  and manual per-dir watches risk silently missing events in freshly
  created dirs — worse than the descriptor cost. The scanner prunes;
  overflow→rescan covers event floods.
- **Replay loses scroll margins/charsets/saved cursor**: capture-pane
  cannot convey modes (the same reason pastes moved server-side); apps
  repaint on the next resize/redraw. Unfixable without tmux support.
- **Mixed wiki+external edge order** (doc order vs grouped): fix when next
  touching graph.rs — changes link order, needs a fixture re-count.
- **URL-extractor edge cases** (uppercase scheme, `prefixhttps://`,
  trailing-paren balancing for Wikipedia URLs, uppercase `WWW.` dedup):
  queued cleanup pass for the weburl/extractor pair.
- Unicode casefolding, `$EDITOR` quoted args, AltGr/Alt-punct chords, full
  `%begin`-identity validation, and per-pane dead-state reconciliation:
  accepted or awaiting real-world symptoms — the E3 punch-list absorbs the
  input ones.

### Third audit (2026-08-16)

Three independent read-only passes covered core graph/Markdown logic, the GUI
and tmux lifecycle, and builds/tests. Every confirmed high, medium and low
finding was fixed in its own focused commit. The remediation includes:

- canonical vault confinement and byte limits for preview images; bounded
  Markdown scanning/previews and linear, size-bounded view-state loading;
- race-safe view-state saves, preservation of unreadable config, cancellation
  and shutdown for search/discovery workers, bounded terminal queues and
  per-frame pumping, retry backoff, and off-UI tmux lifecycle subprocesses;
- per-occurrence wikilink rendering, qualified embed resolution, disjoint
  virtual identities, collision-free native path identity, full terminal cell
  text, centered long-line snippets, and authored URL label retention;
- shared scanner/creation skip policy, fixture byte preconditions, Unix-only
  test gates, locked dependency CI, Rust 1.95 MSRV CI, a no-GUI core/statistics
  build, and process-level CLI coverage including non-UTF-8 vault paths.

The final matrix passes formatting, strict all-target Clippy, the Rust 1.95
all-target build, the no-default-features headless build, and 268 tests
(library, GUI state machine, CLI, fixture, and real-tmux integration).

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
