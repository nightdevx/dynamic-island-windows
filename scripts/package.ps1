# Builds both installers: dist\DynamicIsland-Setup-<version>.exe, and with the Razer Barracuda X
# headset battery, dist\DynamicIsland-Barracuda-Setup-<version>.exe.
# Needs Inno Setup 6 (https://jrsoftware.org/isinfo.php).
# Usage: powershell -File scripts\package.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
Push-Location $root
try {
    $iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 not found' }
    $target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
    $exe = Join-Path $target 'release\island.exe'
    foreach ($build in @(@{ Name = 'DynamicIsland-Setup'; Features = @() }, @{ Name = 'DynamicIsland-Barracuda-Setup'; Features = @('--features', 'headset') })) {
        cargo build --release @($build.Features)
        if ($LASTEXITCODE) { throw 'cargo build failed' }
        $version = (Get-Item $exe).VersionInfo.ProductVersion
        & $iscc /Q "/DExe=$exe" "/F$($build.Name)-$version" installer\island.iss
        if ($LASTEXITCODE) { throw 'ISCC failed' }
        Get-Item "dist\$($build.Name)-$version.exe" | ForEach-Object { "{0} ({1:N0} KB)" -f $_.FullName, ($_.Length / 1KB) }
    }
} finally {
    Pop-Location
}
