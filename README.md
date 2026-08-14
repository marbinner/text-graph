# text-graph

A fast, native graph viewer for a folder of markdown notes — with your AI
agents living inside it. Point it at a vault (an Obsidian vault works
as-is) and get an Obsidian-style force-directed graph you can drive
entirely from the keyboard, ranger-style. Terminal agents (claude, codex,
pi, …) running in the vault appear as **live, typeable terminal cards**
tethered to the folder they work in: watch them stream, jump in to type,
and watch the graph ripple as they write notes. One static Rust binary —
no Electron, no webview, no runtime.

```
cargo run --release -- ~/notes          # open the graph window
cargo run --release -- stats ~/notes    # headless statistics
```

The vault is the database: nodes are your `.md` files and directories, edges
come from the directory structure and `[[wikilinks]]`. text-graph never
edits your notes — it writes only the empty file or folder you explicitly
create from the right-click menu, plus a small hidden `.text-graph/` state
dir (camera + card arrangement). Editing happens in your own editor, and the
graph live-reloads when you save.

## The graph model

| Element | Source |
|---|---|
| **File node** | every `.md` file (`.obsidian/`, `.trash/`, hidden dirs skipped; git-ignore semantics deliberately off) |
| **Dir node** | every directory with markdown somewhere beneath it (empty ones pruned) |
| **Ghost node** | a `[[target]]` that resolves to nothing — a note you've referenced but not written; drawn hollow |
| **Contains edge** | filesystem parent → child; a true tree by construction, and the layout's skeleton |
| **WikiLink edge** | `[[...]]` in note bodies; drawn as faint curves, bright on the hovered node |

Zoomed in, discs reveal a type glyph: a folder silhouette on dirs, a
dog-eared page on files, an outlined page on ghosts — zoomed out they stay
plain color-coded circles (blue dirs, gray files), so huge graphs render
cheap.

Selecting a node opens the **navigator**: a ranger-style pane with a
clickable breadcrumb, the selection's siblings in a cursor column (dirs
colored, `/`-suffixed), and a preview — rendered markdown for notes, the
child listing for folders. Walk it with vim keys (`hjkl`, `gg`, `G`, `f`
to find within the directory); the graph camera follows the walk, so the
neighborhood you're reading is always the neighborhood you're seeing. A
**connections strip** along the bottom lists everything the node touches,
color-coded and clickable: blue `▸ folder/` and gray `▸ file` children,
amber `→` outgoing links, purple `←` incoming links.

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
5. Not edges at all: `![[embeds]]`, anything inside code fences or inline
   code, self-links, and *unresolved* links with asset extensions (png/pdf/…)
   — a real note named `pic.png.md` or `Drawing.excalidraw.md` stays
   linkable; only plain references to images and files are skipped.
6. Still unresolved → a ghost node.

Labels prefer frontmatter `title:`, then the first alias, then the file stem.

## Controls

The one rule: **selection is the mode**. With nothing selected, the
keyboard drives the camera; select a node and the same keys walk the vault
ranger-style (the camera follows); focus a terminal and every key goes to
the agent until `Ctrl+Q`.

### Camera (nothing selected)

| Input | Action |
|---|---|
| drag empty space / `h` `j` `k` `l` | pan (hold to glide) |
| mouse wheel / `u` `d` | zoom (wheel zooms toward the cursor) |
| `0` / `Home` | reset — fit the whole graph |
| `z` | center on the selection |
| drag a node | move it — pins to the cursor, the simulation responds live |
| hover | highlight the node's neighborhood, dim everything else |
| `/` or `Ctrl+F` | fuzzy search over names, aliases, paths — and agent terminals; matches stay lit, `Enter` jumps to the ringed best hit (a winning terminal lands focused, ready to type), `Esc` closes |

### Navigator (a node is selected)

Click any node (or search into one) to open the navigator pane:
breadcrumb, sibling column with the cursor, preview, and the color-coded
connections strip.

