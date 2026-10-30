using System;

namespace LowPolyMap.Diagnostics;

/// <summary>One benchmark: what the camera flies, at which local hour, whether the world is reconfigured on a timer, and which seed world
/// it runs over (empty = the tile archive; a seed world generates every tile, so no tile server or URL is needed).</summary>
public sealed record BenchmarkDef(string Name, Func<IFlightPath> Path, double Hour, double ReconfigureEveryS = 0, string World = "")
{
    const double Day = 13.0, Night = 22.0;
    public const string SeedTwoCities = "res://world/two-cities/world.json";

    /// <summary>The registered benchmarks (docs: docs/bench/diagnostics.md).</summary>
    public static readonly BenchmarkDef[] All =
    {
        new("low-250", () => new RacetrackPath(250, 300), Day),
        new("climb", () => new RacetrackPath(250, 100, 8000, 150), Day),
        new("zoom", () => new ZoomPath(), Day),
        new("night-low", () => new RacetrackPath(250, 300), Night),
        new("reconfigure", () => new RacetrackPath(250, 300), Day, ReconfigureEveryS: 10),
        // Generated Vilnius (radius 8 km around the origin = the `vilnius` seed spot): the same racetrack as low-250, over the seed world.
        new("seed-low", () => new RacetrackPath(250, 300), Day, World: SeedTwoCities),
    };

    public static BenchmarkDef? Find(string name) => Array.Find(All, d => string.Equals(d.Name, name, StringComparison.OrdinalIgnoreCase));

    public static string Names => string.Join(", ", Array.ConvertAll(All, d => d.Name));
}
