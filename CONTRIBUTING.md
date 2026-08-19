# Contributing

Contributions are welcome, including — especially — ones written by agents.

## How this project is built

Every line of text-graph was written by an AI agent working from a human's
direction, and that is expected to continue. It shapes what this file asks
for: rules that live only in prose get missed, so the invariants that matter
live in the compiler, in clippy, in test names that read as sentences, and in
doc comments sitting on the code they govern. If you find yourself needing a
rule that exists nowhere but a document, that is a bug in the project, and
moving it into a type or a test is a welcome change on its own.

`CLAUDE.md` is the map for an agent editing this repository: the module
contracts, the invariants that span modules, and the gotchas already paid for
in bugs. It is meant to shrink over time, not grow. `PLAN.md` is the roadmap
and the record of decisions — including the ones that were made and rejected.
`human-notes.md` is exactly what it says.

## The gate

```
scripts/check.sh
```

That is the whole thing: formatting, the GUI-free layering check, clippy at
zero warnings, the full test suite including examples, plus MSRV and the
advisory audit when those toolchains are installed. It mirrors CI, so a green
run locally means a green run there.

Two rules about running it, both learned the hard way:

- **Gate on the exit code, never on grepped output.** `grep` returns 0 when it
  *matches* an error line; that once let a broken commit through.
- **Don't pipe it.** `scripts/check.sh | tail -1 && git commit` gates on
  `tail`'s exit code, which is always 0. Use `scripts/check.sh > /dev/null`
  (errors still reach stderr) or check `$?` explicitly.

## Shape of a change

- **One commit per coherent change**, each building green on its own. A branch
  of ten small green commits is much easier to review — and to revert — than
  one large one.
- **New behavior comes with a test.** Keybindings and viewer state go in
  `src/app/kb_tests/` (headless egui, no renderer); everything else is a unit
  test beside the code or an integration test in `tests/`.
- **Name tests as sentences.** `a_narrow_result_set_does_not_shrink_the_finder_for_good`
  tells whoever breaks it what the rule was. `test_finder_width` does not.
- **Editing `fixtures/vault/` means re-counting `fixtures/EXPECTED.md`** and
  updating `tests/fixture_vault.rs` in the same commit. The integration tests
  assert hand-counted numbers on purpose.
- **Determinism is a feature.** No randomness, no unsorted-map iteration, no
  walk-order dependence anywhere that feeds node ids, layout, or output. There
  is a build-twice test that will catch you.
- **The core stays GUI-free.** `cargo check --no-default-features` must keep
  passing: every library module is headless-testable, and only `src/app/`
  touches egui. New GUI-only dependencies are optional and pulled in by the
  `gui` feature explicitly.
- **Dependencies arrive at the milestone that needs them**, not before.

## Things worth knowing before you start

- The tmux tests spawn a real server on a **private socket**
  (`tmux -L tg-test-<pid>`) and kill only that server. Never point a test at
  the default one — someone is working in it.
- A running viewer writes `.text-graph/` into whatever vault it opens. Smoke
  tests copy `fixtures/vault` to a scratch directory first, and set
  `XDG_CONFIG_HOME` to a scratch directory too, so a test run can't touch
  anyone's real config.
- Measure before optimizing. `cargo run --release --example perf_probe <vault>`
  times the headless pipeline and `fixtures/gen-stress.sh N` builds a big vault
  to point it at; the ⚙ *frame statistics* setting overlays per-stage frame
  times in the running viewer. Both exist so performance work starts from
  numbers.

## Reporting things

Bug reports are most useful with the vault shape that triggered them (sizes and
structure, not your notes), your platform and tmux version, and whether the
viewer was launched from a terminal or a desktop entry — several real bugs came
down to the environment a GUI launcher does *not* pass through.

## Licensing

Contributions are accepted under the same terms as the project: MIT OR
Apache-2.0, at the user's option. See `LICENSE-MIT`, `LICENSE-APACHE`, and
`THIRD-PARTY.md` for the bundled fonts.
