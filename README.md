# text-graph

A fast, native graph viewer for a folder of markdown notes — with your AI
agents living inside it. Point it at a vault (an Obsidian vault works
as-is) and get an Obsidian-style force-directed graph you can drive
entirely from the keyboard. Terminal agents (claude, codex,
pi, …) running in the vault appear as **live, typeable terminal cards**
tethered to the folder they work in: watch them stream, jump in to type,
and watch the graph ripple as they write notes. One native Rust binary —
no Electron, no webview, no runtime.

```
cargo run --release -- ~/notes          # open the graph window
cargo run --release -- stats ~/notes    # headless statistics
```

## Install

There is a flake, so on NixOS — or anywhere with nix flakes — nothing needs to
be built by hand:

```
nix run github:marbinner/text-graph -- ~/notes
```

Declaratively, add the flake as an input and its overlay to your NixOS config:

```nix
inputs.text-graph.url = "github:marbinner/text-graph";
# ...
nixpkgs.overlays = [ inputs.text-graph.overlays.default ];
environment.systemPackages = [ pkgs.text-graph ];
```

`nix develop` gives you a shell with the toolchain, tmux and the GUI libraries,
where `scripts/check.sh` runs as-is.

Building it yourself instead, the binary wants a GL-capable session (Wayland or
X11) and `tmux` on `PATH` for the terminal cards — without tmux everything else
still works and the cards are simply absent. `pkg-config` and the Xorg client
headers must be present at build time even for a Wayland-only session, because
winit's `x11-dl` probes for them.

The vault is the database: nodes are your `.md` files and directories, edges
come from the directory structure and `[[wikilinks]]`. text-graph never
edits your notes — it writes only the empty file or folder you explicitly
create from the right-click menu, plus a small hidden `.text-graph/` state
dir (camera, card arrangements, pins). Your preferences live with you, not
with the vault, in `~/.config/text-graph/config`. Editing happens in your
own editor, and the graph live-reloads when you save.

## The graph model

| Element | Source |
|---|---|
| **File node** | every `.md` file (hidden dirs, `.obsidian/`, `.trash/`, `node_modules/`, `target/`, `__pycache__/` skipped; git-ignore semantics deliberately off) |
| **Image node** | every raster image (png/jpg/jpeg/gif/webp/bmp) — rendered as its actual picture once you zoom in |
| **Asset node** | every other visible file — code, config, data, binaries. Text-classified assets get previews; all are linkable by full name (`[[data.csv]]`) |
| **Dir node** | every directory with files somewhere beneath it |
| **Ghost node** | a `[[target]]` that resolves to nothing — a note you've referenced but not written; drawn hollow |
| **Web node** | every external URL cited anywhere — ONE cyan globe per normalized URL however many notes cite it, so shared sources become visible bridges between their citers; `w` toggles them, Enter/double-click opens the browser |
| **Contains edge** | filesystem parent → child; a true tree by construction, and the layout's skeleton — drawn as a **tapered wedge**, thick at the parent thinning to the child |
| **WikiLink edge** | `[[...]]` in note bodies; faint **arrowheaded** curves, bright on the hovered node |
| **External edge** | note → cited URL; fainter cyan curves — context, not structure |

Big enough to read, every node paints as its **file-type icon** (Nerd Font
glyphs bundled in `assets/icons.ttf`): the python logo on `.py`, the css
shield, markdown pages, blue folders, per-language colors — zoomed way
out they collapse to plain color-coded discs, so huge graphs render
cheap. Ghosts stay hollow outlines. **The hierarchy is legible at a
glance**: folders shrink and darken with tree depth (the root is the
biggest, brightest thing on the canvas) and their names render in blue
at a size that scales with the node — a readable directory outline over
the graph, wedge edges pointing the way down.

