#!/usr/bin/env sh
# Generates a large flat vault for layout/perf stress testing (the "800-sibling
# folder" case). Output is gitignored — regenerate any time. Deterministic.
#
#   ./gen-stress.sh [N]      default N=500
set -eu
N="${1:-500}"
OUT="$(dirname "$0")/stress-vault/flat"
rm -rf "$OUT"
mkdir -p "$OUT"
i=0
while [ "$i" -lt "$N" ]; do
    next=$(( (i + 1) % N ))
    printf -- '---\ntitle: Note %03d\n---\n\nSee [[note-%03d]] and the [[hub]].\n' \
        "$i" "$next" > "$OUT/$(printf 'note-%03d' "$i").md"
    i=$(( i + 1 ))
done
printf -- '# Hub\n\nEvery note links here.\n' > "$OUT/hub.md"
echo "generated $N notes + hub in $OUT"
