#!/bin/bash
set -euo pipefail

workspace="$(cd "$(dirname "$0")/.." && pwd)"
icon_source="$workspace/assets/app-icon.svg"
target_dir="${CARGO_TARGET_DIR:-target}"
case "$target_dir" in
  /*) ;;
  *) target_dir="$workspace/$target_dir" ;;
esac
bundle="$target_dir/release/bundle/osx/Hane.app"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/hane-icon.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

master_png=""
if /usr/bin/qlmanage -t -s 1024 -o "$tmp" "$icon_source" >/dev/null 2>&1; then
  master_png="$(find "$tmp" -maxdepth 1 -type f -name '*.png' -print -quit)"
fi
if [[ -z "$master_png" || ! -s "$master_png" ]]; then
  master_png="$tmp/app-icon.png"
  /usr/bin/sips -s format png "$icon_source" --out "$master_png" >/dev/null
fi

iconset="$tmp/Hane.iconset"
mkdir -p "$iconset"
resize() {
  local size="$1"
  local output="$2"
  /usr/bin/sips -z "$size" "$size" "$master_png" --out "$iconset/$output" >/dev/null
}
resize 16 icon_16x16.png
resize 32 icon_16x16@2x.png
resize 32 icon_32x32.png
resize 64 icon_32x32@2x.png
resize 128 icon_128x128.png
resize 256 icon_128x128@2x.png
resize 256 icon_256x256.png
resize 512 icon_256x256@2x.png
resize 512 icon_512x512.png
resize 1024 icon_512x512@2x.png
/usr/bin/iconutil -c icns "$iconset" -o "$tmp/Hane.icns"

cd "$workspace"
cargo build --release --locked -p hane

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$target_dir/release/hane" "$bundle/Contents/MacOS/hane"
cp "$tmp/Hane.icns" "$bundle/Contents/Resources/Hane.icns"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeExtensions</key>
      <array>
        <string>md</string>
      </array>
      <key>CFBundleTypeName</key>
      <string>Markdown Document</string>
      <key>CFBundleTypeRole</key>
      <string>Editor</string>
      <key>LSHandlerRank</key>
      <string>Alternate</string>
    </dict>
  </array>
  <key>CFBundleDisplayName</key>
  <string>Hane</string>
  <key>CFBundleExecutable</key>
  <string>hane</string>
  <key>CFBundleIconFile</key>
  <string>Hane.icns</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.hide212131.hane</string>
  <key>CFBundleName</key>
  <string>Hane</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST
/usr/bin/plutil -lint "$bundle/Contents/Info.plist" >/dev/null

# Finder lists Hane under "Open With" only for the document types declared
# here. Hane must offer exactly `.md` (not generic text) and must stay an
# alternate handler so the user, not the bundle, chooses the default app.
plist_buddy=/usr/libexec/PlistBuddy
info_plist="$bundle/Contents/Info.plist"
plist_value() {
  "$plist_buddy" -c "Print :$1" "$info_plist" 2>/dev/null
}
if [[ "$(plist_value CFBundleDocumentTypes:0:CFBundleTypeExtensions:0)" != "md" ]]; then
  echo "Info.plist must declare the md document type" >&2
  exit 1
fi
if plist_value CFBundleDocumentTypes:0:CFBundleTypeExtensions:1 >/dev/null; then
  echo "Info.plist must declare only the md extension" >&2
  exit 1
fi
if plist_value CFBundleDocumentTypes:1 >/dev/null; then
  echo "Info.plist must declare only one document type" >&2
  exit 1
fi
if plist_value CFBundleDocumentTypes:0:LSItemContentTypes >/dev/null; then
  echo "Info.plist must not claim generic content types" >&2
  exit 1
fi
if [[ "$(plist_value CFBundleDocumentTypes:0:LSHandlerRank)" != "Alternate" ]]; then
  echo "Info.plist document type must use LSHandlerRank Alternate" >&2
  exit 1
fi

touch "$bundle"
echo "Built: $bundle"