**The closer you look, the more you see.** Labels ease in early (hubs
first) and moving the mouse acts as a label flashlight: names near the
cursor fade in even when the zoom would hide them. Zoom into an image and
its disc becomes the picture (decoded on a background thread, so big
photos never hitch a frame); zoom into a note — or any text-classified
asset — and it opens into a card showing its first lines (prose
proportional, code monospace), text growing with the zoom. And hovering
**any node** for a moment pops up the full thing, headed by its metadata
— edited/created age, size, lines · words, and a links line (`N out · N
in · N external`, external URLs listed): notes render as markdown, text
assets raw, images large, folders show their listing plus direct and
recursive counts and the total wiki + external links leaving their files,
ghosts list their referencers — all without selecting anything.

Selecting a node opens the **preview pane**: file-type glyph, name, the
path as clickable ancestors, size and age, then the file itself —
rendered markdown for notes, source with line numbers for code (`r`
switches either way), the child listing for folders, the picture for
images — with **syntax colouring** in both, from the same themes. `p`
climbs to the parent; everything else about getting somewhere is the
finder. A **connections strip** along the bottom lists
everything the node touches, color-coded and clickable: blue `▸ folder/`
and gray `▸ file` children, amber `→` outgoing links, purple `←` incoming
links.

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
5. A plain `[[pic.png]]` or `[[data.csv]]` (with extension) resolves to the
   **image or asset node** if that file exists in the vault.
6. Not edges at all: `![[embeds]]`, anything inside code fences or inline
   code, self-links, and *unresolved* links with asset extensions (png/pdf/…)
   — a real note named `pic.png.md` or `Drawing.excalidraw.md` stays
   linkable; only references to assets that don't exist are skipped.
7. Still unresolved → a ghost node.

Labels prefer frontmatter `title:`, then the first alias, then the file stem.

## Controls

The one rule: **the finder chooses, the keyboard drives**. `hjkl` pan and
`s`/`d` zoom whatever is selected; `f` and `b` are how you get to a file;
focus a terminal and every key goes to the agent until `Ctrl+Q`.

### Camera

| Input | Action |
|---|---|
| drag empty space / `h` `j` `k` `l` | pan (hold to glide) |
| mouse wheel / `s` `d` | zoom out / in (the wheel zooms toward the cursor) |
| `gg` | back out to the whole graph |
| `G` | zoom to the selection's **neighbourhood** — what it's connected to, framed |
| `0` / `Home` | reset the zoom, stay where you are |
| `z` | center on the selection |
| drag a node | move it — pins to the cursor, the simulation responds live |
| hover | highlight the node's neighborhood, dim everything else; nearby labels fade in around the cursor |
| hover + linger | metadata + full preview popup for any node — note as markdown, text asset raw, image large, folder stats + listing, ghost referencers; on a compact terminal card, its full live screen |
| `w` | toggle web (cited-URL) nodes — hidden means hidden from view only, the layout never reflows |
| `,` or `⚙` (bottom-right) | **settings** — see below |
| `?` | every keybinding, in the settings window |
| `f`, `/` or `Ctrl+F` | **find** anything — see below |
| `b` | **browse** the folder you're in, in the same list |

### Finding and browsing

Navigation is built around one list. It floats over the middle of the
window with its results stacked underneath, filling everything below the
prompt — telescope style, so your eye stays near the center of the screen
(⚙ *finder position* moves the prompt up for more rows at once, or down
for a more central one) — while the pane on the right previews whatever
is highlighted. It has two **sources**, not two
surfaces:

**Find** (`f`, `/` or `Ctrl+F`) searches everything in the vault: note
names, aliases, paths, the **text inside every file**, and the live agent
terminals. Names, aliases and paths match fuzzily (`apbn` finds
`agent-protocol-benchmark.md`); file content matches literally — every
word you type has to appear on the same line, case-insensitively unless
you type a capital. Content is never indexed: a worker thread streams the
vault per query and stops the moment you type another character, so
nothing goes stale under agents that rewrite notes while you search. With
**nothing typed**, it lists what changed last — the 30 most recently
edited files, newest first, which is usually the question you had.