| Input | Action |
|---|---|
| `j` / `k` | step through siblings (sorted, dirs first) |
| `h` | up to the parent |
| `l` | enter a directory / open a file in the editor |
| `gg` / `G` | first / last sibling |
| `f` | find in this directory — fuzzy-jumps the cursor live as you type |
| `]` / `[` | walk a highlight through the **connections strip** (children `▸`, outgoing `→`, incoming `←`); `Enter` or `l` follows it |
| `Enter` / double-click | open the file in `$VISUAL`/`$EDITOR` (terminal editors get a **new terminal window**; set `$TERMINAL` to choose which); dirs open in the file manager |
| `Esc` | dismiss the link cursor, then deselect — back to camera mode |

### Terminal cards

| Input | Action |
|---|---|
| click a card | select + focus: it expands to full readable size at any zoom, turns **cyan ⌨**, and the keyboard types into the agent |
| `t` | hop the (orange) terminal cursor from card to card, each expanding in place; `Enter` dives in to type |
| `Ctrl+Q` / click empty space | release focus back to the graph |
| double-click a card | fly the camera into it (zoom + center) |
| drag a card | arrange it (it stays put, following its anchor node) |
| drag the corner grip (`tg_` cards, full view) | **natively resize the terminal** — the tmux session itself changes size and the card follows |
| right-click a card | **Attach in terminal…** (a real terminal window on that session) / **Kill terminal** (confirm submenu), plus the anchor folder's creation actions |

### Creating (right-click anywhere)

Context menu for the node under the cursor: **New note…** / **New
folder…** / **New terminal** / **Launch agent** in that folder (a file
targets its folder, empty space the vault root, a card its anchor); on a
ghost node: **write it** into a real note. Details in *Creating from the
graph* below.

Edit any file in the vault and the graph updates ~300ms after you save — new
links, files, and ghosts appear in place, and existing nodes keep their
positions (the layout ripples instead of re-settling). Rebuilds run on a
worker thread, so saves never hitch the UI.

When anything is off — parse warnings, unreadable files, ambiguous links, a
dead file watcher, a failed reload, a tmux session that won't mirror — a
**⚠ badge** appears in the corner; click it for the health list (entries
jump to the affected note).

## Creating from the graph

Right-click is the creation surface; everything lands relative to the node
you clicked:

- **New note…** — type a name (`.md` is implied); `sub/path/name` creates
  the intermediate folders too. The note is created empty, then selected and
  framed as soon as the reload picks it up. Writing content stays in your
  editor.
- **New folder…** — created on disk immediately; it shows up in the graph
  once it holds a note (empty dirs are deliberately pruned).
- **Write a ghost** — right-click a hollow node: the referenced-but-missing
  note is created at the linked path, and every link that pointed at the
  ghost snaps to the real file.
- **Launch agent** — pick claude / codex / pi / … (the same list that drives
  discovery) and it starts in a detached `tg_*` tmux session cwd'd at that
  folder; its live card fades in within a couple of seconds. The session is
  plain tmux and outlives the viewer — `tmux attach -t tg_claude` works from
  any terminal.
- **New terminal** — the same thing with a plain shell (`tg_term`): a
  terminal card at that folder you can type into right in the graph.

And the reverse, on a card: **Attach in terminal…** opens a real terminal
window on that session, landed on that pane (mirror and external client
coexist — it's all tmux), and **Kill terminal** ends the pane (and, if it
was the last one, the session — the card follows).

## Agent terminals in the graph

Right-click a folder node → **Launch agent**, or run any terminal agent
(claude, codex, pi, aider, goose, opencode, gemini — extend with
`TEXT_GRAPH_AGENTS=name,name`) inside tmux with its cwd in the vault:

```
tmux new-session -s work -c ~/notes claude
```

Within ~1.5s a live terminal card appears in the graph, tethered to the node
of the folder the agent runs in. Zoomed out it's a summary — name, folder,
active/idle age, and the pane's own last status line (`✳ Deliberating…`,
the last shell output), so you can see what each agent is doing at a
glance; zoom in and it becomes the full styled screen — colors, cursor,
everything, mirrored in real time. The card under the terminal cursor (or
focused for typing) always renders full-size, whatever the zoom: stand
back and click through your agents to inspect them. Border colors state
the mode — **cyan + ⌨ = your keyboard is in it, orange = selected**,
green = streaming.

