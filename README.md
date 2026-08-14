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

| Input | Action |
|---|---|
| drag empty space | pan |
| mouse wheel | zoom toward the cursor |
| drag a node | move it — pins to the cursor, the simulation responds live |
| hover | highlight the node's neighborhood, dim everything else |
| click | select (opens the detail pane with rendered markdown) |
| `Enter` / double-click | open the file in `$VISUAL`/`$EDITOR` (terminal editors get a **new terminal window**; set `$TERMINAL` to choose which); dirs open in the file manager |
| right-click | context menu for the node under the cursor: **New note…** / **New folder…** / **New terminal** / **Launch agent** in that folder (a file targets its folder, empty space targets the vault root, a terminal card its anchor); on a ghost: **write it** into a real note |
| right-click a terminal card | additionally: **Attach in terminal…** (a real terminal window on the same session) and **Kill terminal** (confirm submenu) |
| `/` or `Ctrl+F` | fuzzy search over names, aliases, paths — and agent terminals (agent name, session, folder); matches stay lit, `Enter` jumps to the ringed best hit — a winning terminal lands focused, ready to type |
| `f` | frame the selection |
| `0` / `Home` | reset the view — fit the whole graph |
| `h` `j` `k` `l` | pan left/down/up/right (vim-style; hold to glide) |
| `u` / `d` | zoom in / out |
| `t` | hop the terminal cursor from card to card (flies to each); `Enter` starts typing into the highlighted one, `Esc` dismisses |
| `Esc` | close search, else deselect |
| click a terminal card | focus it — the keyboard now types into that agent |
| double-click a terminal card | fly the view into that terminal: zooms to a readable size and centers it |
| drag a terminal card | arrange it (it stays put, following its anchor node) |
| drag a card's corner grip (`tg_` cards) | **natively resize the terminal** — the tmux session itself changes size and the card follows; foreign sessions resize from their own terminal |
| `Ctrl+Q` / click empty space | release terminal focus |

Edit any file in the vault and the graph updates ~300ms after you save — new
links, files, and ghosts appear in place, and existing nodes keep their
positions (the layout ripples instead of re-settling).

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
of the folder the agent runs in. Zoomed out it's a summary (`claude · work %0`,
`in notes/ · active|idle 3m`); zoom in and it becomes the full styled screen —
colors, cursor, everything, mirrored in real time.

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
- Sessions named `tg_*` (launched from the graph) always show; other tmux
  panes show while their cwd is in the vault and their foreground command
  matches the agent list (with a grace period covering the moments a tool
  call puts `bash` in the foreground).
- The viewer never sends size hints to sessions it didn't create, so it can
  never reflow a session you're viewing in a real terminal.
- No tmux installed → the feature is simply absent; everything else works.

v1 input limits: `Ctrl+Q` is reserved by the viewer (it releases focus, so
it never reaches the pane), Alt+letter arrives as the plain letter,
Shift+Enter sends Enter, no mouse-into-terminal, no in-graph scrollback —
attach externally (right-click the card, or `tmux attach -t work`) when you
need those.

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
  app.rs      egui shell: transform, input, painting, search, detail pane,
              reload, terminal cards + focus
examples/
  tmux_debug.rs  raw control-client event dump — the mirror's debug harness
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

Two integration tests run against a real tmux on a private socket (skipped
without tmux): one asserts a scripted styled screen mirrors correctly, the
other drives the exact typing path end-to-end and asserts the echo.

House rules: if you touch `fixtures/vault/`, re-count `fixtures/EXPECTED.md`
and update the tests in the same commit — the numbers are asserted exactly.
One commit per change. See `CLAUDE.md` for the details that bite.
