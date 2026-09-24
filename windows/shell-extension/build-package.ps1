[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExePath,
    [ValidateSet('x64', 'arm64')][string]$Architecture = 'arm64',
    [string]$CertificateThumbprint,
    [switch]$Overwrite
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$exe = (Resolve-Path -LiteralPath $ExePath).Path
$output = Split-Path -Parent $exe
$source = Join-Path $PSScriptRoot 'hane_shell_extension.cpp'
$definition = Join-Path $PSScriptRoot 'hane_shell_extension.def'
$stage = Join-Path $output 'hane-shell-package-staging'
$package = Join-Path $output 'Hane.ShellIntegration.msix'
$verifiedOutput = [System.IO.Path]::GetFullPath($output).TrimEnd('\')
$verifiedStage = [System.IO.Path]::GetFullPath($stage)
if ($verifiedStage -ne (Join-Path $verifiedOutput 'hane-shell-package-staging')) {
    throw 'Refusing to use a staging directory outside the requested output directory.'
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    throw "Visual Studio Installer vswhere.exe was not found: $vswhere"
}
$component = if ($Architecture -eq 'x64') {
    'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'
} else {
    'Microsoft.VisualStudio.Component.VC.Tools.ARM64'
}
$installation = & $vswhere -latest -products '*' -requires $component -property installationPath
if (-not $installation) {
    throw "Visual Studio C++ tools for $Architecture were not found."
}
$vsvars = Join-Path ($installation | Select-Object -First 1) 'VC\Auxiliary\Build\vcvarsall.bat'
if (-not (Test-Path -LiteralPath $vsvars)) {
    throw "Visual Studio vcvarsall.bat was not found: $vsvars"
}
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$sdk = Get-ChildItem -LiteralPath $sdkRoot -Directory |
    Where-Object { $_.Name -match '^10\.\d+\.\d+\.\d+$' } |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $sdk) { throw "Windows SDK tools were not found: $sdkRoot" }
$makeappx = Join-Path $sdk.FullName "$Architecture\makeappx.exe"
$signtool = Join-Path $sdk.FullName "$Architecture\signtool.exe"
if (-not (Test-Path -LiteralPath $makeappx)) { throw "Missing makeappx.exe: $makeappx" }
if (-not (Test-Path -LiteralPath $signtool)) { throw "Missing signtool.exe: $signtool" }

$certificate = $null
if ($CertificateThumbprint) {
    $certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$CertificateThumbprint" -ErrorAction Stop
    if (-not $certificate.HasPrivateKey) { throw 'Signing certificate has no private key.' }
}
$publisher = if ($certificate) { $certificate.Subject } else { 'CN=Hane Development' }
$cargoToml = [System.IO.File]::ReadAllText((Join-Path $repo 'Cargo.toml'))
$versionMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"')
if (-not $versionMatch.Success) { throw 'Could not read the workspace package version.' }
$packageVersion = "$($versionMatch.Groups[1].Value).0"

# Compile as the Explorer process architecture. Package identity is distinct
# from the legacy HKCU switches; building does not register anything.
$dll = Join-Path $output 'hane_shell_extension.dll'
$object = Join-Path $output 'hane_shell_extension.obj'
$importLibrary = Join-Path $output 'hane_shell_extension.lib'
$compile = 'call "{0}" {1} >nul && cl.exe /nologo /std:c++17 /utf-8 /EHsc /MT /LD /Fo:"{2}" "{3}" /link /DEF:"{4}" /OUT:"{5}" /IMPLIB:"{6}" shlwapi.lib shell32.lib ole32.lib advapi32.lib' -f $vsvars, $Architecture, $object, $source, $definition, $dll, $importLibrary
& cmd.exe /d /s /c $compile
if ($LASTEXITCODE -ne 0) { throw "Shell extension compilation failed: $LASTEXITCODE" }

if (Test-Path -LiteralPath $stage) { throw "Staging directory already exists: $stage" }
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    $assetDirectory = Join-Path $output 'Assets'
    if (-not (Test-Path -LiteralPath $assetDirectory)) {
        New-Item -ItemType Directory -Path $assetDirectory | Out-Null
    }
    $logo = Join-Path $assetDirectory 'HaneShellIntegration.png'
    if ((Test-Path -LiteralPath $logo) -and -not $Overwrite) {
        throw "Logo already exists: $logo (use -Overwrite for a logo built by this script)"
    }
    Add-Type -AssemblyName System.Drawing
    $icon = [System.Drawing.Icon]::new((Join-Path $repo 'assets\app-icon.ico'))
    try {
        $bitmap = $icon.ToBitmap()
        try { $bitmap.Save($logo, [System.Drawing.Imaging.ImageFormat]::Png) }
        finally { $bitmap.Dispose() }
    }
    finally { $icon.Dispose() }
    $manifest = [System.IO.File]::ReadAllText((Join-Path $PSScriptRoot 'AppxManifest.xml.in'))
    $manifest = $manifest.Replace('@PUBLISHER@', [System.Security.SecurityElement]::Escape($publisher)).Replace('@VERSION@', $packageVersion)
    [System.IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, [System.Text.UTF8Encoding]::new($false))
    if ((Test-Path -LiteralPath $package) -and -not $Overwrite) {
        throw "Package already exists: $package (use -Overwrite for a package built by this script)"
    }
    # Sparse packages intentionally keep hane.exe at ExternalLocation.
    $makeappxArgs = @('pack', '/d', $stage, '/p', $package, '/nv')
    if ($Overwrite) { $makeappxArgs += '/o' }
    & $makeappx @makeappxArgs
    if ($LASTEXITCODE -ne 0) { throw "makeappx failed: $LASTEXITCODE" }
    if ($certificate) {
        & $signtool sign /fd SHA256 /sha1 $certificate.Thumbprint /s My $package
        if ($LASTEXITCODE -ne 0) { throw "signtool failed: $LASTEXITCODE" }
    }
    else {
        Write-Warning 'Package is unsigned and cannot be installed. Pass -CertificateThumbprint for a trusted code-signing certificate.'
    }
    Write-Output $package
}
finally {
    if (Test-Path -LiteralPath $verifiedStage) {
        Remove-Item -LiteralPath $verifiedStage -Recurse -Force
    }
}
