#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SOURCE="$REPO_ROOT/assets/icon.png"
ICONSET_DIR="$REPO_ROOT/target/AppIcon.iconset"
ICNS_FILE="$REPO_ROOT/target/AppIcon.icns"

if [ ! -f "$SOURCE" ]; then
    echo "Error: $SOURCE not found" >&2
    echo "Provide a 1024x1024 PNG as assets/icon.png." >&2
    exit 1
fi

rm -rf "$ICONSET_DIR"
mkdir -p "$ICONSET_DIR"

sizes=(16 32 128 256 512)
for size in "${sizes[@]}"; do
    retina=$((size * 2))
    sips -z "$size" "$size" "$SOURCE" --out "$ICONSET_DIR/icon_${size}x${size}.png" >/dev/null
    sips -z "$retina" "$retina" "$SOURCE" --out "$ICONSET_DIR/icon_${size}x${size}@2x.png" >/dev/null
done

iconutil -c icns "$ICONSET_DIR" -o "$ICNS_FILE"
rm -rf "$ICONSET_DIR"

echo "Generated $ICNS_FILE"
