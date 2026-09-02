#!/bin/bash
set -euo pipefail

workspace="$(cd "$(dirname "$0")/.." && pwd)"
icon_source="$workspace/assets/app-icon.svg"
icon_out="$workspace/assets/app-icon.ico"
sizes=(16 24 32 48 64 128 256)

tmp="$(mktemp -d "${TMPDIR:-/tmp}/hane-ico.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

master_png=""
if /usr/bin/qlmanage -t -s 1024 -o "$tmp" "$icon_source" >/dev/null 2>&1; then
  master_png="$(find "$tmp" -maxdepth 1 -type f -name '*.png' -print -quit)"
fi
if [[ -z "$master_png" || ! -s "$master_png" ]]; then
  master_png="$tmp/app-icon.png"
  /usr/bin/sips -s format png "$icon_source" --out "$master_png" >/dev/null
fi

frame_args=()
for size in "${sizes[@]}"; do
  frame="$tmp/icon_${size}.png"
  /usr/bin/sips -z "$size" "$size" "$master_png" --out "$frame" >/dev/null
  frame_args+=("$frame")
done

python3 - "$icon_out" "${frame_args[@]}" <<'PY'
import sys
from PIL import Image

out_path = sys.argv[1]
frames = [Image.open(path).convert("RGBA") for path in sys.argv[2:]]
frames.sort(key=lambda im: im.size[0], reverse=True)
frames[0].save(
    out_path,
    format="ICO",
    sizes=[im.size for im in frames],
    append_images=frames[1:],
)
PY

echo "Wrote: $icon_out"
