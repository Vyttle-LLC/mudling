# Packages a built mudling-desktop.exe as target\dist\MUDling-<version>-windows-<arch>-setup.exe,
# the per-user installer, and MUDling-<version>-windows-<arch>.zip. Needs Inno Setup 6.
#
#   scripts\package-windows.ps1 -Target aarch64-pc-windows-msvc -Arch arm64
#
# Release builds set MUDLING_VERSION; local builds take the workspace's version.
# It signs nothing: the release workflow signs the app before and the installer after.
param(
    [Parameter(Mandatory)] [string] $Target,
    [Parameter(Mandatory)] [ValidateSet('x64', 'arm64')] [string] $Arch
)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$version = $env:MUDLING_VERSION
if (-not $version) {
    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches.Groups[1].Value
}
$exe = "target\$Target\release\mudling-desktop.exe"
$dist = "target\dist"
$name = "MUDling-$version-windows-$Arch"
Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$dist\$name" | Out-Null

Copy-Item $exe "$dist\$name\MUDling.exe"
Copy-Item LICENSE "$dist\$name\"
Compress-Archive -Path "$dist\$name" -DestinationPath "$dist\$name.zip"

$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
$defines = @(
    "/DAppVersion=$version", "/DArch=$Arch",
    "/DSourceExe=$root\$exe", "/DOutputDir=$root\$dist"
)
& $iscc @defines scripts\mudling.iss
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed' }

Remove-Item -Recurse -Force "$dist\$name"
Get-ChildItem $dist
