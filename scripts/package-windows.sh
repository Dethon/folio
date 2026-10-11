#!/usr/bin/env bash
# Cross-compiles the Windows release from Linux/WSL and packages it two ways:
#
#   dist/folio-windows-x64/        the files, runnable from anywhere
#   dist/Folio-<version>-setup.exe the installer
#
# Requires `cargo install cargo-xwin`, and NSIS for the installer
# (`apt install nsis`). On Debian/Ubuntu without root, NSIS is unpacked into
# target/nsis instead. Set PUBLISHER to name a publisher in the installer.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET=x86_64-pc-windows-msvc
OUT="$ROOT/dist/folio-windows-x64"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

# Prints the makensis command to use, fetching NSIS locally if need be.
find_makensis() {
    if command -v makensis >/dev/null; then
        echo makensis
        return
    fi

    local local_nsis="$ROOT/target/nsis"

    if [[ ! -x "$local_nsis/usr/bin/makensis" ]] && command -v apt-get >/dev/null; then
        echo "makensis not found; unpacking NSIS into target/nsis" >&2
        local debs
        debs="$(mktemp -d)"
        (cd "$debs" && apt-get download nsis nsis-common >/dev/null)
        mkdir -p "$local_nsis"
        for deb in "$debs"/*.deb; do
            dpkg -x "$deb" "$local_nsis"
        done
        rm -rf "$debs"
    fi

    if [[ -x "$local_nsis/usr/bin/makensis" ]]; then
        echo "env NSISDIR=$local_nsis/usr/share/nsis $local_nsis/usr/bin/makensis"
        return
    fi

    return 1
}

"$ROOT/scripts/fetch-pdfium.sh"
(cd "$ROOT" && cargo xwin build --release --target "$TARGET")

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$ROOT/target/$TARGET/release/folio.exe" "$OUT/"
cp "$ROOT/vendor/pdfium/$TARGET/pdfium.dll" "$OUT/"
cp "$ROOT/LICENSE" "$OUT/LICENSE.txt"
cp "$ROOT/vendor/pdfium/$TARGET/LICENSE.pdfium" "$OUT/"
cp "$ROOT/assets/icons/LICENSE" "$OUT/LICENSE.lucide"

echo "Packaged $OUT"

if ! MAKENSIS="$(find_makensis)"; then
    echo "error: NSIS (makensis) is needed to build the installer: apt install nsis" >&2
    exit 1
fi

SETUP="$ROOT/dist/Folio-$VERSION-setup.exe"

# shellcheck disable=SC2086  # MAKENSIS may carry an environment prefix.
$MAKENSIS -V2 -INPUTCHARSET UTF8 \
    -DVERSION="$VERSION" \
    -DDIST="$OUT" \
    -DICON="$ROOT/assets/icon/folio.ico" \
    -DOUTFILE="$SETUP" \
    ${PUBLISHER:+"-DPUBLISHER=$PUBLISHER"} \
    "$ROOT/installer/folio.nsi"

echo "Built $SETUP"
ls -lh "$SETUP" "$OUT"
