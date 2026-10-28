using System;
using System.Runtime.InteropServices;

namespace SimWorld;

/// <summary>
/// CPU time of the calling thread (<c>clock_gettime(CLOCK_THREAD_CPUTIME_ID)</c>; macOS and Linux, unsupported elsewhere). Set against wall time it tells a call that
/// computed from one that waited: a native call whose wall time is far above its thread CPU time was descheduled or suspended (a GC pause, a busy machine), not slow.
/// </summary>
public static class ThreadClock
{
    private const int ClockThreadCpuTimeMacOS = 16, ClockThreadCpuTimeLinux = 3;

    [StructLayout(LayoutKind.Sequential)]
    private struct Timespec { public long Seconds, Nanoseconds; }

    [DllImport("libSystem", EntryPoint = "clock_gettime")]
    private static extern int ClockGetTimeMac(int clockId, out Timespec time);

    [DllImport("libc", EntryPoint = "clock_gettime")]
    private static extern int ClockGetTimeLinux(int clockId, out Timespec time);

    /// <summary>True where <see cref="Nanoseconds"/> returns a reading.</summary>
    public static bool Supported => OperatingSystem.IsMacOS() || OperatingSystem.IsLinux();

    /// <summary>CPU time this thread has used so far, in nanoseconds; -1 where unsupported.</summary>
    public static long Nanoseconds()
    {
        Timespec t;
        if (OperatingSystem.IsMacOS()) { if (ClockGetTimeMac(ClockThreadCpuTimeMacOS, out t) != 0) return -1; }
        else if (OperatingSystem.IsLinux()) { if (ClockGetTimeLinux(ClockThreadCpuTimeLinux, out t) != 0) return -1; }
        else return -1;
        return t.Seconds * 1_000_000_000L + t.Nanoseconds;
    }

    /// <summary>CPU time this thread has used so far, in seconds; NaN where unsupported.</summary>
    public static double Seconds()
    {
        long ns = Nanoseconds();
        return ns < 0 ? double.NaN : ns * 1e-9;
    }
}
