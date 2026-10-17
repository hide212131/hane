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
      <key>CFBundleTypeName</key>
      <string>Markdown Document</string>
      <key>CFBundleTypeRole</key>
      <string>Editor</string>
      <key>LSHandlerRank</key>
      <string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>net.daringfireball.markdown</string>
      </array>
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
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>UTImportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeConformsTo</key>
      <array>
        <string>public.utf8-plain-text</string>
      </array>
      <key>UTTypeDescription</key>
      <string>Markdown Document</string>
      <key>UTTypeIdentifier</key>
      <string>net.daringfireball.markdown</string>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.filename-extension</key>
        <array>
          <string>md</string>
        </array>
      </dict>
    </dict>
  </array>
</dict>
</plist>
PLIST
/usr/bin/plutil -lint "$bundle/Contents/Info.plist" >/dev/null

# Finder lists Hane under "Open With" only for the document types declared
# here. Hane must offer exactly the system Markdown UTI (not generic text) and
# must stay an alternate handler so the user, not the bundle, chooses the
# default app. The system only defines net.daringfireball.markdown on newer
# OSes (macOS 27+), so the bundle imports the same UTI with the md extension and
# UTF-8 plain text conformance. Older systems then resolve .md to that UTI, and
# newer systems coexist with the system definition. The minimum OS version
# follows the build target, not the UTI's availability.
plist_buddy=/usr/libexec/PlistBuddy
info_plist="$bundle/Contents/Info.plist"
plist_value() {
  "$plist_buddy" -c "Print :$1" "$info_plist" 2>/dev/null
}
if [[ "$(plist_value CFBundleDocumentTypes:0:LSItemContentTypes:0)" != "net.daringfireball.markdown" ]]; then
  echo "Info.plist must declare the net.daringfireball.markdown content type" >&2
  exit 1
fi
if plist_value CFBundleDocumentTypes:0:LSItemContentTypes:1 >/dev/null; then
  echo "Info.plist must declare only the Markdown content type" >&2
  exit 1
fi
if plist_value CFBundleDocumentTypes:1 >/dev/null; then
  echo "Info.plist must declare only one document type" >&2
  exit 1
fi
if plist_value CFBundleDocumentTypes:0:CFBundleTypeExtensions >/dev/null; then
  echo "Info.plist must not use the deprecated CFBundleTypeExtensions" >&2
  exit 1
fi
if plist_value UTExportedTypeDeclarations >/dev/null; then
  echo "Info.plist must import, not export, the Markdown UTI" >&2
  exit 1
fi
if plist_value UTImportedTypeDeclarations:1 >/dev/null; then
  echo "Info.plist must import only the Markdown UTI" >&2
  exit 1
fi
if [[ "$(plist_value UTImportedTypeDeclarations:0:UTTypeIdentifier)" != "net.daringfireball.markdown" ]]; then
  echo "Info.plist must import net.daringfireball.markdown" >&2
  exit 1
fi
if [[ "$(plist_value UTImportedTypeDeclarations:0:UTTypeConformsTo:0)" != "public.utf8-plain-text" ]] \
  || plist_value UTImportedTypeDeclarations:0:UTTypeConformsTo:1 >/dev/null; then
  echo "Imported Markdown UTI must conform only to public.utf8-plain-text" >&2
  exit 1
fi
if [[ "$(plist_value UTImportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:0)" != "md" ]] \
  || plist_value UTImportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:1 >/dev/null; then
  echo "Imported Markdown UTI must register only the md extension" >&2
  exit 1
fi
if [[ "$(plist_value LSMinimumSystemVersion)" != "11.0" ]]; then
  echo "Info.plist must keep the build target minimum macOS 11.0" >&2
  exit 1
fi
if [[ "$(plist_value CFBundleDocumentTypes:0:LSHandlerRank)" != "Alternate" ]]; then
  echo "Info.plist document type must use LSHandlerRank Alternate" >&2
  exit 1
fi

touch "$bundle"
echo "Built: $bundle"
