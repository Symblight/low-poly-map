using System;
using System.Diagnostics;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// CPU use of the process and of the main thread, read once a second. The process figure is .NET's <c>TotalProcessorTime</c> (user +
/// system, all threads); the main-thread figure is the thread CPU clock (<see cref="ThreadClock"/>: macOS and Linux, NaN elsewhere). The library
/// does not report its worker threads' CPU (<c>debug_stats</c> has no such key), so "other threads" is the process minus the main thread:
/// the Rust workers (tokio I/O, rayon meshing) plus Godot's own render, audio and GC threads. 100 % = one core fully busy.
/// </summary>
public sealed class CpuTimes
{
    readonly Process _process = Process.GetCurrentProcess();
    double _lastProcess, _lastMain;
    bool _haveLast;

    /// <summary>Fills the three percentages for the <paramref name="wallSeconds"/> since the previous call (the first call returns zeros). Main thread only.</summary>
    public void Sample(double wallSeconds, out double processPct, out double mainPct, out double otherPct)
    {
        _process.Refresh();
        double process = _process.TotalProcessorTime.TotalSeconds, main = ThreadClock.Seconds();
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
}
