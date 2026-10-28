using System;
using System.Diagnostics;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>Subsystems timed by <see cref="PerfMarker"/>. The first seven are fed from the addon's per-frame timing surface
/// (<c>WorldView.Timings</c> / <c>LastUploads</c>), the last two are measured by scopes in the bench.</summary>
public enum PerfCat
{
    /// <summary><c>update_camera</c> / <c>update_camera_geodetic</c> (the call into Rust, incl. the cover work it does on the main thread).</summary>
    UpdateCamera,
    /// <summary><c>poll_ready</c> (the call, i.e. marshalling the delivered tile dictionaries).</summary>
    PollReady,
    /// <summary>Host upload of vector tiles (mesh, ground, lights into pooled nodes).</summary>
    UploadVector,
    /// <summary>Host upload of terrain tiles.</summary>
    UploadTerrain,
    /// <summary>Host upload of landmark tiles.</summary>
    UploadLandmarks,
    /// <summary><c>poll_fades</c> (the call).</summary>
    Fades,
    /// <summary><c>poll_unload</c> (the call).</summary>
    Unload,
    /// <summary><c>take_origin_shift</c> (the call).</summary>
    OriginShift,
    /// <summary>The bench's overlays (F1 detail, F4 perf): update and draw.</summary>
    Overlay,
    /// <summary><c>debug_stats</c> reads, by whoever makes them (the view's 4 Hz LOD read, the overlays, the 1 Hz sampler).</summary>
    DebugStats,
    /// <summary>Host work for the poll_unload batch: cover removal, hiding and retiring the node sets (the call itself is <see cref="Unload"/>).</summary>
    UnloadApply,
    /// <summary>Host work for the poll_fades dictionary: parsing the keys and switching the tiles' materials (the call itself is <see cref="Fades"/>).</summary>
    FadeApply,
    /// <summary>The deferred release of retired node sets (clearing meshes and sprite buffers), time-sliced per frame.</summary>
    ReleaseDrain,
}

/// <summary>A running <see cref="PerfMarker"/> timer; a struct, so <c>using</c> it allocates nothing.</summary>
public readonly struct PerfScope : IDisposable
{
    readonly PerfCat _cat;
    readonly long _start;

    internal PerfScope(PerfCat cat, long start)
    {
        _cat = cat;
        _start = start;
    }

    public void Dispose()
    {
        if (_start != 0) PerfMarker.AddTicks(_cat, Stopwatch.GetTimestamp() - _start);
    }
}

/// <summary>
/// Categorised CPU timers: <c>using (PerfMarker.Scope(PerfCat.Overlay)) { … }</c> or <see cref="AddMs"/> for numbers measured
/// elsewhere. Times add up per frame (<see cref="EndFrame"/>, called by <see cref="PerfMonitor"/> once per frame) into a
/// per-category histogram (0.01 ms buckets to 20 ms) for the mean, p99 and max per frame. Each category is also a Godot custom
/// monitor (<c>SimWorld/&lt;name&gt;_ms</c>, last frame's total) in the editor's Monitors tab. Main thread only. No allocation.
/// </summary>
public static class PerfMarker
{
    public const int Count = 13;
    const string MonitorPrefix = "SimWorld/";

    static readonly string[] Names =
    {
        "update_camera", "poll_ready", "upload_vector", "upload_terrain", "upload_landmarks",
        "fades", "unload", "origin_shift", "overlay", "debug_stats", "unload_apply", "fade_apply", "release_drain",
    };

    static readonly double MsPerTick = 1000.0 / Stopwatch.Frequency;
    static readonly double[] FrameMs = new double[Count];
    static readonly int[] FrameCalls = new int[Count];
    static readonly double[] LastMs = new double[Count];
    static readonly long[] TotalCalls = new long[Count];
    static readonly DurationHistogram[] Histograms = CreateHistograms();

    static DurationHistogram[] CreateHistograms()
    {
        var h = new DurationHistogram[Count];
        for (int i = 0; i < Count; i++) h[i] = new DurationHistogram(0.01, 20.0);
        return h;
    }

    public static string Name(PerfCat cat) => Names[(int)cat];

    public static PerfScope Scope(PerfCat cat) => new(cat, Stopwatch.GetTimestamp());

    internal static void AddTicks(PerfCat cat, long ticks) => AddMs(cat, ticks * MsPerTick, 1);

    /// <summary>Adds <paramref name="ms"/> measured elsewhere to the running frame (<paramref name="calls"/> = how many calls it covers).</summary>
    public static void AddMs(PerfCat cat, double ms, int calls)
    {
        FrameMs[(int)cat] += ms;
        FrameCalls[(int)cat] += calls;
    }

    /// <summary>Closes the frame: each category's accumulated time becomes <see cref="LastFrameMs"/> and a histogram sample.</summary>
    public static void EndFrame()
    {
        for (int i = 0; i < Count; i++)
        {
            double ms = FrameMs[i];
            LastMs[i] = ms;
            Histograms[i].Add(ms);
            TotalCalls[i] += FrameCalls[i];
            FrameMs[i] = 0;
            FrameCalls[i] = 0;
        }
    }

    /// <summary>Clears the histograms and counts (not the running frame).</summary>
    public static void Reset()
    {
        for (int i = 0; i < Count; i++)
        {
            Histograms[i].Reset();
            TotalCalls[i] = 0;
        }
    }

    /// <summary>The category's total in the last closed frame, ms.</summary>
    public static double LastFrameMs(PerfCat cat) => LastMs[(int)cat];
    /// <summary>Mean time per frame since the last <see cref="Reset"/>, ms (frames without a call count as 0).</summary>
    public static double MeanMs(PerfCat cat) => Histograms[(int)cat].Mean;
    public static double P99Ms(PerfCat cat) => Histograms[(int)cat].Percentile(0.99);
    public static double MaxMs(PerfCat cat) => Histograms[(int)cat].Max;

    /// <summary>Registers one custom monitor per category (skips ids that already exist). Main thread.</summary>
    public static void RegisterMonitors()
    {
        for (int i = 0; i < Count; i++)
        {
            int index = i;
            string id = MonitorPrefix + Names[i] + "_ms";
            if (!Performance.HasCustomMonitor(id))
                Performance.AddCustomMonitor(id, Callable.From(() => LastMs[index]));
        }
    }

    /// <summary><c>"update_camera":{"mean_ms":…,"p99_ms":…,"max_ms":…,"calls_per_frame":…},…</c> since the last reset.</summary>
    public static void WriteFields(JsonText j)
    {
        long frames = Histograms[0].Count;
        for (int i = 0; i < Count; i++)
        {
            j.BeginObject(Names[i]);
            j.Field("mean_ms", Math.Round(Histograms[i].Mean, 4));
            j.Field("p99_ms", Math.Round(Histograms[i].Percentile(0.99), 4));
            j.Field("max_ms", Math.Round(Histograms[i].Max, 4));
            j.Field("calls_per_frame", frames == 0 ? 0 : Math.Round(TotalCalls[i] / (double)frames, 3));
            j.End();
        }
    }
}
