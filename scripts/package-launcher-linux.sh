#!/bin/sh
# Builds the Linux x64 portable Kaleidoscope package (lazer only).
#
#   package-launcher-linux.sh <bridge-directory> <output-directory> [version]
#
# <bridge-directory> holds a verified Transfiguration release (the bridge DLL,
# THIRD-PARTY-NOTICES.md, README.md and licenses/). Run from the Illyaverse
# workspace root. Set SKIP_UI_BUILD=1 to reuse an existing tools/kaleidoscope/ui/out.
set -eu

BRIDGE_DIR=${1:?bridge directory required}
OUTPUT=${2:?output directory required}
VERSION=${3:-dev}
BRIDGE=osu.Game.Rulesets.Transfiguration.dll
ROOT=$(pwd)
KALEIDOSCOPE="$ROOT/tools/kaleidoscope"

for required in "$BRIDGE" THIRD-PARTY-NOTICES.md README.md; do
    [ -f "$BRIDGE_DIR/$required" ] || { echo "Bridge directory is missing $required" >&2; exit 1; }
done
[ -e "$OUTPUT" ] && { echo "Output already exists: $OUTPUT" >&2; exit 1; }

# The launcher only starts a bridge whose SHA-256 was compiled into it.
ILLYA_BRIDGE_SHA256=$(sha256sum "$BRIDGE_DIR/$BRIDGE" | cut -d' ' -f1)
export ILLYA_BRIDGE_SHA256
export KALEIDOSCOPE_UPDATE_VERSION="$VERSION"
export CARGO_INCREMENTAL=0

if [ "${SKIP_UI_BUILD:-0}" != 1 ]; then
    npm run build --workspace '@illyaverse/kaleidoscope'
fi
[ -f "$KALEIDOSCOPE/ui/out/index.html" ] || { echo "Interface build output is missing" >&2; exit 1; }

cargo build --manifest-path "$KALEIDOSCOPE/ui/src-tauri/Cargo.toml" --locked --release --features custom-protocol
BINARY="$KALEIDOSCOPE/ui/src-tauri/target/release/kaleidoscope-desktop"
[ -n "${CARGO_TARGET_DIR:-}" ] && BINARY="$CARGO_TARGET_DIR/release/kaleidoscope-desktop"
[ -f "$BINARY" ] || { echo "Launcher binary was not built" >&2; exit 1; }

# The interface is embedded; confirm it before anything is packaged.
"$BINARY" --verify-embedded-interface

STAGE="$OUTPUT/Kaleidoscope-$VERSION-linux-x64"
mkdir -p "$STAGE/licenses"
install -m 0755 "$BINARY" "$STAGE/Kaleidoscope"
install -m 0644 "$BRIDGE_DIR/$BRIDGE" "$STAGE/$BRIDGE"
install -m 0644 "$BRIDGE_DIR/THIRD-PARTY-NOTICES.md" "$STAGE/THIRD-PARTY-NOTICES.md"
install -m 0644 "$BRIDGE_DIR/README.md" "$STAGE/Transfiguration-README.md"
install -m 0644 "$ROOT/LICENSE" "$STAGE/LICENSE"
if [ -d "$BRIDGE_DIR/licenses" ]; then
    for file in "$BRIDGE_DIR"/licenses/*; do install -m 0644 "$file" "$STAGE/licenses/"; done
fi
# Interface fonts: licence texts and provenance live with the interface source.
install -m 0644 "$KALEIDOSCOPE/ui/THIRD-PARTY-NOTICES.md" "$STAGE/Kaleidoscope-THIRD-PARTY-NOTICES.md"
install -m 0644 "$KALEIDOSCOPE/ui/licenses/Nunito.txt" "$STAGE/licenses/Nunito.txt"
install -m 0644 "$KALEIDOSCOPE/ui/licenses/NotoSansTC.txt" "$STAGE/licenses/NotoSansTC.txt"
cat >"$STAGE/README.txt" <<TXT
Kaleidoscope $VERSION for Linux (x64), lazer only.

1. Keep every file in this folder together.
2. Run ./Kaleidoscope (needs GTK 3 and WebKitGTK 4.1 installed).
3. Choose your osu!lazer: the osu.AppImage (make it executable first with
   chmod +x), or the osu! file of an extracted install.
4. Set your server in Settings and press Launch.

Only lazer 2026.1005.1-tachyon (linux-x64 AppImage) is accepted. This Linux
build has not been verified against a real game yet: report problems with your
lazer version and the launcher status text. Official servers are never used.
TXT
(cd "$STAGE" && sha256sum Kaleidoscope "$BRIDGE" > SHA256SUMS.txt)
ARCHIVE="$OUTPUT/Kaleidoscope-$VERSION-linux-x64-portable.tar.gz"
tar -C "$OUTPUT" -czf "$ARCHIVE" "Kaleidoscope-$VERSION-linux-x64"
echo "Package: $STAGE"
echo "Archive: $ARCHIVE"
