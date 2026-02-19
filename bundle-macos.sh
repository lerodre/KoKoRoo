#!/bin/bash
# Builds and creates hostelD.app bundle for macOS with icon and proper Info.plist.
# Usage: ./bundle-macos.sh [--release|--debug]

set -e

MODE="${1:---release}"
if [ "$MODE" = "--debug" ]; then
    BINARY="target/debug/hostelD"
    CARGO_FLAG=""
else
    BINARY="target/release/hostelD"
    CARGO_FLAG="--release"
fi

echo "Building hostelD ($MODE)..."
cargo build $CARGO_FLAG

APP="hostelD.app"
rm -rf "$APP"

mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"

cp "$BINARY" "$APP/Contents/MacOS/hostelD"

# Copy icon
if [ -f "assets/hostelD.icns" ]; then
    cp "assets/hostelD.icns" "$APP/Contents/Resources/hostelD.icns"
else
    echo "Warning: assets/hostelD.icns not found. App will have no icon."
fi

# Create Info.plist
cat > "$APP/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>hostelD</string>
    <key>CFBundleDisplayName</key>
    <string>hostelD</string>
    <key>CFBundleIdentifier</key>
    <string>com.hosteld.app</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundlePackagetype</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>hostelD</string>
    <key>CFBundleIconFile</key>
    <string>hostelD</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>hostelD needs microphone access for voice calls.</string>
    <key>NSCameraUsageDescription</key>
    <string>hostelD needs camera access for webcam sharing.</string>
    <key>NSScreenCaptureUsageDescription</key>
    <string>hostelD needs screen recording access for screen sharing.</string>
</dict>
</plist>
PLIST

echo "Created $APP"
echo "  Binary: $APP/Contents/MacOS/hostelD"
echo "  Icon:   $APP/Contents/Resources/hostelD.icns"
echo ""
echo "To run: open $APP"
echo "To install: cp -r $APP /Applications/"
