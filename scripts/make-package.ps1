# Builds the signed identity package (package\dist\ikoPDF.msix) and exports the public
# certificate (package\dist\ikoPDF.cer). Both get embedded into ikoPDF.exe at build time.
#
# The signing key is a self-signed certificate "CN=ikoPDF" kept in this Windows account's
# personal certificate store (Cert:\CurrentUser\My). It is created on first run and never
# written to a file. Run this again only when package\AppxManifest.xml changes.
# (ASCII only on purpose: Windows PowerShell 5.1 reads BOM-less scripts as ANSI.)
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$package = Join-Path $root 'package'
$dist = Join-Path $package 'dist'
$subject = 'CN=ikoPDF'

function Find-SdkTool([string]$name) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $tool = Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.' } | Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\$name" } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $tool) { throw "$name not found - install the Windows SDK (Visual Studio Build Tools)" }
    return $tool
}
$makeappx = Find-SdkTool 'makeappx.exe'
$signtool = Find-SdkTool 'signtool.exe'

# 1. Signing certificate (code signing, 10 years)
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate -Type Custom -Subject $subject -FriendlyName 'ikoPDF package signing' `
        -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 `
        -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(10) `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
    "created signing certificate $($cert.Thumbprint)"
} else {
    "using signing certificate $($cert.Thumbprint) (valid until $($cert.NotAfter.ToString('yyyy-MM-dd')))"
}

# 2. Assets + staging folder (manifest and logos only; the binaries live in the install folder)
if (-not (Test-Path (Join-Path $package 'Assets\menu.ico'))) { & (Join-Path $PSScriptRoot 'make-assets.ps1') | Out-Null }
$stage = Join-Path $root 'target\package-stage'
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, (Join-Path $stage 'Assets'), $dist | Out-Null
Copy-Item (Join-Path $package 'AppxManifest.xml') $stage
Copy-Item (Join-Path $package 'Assets\*.png') (Join-Path $stage 'Assets')

# 3. Pack and sign
$msix = Join-Path $dist 'ikoPDF.msix'
& $makeappx pack /o /d $stage /nv /p $msix | Select-Object -Last 2
if ($LASTEXITCODE -ne 0) { throw "makeappx failed ($LASTEXITCODE)" }
& $signtool sign /fd SHA256 /sha1 $cert.Thumbprint $msix | Select-Object -Last 2
if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }

# 4. Public certificate (no private key)
Export-Certificate -Cert $cert -FilePath (Join-Path $dist 'ikoPDF.cer') -Type CERT | Out-Null

Get-ChildItem $dist | Select-Object Name, Length
