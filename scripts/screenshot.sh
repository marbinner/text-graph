#!/bin/sh
# Capture a screenshot of the viewer WITHOUT touching the user's session:
# runs a headless sway (wlroots' headless backend renders to memory, never to
# a monitor) with the release binary inside it, then grabs the frame.
#
# Usage: scripts/screenshot.sh [vault] [out.png]
#   defaults: a scratch COPY of fixtures/vault (a running viewer writes
#   .text-graph/ into whatever vault it opens — never point this at the
#   fixture itself), and docs/screenshot.png
#
# Needs sway and grim. XDG_CONFIG_HOME is redirected at a scratch dir so the
# capture can never pick up — or overwrite — your real config.
set -e
repo=$(cd "$(dirname "$0")/.." && pwd)
out=${2:-$repo/docs/screenshot.png}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -n "$1" ]; then
    vault=$1
else
    vault=$work/vault
    cp -r "$repo/fixtures/vault" "$vault"
fi

cargo build --release --locked --manifest-path "$repo/Cargo.toml"

cat > "$work/sway.conf" <<CONF
output HEADLESS-1 resolution 1600x1000
default_border none
exec $repo/target/release/text-graph $vault
exec sh -c 'sleep 10; grim $work/shot.png; swaymsg exit'
CONF

XDG_CONFIG_HOME=$work/config \
WLR_BACKENDS=headless WLR_RENDERER=pixman LIBGL_ALWAYS_SOFTWARE=1 \
    timeout 60 sway -c "$work/sway.conf" > "$work/sway.log" 2>&1 || true

[ -f "$work/shot.png" ] || { echo "no frame captured; see $work/sway.log" >&2; exit 1; }
# crop the empty margin the fixture vault leaves; drop for a full frame
convert "$work/shot.png" -crop 1240x800+0+0 +repage "$out"
echo "wrote $out"
