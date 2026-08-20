#!/bin/sh
# Regenerate assets/reading.ttf — a Latin subset of Inter Regular
# (OFL-1.1, © The Inter Project Authors, https://rsms.me/inter/), the face
# rendered markdown bodies read in. Latin + punctuation + arrows only;
# anything else falls through to egui's default fonts via the "reading"
# family's fallback chain.
#
# The subset also carries a LINE GAP, because egui has no line-height
# setting to reach for: epaint takes a row's height straight from the
# face (ascent - descent + leading), and Inter ships no leading at all,
# which set prose at 1.21 em — tight for reading. The gap is written as
# an absolute value derived from the em, so re-running this is
# idempotent.
#
# Usage: ./gen-reading-font.sh [path-to-Inter-Regular.ttf]
#        (default expects the static hinted TTF from the Inter release
#        zip, e.g. Inter-4.1.zip -> extras/ttf/Inter-Regular.ttf)
set -e
SRC="${1:-$HOME/.local/share/fonts/Inter-Regular.ttf}"
# how tall a row of prose is, in ems — the leading the subset carries
LEADING=1.5
cd "$(dirname "$0")"
uvx --from fonttools fonttools subset "$SRC" \
    --unicodes="U+0020-007E,U+00A0-017F,U+2000-206F,U+20AC,U+2122,U+2190-2199,U+2212" \
    --layout-features='kern,liga,calt' \
    --output-file=reading.ttf --name-IDs='*'
# `cd` above already put us in assets/
uvx --from fonttools python ./set-leading.py reading.ttf "$LEADING"
echo "wrote $(pwd)/reading.ttf"
