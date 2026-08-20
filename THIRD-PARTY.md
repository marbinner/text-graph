# Third-party material

text-graph itself is dual-licensed MIT OR Apache-2.0 (see `LICENSE-MIT` and
`LICENSE-APACHE`). Three fonts are **bundled** in the repository and compiled
into the binary, so their licenses travel with every copy.

## Bundled fonts

Two are licensed under the SIL Open Font License 1.1, whose full text — with
both copyright notices — is `assets/LICENSE-OFL-1.1.txt`. Neither declares a
Reserved Font Name, so the subsets below keep their original name records. The
third is under the Bitstream Vera license (`assets/LICENSE-Bitstream-Vera.txt`).

### `assets/reading.ttf` — Inter

Copyright 2016 The Inter Project Authors (<https://rsms.me/inter/>), OFL-1.1.

A Latin subset of Inter Regular (basic Latin, Latin-1 Supplement, Latin
Extended-A, general punctuation, arrows, a few symbols) — the face rendered
markdown reads in. Anything outside the subset falls through to egui's default
fonts. Regenerate with `assets/gen-reading-font.sh`.

### `assets/math.ttf` — DejaVu Sans

Copyright (c) 2003 Bitstream, Inc.; DejaVu changes are in the public domain
(<https://dejavu-fonts.github.io/>). Bitstream Vera license — full text in
`assets/LICENSE-Bitstream-Vera.txt`. The subset keeps the DejaVu Sans name
records, which is what the license's rename clause asks of a modified font.

A ~270-codepoint subset holding exactly what `src/mathtext.rs` converts math
spans into: operators, relations, arrows, greek, super/subscript modifier
letters and combining accents. It sits behind Inter in the "reading" family,
so it only ever supplies glyphs Inter has no drawing for. The codepoints are
not listed by hand — `assets/gen-math-font.sh` reads them from
`mathtext::glyphs()`, and the test
`every_character_mathtext_can_draw_has_a_glyph` fails when the tables outgrow
the font.

### `assets/icons.ttf` — JetBrainsMono Nerd Font Propo

Copyright 2020 The JetBrains Mono Project Authors
(<https://github.com/JetBrains/JetBrainsMono>), OFL-1.1, with icon glyphs
aggregated by the [Nerd Fonts](https://github.com/ryanoasis/nerd-fonts)
project.

A 35-codepoint subset holding only the file-type icons the canvas paints. The
glyphs come from the Seti-UI, Devicons and Font Awesome ranges Nerd Fonts
collects; those upstream sets carry their own permissive licenses (MIT, OFL,
CC BY), enumerated by the Nerd Fonts project. Codepoints must stay in sync
with the table in `src/filetype.rs`; regenerate with
`assets/gen-icons-font.sh`.

## Rust dependencies

Not vendored — cargo fetches them, and `Cargo.lock` pins the exact set. They
are all permissively licensed (MIT, Apache-2.0, or both); `cargo license` or
`cargo about` will enumerate them for a binary you redistribute.

## The fixture vault

`fixtures/vault/` is synthetic: every note, link and trap in it was written for
this repository's tests. `fixtures/vault/assets/diagram.png` is a 24×16 image made
for these tests, not a third-party asset.
