[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$directory = Split-Path -Parent $PSCommandPath
$package = Join-Path $directory 'Hane.ShellIntegration.msix'
$certificateFile = Join-Path $directory 'Hane.ShellIntegration.cer'
$exe = Join-Path $directory 'hane.exe'
# The Explorer DLL lives at shell-integration\<version>\hane_shell_extension.dll
# so an updated build never replaces a DLL that Explorer still has loaded.
$extensionRoot = Join-Path $directory 'shell-integration'
$extensions = @()
if (Test-Path -LiteralPath $extensionRoot -PathType Container) {
    $extensions = @(Get-ChildItem -LiteralPath $extensionRoot -Directory |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' } |
        ForEach-Object { Join-Path $_.FullName 'hane_shell_extension.dll' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
}
if ($extensions.Count -eq 0) {
    throw "Hane's Windows Explorer bundle is incomplete: $extensionRoot\<version>\hane_shell_extension.dll"
}

foreach ($required in @($package, $certificateFile, $exe)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Hane's Windows Explorer bundle is incomplete: $required"
    }
}

$certificate = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($certificateFile)
if ($certificate.Subject -ne 'CN=Hane Local Test 2026' -or
    $certificate.Issuer -ne $certificate.Subject -or
    $certificate.NotBefore -gt (Get-Date) -or
    $certificate.NotAfter -le (Get-Date) -or
    -not ($certificate.EnhancedKeyUsageList | Where-Object ObjectId -eq '1.3.6.1.5.5.7.3.3')) {
    throw 'The bundled certificate is not a valid Hane code-signing certificate. Nothing was trusted.'
}
$signature = Get-AuthenticodeSignature -LiteralPath $package
if (-not $signature.SignerCertificate -or
    $signature.SignerCertificate.Thumbprint -ne $certificate.Thumbprint) {
    throw 'The MSIX signing certificate does not match the bundled public certificate. Nothing was trusted.'
}

$storePath = "Cert:\LocalMachine\TrustedPeople\$($certificate.Thumbprint)"
if (Test-Path -LiteralPath $storePath) {
    if ($signature.Status -ne 'Valid') {
        throw "The bundled MSIX signature is not valid: $($signature.Status)"
    }
    Write-Output "Hane's MSIX signing certificate is already trusted: $($certificate.Thumbprint)"
    return
}

$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script from an elevated PowerShell window to trust the bundled public certificate. No private key is included.'
}

try {
    Import-Certificate -FilePath $certificateFile -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    $verified = Get-AuthenticodeSignature -LiteralPath $package
    if ($verified.Status -ne 'Valid') {
        throw "The MSIX signature is still not valid after trusting its certificate: $($verified.Status)"
    }
}
catch {
    Remove-Item -LiteralPath $storePath -ErrorAction SilentlyContinue
    throw
}
Write-Output "Trusted Hane's MSIX signing certificate: $($certificate.Thumbprint)"
Write-Output 'Start Hane and enable the file context menu in Settings > General > Windows integration.'
