using System;

namespace LowPolyMap.Diagnostics;

/// <summary>One frame's performance numbers (the same design as the SimPlane game's Diagnostics/PerfStats), sampled by <see cref="PerfMonitor"/>.</summary>
public struct PerfSample
{
    /// <summary>Wall time since the previous frame (ms), not Godot's (smoothed, time-scaled) delta.</summary>
    public double FrameMs;
    /// <summary>CPU time of the process step (all <c>_Process</c> callbacks), ms.</summary>
    public double ProcessMs;
    /// <summary>CPU time of the physics steps of the frame, ms.</summary>
    public double PhysicsMs;
    /// <summary>Wall time from the first to the last <c>_Process</c> callback of the frame (script work on the main thread; measured by <see cref="DiagnosticsNode"/> and its tail), ms.</summary>
    public double ProcessWallMs;
    /// <summary>GPU time of the root viewport, ms (0 where the renderer cannot measure it, e.g. headless).</summary>
    public double GpuMs;
    /// <summary>CPU time the renderer spent on the root viewport, ms.</summary>
    public double RenderCpuMs;
    public int DrawCalls;
    public long Primitives;
    public long VideoMemBytes;
    public long StaticMemBytes;
}

/// <summary>
/// Frame statistics over a window of frames (same design as the SimPlane game): frame-time histogram (0.25 ms buckets to
/// 100 ms), p50/p95/p99, 1 % low FPS, hitches, CPU process/physics time, GPU time (with its own histogram), draw calls,
/// primitives, video and static memory. <see cref="Add"/> and <see cref="Reset"/> allocate nothing, so one instance can be
/// fed every frame and reset per window. <see cref="PerfMonitor"/> owns the session's instances.
/// </summary>
public sealed class PerfStats
{
    public const double BucketMs = 0.25;
    public const double RangeMs = 100.0;
    /// <summary>The 30 fps budget of the minimum machine (the same number the game uses).</summary>
    public const double BudgetFrameMs = 1000.0 / 30.0;

    readonly DurationHistogram _frames = new(BucketMs, RangeMs);
    readonly DurationHistogram _gpu = new(BucketMs, RangeMs);

    /// <summary>Target frame time for the hitch rule; the minimum machine's 30 fps budget by default.</summary>
    public double TargetFrameMs { get; set; } = BudgetFrameMs;
    /// <summary>A hitch is a frame longer than 2x the target frame time and longer than 50 ms.</summary>
    public double HitchMs => Math.Max(2.0 * TargetFrameMs, 50.0);

    public long Frames => _frames.Count;
    public long Hitches { get; private set; }
    /// <summary>Wall time covered (sum of frame times), s.</summary>
    public double DurationS => _frames.Sum / 1000.0;
    public double MeanFrameMs => _frames.Mean;
    public double MaxFrameMs => _frames.Max;
    public double MinFrameMs => _frames.Count == 0 ? 0 : _frames.Min;
    public double AvgFps => _frames.Sum <= 0 ? 0 : _frames.Count * 1000.0 / _frames.Sum;
    public double P50Ms => _frames.Percentile(0.50);
    public double P95Ms => _frames.Percentile(0.95);
    public double P99Ms => _frames.Percentile(0.99);
    /// <summary>1 % low: the frame rate of the mean of the slowest 1 % of frames.</summary>
    public double OnePercentLowFps { get { double w = _frames.WorstMean(0.01); return w <= 0 ? 0 : 1000.0 / w; } }
    /// <summary>Frames over the 33.3 ms budget.</summary>
    public long OverBudget { get; private set; }

    public double MeanProcessMs => Mean(_processSum);
    public double MaxProcessMs { get; private set; }
    public double MeanPhysicsMs => Mean(_physicsSum);
    public double MaxPhysicsMs { get; private set; }
    public double MeanRenderCpuMs => Mean(_renderCpuSum);
    public double MeanProcessWallMs => Mean(_processWallSum);
    public double MaxProcessWallMs { get; private set; }
    /// <summary>What is left of the frame after the process callbacks, the physics steps and the renderer's CPU work: presentation, driver and GPU wait, vsync or idle (never below 0).</summary>
    public double MeanOtherMs => Mean(_otherSum);
    public long GpuFrames => _gpu.Count;
    public double MeanGpuMs => _gpu.Mean;
    public double MaxGpuMs => _gpu.Max;
    public double P95GpuMs => _gpu.Percentile(0.95);
    public double P99GpuMs => _gpu.Percentile(0.99);
    public double MeanDrawCalls => Mean(_drawCallSum);
    public int MaxDrawCalls { get; private set; }
    public double MeanPrimitives => Mean(_primitiveSum);
    public long MaxPrimitives { get; private set; }
    public long MaxVideoMemBytes { get; private set; }
    public long LastVideoMemBytes { get; private set; }
    public long MaxStaticMemBytes { get; private set; }
    public long LastStaticMemBytes { get; private set; }

    double _processSum, _physicsSum, _renderCpuSum, _drawCallSum, _primitiveSum, _processWallSum, _otherSum;

