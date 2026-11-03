# Installs or replaces MUDling for the current user with a published release, the latest by default:
#
#   irm https://raw.githubusercontent.com/Vyttle-LLC/mudling/main/scripts/install.ps1 | iex
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Vyttle-LLC/mudling/main/scripts/install.ps1))) v1.2.3
#
# It runs the release's installer silently. MUDling's data folder is never touched.
param([string] $Tag)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'Vyttle-LLC/mudling'
if (-not $Tag) {
    $Tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
}
if ($Tag -notmatch '^v\d+\.\d+\.\d+$') { throw "No release found (got '$Tag')." }
$version = $Tag.Substring(1)

# The OS's architecture, not this PowerShell's, which may run emulated.
$arch = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    'Arm64' { 'arm64' }
    'X64' { 'x64' }
    default { throw "No Windows build for $_." }
}
$installer = "MUDling-$version-windows-$arch-setup.exe"
$path = Join-Path ([IO.Path]::GetTempPath()) $installer

Write-Host "Downloading MUDling $version..."
Invoke-WebRequest "https://github.com/$repo/releases/download/$Tag/$installer" -OutFile $path
try {
    # The installer closes a running MUDling itself.
    $setup = Start-Process $path -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    if ($setup.ExitCode -ne 0) { throw "The installer failed (exit code $($setup.ExitCode))." }
} finally {
    Remove-Item $path -ErrorAction SilentlyContinue
}
Write-Host "Installed MUDling $version. Find it in the Start menu."