**Browse** (`b`) lists one folder's entries instead, in tree order, and
typing filters *that folder* — the same keys, the same preview, scoped.
`Enter` walks into a directory, `Backspace` on an empty filter walks back
out, `Shift+Enter` takes the folder itself. `Tab` swaps between the two
sources and keeps what you typed: a filter that found nothing here is
usually what you wanted to search the whole vault for.

The preview is the same one for both, and the same one a selected node
gets — there is no second place to look at a file. Notes render as a
**reading view**: Obsidian markdown (wikilinks, callouts, tags, embeds)
drawn in a bundled text face (Inter) with a real heading scale and prose
wrapped at a book-ish line length, centered when the pane is wider. A
**content** hit shows the file's raw lines with every match highlighted
and the hit scrolled into view; `r` switches any note between rendered
markdown and source. A search rides out vault reloads: agents saving notes underneath
you re-scan in the background without emptying the list, moving your
cursor, or blinking the preview. On the canvas, matching nodes stay lit
and the highlighted result **glides into view** as you arrow down — into
the band above the prompt, so it never hides behind it — and it **opens
up** while you look at it: a terminal card expands, a note becomes its
preview card, an image its picture, however far out you are standing.

The pane opens at a quarter of the window and then keeps whatever width
you drag it to (per vault, across restarts).

| Input | Action |
|---|---|
| type | filter — names first, then paths, then terminals, then content hits (with the matching line and its number) |
| `↑` `↓` / `Ctrl+P` `Ctrl+N` | move through the list |
| `PageUp` `PageDown` / `Ctrl+U` `Ctrl+D` | half-page jumps |
| `Enter` / click | take it — select it, frame it, and drop back into the graph on it (a terminal card lands focused and centered, at your current zoom); while browsing, `Enter` on a folder goes *into* it |
| `Shift+Enter` | take a folder itself instead of entering it |
| `Backspace` | browsing, with an empty filter: up one folder |
| `Tab` | swap find ⇄ browse, keeping the query |
| `Ctrl+Enter` | open the file in `$VISUAL`/`$EDITOR` **at the matched line** |
| `Esc` | close the list, keeping the selection |

### A selected node

Note previews render **Obsidian-flavored**: callouts (`> [!warning]`, any
case, title and fold marker included) get their own colour and icon,
`==highlights==` read as emphasis, `%%comments%%` stay hidden, `#tags`
render as chips, and trailing `^block-ids` don't clutter the text.
Inline `$\delta = 2$` and display `$$…$$` math render as Unicode text —
greek, operators, scripts, simple fractions; no TeX engine, so anything
unrecognized stays verbatim rather than vanishing.
`[[wikilinks]]` are real links that jump to their node (ghosts included),
`![[image embeds]]` and relative image paths render inline, relative
markdown links to vault files jump too, footnote-style citations
(`[^raw/2026-….md]` — a wiki convention: sources cited by path) become
links showing the cited note's title, and external links open your
browser. Every camera jump — tree walks, search, link clicks — **glides**
instead of snapping, and never changes your zoom.

| Input | Action |
|---|---|
| `p` | up to the parent folder |
| `b` | **browse this folder** in the finder — the list, scoped |
| `r` | read the preview as source (line numbers) or as markdown |
| `]` / `[` | walk a highlight through the **connections strip** (children `▸`, outgoing `→`, incoming `←`); `Enter` follows it |
| `Enter` / double-click | open the file in `$VISUAL`/`$EDITOR` (terminal editors get a **new terminal window**; set `$TERMINAL` to choose which); dirs open in the file manager |
| `e` | **edit in the graph**: the file — or folder, as the editor's picker — opens in your terminal editor inside a live `tg_edit` card, tethered to the node itself (also on right-click); the card dies when the editor exits |
| `t` | new **terminal** card at this node's folder — focused, ready to type, and placed where you are looking |
| `a` | launch the **default agent** (`,` settings) at this node's folder; its card appears focused and in view |
| `Esc` | dismiss whatever is transient first — the finder, then the settings window, then the link or card cursor — then deselect, back to camera mode |

