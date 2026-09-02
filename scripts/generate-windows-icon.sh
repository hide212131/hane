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
import struct
import sys


def png_dimensions(data: bytes) -> tuple[int, int]:
    # PNG signature (8 bytes) + IHDR chunk length (4) + "IHDR" (4),
    # followed by width(4) and height(4) as big-endian uint32.
    width, height = struct.unpack(">II", data[16:24])
    return width, height


out_path = sys.argv[1]
frame_paths = sys.argv[2:]

frames = []
for path in frame_paths:
    with open(path, "rb") as f:
        data = f.read()
    width, height = png_dimensions(data)
    frames.append((width, height, data))

# Largest first is conventional, but ICO readers don't require an order.
frames.sort(key=lambda frame: frame[0], reverse=True)

with open(out_path, "wb") as out:
    # ICONDIR: reserved(2)=0, type(2)=1 (icon), count(2)
    out.write(struct.pack("<HHH", 0, 1, len(frames)))

    offset = 6 + 16 * len(frames)
    for width, height, data in frames:
        # ICONDIRENTRY: width/height as a single byte each, 0 means 256px.
        entry_width = width if width < 256 else 0
        entry_height = height if height < 256 else 0
        out.write(
            struct.pack(
                "<BBBBHHII",
                entry_width,
                entry_height,
                0,  # color count (0 = no palette, i.e. >=8bpp)
                0,  # reserved
                1,  # color planes
                32,  # bits per pixel
                len(data),
                offset,
            )
        )
        offset += len(data)

    for _width, _height, data in frames:
        out.write(data)
PY

echo "Wrote: $icon_out"
