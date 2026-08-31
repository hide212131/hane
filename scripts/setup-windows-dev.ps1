[CmdletBinding()]
param(
    [switch]$Install,
    [switch]$Verify,
    [ValidateSet("auto", "x64", "arm64")]
    [string]$Architecture = "auto"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$toolchain = "1.93.1"
$resolvedArchitecture = if ($Architecture -eq "auto") {
    switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
        "X64" { "x64" }
        "Arm64" { "arm64" }
        default { throw "Unsupported Windows architecture: $_. Use -Architecture x64 or arm64." }
    }
}
else {
    $Architecture
}
$target = switch ($resolvedArchitecture) {
    "x64" { "x86_64-pc-windows-msvc" }
    "arm64" { "aarch64-pc-windows-msvc" }
}
$rustToolchain = "$toolchain-$target"
$cargoTargetDir = Join-Path $workspace "target\windows-$resolvedArchitecture-$toolchain"
$vcToolsComponent = switch ($resolvedArchitecture) {
    "x64" { "Microsoft.VisualStudio.Component.VC.Tools.x86.x64" }
    "arm64" { "Microsoft.VisualStudio.Component.VC.Tools.ARM64" }
}
$clCandidates = switch ($resolvedArchitecture) {
    "x64" {
        "C:\BuildTools\VC\Tools\MSVC\*\bin\Hostx64\x64\cl.exe"
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\*\bin\Hostx64\x64\cl.exe"
    }
    "arm64" {
        "C:\BuildTools\VC\Tools\MSVC\*\bin\HostARM64\arm64\cl.exe"
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\*\bin\HostARM64\arm64\cl.exe"
    }
}

function Invoke-Cargo([string[]]$Arguments) {
    & rustup run $rustToolchain cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed (exit code $LASTEXITCODE)."
    }
}

function Invoke-Rustup([string[]]$Arguments) {
    & rustup @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "rustup $($Arguments -join ' ') failed (exit code $LASTEXITCODE)."
    }
}

function Require-Command([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found. Re-run with -Install or install it first."
    }
}

function Install-WingetPackage([string]$Id, [string]$Scope = "", [string]$Override = "") {
    Write-Host "Installing $Id..."
    $arguments = @(
        "install", "--id", $Id, "--exact", "--silent",
        "--accept-source-agreements", "--accept-package-agreements"
    )
    if ($Scope) {
        $arguments += @("--scope", $Scope)
    }
    if ($Override) {
        $arguments += @("--override", $Override)
    }
    & winget @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "winget failed to install $Id (exit code $LASTEXITCODE)."
    }
}

function Import-VsEnvironment {
    $vswherePath = (Get-Command vswhere.exe -ErrorAction SilentlyContinue).Source
    if (-not $vswherePath -and (Test-Path -LiteralPath "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe")) {
        $vswherePath = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
    }
    if (-not $vswherePath) {
        return
    }

    $installation = & $vswherePath -latest -products * -requires $vcToolsComponent -property installationPath
    if (-not $installation) {
        return
    }

    $vcvars = Join-Path $installation "VC\Auxiliary\Build\vcvarsall.bat"
    if (-not (Test-Path -LiteralPath $vcvars)) {
        return
    }

    Write-Host "Loading MSVC environment from $vcvars ($resolvedArchitecture)..."
    $lines = cmd.exe /d /c "call `"$vcvars`" $resolvedArchitecture >nul && set"
    foreach ($line in $lines) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
        }
    }
}

if ($Install) {
    Require-Command winget
    $llvmIsInstalled = (Get-Command clang.exe -ErrorAction SilentlyContinue) -or
        (Test-Path -LiteralPath (Join-Path ${env:ProgramFiles} "LLVM\bin\clang.exe"))
    if (-not $llvmIsInstalled) {
        # LLVM's Windows installer is machine-scoped; winget may request UAC.
        Install-WingetPackage "LLVM.LLVM" "machine"
    }
    $cl = Get-ChildItem $clCandidates -ErrorAction SilentlyContinue
    if (-not $cl) {
        Install-WingetPackage "Microsoft.VisualStudio.2022.BuildTools" "" "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    }
}

# winget-installed programs are not necessarily visible in the current
# PowerShell process, so add the conventional LLVM locations explicitly.
$llvmCandidates = @(
    (Join-Path ${env:ProgramFiles} "LLVM\bin"),
    (Join-Path ${env:ProgramFiles(x86)} "LLVM\bin"),
    (Join-Path ${env:LOCALAPPDATA} "Programs\LLVM\bin")
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
foreach ($directory in $llvmCandidates) {
    if ($env:Path -notlike "*$directory*") {
        $env:Path = "$directory;$env:Path"
    }
}

Import-VsEnvironment
Require-Command rustup
Require-Command clang
Require-Command clang-cl
Require-Command link

Invoke-Rustup @("toolchain", "install", $toolchain, "--target", $target, "--profile", "minimal")
Invoke-Rustup @("component", "add", "clippy", "rustfmt", "--toolchain", $rustToolchain)
$toolchainRustc = & rustup which --toolchain $rustToolchain rustc
if ($LASTEXITCODE -ne 0) {
    throw "Unable to locate rustc for toolchain $rustToolchain."
}
$toolchainBin = Split-Path -Parent $toolchainRustc
if ($env:Path -notlike "$toolchainBin;*") {
    # Cargo subcommands such as cargo-clippy and cargo-fmt are resolved through
    # PATH, so put the selected toolchain ahead of any system Rust install.
    $env:Path = "$toolchainBin;$env:Path"
}
$env:CARGO_TARGET_DIR = $cargoTargetDir

$clang = (Get-Command clang.exe).Source
$clangCl = (Get-Command clang-cl.exe).Source
# Use the MSVC-compatible driver for every C dependency. Plain clang can
# compile ring, but makes libgit2-sys expose POSIX symbols at MSVC link time.
$env:CC = $clangCl
$env:CXX = $clangCl

Write-Host "Windows Hane toolchain is ready."
Write-Host "  Target: $target"
Write-Host "  Rust:   $(& rustup run $rustToolchain rustc --version)"
Write-Host "  clang:  $(& clang --version | Select-Object -First 1)"
Write-Host "  linker: $((Get-Command link).Source)"

if ($Verify) {
    Push-Location $workspace
    try {
        Invoke-Cargo @("test", "--workspace", "--locked")
        Invoke-Cargo @("fmt", "--all", "--", "--check")
        # The repository currently has known clippy warnings; report them
        # without preventing the local Windows binary from being produced.
        Invoke-Cargo @("clippy", "--workspace", "--all-targets", "--all-features", "--locked")
        Invoke-Cargo @("build", "--release", "--locked", "-p", "hane")
        Write-Host "Built: $(Join-Path $cargoTargetDir 'release\hane.exe')"
    }
    finally {
        Pop-Location
    }
}
