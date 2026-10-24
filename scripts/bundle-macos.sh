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

# The bundle carries the version, source commit and updater protocol that the
# update helper compares with the signed catalog. The commit must be a full
# SHA: the release workflows pass HANE_SOURCE_SHA, local builds use HEAD.
version="$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)"$/\1/p' Cargo.toml | head -n 1)"
if [[ -z "$version" ]]; then
  echo "Could not read the workspace package version from Cargo.toml" >&2
  exit 1
fi
source_sha="${HANE_SOURCE_SHA:-$(git rev-parse HEAD 2>/dev/null || true)}"
if ! [[ "$source_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "HANE_SOURCE_SHA (or a git checkout) must provide a 40-digit lowercase source commit" >&2
  exit 1
fi
updater_protocol=1
# hane-updater/build.rs embeds the same commit; both binaries are built here.
export HANE_SOURCE_SHA="$source_sha"
cargo build --release --locked -p hane -p hane-updater

# The helper must report the identity the bundle claims. Release builds repeat
# this check on the extracted ZIP.
python3 "$workspace/scripts/release_catalog.py" binary-identity \
  --binary "$target_dir/release/hane-updater" \
  --version "$version" --target aarch64-apple-darwin --source-sha "$source_sha"

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Helpers" "$bundle/Contents/Resources"
cp "$target_dir/release/hane" "$bundle/Contents/MacOS/hane"
cp "$target_dir/release/hane-updater" "$bundle/Contents/Helpers/hane-updater"
chmod 755 "$bundle/Contents/MacOS/hane" "$bundle/Contents/Helpers/hane-updater"
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
  <key>NSServices</key>
  <array>
    <dict>
      <key>NSMenuItem</key>
      <dict>
        <key>default</key>
        <string>Haneで開く</string>
      </dict>
      <key>NSMessage</key>
      <string>openFolderInHane</string>
      <key>NSPortName</key>
      <string>Hane</string>
      <key>NSRequiredContext</key>
      <dict>
        <key>NSApplicationIdentifier</key>
        <string>com.apple.finder</string>
      </dict>
      <key>NSSendFileTypes</key>
      <array>
        <string>public.folder</string>
      </array>
    </dict>
  </array>
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

# Update identity read by hane-update (crates/update/src/macos_apply.rs).
update_plist="$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $version" "$update_plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $version" "$update_plist"
/usr/libexec/PlistBuddy -c "Add :HaneSourceCommit string $source_sha" "$update_plist"
/usr/libexec/PlistBuddy -c "Add :HaneUpdaterProtocol integer $updater_protocol" "$update_plist"
/usr/bin/plutil -lint "$update_plist" >/dev/null

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
# Folders reach Hane through a Finder-only Service, never as a document type
# (which would change "Open With" candidates and default applications).
if [[ "$(plist_value NSServices:0:NSMessage)" != "openFolderInHane" ]] \
  || [[ "$(plist_value NSServices:0:NSRequiredContext:NSApplicationIdentifier)" != "com.apple.finder" ]] \
  || [[ "$(plist_value NSServices:0:NSSendFileTypes:0)" != "public.folder" ]] \
  || plist_value NSServices:0:NSSendFileTypes:1 >/dev/null \
  || plist_value NSServices:1 >/dev/null; then
  echo "Info.plist must declare only the Finder folder Service openFolderInHane" >&2
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

for pair in \
  "CFBundleIdentifier:io.github.hide212131.hane" \
  "CFBundleShortVersionString:$version" \
  "CFBundleVersion:$version" \
  "HaneSourceCommit:$source_sha" \
  "HaneUpdaterProtocol:$updater_protocol"; do
  if [[ "$(plist_value "${pair%%:*}")" != "${pair#*:}" ]]; then
    echo "Info.plist ${pair%%:*} must be ${pair#*:}" >&2
    exit 1
  fi
done
if [[ ! -x "$bundle/Contents/Helpers/hane-updater" || ! -x "$bundle/Contents/MacOS/hane" ]]; then
  echo "Bundle must contain executable Contents/MacOS/hane and Contents/Helpers/hane-updater" >&2
  exit 1
fi

# Apple Silicon only runs code that carries a valid signature, so the finished
# bundle is sealed with a local ad-hoc signature (identity "-"): no Developer ID,
# Team ID, notarization or stapling is involved, and quarantine and Gatekeeper
# are left untouched (the user still has to allow the app once). The linker's
# own ad-hoc signatures no longer match once the Info.plist and resources are
# added, so the seal is made from the inside out after everything is in place:
# the helper, the main executable, then the bundle itself (which writes
# Contents/_CodeSignature/CodeResources).
#
# The release workflow afterwards generates the Ed25519-signed inventory over
# every file of this sealed bundle, CodeResources included, and stores it
# OUTSIDE Hane.app (Hane.app.update/<version>-<source_sha>/). Nothing may
# modify the bundle after this point; adding a file inside would break the seal.
chmod 755 "$bundle/Contents/MacOS/hane" "$bundle/Contents/Helpers/hane-updater"
/usr/bin/codesign --force --sign - --timestamp=none "$bundle/Contents/Helpers/hane-updater"
/usr/bin/codesign --force --sign - --timestamp=none "$bundle/Contents/MacOS/hane"
/usr/bin/codesign --force --sign - --timestamp=none "$bundle"
for binary in "$bundle/Contents/MacOS/hane" "$bundle/Contents/Helpers/hane-updater"; do
  python3 "$workspace/scripts/release_catalog.py" macho-signed --binary "$binary"
done

# Signing must not change the helper identity the catalog binds.
python3 "$workspace/scripts/release_catalog.py" binary-identity \
  --binary "$bundle/Contents/Helpers/hane-updater" \
  --version "$version" --target aarch64-apple-darwin --source-sha "$source_sha"

if [[ ! -f "$bundle/Contents/_CodeSignature/CodeResources" ]]; then
  echo "Bundle must be sealed (Contents/_CodeSignature/CodeResources is missing)" >&2
  exit 1
fi
if [[ -e "$bundle/Contents/Resources/hane-install.json" || -e "$bundle/Contents/Resources/hane-install.json.sig" ]]; then
  echo "The Ed25519 inventory must not be stored inside Hane.app" >&2
  exit 1
fi
/usr/bin/codesign --verify --deep --strict --verbose=2 "$bundle"

touch "$bundle"
echo "Built: $bundle"
