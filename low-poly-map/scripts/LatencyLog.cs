using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// <c>--latency-log=&lt;seconds&gt;</c>: prints the library's streaming latency table (debug_stats()["latency"], api 1.3) and the host's
/// per-ring upload times every N seconds and once on exit, e.g. for a fixed autopilot run:
/// <c>$G --headless --path . --quit-after 3600 -- --autopilot=250 --latency-log=10</c>. Created by <see cref="BenchArgs"/>.
/// </summary>
public partial class LatencyLog : Node
{
    private const string Arg = "--latency-log=";

    private WorldView _view = null!;
    private double _every;
    private double _t;
    private readonly StreamerStats _stats = new();

    /// <summary>The interval from the command line, or 0 when the argument is absent.</summary>
    public static double Requested()
    {
        foreach (var arg in OS.GetCmdlineUserArgs())
            if (arg.StartsWith(Arg, System.StringComparison.Ordinal)
                && double.TryParse(arg.Substring(Arg.Length), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var s))
                return System.Math.Max(s, 1.0);
        return 0;
    }

    public static void Attach(WorldView view, double every)
    {
        view.AddChild(new LatencyLog { Name = "LatencyLog", _view = view, _every = every });
    }

    public override void _Process(double delta)
    {
        _t += delta;
        if (_t < _every) return;
        _t = 0;
        Print("every " + _every.ToString("0") + " s");
    }

    public override void _ExitTree() => Print("on exit");

    private void Print(string when)
    {
        if (!_view.Streamer.DebugStats(_stats)) return;
        var sb = new System.Text.StringBuilder();
        sb.Append("[LatencyLog] ").Append(when).Append(", ").Append((Time.GetTicksMsec() / 1000.0).ToString("0"))
          .Append(" s since start; queued R0..R3 ").Append(string.Join("/", _stats.Queued))
          .Append(", loaded ").Append(string.Join("/", _stats.Loaded)).Append('\n');
        if (_stats.Latency.Count == 0) sb.Append("  no latency data (library api < 1.3, or nothing delivered yet)\n");
        else sb.Append("  stage ms p50/p95/max (n), last 128 tiles per band:\n").Append(Indent(_stats.LatencyText()));
        sb.Append("  host upload ms avg/max:");
        for (int r = 0; r < 4; r++)
        {
            int n = _view.TileUploadCount[r];
            sb.Append(" | R").Append(r).Append(' ');
            if (n == 0) { sb.Append('-'); continue; }
            sb.Append((_view.TileUploadUsSum[r] / 1000.0 / n).ToString("0.00")).Append('/').Append((_view.TileUploadUsMax[r] / 1000.0).ToString("0.00"));
        }
        GD.Print(sb.ToString());
    }

    private static string Indent(string lines) => "  " + lines.TrimEnd('\n').Replace("\n", "\n  ") + "\n";
}
