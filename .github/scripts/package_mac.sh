#!/bin/sh
# Wraps the macOS build of iAi into iAi.app and puts that on a disk image,
# the file a Mac user downloads, opens and drags into Applications.
#
#   .github/scripts/package_mac.sh <folder holding the built iai> <output .dmg> <build number> [pro]
#
# With `pro` (a build with the Pro part, so with the `raw-dll` feature) the
# RAW library libiai_raw.dylib goes beside iai in Contents/MacOS, where the
# app looks for it (crates/iai-formats/src/raw_library.rs); cargo leaves it
# in the build folder or its deps. The free build links it in: no file.
#
# Runs on macOS only (sips, iconutil, codesign and hdiutil come with it). The
# app is signed ad hoc, not with an Apple Developer ID: the first time it is
# opened macOS asks the user to allow it (README, "Download for macOS").
set -eu

from="$1"
dmg="$2"
build="$3"
kind="${4:-free}"
here="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
app="$work/stage/iAi.app"

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$from/iai" "$app/Contents/MacOS/iai"
chmod +x "$app/Contents/MacOS/iai"
if [ "$kind" = "pro" ]; then
    raw=""
    for place in "$from/libiai_raw.dylib" "$from/deps/libiai_raw.dylib"; do
        if [ -f "$place" ]; then
            raw="$place"
            break
        fi
    done
    if [ -z "$raw" ]; then
        echo "no libiai_raw.dylib in $from or $from/deps: the Pro build reads RAW files with it" >&2
        exit 1
    fi
    cp "$raw" "$app/Contents/MacOS/libiai_raw.dylib"
fi

# The icon: the largest picture in assets/iai.ico, scaled into an iconset.
# Without it the app shows the plain icon; that is no reason to stop.
icon=""
if sips -s format png "$here/assets/iai.ico" --out "$work/icon.png" >/dev/null 2>&1; then
    set_dir="$work/iai.iconset"
    mkdir -p "$set_dir"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$work/icon.png" --out "$set_dir/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" "$work/icon.png" --out "$set_dir/icon_${size}x${size}@2x.png" >/dev/null
    done
    if iconutil -c icns "$set_dir" -o "$app/Contents/Resources/iai.icns"; then
        icon="iai"
    fi
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -n 1)"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>iAi</string>
    <key>CFBundleDisplayName</key><string>iAi</string>
    <key>CFBundleIdentifier</key><string>io.github.tongnghia2026.iai</string>
    <key>CFBundleExecutable</key><string>iai</string>
    <key>CFBundleIconFile</key><string>$icon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$build</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

ln -s /Applications "$work/stage/Applications"
rm -f "$dmg"
hdiutil create -volname "iAi" -srcfolder "$work/stage" -ov -format UDZO "$dmg"
rm -rf "$work"
echo "packed $dmg"