**Click the card and type** — or drive it all from the keyboard: `t` hops
a highlight from card to card, `Enter` dives into the highlighted one, and
`/` finds an agent by name, session, or folder (`Enter` lands focused).
While focused, every key goes to the agent — Enter, Esc, arrows,
Shift+Tab, Ctrl chords including Ctrl+C to interrupt — and graph keybinds
suspend; `Ctrl+Q` or clicking empty space gives you the graph back.
**Double-click a card** to fly the view into it: the graph zooms to a level
where the terminal is full-size and readable, centered on that card — pan
or zoom back out whenever you like. A card stays up for the pane's whole
lifetime, including while the agent runs long tool calls. Graph-launched
(`tg_`) cards have a grip in the corner: dragging it resizes the **actual
tmux session** (`resize-window`), the TUI reflows natively, and the card
follows — the same thing that happens if you resize it from an attached
terminal. Foreign sessions deliberately have no grip: resizing them would
reflow the real terminal you're viewing them in. Drag cards to
arrange your workspace — the arrangement and your camera survive restarts
(saved to `.text-graph/view` in the vault), and arrangements are remembered
**by session name**: relaunch `tg_claude` tomorrow and its card lands back
where you left it. Watch the card glow while an agent streams, and the
graph ripple as it writes notes.

How it works, and why it's safe:

- The viewer attaches to tmux as a **control-mode client** (the iTerm2
  approach). tmux stays the real terminal: sessions persist when the viewer
  closes, `tmux attach` from any terminal keeps working, and tmux answers all
  the TUI's terminal queries — the viewer only renders display streams.
- Sessions named `tg_*` (launched from the graph) always show while their
  cwd is in the vault; other tmux panes additionally need their foreground
  command to match the agent list. Once recognized, a pane's identity is
  **sticky for its lifetime** (pinned to the pane's root process), so
  tool calls that put `bash` in the foreground for minutes don't drop the
  card.
- The viewer never sends size hints to sessions it didn't create, so it can
  never reflow a session you're viewing in a real terminal.
- No tmux installed → the feature is simply absent; everything else works.

v1 input limits: `Ctrl+Q` is reserved by the viewer (it releases focus, so
it never reaches the pane), Shift+Enter sends Enter, no mouse-into-terminal,
no in-graph scrollback — attach externally (right-click the card, or
`tmux attach -t work`) when you need those. Alt chords (Alt+b/f word
motion, Alt+digit args) work, and multiline paste is bracketed-paste
aware — pasting into claude doesn't submit on every newline.

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
  create.rs   new note/folder: path validation + create-only fs writes
  graph.rs    arena: typed nodes, Contains tree, overlay links
  layout.rs   pure radial layout — the simulation's deterministic seed
  sim.rs      force simulation (springs, repulsion, gravity, cooling)
  state.rs    per-vault persistence (.text-graph/view: camera, card layout)
  stats.rs    headless statistics (`stats` subcommand)
  tmux.rs     tmux control-mode client (protocol parse, %output unescape)
  mirror.rs   per-pane screens: vt100 parsers behind a TermGrid facade
  agents.rs   which tmux panes count as agents (allowlist, tg_*, grace) + launch
  keys.rs     keyboard → send-keys commands (tmux names + raw hex)
  app/        egui shell: transform, input, painting, search, navigator,
              reload worker, terminal cards + focus; diag.rs = health badge
examples/
  tmux_debug.rs       raw control-client event dump — the mirror's debug harness
  discovery_probe.rs  what discovery + mirrors see for a vault, headless
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

A suite of integration tests runs against a real tmux on a private socket
(skipped without tmux): scripted styled-screen mirroring, the exact typing
path end-to-end, native resize propagation, and agent launching. The
mirror's protocol layer (reply correlation, capture replay, cursor
restore) is additionally unit-tested without tmux.

House rules: if you touch `fixtures/vault/`, re-count `fixtures/EXPECTED.md`
and update the tests in the same commit — the numbers are asserted exactly.
One commit per change. See `CLAUDE.md` for the details that bite.