### Terminal cards

| Input | Action |
|---|---|
| `Tab` / `Shift+Tab` | step through every card in a stable order — each expands where it sits, at your zoom; `Enter` goes in |
| hover + linger on a compact card | **peek**: the full live screen pops up at readable size — inspect an agent without zooming, focusing, or pinning |
| click a card | select + focus: it expands to full readable size at any zoom, turns **cyan ⌨**, and the keyboard types into the agent |
| **Ctrl+click a card** | 📌 pin it open — expanded at any zoom, several at once, without taking the keyboard; Ctrl+click again unpins. Pins survive restarts |
| `Ctrl+Q` | let go of everything — card focus, cursor, selection, open overlays — so the next key reaches the graph |
| click away | release focus back to the graph in one gesture — a click on a node also selects it, a click on empty space just releases (your selection stays) |
| double-click a card | fly the camera into it (zoom + center) |
| drag a card | arrange it (it stays put, following its anchor node) |
| drag the corner grip (`tg_` cards, full view) | **natively resize the terminal** — the tmux session itself changes size and the card follows |
| right-click a card | **Attach in terminal…** (a real terminal window on that session) / **Kill terminal** (confirm submenu), plus the anchor folder's creation actions |

### Settings (`,` or the ⚙ badge)

One centered window, sectioned down the left, everything applying the
moment you change it — the canvas stays live behind it so a slider can be
judged against the graph it's changing. Type in the filter box to find a
setting by name, by section, or by a word from its explanation ("dwell"
finds the hover delay). A row that's off its default grows a `↺`.

| Section | What's in it |
|---|---|
| appearance | theme, label density, node size, how far unrelated nodes fade, canvas thumbnails and text previews |
| motion | layout spread, freeze the layout, camera glide, zoom speed |
| previews | hover popups on/off, the dwell before one opens, the picker's follow delay |
| search | whether file contents are scanned at all, and the per-file size ceiling |
| tools | editor, terminal and file-manager commands |
| agents | the default agent, and extra commands to allow |
| keys | every keybinding (`?` opens straight here) |

The **tools** section matters more than it looks: a viewer started from a
desktop entry or an IDE inherits an environment you never set, so
`$VISUAL`/`$EDITOR`/`$TERMINAL` may simply not be there. Set them here and
they win; leave them blank and the environment is used exactly as before.

Preferences are **per user**, in `~/.config/text-graph/config` (or
`$XDG_CONFIG_HOME`) — theme and editor follow you between vaults, while
camera, card arrangement and pins stay with the vault. It's a plain
`key<TAB>value` file, safe to hand-edit and safe to symlink into a
dotfiles repo (saves resolve the link instead of replacing it); values out
of range are clamped on load, and keys a newer version wrote are carried
through untouched. Anything an older build stored per vault (theme,
default agent) migrates across the first time you open that vault.

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

### Safety and bounded work

Vault contents and saved state are treated as untrusted input:

- Markdown extraction reads at most 8 MiB from a note, and rendered note
  previews read at most 1 MiB. Larger notes remain graph nodes, with a visible
  truncation notice in the preview.
- Images embedded in Markdown previews accept only canonical regular files
  inside the canonical vault, capped at 64 MiB. Authored `file://` images,
  absolute or escaping paths, symlinks out of the vault, devices, and oversized
  images are neutralized instead of reaching the generic file loader.
- Filesystem operations retain native paths rather than reconstructing them
  from display text. On Unix, distinct non-UTF-8 filenames remain distinct and
  can still be searched, previewed, edited, and passed to tmux.
- View-state input is size-bounded and parsed in linear time. Saves use a
  private create-only temporary file followed by rename; Unix saves also use
  no-follow, directory-relative operations. An unreadable user config is
  reported rather than treated as missing and overwritten by migration.
