# Downloads w64devkit and places it in Installer\payload\toolchain
$ErrorActionPreference = "Stop"
$ver  = "2.10.0"
# SHA256 of w64devkit-x64-2.10.0.7z.exe as published by GitHub for the release asset.
# Change it together with $ver; the downloaded file is never run on a mismatch.
$sha256 = "18d0a4c71a166f8401ab6305781bec5882b40b5e06ba9807c61cb5f3b3c6325e"
$dest = Join-Path $PSScriptRoot "..\payload\toolchain"
$sfx  = Join-Path $env:TEMP "w64devkit-x64-$ver.7z.exe"
$work = Join-Path $env:TEMP "w64devkit-extract"

$url = "https://github.com/skeeto/w64devkit/releases/download/v$ver/w64devkit-x64-$ver.7z.exe"
Write-Host "Downloading $url ..."
Invoke-WebRequest $url -OutFile $sfx

$actual = (Get-FileHash -LiteralPath $sfx -Algorithm SHA256).Hash
if ($actual -ne $sha256) {
    Remove-Item -LiteralPath $sfx -Force
    throw "SHA256 mismatch for $url (expected $sha256, got $actual); the download was deleted"
}

Remove-Item $work, $dest -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null

# Self-extracting 7z: -o<dir> sets output folder, -y answers yes to prompts
Write-Host "Extracting..."
& $sfx "-o$work" -y | Out-Null
if ($LASTEXITCODE) { throw "Extraction failed (exit code $LASTEXITCODE)" }

# The archive has a root folder (w64devkit/); its contents are the toolchain
New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
Move-Item (Join-Path $work "w64devkit") $dest

if (-not (Test-Path "$dest\bin\gcc.exe")) {
    throw "bin\gcc.exe is missing, unexpected folder structure"
}
Write-Host "Toolchain ready at $dest"