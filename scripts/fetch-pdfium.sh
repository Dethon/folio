#!/usr/bin/env bash
# Downloads the prebuilt PDFium libraries Folio links against at runtime.
# The version must match the `pdfium_*` feature selected in Cargo.toml.
set -euo pipefail

VERSION="${PDFIUM_VERSION:-7881}"
BASE="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${VERSION}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)/vendor/pdfium"

fetch() {
    local asset="$1" dir="$2" lib="$3"
    if [[ -f "$ROOT/$dir/$(basename "$lib")" ]]; then
        echo "pdfium $dir: already present"
        return
    fi
    local tmp
    tmp="$(mktemp -d)"
    echo "pdfium $dir: downloading $asset ($VERSION)"
    curl -fsSL "$BASE/$asset.tgz" -o "$tmp/pdfium.tgz"
    tar -xzf "$tmp/pdfium.tgz" -C "$tmp"
    mkdir -p "$ROOT/$dir"
    cp "$tmp/$lib" "$ROOT/$dir/"
    cp "$tmp/LICENSE" "$ROOT/$dir/LICENSE.pdfium"
    rm -rf "$tmp"
}

fetch pdfium-win-x64 x86_64-pc-windows-msvc bin/pdfium.dll
fetch pdfium-linux-x64 x86_64-unknown-linux-gnu lib/libpdfium.so
