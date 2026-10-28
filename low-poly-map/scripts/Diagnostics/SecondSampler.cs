using System;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// The once-a-second readings: process memory, Godot's static memory, the managed heap and allocation rate, the library's own
/// accounting from <c>debug_stats()</c> (memory, jobs, data tiles, tiles per layer), CPU use (<see cref="CpuTimes"/>), the frame split of the
/// last one-second window, and the view's churn / upload / frustum figures (<see cref="ViewRates"/>). One reused array, no allocation beyond what
/// the Godot marshalling of the stats dictionary needs; <see cref="DiagnosticsNode"/> calls <see cref="Sample"/> at 1 Hz.
/// </summary>
public sealed class SecondSampler
{
    const double MB = 1048576.0;

    readonly WorldView _view;
    readonly CpuTimes _cpu = new();
    readonly ViewRates _rates;
    readonly StreamerStats _stats = new();
    readonly double[] _values = new double[MetricSet.Count];
    long _lastAllocated = GC.GetTotalAllocatedBytes();
    double _lastSeconds;

    public SecondSampler(WorldView view)
    {
        _view = view;
        _rates = new ViewRates(view);
    }

    /// <summary>The latest values by <see cref="Metric"/>; all 0 until the first <see cref="Sample"/>.</summary>
    public double[] Values => _values;

    /// <summary>Reads everything at <paramref name="seconds"/> after the start of the run and returns <see cref="Values"/>.</summary>
    public double[] Sample(double seconds)
    {
        long allocated = GC.GetTotalAllocatedBytes();
        double dt = Math.Max(seconds - _lastSeconds, 1e-3);
        _values[(int)Metric.ProcessMb] = ProcessMemory.Bytes() / MB;
        _values[(int)Metric.GodotStaticMb] = OS.GetStaticMemoryUsage() / MB;
        _values[(int)Metric.GcHeapMb] = GC.GetTotalMemory(false) / MB;
        _values[(int)Metric.GcAllocMbPerS] = (allocated - _lastAllocated) / MB / dt;
        _lastAllocated = allocated;
        _lastSeconds = seconds;
        ReadLibrary();
        _cpu.Sample(dt, out double process, out double main, out double other);
        _values[(int)Metric.CpuProcessPct] = process;
        _values[(int)Metric.CpuMainPct] = main;
        _values[(int)Metric.CpuOtherPct] = other;
        var window = PerfMonitor.Window;
        _values[(int)Metric.FrameMs] = window.MeanFrameMs;
        _values[(int)Metric.ProcessWallMs] = window.MeanProcessWallMs;
        _values[(int)Metric.RenderCpuMs] = window.MeanRenderCpuMs;
        _values[(int)Metric.OtherWaitMs] = window.MeanOtherMs;
        _rates.Sample(dt, _values);
        return _values;
    }

    void ReadLibrary()
    {
        // Without the stats (library missing) the library metrics stay NaN, which the series ignore.
        bool ok = _view.Streamer.DebugStats(_stats);
        _values[(int)Metric.LibMemoryMb] = ok ? _stats.Memory("total") : double.NaN;
        _values[(int)Metric.LibDataTilesMb] = ok ? _stats.Memory("data_tiles") : double.NaN;
        _values[(int)Metric.LibHeightTilesMb] = ok ? _stats.Memory("height_tiles") : double.NaN;
        _values[(int)Metric.JobsQueued] = ok && _stats.HasJobs ? _stats.JobsQueued : double.NaN;
        _values[(int)Metric.JobsRunning] = ok && _stats.HasJobs ? _stats.JobsRunning : double.NaN;
        _values[(int)Metric.DataTilesLoaded] = ok ? _stats.DataTilesLoaded : double.NaN;
        _values[(int)Metric.TilesQueued] = ok ? Queued(_stats) : double.NaN;
        _values[(int)Metric.TilesVector] = ok ? Sum(_stats.Loaded, StreamerStats.Rings) : double.NaN;
        _values[(int)Metric.TilesTerrain] = ok ? Sum(_stats.TerrainLoaded, _stats.TerrainRings) : double.NaN;
        _values[(int)Metric.TilesLandmarks] = ok ? _stats.LandmarksLoaded : double.NaN;
        _values[(int)Metric.PoolNodes] = _view.PoolCreated + _view.TerrainPoolCreated;
    }

    static double Queued(StreamerStats s) =>
        Sum(s.Queued, StreamerStats.Rings) + Sum(s.Fetching, StreamerStats.Rings) + Sum(s.Meshing, StreamerStats.Rings)
        + Sum(s.TerrainQueued, s.TerrainRings) + Sum(s.TerrainFetching, s.TerrainRings) + Sum(s.TerrainMeshing, s.TerrainRings);

    static double Sum(int[] values, int count)
    {
        int sum = 0;
        for (int i = 0; i < count && i < values.Length; i++) sum += values[i];
        return sum;
    }
}
