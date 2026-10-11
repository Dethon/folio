# Downloads the prebuilt PDFium library Folio links against at runtime.
# The version must match the `pdfium_*` feature selected in Cargo.toml.
$ErrorActionPreference = "Stop"

$version = if ($env:PDFIUM_VERSION) { $env:PDFIUM_VERSION } else { "7881" }
$target = "x86_64-pc-windows-msvc"
$dest = Join-Path $PSScriptRoot "..\vendor\pdfium\$target"

if (Test-Path (Join-Path $dest "pdfium.dll")) {
    Write-Host "pdfium ${target}: already present"
    exit 0
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) "pdfium-$version"
New-Item -ItemType Directory -Force -Path $tmp, $dest | Out-Null

Write-Host "pdfium ${target}: downloading ($version)"
Invoke-WebRequest "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F$version/pdfium-win-x64.tgz" -OutFile "$tmp\pdfium.tgz"
tar -xzf "$tmp\pdfium.tgz" -C $tmp

Copy-Item "$tmp\bin\pdfium.dll" $dest
Copy-Item "$tmp\LICENSE" (Join-Path $dest "LICENSE.pdfium")
Remove-Item -Recurse -Force $tmp
