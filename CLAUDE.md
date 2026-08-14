# text-graph — working notes

## Commands

- `cargo test` — unit + integration. Integration (`tests/fixture_vault.rs`)
  asserts the exact hand-counted numbers in `fixtures/EXPECTED.md`.
- `cargo clippy --all-targets` — keep it at zero warnings.
- `cargo run --release -- <vault>` opens the GUI; `-- stats <vault>` is headless.
- GUI smoke test without interaction: `timeout 6 cargo run --release -- fixtures/vault`
  → exit 124 means it launched and ran fine.

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
- `layout.rs` and `sim.rs` must stay egui-free (headless-testable).
  `app.rs` is the only module allowed to touch egui.
- The sim is seeded from `layout::radial` and has zero randomness — same
  vault, same picture. The coincident-node nudge is index-derived, not random.
- Node bodies are never held in memory; the detail pane reads the selected
  file on demand (`vault::read_body`).
- Cross-reload identity is `app::Viewer::ident` (ghosts namespaced as
  `[[target]]` because their "path" is raw target text).

## egui 0.36 gotchas (cost us compile time already)

- `eframe::App` has `fn ui(&mut self, ui: &mut Ui, ...)` — not `update(ctx)`.
- Panels are unified: `egui::Panel::top/right(...)`, shown with
  `.show(ui, ...)` (takes a `Ui`, not a `Context`). No `TopBottomPanel`.
- `painter.circle_filled` returns `ShapeIdx` — don't use it as a match-arm
  tail expression.
- `egui_commonmark` 0.25 pairs with egui 0.36; viewer API is
  `CommonMarkViewer::new().show(ui, &mut cache, text)`.
