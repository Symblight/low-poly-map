using System;
using System.Runtime.InteropServices;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Memory of the whole process, read cheaply (microseconds, no child process): on macOS the physical footprint from
/// <c>proc_pid_rusage</c> (what Activity Monitor shows; resident size misses compressed pages and would hide a leak), elsewhere the
/// working set. Call it at 1 Hz at most.
/// </summary>
public static class ProcessMemory
{
    const int RusageInfoV0 = 0;
    const int PhysFootprintOffset = 72;   // rusage_info_v0: uuid[16], then ten u64 fields; ri_phys_footprint is the eighth
    static readonly byte[] Buffer = new byte[256];

    [DllImport("libSystem", EntryPoint = "proc_pid_rusage")]
    static extern int ProcPidRusage(int pid, int flavor, byte[] buffer);

    public static string Kind => OperatingSystem.IsMacOS() ? "physical footprint" : "working set";

    /// <summary>The process memory in bytes.</summary>
    public static long Bytes()
    {
        if (OperatingSystem.IsMacOS() && ProcPidRusage(Environment.ProcessId, RusageInfoV0, Buffer) == 0)
            return (long)BitConverter.ToUInt64(Buffer, PhysFootprintOffset);
        return Environment.WorkingSet;
    }
}
