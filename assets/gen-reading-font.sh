#!/bin/sh
# Regenerate assets/reading.ttf — a Latin subset of Inter Regular
# (OFL-1.1, © The Inter Project Authors, https://rsms.me/inter/), the face
# rendered markdown bodies read in. Latin + punctuation + arrows only;
# anything else falls through to egui's default fonts via the "reading"
# family's fallback chain.
#
# Usage: ./gen-reading-font.sh [path-to-Inter-Regular.ttf]
#        (default expects the static hinted TTF from the Inter release
#        zip, e.g. Inter-4.1.zip -> extras/ttf/Inter-Regular.ttf)
set -e
SRC="${1:-$HOME/.local/share/fonts/Inter-Regular.ttf}"
cd "$(dirname "$0")"
uvx --from fonttools fonttools subset "$SRC" \
    --unicodes="U+0020-007E,U+00A0-017F,U+2000-206F,U+20AC,U+2122,U+2190-2199,U+2212" \
    --layout-features='kern,liga,calt' \
    --output-file=reading.ttf --name-IDs='*'
echo "wrote $(pwd)/reading.ttf"
