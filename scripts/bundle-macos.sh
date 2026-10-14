#!/bin/sh
# bundle-macos.sh <version> <out-dir>
#
# Build the release binaries and assemble Keleuo.app in <out-dir>. `keleuo` is the window;
# `keleuo-hook` sits beside it because the window finds it next to its own executable.
# Not signed here: the release workflow signs and notarizes the bundle it produces.

set -eu

VERSION=$1
OUT=$2
REPO=$(cd "$(dirname "$0")/.." && pwd)
APP="$OUT/Keleuo.app"

cd "$REPO"
cargo build -p keleuo-app --release --bins

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/keleuo target/release/keleuo-hook "$APP/Contents/MacOS/"
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
    <string>$VERSION</string>
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
