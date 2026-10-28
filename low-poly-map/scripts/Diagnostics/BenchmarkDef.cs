using System;

namespace LowPolyMap.Diagnostics;

/// <summary>One benchmark: what the camera flies, at which local hour, and whether the world is reconfigured on a timer.</summary>
public sealed record BenchmarkDef(string Name, Func<IFlightPath> Path, double Hour, double ReconfigureEveryS = 0)
{
    const double Day = 13.0, Night = 22.0;

    /// <summary>The registered benchmarks (docs: low-poly-map/CLAUDE.md, "Diagnostics").</summary>
    public static readonly BenchmarkDef[] All =
    {
        new("low-250", () => new RacetrackPath(250, 300), Day),
        new("climb", () => new RacetrackPath(250, 100, 8000, 150), Day),
        new("zoom", () => new ZoomPath(), Day),
        new("night-low", () => new RacetrackPath(250, 300), Night),
        new("reconfigure", () => new RacetrackPath(250, 300), Day, ReconfigureEveryS: 10),
    };

    public static BenchmarkDef? Find(string name) => Array.Find(All, d => string.Equals(d.Name, name, StringComparison.OrdinalIgnoreCase));

    public static string Names => string.Join(", ", Array.ConvertAll(All, d => d.Name));
}
