#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
if command -v cargo >/dev/null 2>&1; then
  CARGO_BIN="$(command -v cargo)"
else
  CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
"$CARGO_BIN" build --release --locked --manifest-path engine/Cargo.toml
swift build -c release --product RecoveryCard
APP="dist/Recovery Card.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp .build/release/RecoveryCard "$APP/Contents/MacOS/RecoveryCard"
cp engine/target/release/recovery-engine "$APP/Contents/Helpers/recovery-engine"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Recovery Card</string>
<key>CFBundleDisplayName</key><string>Recovery Card</string>
<key>CFBundleIdentifier</key><string>local.recoverycard.app</string>
<key>CFBundleExecutable</key><string>RecoveryCard</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$APP/Contents/Helpers/recovery-engine"
codesign --force --sign - "$APP"
codesign --verify --deep --strict "$APP"
printf 'Built %s/%s\n' "$PWD" "$APP"
