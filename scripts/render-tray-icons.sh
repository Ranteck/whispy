#!/usr/bin/env bash
# Keep rasterization out of the build so installed binaries need no image tools.
set -euo pipefail

icon_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../crates/tray/icons" && pwd)"

for state in idle recording transcribing error; do
  rsvg-convert -w 48 -h 48 "$icon_dir/$state.svg" \
    | magick png:- -depth 8 "rgba:$icon_dir/$state.rgba"
done

printf 'Rendered four 48x48 RGBA tray icons.\n' >&2
