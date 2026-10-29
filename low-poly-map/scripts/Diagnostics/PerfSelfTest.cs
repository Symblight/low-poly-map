using System;
using System.Diagnostics;
using System.Text.Json;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// <c>--perf-selftest</c>: checks the measuring code itself, no world needed (<c>$G --headless --path . -- --tile-url= --perf-selftest</c>):
/// histogram percentiles and the 1 % low against values computed from the raw frame times, the slope of <see cref="Series"/>, the
/// marker accounting, <see cref="JsonText"/> output, and that the sampler and the markers allocate nothing and cost microseconds.
/// Prints <c>PERF SELFTEST PASS/FAIL</c>; exit code 0 / 1.
/// </summary>
public static class PerfSelfTest
{
    public static bool Requested() => Array.Exists(OS.GetCmdlineUserArgs(), a => a == "--perf-selftest");

    public static bool Run()
    {
        int failures = 0;
        void Check(string name, bool ok, string detail)
        {
            GD.Print($"{(ok ? "PASS" : "FAIL")}  {name}: {detail}");
            if (!ok) failures++;
        }

        Percentiles(Check);
        SlopeAndMarkers(Check);
        Json(Check);
        Costs(Check);
        GD.Print(failures == 0 ? "PERF SELFTEST PASS" : $"PERF SELFTEST FAIL: {failures}");
        return failures == 0;
    }

    static void Percentiles(Action<string, bool, string> check)
    {
        var rng = new Random(7);
        var stats = new PerfStats();
        var raw = new double[10000];
        for (int i = 0; i < raw.Length; i++)
        {
            raw[i] = 4.0 + rng.NextDouble() * 12.0 + (rng.NextDouble() < 0.02 ? 20.0 : 0.0);   // 4..16 ms with 2 % spikes
            stats.Add(new PerfSample { FrameMs = raw[i] });
        }
        Array.Sort(raw);
        double Exact(double p) => raw[(int)Math.Ceiling(p * raw.Length) - 1];
        foreach (var (name, p, got) in new[] { ("p50", 0.50, stats.P50Ms), ("p95", 0.95, stats.P95Ms), ("p99", 0.99, stats.P99Ms) })
            check($"frame {name}", got >= Exact(p) && got <= Exact(p) + PerfStats.BucketMs + 1e-9, $"histogram {got:0.###} ms, exact {Exact(p):0.###} ms (one bucket = {PerfStats.BucketMs} ms)");
        int worst = (int)Math.Ceiling(0.01 * raw.Length);
        double exactLow = 1000.0 / Mean(raw, raw.Length - worst);
        check("1 % low fps", Math.Abs(stats.OnePercentLowFps - exactLow) / exactLow < 0.01, $"{stats.OnePercentLowFps:0.##} fps, exact {exactLow:0.##} fps");
        check("hitches", stats.Hitches == 0 && stats.MaxFrameMs < stats.HitchMs, $"max {stats.MaxFrameMs:0.#} ms against the {stats.HitchMs:0.#} ms threshold");
    }

    static double Mean(double[] values, int from)
    {
        double sum = 0;
        for (int i = from; i < values.Length; i++) sum += values[i];
        return sum / (values.Length - from);
    }

    static void SlopeAndMarkers(Action<string, bool, string> check)
    {
        var series = new Series();
        for (int t = 0; t < 120; t++) series.Add(1000 + t, 200 + 0.5 * t + (t % 2 == 0 ? 0.3 : -0.3));
        check("series slope", Math.Abs(series.SlopePerMinute - 30.0) < 0.2, $"{series.SlopePerMinute:0.###} MB/min, expected 30");
        series.Add(1200, double.NaN);
        check("series ignores NaN", series.Count == 120, $"{series.Count} samples");

        PerfMarker.Reset();
        for (int frame = 0; frame < 10; frame++)
        {
            PerfMarker.AddMs(PerfCat.PollReady, 2.0, 1);
            if (frame % 2 == 0) PerfMarker.AddMs(PerfCat.Unload, 4.0, 2);
            PerfMarker.EndFrame();
        }
        check("marker mean", Math.Abs(PerfMarker.MeanMs(PerfCat.PollReady) - 2.0) < 1e-9 && Math.Abs(PerfMarker.MeanMs(PerfCat.Unload) - 2.0) < 1e-9,
            $"poll_ready {PerfMarker.MeanMs(PerfCat.PollReady):0.###}, unload {PerfMarker.MeanMs(PerfCat.Unload):0.###} (every other frame 4 ms = mean 2)");
        PerfMarker.Reset();
    }

