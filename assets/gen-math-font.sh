#!/bin/sh
# Regenerate assets/math.ttf — the face rendered math is set in.
#
# A math span is drawn ENTIRELY in this font, not glyph-by-glyph out of
# whatever the reading chain happens to have: epaint centres a fallback
# face against the primary one, so a `∑` borrowed from another font sits
# off the baseline of the letters beside it. One face, one baseline.
#
# That means it has to cover everything src/mathtext.rs can emit,
# including the Mathematical Alphanumeric italics that ARE math italic
# (a renderer's italics flag only shears an upright glyph). No single
# Noto face has all of it, so the sources below are asked in order —
# each for what the ones before it could not draw — and the subsets are
# merged into ONE file, because a second file would be a fallback again.
#
# The codepoints come from `mathtext::glyphs()` rather than a list kept
# here by hand, so a new symbol in the table cannot silently outrun the
# font. The app-side test `every_character_mathtext_can_draw_has_a_glyph`
# is the gate that says when this needs re-running.
#
# All sources are Noto, OFL-1.1 (see assets/LICENSE-OFL-1.1.txt).
#
# Usage: ./gen-math-font.sh [source.ttf …]   (defaults below)
set -e
cd "$(dirname "$0")/.."
if [ $# -gt 0 ]; then
    set -- "$@"
else
    set -- /usr/share/fonts/truetype/noto/NotoSansMath-Regular.ttf \
           /usr/share/fonts/truetype/noto/NotoSans-Regular.ttf \
           /usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo run --quiet --example math_glyphs > "$work/want.txt"
parts=""
n=0
for src in "$@"; do
    n=$((n + 1))
    # what is still missing after the sources already taken
    uvx --from fonttools python - "$src" "$work/want.txt" "$work/take-$n.txt" "$work/want.txt" <<'PY'
import sys
from fontTools.ttLib import TTFont
src, wanted, take, rest = sys.argv[1:5]
cmap = set()
for t in TTFont(src, lazy=True)['cmap'].tables:
    cmap |= set(t.cmap.keys())
# newlines are the span's own line breaks, never a glyph
want = {c for c in open(wanted).read() if c != '\n'}
here = {c for c in want if ord(c) in cmap}
open(take, 'w').write(''.join(sorted(here)))
open(rest, 'w').write(''.join(sorted(want - here)))
PY
    [ -s "$work/take-$n.txt" ] || continue
    uvx --from fonttools fonttools subset "$src" \
        --text-file="$work/take-$n.txt" --layout-features='' \
        --output-file="$work/part-$n.ttf" --name-IDs='*'
    parts="$parts $work/part-$n.ttf"
done
if [ -s "$work/want.txt" ]; then
    echo "no source font draws: $(cat "$work/want.txt")" >&2
    exit 1
fi
# shellcheck disable=SC2086 # the parts are generated paths, never globs
uvx --from fonttools fonttools merge --output-file=assets/math.ttf $parts
echo "wrote $(pwd)/assets/math.ttf"
