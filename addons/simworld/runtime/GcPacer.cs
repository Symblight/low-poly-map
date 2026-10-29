using System;
using System.Diagnostics;
using System.Runtime;

namespace SimWorld;

/// <summary>
/// Moves the managed garbage collections out of the frame loop. The .NET collector runs a gen0 collection every 8 MB of allocation, and in a process with Godot's tens of thousands of wrapper
/// objects each one stops the main thread for 5 to 10 ms (marking the handle tables, not the garbage; a gen2 triggered by the large-object heap takes 10 to 30 ms). The 8 MB budget
/// cannot be raised from a runtimeconfig (only the environment variable <c>DOTNET_GCgen0size</c> does), so the pacer asks for it at run time: <see cref="GC.TryStartNoGCRegion(long)"/> lets the program allocate
/// <see cref="BudgetBytes"/> without any collection (memory is committed as it is allocated, not up front). At 85 % of the budget the pacer ends the region, collects once and starts the next, so the
/// pause happens at most once per budget instead of every 8 MB: with 64 MB and the 0.2 MB/s the frame loop should allocate, once in five minutes. The trade: the working set grows by up to the budget,
/// finalizable garbage (and the native memory of un-disposed wrappers) waits that long for its collection, and an allocation burst larger than the budget ends the region early (an ordinary collection
/// then). Main thread, one call per frame. Off unless <see cref="WorldRenderSettings.GcQuietBudgetMb"/> is above zero.
/// </summary>
public sealed class GcPacer
{
    private const double RotateAt = 0.85;
    private long _startedAtBytes;
    private bool _supported = true;

    /// <summary>Allocation allowed per region.</summary>
    public long BudgetBytes { get; }
    /// <summary>Regions started, planned rotations, regions the runtime ended before the plan (an allocation burst, or an explicit collection elsewhere), and starts that were refused.</summary>
    public int Starts { get; private set; }
    public int Rotations { get; private set; }
    public int EarlyExits { get; private set; }
    public int Refused { get; private set; }
    /// <summary>Longest time one rotation (end region, collect, start the next) took, milliseconds.</summary>
    public double RotationMsMax { get; private set; }

    public GcPacer(int budgetMb)
    {
        BudgetBytes = budgetMb * 1024L * 1024L;
        _startedAtBytes = GC.GetTotalAllocatedBytes(false);   // a region left over from a previous view counts from now
    }

    /// <summary>True while the runtime is inside a no-GC region.</summary>
    public bool InRegion => GCSettings.LatencyMode == GCLatencyMode.NoGCRegion;

    public void Tick()
    {
        if (!_supported) return;
        if (InRegion)
        {
            if (GC.GetTotalAllocatedBytes(false) - _startedAtBytes < BudgetBytes * RotateAt) return;
            Rotate();
            return;
        }
        if (Starts > 0) EarlyExits++;   // we were in a region before and the runtime left it
        Start();
    }

    private void Rotate()
    {
        long t0 = Stopwatch.GetTimestamp();
        try { GC.EndNoGCRegion(); }
        catch (InvalidOperationException) { /* the runtime ended it between the check and here */ }
        GC.Collect(0, GCCollectionMode.Forced, blocking: true, compacting: false);
        Rotations++;
        Start();
        double ms = (Stopwatch.GetTimestamp() - t0) * 1000.0 / Stopwatch.Frequency;
        if (ms > RotationMsMax) RotationMsMax = ms;
    }

    private void Start()
    {
        _startedAtBytes = GC.GetTotalAllocatedBytes(false);
        try
        {
            if (GC.TryStartNoGCRegion(BudgetBytes)) { Starts++; return; }
            Refused++;
        }
        catch (Exception e) when (e is ArgumentOutOfRangeException or InvalidOperationException or NotSupportedException)
        {
            _supported = false;   // this runtime has no such region (or the size is out of range): stay out of the way
            Godot.GD.PushWarning($"[GcPacer] no-GC region of {BudgetBytes >> 20} MB not available ({e.GetType().Name}); the garbage collector runs as usual");
        }
    }
}
