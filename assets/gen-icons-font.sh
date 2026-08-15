#!/bin/sh
# Regenerate assets/icons.ttf — a subset of JetBrainsMono Nerd Font Propo
# (OFL-1.1; glyphs aggregated by the Nerd Fonts project) holding only the
# file-type icons text-graph paints on the canvas. Codepoints must stay in
# sync with the table in src/filetype.rs.
#
# Usage: ./gen-icons-font.sh [path-to-nerd-font.ttf]
set -e
SRC="${1:-$HOME/.local/share/fonts/JetBrainsMonoNerdFontPropo-Regular.ttf}"
cd "$(dirname "$0")"
uvx --from fonttools fonttools subset "$SRC" \
    --unicodes="U+E5FE,U+E5FF,U+E60B,U+E615,U+E61D,U+E61E,U+E620,U+E626,U+E628,U+E706,U+E736,U+E738,U+E739,U+E73C,U+E73D,U+E73E,U+E749,U+E74E,U+E755,U+E795,U+E7A8,U+E7B0,U+E7BA,U+F013,U+F016,U+F023,U+F03E,U+F0AC,U+F0CE,U+F0F6,U+F1C1,U+F1C6,U+F1C7,U+F1C8,U+F48A" \
    --output-file=icons.ttf --name-IDs='*'
echo "wrote $(pwd)/icons.ttf"