    static void Json(Action<string, bool, string> check)
    {
        var j = new JsonText();
        j.Begin();
        j.Field("name", "a \"quoted\" {value}, with: punctuation");
        j.BeginObject("nested");
        j.Field("n", 1.5);
        j.Field("nan", double.NaN);
        j.End();
        j.Field("flag", true);
        j.End();
        try
        {
            using var compact = JsonDocument.Parse(j.ToString());
            using var pretty = JsonDocument.Parse(j.Pretty());
            bool same = compact.RootElement.GetProperty("name").GetString() == pretty.RootElement.GetProperty("name").GetString()
                && pretty.RootElement.GetProperty("nested").GetProperty("n").GetDouble() == 1.5;
            check("json", same, "compact and indented text parse to the same values; NaN is written as a string");
        }
        catch (JsonException e)
        {
            check("json", false, e.Message);
        }
    }

    static void Costs(Action<string, bool, string> check)
    {
        const int N = 100000;
        long allocated = GC.GetAllocatedBytesForCurrentThread();
        long start = Stopwatch.GetTimestamp();
        for (int i = 0; i < N; i++) using (PerfMarker.Scope(PerfCat.Overlay)) { }
        double scopeNs = (Stopwatch.GetTimestamp() - start) * 1e9 / Stopwatch.Frequency / N;
        long scopeBytes = GC.GetAllocatedBytesForCurrentThread() - allocated;
        PerfMarker.EndFrame();
        PerfMarker.Reset();
        check("marker scope", scopeBytes == 0 && scopeNs < 1000, $"{scopeNs:0} ns each, {scopeBytes} bytes allocated over {N}");

        // The allocation probe books the bytes of a nested scope once, and its own scopes allocate nothing.
        AllocProbe.Enabled = true;
        AllocProbe.Reset();
        allocated = GC.GetAllocatedBytesForCurrentThread();
        for (int i = 0; i < N; i++) using (AllocProbe.Measure(AllocSite.UpdateCamera)) using (AllocProbe.Measure(AllocSite.GroundHeight)) { }
        long probeBytes = GC.GetAllocatedBytesForCurrentThread() - allocated;
        AllocProbe.Reset();
        long expected = 0;
        using (AllocProbe.Measure(AllocSite.UpdateCamera))
        {
            var a = new byte[1000];
            using (AllocProbe.Measure(AllocSite.GroundHeight)) { var b = new byte[2000]; GC.KeepAlive(b); }
            GC.KeepAlive(a);
        }
        expected = AllocProbe.BytesOf(AllocSite.UpdateCamera) + AllocProbe.BytesOf(AllocSite.GroundHeight);
        bool nestedOk = AllocProbe.BytesOf(AllocSite.GroundHeight) is >= 2000 and < 2100 && AllocProbe.BytesOf(AllocSite.UpdateCamera) is >= 1000 and < 1100;
        AllocProbe.Enabled = false;
        AllocProbe.Reset();
        check("AllocProbe", probeBytes == 0 && nestedOk && expected < 3300, $"{probeBytes} bytes allocated over {N} nested scopes; nested sites book {expected} bytes for 3000 allocated (exclusive)");

        var stats = new PerfStats();
        var sample = new PerfSample { FrameMs = 5, ProcessMs = 4, DrawCalls = 900, Primitives = 1000000 };
        allocated = GC.GetAllocatedBytesForCurrentThread();
        start = Stopwatch.GetTimestamp();
        for (int i = 0; i < N; i++) stats.Add(sample);
        double addNs = (Stopwatch.GetTimestamp() - start) * 1e9 / Stopwatch.Frequency / N;
        long addBytes = GC.GetAllocatedBytesForCurrentThread() - allocated;
        check("PerfStats.Add", addBytes == 0 && addNs < 2000, $"{addNs:0} ns each, {addBytes} bytes allocated over {N}");

        // The real sampler: the Godot monitor reads and the render-time reads of one frame (PerfMonitor.Frame also feeds the session).
        const int Frames = 2000;
        for (int i = 0; i < 200; i++) PerfMonitor.Frame();   // warm-up: the first calls compile and cache the marshalling
        allocated = GC.GetAllocatedBytesForCurrentThread();
        start = Stopwatch.GetTimestamp();
        for (int i = 0; i < Frames; i++) PerfMonitor.Frame();
        double frameUs = (Stopwatch.GetTimestamp() - start) * 1e6 / Stopwatch.Frequency / Frames;
        long frameBytes = GC.GetAllocatedBytesForCurrentThread() - allocated;
        PerfMonitor.Reset();
        check("PerfMonitor.Frame", frameBytes == 0 && frameUs < 50, $"{frameUs:0.0} us each (budget 50 us), {frameBytes} bytes allocated over {Frames}");
    }
}
