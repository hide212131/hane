# Builds the isolated update validation binaries for Windows (Issue #510).
#
# Opt-in only: the product Release path (.github/workflows/windows-release.yml)
# never uses this script. Both binaries carry the validation-feed feature, so the
# app uses the loopback feed and the dedicated credential namespace, and the
# helper reports the `validation-feed` profile in its identity record, which
# `release_catalog.py verify-windows-zip --validation-feed` requires.
#
# Usage: scripts/build-windows-validation.ps1 -Version <x.y.z> -SourceSha <40 hex>
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$SourceSha
)
$ErrorActionPreference = 'Stop'

if ($SourceSha -notmatch '^[0-9a-f]{40}$') {
    throw 'SourceSha must be 40 lowercase hex digits.'
}

$env:HANE_SOURCE_SHA = $SourceSha
$Target = 'x86_64-pc-windows-msvc'
cargo build --release --locked --target $Target -p hane -p hane-updater `
    --features hane/validation-feed,hane-updater/validation-feed
if ($LASTEXITCODE -ne 0) { throw 'The validation-feed build failed.' }

# With an explicit --target the artifacts are under target/<triple>/release.
$OutDir = "target/$Target/release"

python scripts/release_catalog.py binary-identity `
    --binary "$OutDir/hane-updater.exe" `
    --version $Version --target $Target `
    --source-sha $SourceSha --validation-feed
if ($LASTEXITCODE -ne 0) { throw 'The helper does not report the validation-feed profile.' }

python scripts/release_catalog.py app-profile `
    --binary "$OutDir/hane.exe" `
    --version $Version --validation-feed
if ($LASTEXITCODE -ne 0) { throw 'The app does not report the validation-feed profile.' }
