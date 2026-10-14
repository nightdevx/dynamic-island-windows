# Long-run leak check: every few minutes, appends the island's memory, handles, threads and
# GDI/USER objects to a CSV. A flat line over a day means nothing leaks.
# Usage: powershell -File scripts\soak.ps1 [-Hours 24] [-EveryMinutes 5]
param([double]$Hours = 24, [int]$EveryMinutes = 5, [string]$Out = "$env:LOCALAPPDATA\DynamicIsland\soak.csv")

Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class Gui { [DllImport("user32.dll")] public static extern uint GetGuiResources(IntPtr process, uint flags); }'

if (-not (Test-Path $Out)) { 'time,pid,private_mb,working_set_mb,handles,threads,gdi,user,cpu_s' | Out-File $Out -Encoding utf8 }
$end = (Get-Date).AddHours($Hours)
while ((Get-Date) -lt $end) {
    $p = Get-Process -Name island -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($p) {
        # Invariant culture: a Turkish decimal comma would break the CSV.
        $line = [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0},{1},{2:F2},{3:F2},{4},{5},{6},{7},{8:F2}',
            (Get-Date -Format s), $p.Id, ($p.PrivateMemorySize64 / 1MB), ($p.WorkingSet64 / 1MB), $p.HandleCount, $p.Threads.Count,
            [Gui]::GetGuiResources($p.Handle, 0), [Gui]::GetGuiResources($p.Handle, 1), $p.TotalProcessorTime.TotalSeconds)
        $line | Out-File $Out -Append -Encoding utf8
    } else {
        "$(Get-Date -Format s),,,,,,,," | Out-File $Out -Append -Encoding utf8
    }
    Start-Sleep -Seconds ($EveryMinutes * 60)
}
