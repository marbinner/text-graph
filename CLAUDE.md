# text-graph — working notes

## Commands

- `cargo test` — unit + integration. Integration (`tests/fixture_vault.rs`)
  asserts the exact hand-counted numbers in `fixtures/EXPECTED.md`.
- `cargo clippy --all-targets` — keep it at zero warnings.
- `cargo run --release -- <vault>` opens the GUI; `-- stats <vault>` is headless.
- GUI smoke test without interaction: `timeout 6 cargo run --release -- fixtures/vault`
  → exit 124 means it launched and ran fine.
- tmux tests: `tests/tmux_mirror.rs` spawns a real tmux on a **private socket**
  (`tmux -L tg-test-<pid>`) and kills only that server; it skips (passes) when
  tmux is absent. Never point tests at the user's default tmux server.

## House rules

- **One commit per change** (user preference): each coherent edit builds,
  passes tests, and is committed before the next one starts.
- **Fixture contract**: any edit to `fixtures/vault/` requires re-counting
  `fixtures/EXPECTED.md` and updating `tests/fixture_vault.rs` in the same
  commit. Cross-check: raw `[[` count = edges + traps + embed + self-link
  (current expected total is stated in EXPECTED.md).
- **Determinism is a feature**: never introduce randomness, unsorted-map
  iteration, or walk-order dependence anywhere that feeds NodeIds, layout,
  or output. There's a build-twice test that will catch you.
- Dependencies are added at the milestone that needs them, not before.
- Never mutate `fixtures/vault/` from a running-app smoke test — copy it to
  the scratchpad first.

## Architecture invariants

- `vault.rs` parses per-file with no global state; `resolve.rs` is the only
  place link strings become NodeIds. New edge sources = extractor + resolver
  case + render arm.
- `layout.rs`, `sim.rs`, `tmux.rs`, `mirror.rs`, `agents.rs`, and `keys.rs`
  must stay egui-free (headless-testable). `app.rs` is the only module
  allowed to touch egui.
- Terminals: the viewer is a tmux **control-mode client** — never own a PTY,
  never send size hints (`set_size`) or `resize-window` to sessions we
  didn't create (it would reflow the user's real terminal view). The corner
  resize grip exists only on `ours` (tg_) cards for exactly this reason. Special keys go as tmux key NAMES
  (tmux applies pane modes); text/Ctrl-chords go as `send-keys -H` hex
  (quoting-proof); Ctrl+C/X arrive from egui as Copy/Cut events, not Key
  events. While a terminal is focused, keyboard events are DRAINED from
  egui's input (not just read) so widget focus/shortcuts never fire. After
  a capture replay the pane cursor MUST be restored via the queried
  position — the replay parks it at the bottom row (regression-guarded by
  `typed_input_round_trips` and the headless mirror pump test). Reply
  blocks terminate only on the %end/%error matching their %begin's command
  number — protocol-shaped screen text is data. Agent identity is sticky
  for a pane's lifetime (tool calls flip pane_current_command to bash for
  arbitrarily long) and pinned to the pane's root pid (pane ids restart at
  %0 on a new tmux server); GRACE only governs remembering vanished panes.
  The control-mode reader must consume BYTES (read_until + lossy), never
  read_line — panes emit raw >=0x80 bytes and an Err would kill the mirror.
- Card interaction contract: click = focus (keyboard → pane, graph keybinds
  suspend), drag = arrange (world-space offset from anchor in
  `term_offsets`), Ctrl+Q or click-away = release. Cards win pointer
  contention over nodes beneath them via last-frame `term_rects`.
- The sim is seeded from `layout::radial` and has zero randomness — same
  vault, same picture. The coincident-node nudge is index-derived, not random.
- Node bodies are never held in memory; the detail pane reads the selected
  file on demand (`vault::read_body`).
- Cross-reload identity is `app::Viewer::ident` (ghosts namespaced as
  `[[target]]` because their "path" is raw target text).
- View state persists to `<vault>/.text-graph/view` (state.rs). The dot-dir
  is hidden, so the walker and the reload watcher are blind to it — saves
  can never cause reload loops; keep it that way. Card arrangements are
  keyed by SESSION NAME and parked in `restore_offsets` when a session is
  absent (pane ids change across tmux restarts) — never hard-drop an
  arrangement. Saves are debounced (3s heartbeat repaint keeps them running
  once the sim settles) and the file is sorted for determinism.

## egui 0.36 gotchas (cost us compile time already)

- `eframe::App` has `fn ui(&mut self, ui: &mut Ui, ...)` — not `update(ctx)`.
- Panels are unified: `egui::Panel::top/right(...)`, shown with
  `.show(ui, ...)` (takes a `Ui`, not a `Context`). No `TopBottomPanel`.
- `painter.circle_filled` returns `ShapeIdx` — don't use it as a match-arm
  tail expression.
- `egui_commonmark` 0.25 pairs with egui 0.36; viewer API is
  `CommonMarkViewer::new().show(ui, &mut cache, text)`.
- `Painter::rect_stroke` needs a `StrokeKind` argument; `Painter::layout_job`
  exists (use it — `ctx().fonts()` gives a read-only view that can't layout).
- `gen` is a reserved keyword in edition 2024 — don't name variables that.
- egui quits on Ctrl+Q by default (`Options::quit_shortcuts`); `run()`
  clears it because Ctrl+Q releases terminal focus. Don't reintroduce it.
- Debugging the mirror: `examples/tmux_debug.rs` dumps raw control-client
  events against any socket/session (`cargo run --example tmux_debug S t`).
