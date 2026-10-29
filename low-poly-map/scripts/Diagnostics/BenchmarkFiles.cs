using System;
using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Text.Json.Nodes;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Where benchmark results and baselines live, and the comparison with a baseline. Results go to <c>low-poly-map/bench_results/</c>
/// (git-ignored; <c>user://bench_results</c> in an export), baselines to <c>bench_results/baselines/&lt;machine&gt;.json</c> (committed: a
/// regression is only meaningful against numbers from the same machine, so each machine commits its own file). A baseline file is one
/// object keyed by <c>&lt;benchmark&gt;@&lt;library flavour&gt;</c> (a debug library is 5-10 times slower than a release one) holding
/// a short summary; <c>--update-baseline</c> writes the entry of the run. A frame p95 more than 10 % above the baseline is a regression.
/// </summary>
public static class BenchmarkFiles
{
    public const double RegressionFraction = 0.10;

    public static string ResultsDir() => OS.HasFeature("editor")
        ? Path.Combine(ProjectSettings.GlobalizePath("res://"), "bench_results")
        : ProjectSettings.GlobalizePath("user://bench_results");

    /// <summary><paramref name="requested"/> (absolute, <c>res://</c> / <c>user://</c>, or relative to the results folder), or <c>&lt;ResultsDir&gt;/&lt;UTC&gt;-&lt;name&gt;.json</c>.</summary>
    public static string OutputPath(string requested, string name)
    {
        if (requested.StartsWith("res://", StringComparison.Ordinal) || requested.StartsWith("user://", StringComparison.Ordinal))
            return ProjectSettings.GlobalizePath(requested);
        if (requested.Length > 0) return Path.GetFullPath(Path.Combine(ResultsDir(), requested));
        string stamp = DateTime.UtcNow.ToString("yyyyMMdd-HHmmss", CultureInfo.InvariantCulture) + "Z";
        return Path.Combine(ResultsDir(), $"{stamp}-{name}.json");
    }

    public static string BaselinePath() => Path.Combine(ResultsDir(), "baselines", MachineInfo.Key() + ".json");

    /// <summary><c>&lt;benchmark&gt;@&lt;flavour&gt;</c>, plus <c>@gcq&lt;MB&gt;</c> when the collector pacer ran: a paced run never compares with, or overwrites, an unpaced baseline.</summary>
    public static string BaselineKey(string name, string libraryFlavour, int gcQuietMb) => gcQuietMb > 0 ? $"{name}@{libraryFlavour}@gcq{gcQuietMb}" : $"{name}@{libraryFlavour}";

    /// <summary>Compares the run with the entry <paramref name="key"/> of this machine's baseline: the <c>baseline</c> object of the result
    /// (with <c>regression</c> true when the frame p95 grew by more than 10 %), or <c>null</c> without a baseline.</summary>
    public static RawJson Compare(string key, PerfStats now)
    {
        string path = BaselinePath();
        if (!File.Exists(path)) return default;
        var j = new JsonText();
        j.Begin();
        j.Field("file", path);
        j.Field("key", key);
        try
        {
            using var doc = JsonDocument.Parse(File.ReadAllText(path));
            if (!doc.RootElement.TryGetProperty(key, out var entry) || !entry.TryGetProperty("frame_p95_ms", out var p95))
            {
                j.Field("error", $"no '{key}' entry in the baseline");
            }
            else
            {
                double baseP95 = p95.GetDouble(), change = baseP95 > 0 ? (now.P95Ms - baseP95) / baseP95 : 0;
                j.Field("frame_p95_ms", baseP95);
                j.Field("current_frame_p95_ms", Math.Round(now.P95Ms, 3));
                j.Field("frame_p95_change_pct", Math.Round(change * 100.0, 1));
                if (entry.TryGetProperty("frame_mean_ms", out var mean) && mean.GetDouble() > 0)
                    j.Field("frame_mean_change_pct", Math.Round((now.MeanFrameMs - mean.GetDouble()) / mean.GetDouble() * 100.0, 1));
                j.Field("threshold_pct", RegressionFraction * 100.0);
                j.Field("regression", change > RegressionFraction);
                if (change > RegressionFraction)
                    GD.PushWarning($"[Benchmark] regression at p95: {baseP95:F2} ms -> {now.P95Ms:F2} ms ({change * 100:F1} %) against {path}");
            }
        }
        catch (Exception e) when (e is JsonException or IOException or InvalidOperationException or FormatException)
        {
            j.Field("error", e.Message);
        }
        j.End();
        return new RawJson(j.ToString());
    }

    /// <summary>Writes (or replaces) the entry <paramref name="key"/> of this machine's baseline file; the other entries stay.</summary>
    public static string Update(string key, string summaryJson)
    {
        string path = BaselinePath();
        var root = File.Exists(path) ? JsonNode.Parse(File.ReadAllText(path)) as JsonObject ?? new JsonObject() : new JsonObject();
        root[key] = JsonNode.Parse(summaryJson);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, root.ToJsonString(new JsonSerializerOptions { WriteIndented = true }) + "\n");
        return path;
    }
}
