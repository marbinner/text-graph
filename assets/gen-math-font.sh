#!/bin/sh
# Regenerate assets/math.ttf — the math face in the "reading" family's
# fallback chain. Inter (assets/reading.ttf) draws no operators, no
# scripts and no combining accents, and neither does egui's default
# proportional font, so a converted `$…$` span used to reach the reader as
# a row of replacement boxes.
#
# The subset is exactly what src/mathtext.rs can emit: the codepoints come
# from `mathtext::glyphs()` rather than a list kept here by hand, so a new
# symbol in the table cannot silently outrun the font. The app-side test
# `every_character_mathtext_can_draw_has_a_glyph` is the gate that says
# when this needs re-running.
#
# Source: DejaVu Sans (Bitstream Vera license, see
# assets/LICENSE-Bitstream-Vera.txt) — the one widely installed face that
# covers the whole inventory, modifier-letter scripts included.
#
# Usage: ./gen-math-font.sh [path-to-DejaVuSans.ttf]
set -e
SRC="${1:-/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf}"
cd "$(dirname "$0")/.."
glyphs=$(mktemp)
trap 'rm -f "$glyphs"' EXIT
cargo run --quiet --example math_glyphs > "$glyphs"
uvx --from fonttools fonttools subset "$SRC" \
    --text-file="$glyphs" \
    --layout-features='kern' \
    --output-file=assets/math.ttf --name-IDs='*'
echo "wrote $(pwd)/assets/math.ttf"
