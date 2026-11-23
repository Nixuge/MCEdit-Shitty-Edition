#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REWRITE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$REWRITE_DIR/.." && pwd)"

BINARY="$REWRITE_DIR/target/release/mcedit-rs"
OUT_DIR="$REWRITE_DIR/dist"
VERSION="0.1.0"
APP_NAME="MCEdit-Unified"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary) BINARY="$2"; shift 2 ;;
        --out) OUT_DIR="$2"; shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

ICO_PATH="$REPO_ROOT/mcedit.ico"
if [[ ! -f "$ICO_PATH" ]]; then
    echo "Error: $ICO_PATH not found" >&2
    exit 1
fi

if [[ ! -f "$BINARY" ]]; then
    echo "Error: binary not found at $BINARY (build with 'cargo build --release --bin mcedit-rs' first)" >&2
    exit 1
fi

APP_BUNDLE="$OUT_DIR/${APP_NAME}.app"
CONTENTS="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RES_DIR="$CONTENTS/Resources"

rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR" "$RES_DIR"

# 1. Copy executable
cp "$BINARY" "$MACOS_DIR/mcedit-rs"
chmod +x "$MACOS_DIR/mcedit-rs"

# 2. Generate MCEdit.icns directly from root mcedit.ico via sips
sips -s format icns "$ICO_PATH" --out "$RES_DIR/MCEdit.icns" >/dev/null

# 3. Write Info.plist
cat > "$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>MCEdit Unified</string>
    <key>CFBundleExecutable</key>
    <string>mcedit-rs</string>
    <key>CFBundleIconFile</key>
    <string>MCEdit</string>
    <key>CFBundleIdentifier</key>
    <string>org.mcedit-unified.mcedit-rs</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>MCEdit-Unified</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

# 4. Ad-hoc sign so macOS launches cleanly
if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$APP_BUNDLE" >/dev/null 2>&1 || true
fi

echo "Built $APP_BUNDLE"
