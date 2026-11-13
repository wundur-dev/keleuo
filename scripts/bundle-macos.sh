#!/bin/sh
# bundle-macos.sh <version> <out-dir> [build-number]
#
# Build the release binaries for Apple silicon and Intel, join each pair into one universal
# binary, and assemble Wundur.app in <out-dir>. `wundur` is the window; `wundur-hook` and
# `wundur-mcp` sit beside it because the window finds them next to its own executable.
# The build number (CFBundleVersion) defaults to 1; the release workflow passes its run number.
# Not signed here: the release workflow signs and notarizes the bundle it produces.

set -eu

VERSION=$1
OUT=$2
BUILD=${3:-1}
REPO=$(cd "$(dirname "$0")/.." && pwd)
APP="$OUT/Wundur Studio.app"

cd "$REPO"
TARGET=${CARGO_TARGET_DIR:-target}
for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build -p wundur-app --release --bins --target "$arch"
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for bin in wundur wundur-hook wundur-mcp; do
    lipo -create -output "$APP/Contents/MacOS/$bin" \
        "$TARGET/aarch64-apple-darwin/release/$bin" "$TARGET/x86_64-apple-darwin/release/$bin"
done
cp crates/wundur-app/assets/icon/Wundur.icns "$APP/Contents/Resources/Wundur.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>wundur</string>
    <key>CFBundleIdentifier</key>
    <string>ai.wundur.app</string>
    <key>CFBundleName</key>
    <string>Wundur Studio</string>
    <key>CFBundleDisplayName</key>
    <string>Wundur Studio</string>
    <key>CFBundleIconFile</key>
    <string>Wundur</string>
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
    <key>NSMicrophoneUsageDescription</key>
    <string>Wundur listens when you press the microphone button, to type what you say into your message.</string>
    <key>NSSpeechRecognitionUsageDescription</key>
    <string>Wundur turns what you say into text for your message, on this Mac where it can.</string>
</dict>
</plist>
EOF

echo "Built $APP ($VERSION)"
