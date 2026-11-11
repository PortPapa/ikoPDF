# Builds ikoPDF.exe (menu DLL first, because the exe embeds it) and publishes it as a
# GitHub release together with version.txt, which installed copies read to find updates.
#
#   scripts\release.ps1               build + publish release v<version from Cargo.toml>
#   scripts\release.ps1 -SkipPublish  build only
#
# Requires: Rust (MSVC), package\dist\ikoPDF.msix (scripts\make-package.ps1), GitHub CLI (gh).
# (ASCII only on purpose: Windows PowerShell 5.1 reads BOM-less scripts as ANSI.)
param(
    [switch]$SkipPublish,
    [string]$Repository = 'PortPapa/ikoPDF',
    [string]$Notes = ''
)
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$cargoToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'version not found in Cargo.toml' }
$version = $Matches[1]

foreach ($f in 'package\dist\ikoPDF.msix', 'package\dist\ikoPDF.cer') {
    if (-not (Test-Path (Join-Path $root $f))) { throw "$f is missing - run scripts\make-package.ps1 first" }
}

Push-Location $root
# cargo and gh print progress on stderr; don't let that count as a script error.
$ErrorActionPreference = 'Continue'
try {
    cargo build --release -p ikopdf_shell
    if ($LASTEXITCODE -ne 0) { throw "building the menu DLL failed ($LASTEXITCODE)" }
    cargo build --release -p ikopdf
    if ($LASTEXITCODE -ne 0) { throw "building ikoPDF.exe failed ($LASTEXITCODE)" }
} finally { Pop-Location }
$ErrorActionPreference = 'Stop'

$exe = Join-Path $root 'target\release\ikoPDF.exe'
$assets = Join-Path $root 'target\release-assets'
New-Item -ItemType Directory -Force $assets | Out-Null
Copy-Item $exe (Join-Path $assets 'ikoPDF.exe') -Force
[System.IO.File]::WriteAllText((Join-Path $assets 'version.txt'), $version)
"built ikoPDF.exe v$version ({0:N0} bytes)" -f (Get-Item $exe).Length

if ($SkipPublish) { return }

if (-not $Notes) { $Notes = "ikoPDF v$version" }
$ErrorActionPreference = 'Continue'
gh release create "v$version" (Join-Path $assets 'ikoPDF.exe') (Join-Path $assets 'version.txt') `
    --repo $Repository --title "v$version" --notes $Notes --latest
if ($LASTEXITCODE -ne 0) { throw "gh release create failed ($LASTEXITCODE)" }
"published https://github.com/$Repository/releases/tag/v$version"
