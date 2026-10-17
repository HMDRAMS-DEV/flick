#!/bin/bash
# Builds target/Flick.app. With --install, copies it to /Applications and links `flick` into ~/.cargo/bin.
set -euo pipefail
cd "$(dirname "$0")"

# Universal binary: Apple Silicon and Intel.
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
APP="target/Flick.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/flick" \
    target/aarch64-apple-darwin/release/flick target/x86_64-apple-darwin/release/flick

# Helpers for "Fix with Claude" (src/fix.rs): flick-segment makes masks (Segment Anything and
# Vision), flick-listen does dictation. target/flick-* is also what `cargo run` finds.
for tool in segment listen; do
    # flick-listen carries its own Info.plist, which holds the mic and speech permission text.
    flags=()
    [[ $tool == listen ]] && flags=(-Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker tools/listen.plist)
    for arch in arm64 x86_64; do
        swiftc -O -target "$arch-apple-macos13" "tools/$tool.swift" -o "target/flick-$tool-$arch" ${flags[@]+"${flags[@]}"}
    done
    lipo -create -output "target/flick-$tool" "target/flick-$tool-arm64" "target/flick-$tool-x86_64"
    rm "target/flick-$tool-arm64" "target/flick-$tool-x86_64"
    cp "target/flick-$tool" "$APP/Contents/MacOS/flick-$tool"
done

ICONSET="target/AppIcon.iconset"
rm -rf "$ICONSET"
swift tools/makeicon.swift "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>               <string>Flick</string>
    <key>CFBundleDisplayName</key>        <string>Flick</string>
    <key>CFBundleIdentifier</key>         <string>com.ramihmd.flick</string>
    <key>CFBundleExecutable</key>         <string>flick</string>
    <key>CFBundleIconFile</key>           <string>AppIcon</string>
    <key>CFBundlePackageType</key>        <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>$VERSION</string>
    <key>CFBundleVersion</key>            <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>     <string>13.0</string>
    <key>NSHighResolutionCapable</key>    <true/>
    <key>LSApplicationCategoryType</key>  <string>public.app-category.photography</string>
    <key>NSMicrophoneUsageDescription</key> <string>Flick listens while you tell Claude how to fix a photo.</string>
    <key>NSSpeechRecognitionUsageDescription</key> <string>Flick turns what you say into the request it sends to Claude.</string>
    <key>NSAppleEventsUsageDescription</key> <string>Flick adds the photos you pick to a new album in Photos.</string>
</dict>
</plist>
PLIST
# Ad-hoc for local use. scripts/release.sh re-signs with the Developer ID.
codesign --force --sign - "$APP/Contents/MacOS/flick-segment" >/dev/null 2>&1
codesign --force --sign - "$APP/Contents/MacOS/flick-listen" >/dev/null 2>&1
codesign --force --sign - "$APP" >/dev/null 2>&1
echo "built $APP"

if [[ "${1:-}" == "--install" ]]; then
    rm -rf /Applications/Flick.app
    cp -R "$APP" /Applications/Flick.app
    mkdir -p "$HOME/.cargo/bin"
    ln -sf /Applications/Flick.app/Contents/MacOS/flick "$HOME/.cargo/bin/flick"
    echo "installed /Applications/Flick.app and ~/.cargo/bin/flick"
fi
