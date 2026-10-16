#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
TARGET="${1:-$(rustc -vV | awk '/^host:/ {print $2}')}"
case "$TARGET" in
  aarch64-apple-darwin) ARCH="arm64" ;;
  x86_64-apple-darwin) ARCH="x86_64" ;;
  *) echo "Unsupported macOS target: $TARGET" >&2; exit 1 ;;
esac
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
APP="$ROOT/dist/BABY Watch.app"
ARCHIVE="$ROOT/dist/baby-watch-macos-$ARCH.zip"
cargo build --locked --release --target "$TARGET"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/baby-watch" "$APP/Contents/MacOS/baby-watch"
chmod +x "$APP/Contents/MacOS/baby-watch"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>baby-watch</string>
<key>CFBundleIdentifier</key><string>com.baby-watch.app</string>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>CFBundleName</key><string>BABY Watch</string>
<key>CFBundleDisplayName</key><string>BABY Watch</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$VERSION</string>
<key>CFBundleVersion</key><string>$VERSION</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Ad-hoc signing permits a locally runnable bundle; this is not Apple notarization.
codesign --force --deep --sign - "$APP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ARCHIVE"
shasum -a 256 "$ARCHIVE" | sed "s|$ROOT/dist/||" > "$ROOT/dist/baby-watch-macos-$ARCH.sha256"
printf 'Built: %s\n' "$ARCHIVE"
