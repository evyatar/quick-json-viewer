#!/usr/bin/env bash
set -e

# Ensure the native target is available
rustup target add aarch64-apple-darwin 2>/dev/null || true

cargo build --release --target aarch64-apple-darwin

APP=quick-json-viewer.app
mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"
cp target/aarch64-apple-darwin/release/quick-json-viewer "$APP/Contents/MacOS/quick-json-viewer"
chmod +x "$APP/Contents/MacOS/quick-json-viewer"

# App icon
SRC_ICON="src/icon.png"
if [ -f "$SRC_ICON" ]; then
    ICONSET=$(mktemp -d)/AppIcon.iconset
    mkdir -p "$ICONSET"
    for SIZE in 16 32 64 128 256 512; do
        sips -z $SIZE $SIZE "$SRC_ICON" --out "$ICONSET/icon_${SIZE}x${SIZE}.png"     >/dev/null
    done
    # @2x variants
    sips -z 32   32   "$SRC_ICON" --out "$ICONSET/icon_16x16@2x.png"   >/dev/null
    sips -z 64   64   "$SRC_ICON" --out "$ICONSET/icon_32x32@2x.png"   >/dev/null
    sips -z 256  256  "$SRC_ICON" --out "$ICONSET/icon_128x128@2x.png" >/dev/null
    sips -z 512  512  "$SRC_ICON" --out "$ICONSET/icon_256x256@2x.png" >/dev/null
    sips -z 1024 1024 "$SRC_ICON" --out "$ICONSET/icon_512x512@2x.png" >/dev/null
    iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
    rm -rf "$(dirname "$ICONSET")"
    echo "Icon:  AppIcon.icns generated"
else
    echo "Warning: $SRC_ICON not found, skipping icon"
fi

# plist
cat > "$APP/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>JSON Viewer</string>
  <key>CFBundleIdentifier</key><string>com.evyatar.quick-json-viewer</string>
  <key>CFBundleVersion</key><string>1.0.0</string>
  <key>CFBundleExecutable</key><string>quick-json-viewer</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeExtensions</key>
      <array>
        <string>json</string><string>jsonl</string><string>ndjson</string>
      </array>
      <key>CFBundleTypeName</key><string>JSON File</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Owner</string>
      <key>LSItemContentTypes</key>
      <array><string>public.json</string></array>
    </dict>
  </array>
</dict>
</plist>
EOF

# ad-hoc codesign
codesign --force --deep --sign - "$APP"

echo "Built: $APP"
echo "Run:   open $APP"
echo "Or:    ./$APP/Contents/MacOS/quick-json-viewer"