- Terminal events use a bounded queue and a fixed per-frame processing budget.
  Discovery, attach retries, and launch/kill work are also bounded or moved off
  the UI thread, so a noisy or unhealthy tmux server cannot monopolize a frame.

Creation helpers also refuse overwrites, symlinked destination directories,
and subtrees such as `target/` or `node_modules/` that the scanner deliberately
prunes.

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
- **Launch agent** — one click on **Launch <default>** starts your default
  agent (pi out of the box; change it in ⚙ settings), or pick claude /
  codex / … from the submenu (the same list that drives discovery). It
  starts in a detached `tg_*` tmux session cwd'd at that folder; its live
  card fades in within a couple of seconds, on screen where you are
  looking — it takes the keyboard the moment it appears, so it never opens
  off the edge of the view. From then on it's an ordinary card: drag it
  where you want it and it stays, across restarts. The session is
  plain tmux and outlives the viewer — `tmux attach -t tg_claude` works from
  any terminal. Launches resolve the agent against the tmux server's PATH
  (not just the viewer's — IDE-launched viewers carry stripped ones), and
  if the command still dies instantly, the status line says so instead of
  pretending it worked.
- **New terminal** — the same thing with a plain shell (`tg_term`): a
  terminal card at that folder you can type into right in the graph.
- **Edit here** — on a text file (`e`, or right-click): your terminal
  editor opens on it in a `tg_edit` session whose card tethers to the
  file's own node (the binding rides the session's `@tg_anchor` tmux
  option, so it survives viewer restarts); the card dies with the editor,
  and the pane is told it's dark (`COLORFGBG`) so editors pick their dark
  theme.

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
of the folder the agent runs in. Zoomed out it's a summary — a status dot
(green streaming · gray idle), name, folder, idle age, and the pane's last
**three** contentful lines (newest brightest: `✳ Deliberating…`, the last
shell output), so a whole fleet reads at a glance — and lingering on a
compact card peeks its full screen without touching anything. Zoom in and
it becomes the full styled screen — colors, cursor, everything, mirrored
in real time. The card under the terminal cursor (or
focused for typing) always renders full-size, whatever the zoom: stand
back and click through your agents to inspect them — or **Ctrl+click
several to pin them open** (📌 in the title) and watch a whole fleet at
once. Border colors state the mode — **cyan + ⌨ = your keyboard is in it,
orange = selected**, green = streaming.

