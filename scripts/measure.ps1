# Measures the island process over an interval: memory, CPU cycles, wakeups (context switches) and GPU engine use.
# Usage: powershell -File scripts\measure.ps1 [-Name island] [-Seconds 60]
param([string]$Name = 'island', [int]$Seconds = 60)

$p = Get-Process -Name $Name -ErrorAction Stop | Select-Object -First 1

# Exact per-process cycle and context switch counts. Thread perf counters are localized,
# slow with thousands of threads, and miss short intervals, so read the kernel snapshot.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Snap {
    [DllImport("ntdll.dll")] static extern int NtQuerySystemInformation(int cls, IntPtr buf, int len, out int ret);
    // x64 SYSTEM_PROCESS_INFORMATION: threads at +4, CycleTime at +24, UniqueProcessId at +80,
    // thread array at +256; SYSTEM_THREAD_INFORMATION is 80 bytes, UniqueThread at +48,
    // ContextSwitches at +64.
    // Returns the process cycle count and a map of thread id to context switch count.
    public static System.Collections.Generic.Dictionary<long, ulong> Read(int pid, out ulong cycles) {
        cycles = 0;
        int len = 1 << 20;
        while (true) {
            IntPtr buf = Marshal.AllocHGlobal(len);
            try {
                int ret;
                int status = NtQuerySystemInformation(5, buf, len, out ret);
                if (status == unchecked((int)0xC0000004)) { len = Math.Max(len * 2, ret + 65536); continue; }
                if (status != 0) throw new Exception("NtQuerySystemInformation 0x" + status.ToString("X"));
                for (IntPtr e = buf; ; ) {
                    if ((long)Marshal.ReadIntPtr(e, 80) == pid) {
                        var map = new System.Collections.Generic.Dictionary<long, ulong>();
                        int threads = Marshal.ReadInt32(e, 4);
                        for (int i = 0; i < threads; i++)
                            map[(long)Marshal.ReadIntPtr(e, 256 + i * 80 + 48)] = (uint)Marshal.ReadInt32(e, 256 + i * 80 + 64);
                        cycles = (ulong)Marshal.ReadInt64(e, 24);
                        return map;
                    }
                    int next = Marshal.ReadInt32(e, 0);
                    if (next == 0) return null;
                    e = new IntPtr(e.ToInt64() + next);
                }
            } finally { Marshal.FreeHGlobal(buf); }
        }
    }
}
'@

# GPU Engine is a v2 provider and keeps its English name on every locale.
$gpu = "\GPU Engine(pid_$($p.Id)_*)\Utilization Percentage"
$interval = [Math]::Min($Seconds, 5)
$samples = [Math]::Max(1, [int]($Seconds / $interval))

$c0 = [uint64]0; $c1 = [uint64]0
$before = [Snap]::Read($p.Id, [ref]$c0)
$watch = [Diagnostics.Stopwatch]::StartNew()
$data = Get-Counter -Counter $gpu -SampleInterval $interval -MaxSamples $samples -ErrorAction SilentlyContinue
$after = [Snap]::Read($p.Id, [ref]$c1)
$elapsed = $watch.Elapsed.TotalSeconds
$p.Refresh()
# Threads that started during the interval count from zero; threads that exited are lost.
$switches = 0
foreach ($tid in $after.Keys) { $switches += $after[$tid] - $(if ($before.ContainsKey($tid)) { $before[$tid] } else { 0 }) }

$gpuByAdapter = @{}
foreach ($s in $data) {
    foreach ($c in $s.CounterSamples) {
        $luid = if ($c.InstanceName -match 'luid_(0x[0-9a-f]+_0x[0-9a-f]+)') { $Matches[1] } else { '?' }
        $gpuByAdapter[$luid] = [Math]::Max([double]$gpuByAdapter[$luid], $c.CookedValue)
    }
}

[pscustomobject]@{
    Pid               = $p.Id
    Seconds           = [Math]::Round($elapsed, 1)
    PrivateMB         = [Math]::Round($p.PrivateMemorySize64 / 1MB, 1)
    WorkingSetMB      = [Math]::Round($p.WorkingSet64 / 1MB, 1)
    MegaCycles        = [Math]::Round(($c1 - $c0) / 1e6, 2)
    CtxSwitchPerSec   = [Math]::Round($switches / $elapsed, 2)
    Threads           = "$($after.Count) (start $($before.Count))"
    Handles           = $p.HandleCount
    GpuPeakByLuid     = ($gpuByAdapter.GetEnumerator() | ForEach-Object { '{0}={1:N1}%' -f $_.Key, $_.Value }) -join ' '
} | Format-List