    double Mean(double sum) => _frames.Count == 0 ? 0 : sum / _frames.Count;

    public void Add(in PerfSample s)
    {
        _frames.Add(s.FrameMs);
        if (s.FrameMs > HitchMs) Hitches++;
        if (s.FrameMs > BudgetFrameMs) OverBudget++;
        if (s.GpuMs > 0) _gpu.Add(s.GpuMs);
        _processSum += s.ProcessMs;
        _physicsSum += s.PhysicsMs;
        _renderCpuSum += s.RenderCpuMs;
        _processWallSum += s.ProcessWallMs;
        _otherSum += Math.Max(0, s.FrameMs - s.ProcessWallMs - s.PhysicsMs - s.RenderCpuMs);
        if (s.ProcessWallMs > MaxProcessWallMs) MaxProcessWallMs = s.ProcessWallMs;
        _drawCallSum += s.DrawCalls;
        _primitiveSum += s.Primitives;
        if (s.ProcessMs > MaxProcessMs) MaxProcessMs = s.ProcessMs;
        if (s.PhysicsMs > MaxPhysicsMs) MaxPhysicsMs = s.PhysicsMs;
        if (s.DrawCalls > MaxDrawCalls) MaxDrawCalls = s.DrawCalls;
        if (s.Primitives > MaxPrimitives) MaxPrimitives = s.Primitives;
        if (s.VideoMemBytes > MaxVideoMemBytes) MaxVideoMemBytes = s.VideoMemBytes;
        if (s.StaticMemBytes > MaxStaticMemBytes) MaxStaticMemBytes = s.StaticMemBytes;
        LastVideoMemBytes = s.VideoMemBytes;
        LastStaticMemBytes = s.StaticMemBytes;
    }

    public void Reset()
    {
        _frames.Reset();
        _gpu.Reset();
        Hitches = OverBudget = 0;
        _processSum = _physicsSum = _renderCpuSum = _drawCallSum = _primitiveSum = _processWallSum = _otherSum = 0;
        MaxProcessWallMs = 0;
        MaxProcessMs = MaxPhysicsMs = 0;
        MaxDrawCalls = 0;
        MaxPrimitives = MaxVideoMemBytes = LastVideoMemBytes = MaxStaticMemBytes = LastStaticMemBytes = 0;
    }

    /// <summary>All the numbers as fields of the object <paramref name="j"/> is in (ms rounded to 0.001).</summary>
    public void WriteFields(JsonText j)
    {
        j.Field("frames", Frames);
        j.Field("duration_s", R(DurationS));
        j.Field("avg_fps", R(AvgFps));
        j.Field("one_percent_low_fps", R(OnePercentLowFps));
        j.Field("frame_mean_ms", R(MeanFrameMs));
        j.Field("frame_min_ms", R(MinFrameMs));
        j.Field("frame_p50_ms", R(P50Ms));
        j.Field("frame_p95_ms", R(P95Ms));
        j.Field("frame_p99_ms", R(P99Ms));
        j.Field("frame_max_ms", R(MaxFrameMs));
        j.Field("percentile_resolution_ms", BucketMs);
        j.Field("hitch_threshold_ms", R(HitchMs));
        j.Field("hitches", Hitches);
        j.Field("budget_ms", R(BudgetFrameMs));
        j.Field("frames_over_budget", OverBudget);
        j.Field("cpu_process_mean_ms", R(MeanProcessMs));
        j.Field("cpu_process_max_ms", R(MaxProcessMs));
        j.Field("cpu_physics_mean_ms", R(MeanPhysicsMs));
        j.Field("cpu_physics_max_ms", R(MaxPhysicsMs));
        j.Field("render_cpu_mean_ms", R(MeanRenderCpuMs));
        j.Field("process_wall_mean_ms", R(MeanProcessWallMs));
        j.Field("process_wall_max_ms", R(MaxProcessWallMs));
        j.Field("other_wait_mean_ms", R(MeanOtherMs));
        j.Field("gpu_frames", GpuFrames);
        j.Field("gpu_mean_ms", R(MeanGpuMs));
        j.Field("gpu_p95_ms", R(P95GpuMs));
        j.Field("gpu_p99_ms", R(P99GpuMs));
        j.Field("gpu_max_ms", R(MaxGpuMs));
        j.Field("draw_calls_mean", R(MeanDrawCalls));
        j.Field("draw_calls_max", MaxDrawCalls);
        j.Field("primitives_mean", Math.Round(MeanPrimitives));
        j.Field("primitives_max", MaxPrimitives);
        j.Field("video_mem_mb_max", Math.Round(MaxVideoMemBytes / 1048576.0, 1));
        j.Field("video_mem_mb_last", Math.Round(LastVideoMemBytes / 1048576.0, 1));
        j.Field("static_mem_mb_max", Math.Round(MaxStaticMemBytes / 1048576.0, 1));
        j.Field("static_mem_mb_last", Math.Round(LastStaticMemBytes / 1048576.0, 1));
    }

    static double R(double v) => Math.Round(v, 3);
}