**Click the card and type** — or `/` finds an agent by name, session, or
folder (`Enter` lands focused). Cards you launch yourself (agent,
terminal, editor) focus automatically the moment they appear — launch and
just start typing.
While focused, every key goes to the agent — Enter, Esc, arrows,
Shift+Tab, Ctrl chords including Ctrl+C to interrupt — and graph keybinds
suspend; `Ctrl+Q` or clicking empty space gives you the graph back.
**Double-click a card** to fly the view into it: the graph zooms to a level
where the terminal is full-size and readable, centered on that card — pan
or zoom back out whenever you like. A card stays up for the pane's whole
lifetime, including while the agent runs long tool calls. Graph-launched
(`@tg_owner=text-graph`) cards have a grip in the corner: dragging it
resizes the **actual
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
- Sessions carrying `@tg_owner=text-graph` (set on graph launches) always
  show while their cwd is in the vault; other tmux panes additionally need
  their foreground
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
aware — pasting into claude doesn't submit on every newline. (tmux itself
applies the markers from the pane's live mode, so this holds even for
sessions that were already running when the viewer attached.)

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
  vault.rs    walk + frontmatter/wikilink/URL extraction (per-file, no global state)
  filetype.rs extension classification: textual? which icon glyph and color?
  mdview.rs   Obsidian-flavor rewrite; vault-confined local image loading
  resolve.rs  Obsidian-style link resolution (stems → aliases → ghosts)
  create.rs   new note/folder: path validation + create-only fs writes
  graph.rs    arena: typed nodes, Contains tree, overlay links
  layout.rs   pure radial layout — the simulation's deterministic seed
  sim.rs      force simulation (springs, repulsion, gravity, cooling)
  state.rs    per-vault persistence (.text-graph/view: camera, cards, pins)
  config.rs   per-user preferences: one registry the file, the settings
              window and the clamps are all derived from
  stats.rs    headless statistics (`stats` subcommand)
  thumb.rs    [gui feature] image file → downscaled RGBA pixels
  tmux.rs     tmux control-mode client (protocol parse, %output unescape)
  mirror.rs   per-pane screens: vt100 parsers behind a TermGrid facade
  agents.rs   which tmux panes count as agents (allowlist, owner marker, grace) + launch
  keys.rs     keyboard → tmux commands (key names + raw hex + buffer pastes)
  highlight.rs [gui feature] syntect source colouring as plain RGB spans
  search.rs   the picker's engine: fuzzy name/path scoring, literal content
              scanning (streamed from disk, never indexed), ranked rows
  app/        egui shell, split by concern:
              mod.rs = the Viewer struct, theme, shared node geometry,
              side panel; canvas.rs = the frame as a pipeline of named
              stages (paint order = stacking order); camera.rs = the
              world⇄screen transform, rect compensation, glide;
              keymap.rs = the keybinding table + dispatcher (guards and
              key-repeat rules applied centrally); picker.rs = the list
              overlay and its sources (find / browse / recent) over lib
              search.rs; navigator.rs = the side pane (THE previewer:
              header, bodies, connections strip); terminals.rs = card
              state + sync/paint/forwarding/gestures/lifecycle;
              actions.rs = right-click menu, create dialog, spawning;
              reload.rs = watcher/scan-worker substruct + apply +
              persistence; images.rs = thumbnail textures, previews.rs =
              canvas text previews + hover popup, diag.rs = health
              badge, settings.rs = the ⚙ window + the key list;
              kb_tests/ = the headless state-machine tests, by topic
scripts/
  check.sh    the full local gate chain (mirrors CI), exit-code gated
assets/
  icons.ttf           bundled Nerd Font subset for file-type glyphs (OFL-1.1)
  gen-icons-font.sh   regenerates it; codepoints mirror src/filetype.rs
  reading.ttf         bundled Inter subset (OFL-1.1) — the face rendered
                      markdown reads in; non-Latin falls back to egui's fonts
  gen-reading-font.sh regenerates it from an Inter release TTF
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

Rust 1.95 or newer is required. The minimum version is checked in CI.
The statistics CLI can be built without the native GUI stack:

```
cargo run --no-default-features -- stats <vault-path>
```

`scripts/check.sh` runs the whole gate chain CI runs — formatting, the
GUI-free layering check, clippy at zero warnings, all tests, and (when the
toolchains are installed) the MSRV build and dependency audit:

```
scripts/check.sh
```

The individual gates, for selective runs:

```
cargo fmt -- --check
cargo check --locked --no-default-features --lib --bin text-graph
cargo +1.95.0 check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

A suite of integration tests runs against a real tmux on a private socket
(skipped without tmux): scripted styled-screen mirroring, the exact typing
path end-to-end, native resize propagation, and agent launching. The
mirror's protocol layer (reply correlation, capture replay, cursor
restore) is additionally unit-tested without tmux, and the keyboard state
machine (modal hjkl, Esc ordering, link walking, the picker) is driven
through a headless egui harness (`egui_kittest`). Process-level CLI tests cover
help, version, usage errors, fixture statistics, and non-UTF-8 vault paths.

For performance work: `cargo run --release --example perf_probe <vault>`
times the headless pipeline (scan, build, reload carry-over, simulation
settle, content search) — `fixtures/gen-stress.sh N` generates large
synthetic vaults — and the ⚙ *frame statistics* setting overlays
per-stage frame times and the repaint rate in the running viewer.

House rules: if you touch `fixtures/vault/`, re-count `fixtures/EXPECTED.md`
and update the tests in the same commit — the numbers are asserted exactly.
One commit per change. See `CLAUDE.md` for the details that bite.
