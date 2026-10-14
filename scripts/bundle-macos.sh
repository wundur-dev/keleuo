#!/bin/sh
# bundle-macos.sh <version> <out-dir> [build-number]
#
# Build the release binaries for Apple silicon and Intel, join each pair into one universal
# binary, and assemble Keleuo.app in <out-dir>. `keleuo` is the window;
# `keleuo-hook` sits beside it because the window finds it next to its own executable.
# The build number (CFBundleVersion) defaults to 1; the release workflow passes its run number.
# Not signed here: the release workflow signs and notarizes the bundle it produces.

set -eu

VERSION=$1
OUT=$2
BUILD=${3:-1}
REPO=$(cd "$(dirname "$0")/.." && pwd)
APP="$OUT/Keleuo.app"

cd "$REPO"
TARGET=${CARGO_TARGET_DIR:-target}
for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build -p keleuo-app --release --bins --target "$arch"
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for bin in keleuo keleuo-hook; do
    lipo -create -output "$APP/Contents/MacOS/$bin" \
        "$TARGET/aarch64-apple-darwin/release/$bin" "$TARGET/x86_64-apple-darwin/release/$bin"
done
cp crates/keleuo-app/assets/icon/Keleuo.icns "$APP/Contents/Resources/Keleuo.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>keleuo</string>
    <key>CFBundleIdentifier</key>
    <string>ai.wundur.keleuo</string>
    <key>CFBundleName</key>
    <string>Keleuo</string>
    <key>CFBundleDisplayName</key>
    <string>Keleuo</string>
    <key>CFBundleIconFile</key>
    <string>Keleuo</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$BUILD</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

echo "Built $APP ($VERSION)"
