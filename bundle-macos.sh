#!/bin/bash
# Builds and creates KoKoRoo.app bundle for macOS with icon and proper Info.plist.
# Usage: ./bundle-macos.sh [--release|--debug]

set -e

MODE="${1:---release}"
if [ "$MODE" = "--debug" ]; then
    BINARY="target/debug/kokoroo"
    CARGO_FLAG=""
else
    BINARY="target/release/kokoroo"
    CARGO_FLAG="--release"
fi

echo "Building KoKoRoo ($MODE)..."
cargo build $CARGO_FLAG

APP="KoKoRoo.app"
rm -rf "$APP"

mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"

cp "$BINARY" "$APP/Contents/MacOS/kokoroo"

# Copy icon
if [ -f "assets/kokoroo.icns" ]; then
    cp "assets/kokoroo.icns" "$APP/Contents/Resources/kokoroo.icns"
else
    echo "Warning: assets/kokoroo.icns not found. App will have no icon."
fi

# Create Info.plist
cat > "$APP/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>KoKoRoo</string>
    <key>CFBundleDisplayName</key>
    <string>KoKoRoo</string>
    <key>CFBundleIdentifier</key>
    <string>com.kokoroo.app</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundlePackagetype</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>kokoroo</string>
    <key>CFBundleIconFile</key>
    <string>kokoroo</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>KoKoRoo needs microphone access for voice calls.</string>
    <key>NSCameraUsageDescription</key>
    <string>KoKoRoo needs camera access for webcam sharing.</string>
    <key>NSScreenCaptureUsageDescription</key>
    <string>KoKoRoo needs screen recording access for screen sharing.</string>
</dict>
</plist>
PLIST

echo "Created $APP"
echo "  Binary: $APP/Contents/MacOS/kokoroo"
echo "  Icon:   $APP/Contents/Resources/kokoroo.icns"
echo ""
echo "To run: open $APP"
echo "To install: cp -r $APP /Applications/"
