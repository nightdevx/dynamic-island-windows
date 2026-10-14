# Builds the release exe and the installer: dist\DynamicIsland-Setup-<version>.exe.
# Needs Inno Setup 6 (https://jrsoftware.org/isinfo.php).
# Usage: powershell -File scripts\package.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE) { throw 'cargo build failed' }
    $target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
    $exe = Join-Path $target 'release\island.exe'
    $iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 not found' }
    & $iscc /Q "/DExe=$exe" installer\island.iss
    if ($LASTEXITCODE) { throw 'ISCC failed' }
    Get-ChildItem dist\*.exe | Sort-Object LastWriteTime | Select-Object -Last 1 | ForEach-Object { "{0} ({1:N0} KB)" -f $_.FullName, ($_.Length / 1KB) }
} finally {
    Pop-Location
}
