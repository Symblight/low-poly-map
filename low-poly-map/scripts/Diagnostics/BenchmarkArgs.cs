using System;
using System.Globalization;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// The benchmark launch arguments (after <c>--</c>): <c>--benchmark=&lt;name&gt;</c>, <c>--frames=N</c> or <c>--seconds=S</c> (default 60),
/// <c>--soak=S</c> (run S seconds and report the first and the last minute), <c>--out=&lt;path.json&gt;</c>, <c>--quit</c>,
/// <c>--update-baseline</c>, <c>--vsync</c>, <c>--bench-window=WxH|fullscreen</c> (default 1600x900). A bare <c>--soak=S</c> runs <c>low-250</c>.
/// </summary>
public sealed record BenchmarkArgs(string Name, int Frames, double Seconds, double SoakSeconds, string Out, bool Quit, bool Vsync, bool UpdateBaseline, string Window)
{
    public const string DefaultName = "low-250";
    public const double DefaultSeconds = 60.0;

    /// <summary>Null when neither <c>--benchmark</c> nor <c>--soak</c> was given.</summary>
    public static BenchmarkArgs? Parse()
    {
        string? name = null, output = null, window = null;
        double seconds = 0, soak = 0;
        int frames = 0;
        bool benchmark = false, quit = false, vsync = false, update = false;
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith("--benchmark", StringComparison.Ordinal) && (arg.Length == 11 || arg[11] == '='))
            {
                benchmark = true;
                name = arg.Length > 12 ? arg.Substring(12) : null;
            }
            else if (arg.StartsWith("--seconds=", StringComparison.Ordinal)) seconds = Number(arg, 10);
            else if (arg.StartsWith("--soak=", StringComparison.Ordinal)) soak = Number(arg, 7);
            else if (arg.StartsWith("--frames=", StringComparison.Ordinal)) frames = (int)Number(arg, 9);
            else if (arg.StartsWith("--out=", StringComparison.Ordinal)) output = arg.Substring(6);
            else if (arg == "--quit") quit = true;
            else if (arg == "--vsync") vsync = true;
            else if (arg == "--update-baseline") update = true;
            else if (arg.StartsWith("--bench-window=", StringComparison.Ordinal)) window = arg.Substring(15);
        }
        if (!benchmark && soak <= 0) return null;
        if (soak > 0) seconds = soak;
        else if (seconds <= 0 && frames <= 0) seconds = DefaultSeconds;
        return new BenchmarkArgs(string.IsNullOrEmpty(name) ? DefaultName : name, frames, seconds, soak, output ?? "", quit, vsync, update, window ?? "");
    }

    static double Number(string arg, int from) =>
        double.TryParse(arg.AsSpan(from), NumberStyles.Float, CultureInfo.InvariantCulture, out var v) ? v : 0;
}
