using System;
using System.Diagnostics;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// The bench's performance diagnostics, one node per run (created by <see cref="BenchArgs"/>). Its process priority is the lowest
/// of the scene, so every frame it runs before everything else: it rolls over the view's timing frame (the time inside the library
/// calls and the uploads of the frame that just ended), feeds them to the <see cref="PerfMarker"/> categories, takes the frame
/// sample (<see cref="PerfMonitor.Frame"/>) and, once a second, the memory / library readings of <see cref="SecondSampler"/>. It
/// measures its own cost (<see cref="FrameCost"/>, <see cref="SecondCost"/>) so the report can show what measuring costs.
/// </summary>
public partial class DiagnosticsNode : Node
{
    const double SamplePeriodS = 1.0;
    static readonly double MsPerTick = 1000.0 / Stopwatch.Frequency;

    WorldView _view = null!;
    SecondSampler _sampler = null!;
    readonly Stopwatch _clock = Stopwatch.StartNew();
    double _sinceSample;

    /// <summary>Cost of the per-frame part (timing roll-over, marker feed, frame sample), ms per frame.</summary>
    public DurationHistogram FrameCost { get; } = new(0.001, 5.0);
    /// <summary>Cost of the once-a-second part (process / GC / library readings), ms per sample.</summary>
    public DurationHistogram SecondCost { get; } = new(0.01, 20.0);

    /// <summary>Managed bytes the per-frame part allocated since the node started (stays 0: the claim "no allocation per frame" is measured, not assumed).</summary>
    public long FrameAllocatedBytes { get; private set; }

    public SecondSampler Sampler => _sampler;

    /// <summary>Forgets the cost figures (the benchmark does it when the warm-up ends).</summary>
    public void ResetCosts()
    {
        FrameCost.Reset();
        SecondCost.Reset();
        FrameAllocatedBytes = 0;
    }

    /// <summary>A run in progress (the benchmark) puts one line for the F4 table here.</summary>
    public Func<string>? StatusLine { get; set; }

    /// <summary>Seconds since the node started (the time base of the series).</summary>
    public double Seconds => _clock.Elapsed.TotalSeconds;

    /// <summary>Raised after every once-a-second reading with the seconds since start and the values (<see cref="SecondSampler.Values"/>, valid during the call).</summary>
    public event Action<double, double[]>? Sampled;

    public static DiagnosticsNode Attach(WorldView view, Node parent)
    {
        var node = new DiagnosticsNode { Name = "Diagnostics", ProcessMode = ProcessModeEnum.Always, ProcessPriority = int.MinValue, _view = view };
        node._sampler = new SecondSampler(view);
        parent.CallDeferred(Node.MethodName.AddChild, node);   // the scene is still setting up its children when BenchArgs runs
        return node;
    }

    public override void _Ready()
    {
        PerfMonitor.Attach(GetViewport());
        PerfMarker.RegisterMonitors();
        AddChild(new PerfOverlay(this));
        AddChild(new ProcessTail());
    }

    public override void _Process(double delta)
    {
        long start = Stopwatch.GetTimestamp();
        long allocated = GC.GetAllocatedBytesForCurrentThread();
        _view.EndTimingFrame();
        FeedMarkers();
        PerfMonitor.Frame();
        PerfMonitor.BeginProcess();
        FrameAllocatedBytes += GC.GetAllocatedBytesForCurrentThread() - allocated;
        FrameCost.Add((Stopwatch.GetTimestamp() - start) * MsPerTick);

        _sinceSample += delta;
        if (_sinceSample < SamplePeriodS) return;
        _sinceSample = 0;
        long second = Stopwatch.GetTimestamp();
        var values = _sampler.Sample(Seconds);
        SecondCost.Add((Stopwatch.GetTimestamp() - second) * MsPerTick);
        Sampled?.Invoke(Seconds, values);
    }

    /// <summary>Hands the closed frame's library timings and uploads to the marker categories.</summary>
    void FeedMarkers()
    {
        var t = _view.Timings;
        var u = _view.LastUploads;
        Feed(PerfCat.UpdateCamera, t, StreamerCall.UpdateCamera);
        Feed(PerfCat.PollReady, t, StreamerCall.PollReady);
        Feed(PerfCat.Fades, t, StreamerCall.PollFades);
        Feed(PerfCat.Unload, t, StreamerCall.PollUnload);
        Feed(PerfCat.OriginShift, t, StreamerCall.TakeOriginShift);
        Feed(PerfCat.DebugStats, t, StreamerCall.DebugStats);
        PerfMarker.AddMs(PerfCat.UploadVector, u.VectorMs, u.Vector);
        PerfMarker.AddMs(PerfCat.UploadTerrain, u.TerrainMs, u.Terrain);
        PerfMarker.AddMs(PerfCat.UploadLandmarks, u.LandmarkMs, u.Landmarks);
    }

    static void Feed(PerfCat cat, StreamerTimings timings, StreamerCall call) => PerfMarker.AddMs(cat, timings.FrameMs(call), timings.FrameCalls(call));
}
