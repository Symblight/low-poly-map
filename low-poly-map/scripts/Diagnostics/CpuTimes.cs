using System;
using System.Diagnostics;
using System.Runtime.InteropServices;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// CPU use of the process and of the main thread, read once a second. The process figure is .NET's <c>TotalProcessorTime</c> (user +
/// system, all threads); the main-thread figure is <c>clock_gettime(CLOCK_THREAD_CPUTIME_ID)</c> (macOS only; NaN elsewhere). The library
/// does not report its worker threads' CPU (<c>debug_stats</c> has no such key), so "other threads" is the process minus the main thread:
/// the Rust workers (tokio I/O, rayon meshing) plus Godot's own render, audio and GC threads. 100 % = one core fully busy.
/// </summary>
public sealed class CpuTimes
{
    const int ClockThreadCpuTimeMacOS = 16;

    [StructLayout(LayoutKind.Sequential)]
    struct Timespec { public long Seconds, Nanoseconds; }

    [DllImport("libSystem", EntryPoint = "clock_gettime")]
    static extern int ClockGetTime(int clockId, out Timespec time);

    readonly Process _process = Process.GetCurrentProcess();
    double _lastProcess, _lastMain;
    bool _haveLast;

    /// <summary>Fills the three percentages for the <paramref name="wallSeconds"/> since the previous call (the first call returns zeros). Main thread only.</summary>
    public void Sample(double wallSeconds, out double processPct, out double mainPct, out double otherPct)
    {
        _process.Refresh();
        double process = _process.TotalProcessorTime.TotalSeconds, main = MainThreadSeconds();
        processPct = mainPct = otherPct = 0;
        if (_haveLast && wallSeconds > 1e-3)
        {
            processPct = (process - _lastProcess) / wallSeconds * 100.0;
            mainPct = double.IsNaN(main) ? double.NaN : (main - _lastMain) / wallSeconds * 100.0;
            otherPct = double.IsNaN(mainPct) ? double.NaN : Math.Max(0, processPct - mainPct);
        }
        _lastProcess = process;
        _lastMain = main;
        _haveLast = true;
    }

    static double MainThreadSeconds()
    {
        if (!OperatingSystem.IsMacOS() || ClockGetTime(ClockThreadCpuTimeMacOS, out var t) != 0) return double.NaN;
        return t.Seconds + t.Nanoseconds * 1e-9;
    }
}
