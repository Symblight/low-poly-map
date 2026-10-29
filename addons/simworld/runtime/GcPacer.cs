using System;
using System.Diagnostics;
using System.Runtime;

namespace SimWorld;

/// <summary>
/// Moves the managed garbage collections out of the frame loop. The .NET collector runs a gen0 collection every 8 MB of allocation, and in a process with Godot's tens of thousands of wrapper
/// objects each one stops the main thread for 5 to 10 ms (marking the handle tables, not the garbage; a gen2 triggered by the large-object heap takes 10 to 30 ms). The 8 MB budget
/// cannot be raised from a runtimeconfig (only the environment variable <c>DOTNET_GCgen0size</c> does), so the pacer asks for it at run time: <see cref="GC.TryStartNoGCRegion(long)"/> lets the program allocate
/// <see cref="BudgetBytes"/> without any collection (memory is committed as it is allocated, not up front). At 85 % of the budget the pacer ends the region and starts the next; the start itself runs one
/// full blocking collection (CoreCLR's <c>StartNoGCRegion</c> collects <c>max_generation</c> to make the budget free), so the pause happens once per budget instead of every 8 MB. The trade: the working
/// set grows by up to the budget, finalizable garbage (and the native memory of un-disposed wrappers) waits that long for its collection, and an allocation burst larger than the budget ends the region
/// early (an ordinary collection then).
/// <para>The region is process state, so there is one pacer per process (<see cref="Acquire"/> counts its users, the last <see cref="Release"/> ends the region), ticked once per process frame whichever
/// view calls first. A refused start backs off for <see cref="RefusedBackoffS"/> (a refused start may itself have run a full collection; retrying every frame under memory pressure would collect every
/// frame), an OS memory warning (<see cref="OnMemoryWarning"/>) ends the region, collects everything and keeps the pacer out for <see cref="MemoryWarningHoldS"/>. A host that knows a quiet moment
/// (menu, pause, loading screen) calls <see cref="RotateNow"/> so the rotation does not land wherever the allocation count happens to cross. Main thread only. Off unless
/// <see cref="WorldRenderSettings.GcQuietBudgetMb"/> is above zero; it is the game's policy, the addon never turns it on by itself.</para>
/// </summary>
public sealed class GcPacer
{
    private const double RotateAt = 0.85;
    /// <summary>Seconds without a new start after the runtime refused one.</summary>
    public const double RefusedBackoffS = 10;
    /// <summary>Seconds the pacer stays out after an OS memory warning.</summary>
    public const double MemoryWarningHoldS = 60;

    private static GcPacer? _shared;
    private static int _users;

    private long _startedAtBytes;
    private bool _supported = true;
    private bool _lastStartOk;
    private long _holdUntil;                       // Stopwatch timestamp before which no region is started
    private ulong _tickedFrame = ulong.MaxValue;

    /// <summary>Allocation allowed per region (the largest budget any user asked for; a raise applies at the next start).</summary>
    public long BudgetBytes { get; private set; }
    /// <summary>Views using the process's pacer.</summary>
    public static int Users => _users;
    /// <summary>Regions started, planned rotations, regions the runtime ended before the plan (an allocation burst, or an explicit collection elsewhere), starts that were refused, and OS memory warnings handled.</summary>
    public int Starts { get; private set; }
    public int Rotations { get; private set; }
    public int EarlyExits { get; private set; }
    public int Refused { get; private set; }
    public int MemoryWarnings { get; private set; }
    /// <summary>Longest time one rotation (end region, start the next with its collection) took, milliseconds.</summary>
    public double RotationMsMax { get; private set; }

    private GcPacer(int budgetMb)
    {
        BudgetBytes = budgetMb * 1024L * 1024L;
        _startedAtBytes = GC.GetTotalAllocatedBytes(false);
    }

    /// <summary>The process's pacer for one more user.</summary>
    public static GcPacer Acquire(int budgetMb)
    {
        _shared ??= new GcPacer(budgetMb);
        _users++;
        long bytes = budgetMb * 1024L * 1024L;
        if (bytes > _shared.BudgetBytes) _shared.BudgetBytes = bytes;
        return _shared;
    }

    /// <summary>
    /// Drops one user; the last one ends the region and collects at once, finalizers included, so the collector is back on its own schedule with nothing owed. The collection is not
    /// optional: a region ended at quit with a budget of pending finalizable wrappers left the finalizer thread calling into the engine during its teardown (a destroyed mutex,
    /// <c>mutex lock failed</c>, abort). Here, inside <c>_ExitTree</c>, the engine is still whole.
    /// </summary>
    public void Release()
    {
        if (_users > 0) _users--;
        if (_users > 0) return;
        if (ReferenceEquals(_shared, this)) _shared = null;
        if (!InRegion) return;
        EndRegion();
        GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: false);
        GC.WaitForPendingFinalizers();
    }

    /// <summary>True while the runtime is inside a no-GC region.</summary>
    public bool InRegion => GCSettings.LatencyMode == GCLatencyMode.NoGCRegion;

    /// <summary>Once per process frame; a second call in the same frame (another view) does nothing.</summary>
    public void Tick()
    {
        if (!_supported) return;
        ulong frame = Godot.Engine.GetProcessFrames();
        if (frame == _tickedFrame) return;
        _tickedFrame = frame;
        if (InRegion)
        {
            if (GC.GetTotalAllocatedBytes(false) - _startedAtBytes < BudgetBytes * RotateAt) return;
            Rotate();
            return;
        }
        if (Stopwatch.GetTimestamp() < _holdUntil) return;
        if (_lastStartOk) EarlyExits++;   // we were in a region and the runtime left it
        Start();
    }

    /// <summary>Rotates now (end the region, collect, start the next) for a quiet moment the host knows about. Does nothing while off, on hold or outside a region.</summary>
    public void RotateNow()
    {
        if (_supported && InRegion) Rotate();
    }

    /// <summary>The OS asked for memory (<c>NOTIFICATION_OS_MEMORY_WARNING</c>): ends the region, collects everything and stays out for <see cref="MemoryWarningHoldS"/>.</summary>
    public void OnMemoryWarning()
    {
        MemoryWarnings++;
        EndRegion();
        GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true);
        _holdUntil = Stopwatch.GetTimestamp() + (long)(MemoryWarningHoldS * Stopwatch.Frequency);
    }

    private void Rotate()
    {
        long t0 = Stopwatch.GetTimestamp();
        EndRegion();
        // No explicit collection here: the start below runs a full blocking collection itself; a Collect(0) first would walk the handle tables twice.
        Rotations++;
        Start();
        double ms = (Stopwatch.GetTimestamp() - t0) * 1000.0 / Stopwatch.Frequency;
        if (ms > RotationMsMax) RotationMsMax = ms;
    }

    private void EndRegion()
    {
        if (!InRegion) return;
        try { GC.EndNoGCRegion(); }
        catch (InvalidOperationException) { /* the runtime ended it between the check and here */ }
    }

    private void Start()
    {
        _startedAtBytes = GC.GetTotalAllocatedBytes(false);
        _lastStartOk = false;
        try
        {
            if (GC.TryStartNoGCRegion(BudgetBytes)) { Starts++; _lastStartOk = true; return; }
            Refused++;
            _holdUntil = Stopwatch.GetTimestamp() + (long)(RefusedBackoffS * Stopwatch.Frequency);
        }
        catch (InvalidOperationException)
        {
            // Someone else (host code) holds a region: leave it alone and look again after the backoff.
            Refused++;
            _holdUntil = Stopwatch.GetTimestamp() + (long)(RefusedBackoffS * Stopwatch.Frequency);
        }
        catch (Exception e) when (e is ArgumentOutOfRangeException or NotSupportedException)
        {
            _supported = false;   // this runtime has no such region (or the size is out of range): stay out of the way
            Godot.GD.PushWarning($"[GcPacer] no-GC region of {BudgetBytes >> 20} MB not available ({e.GetType().Name}); the garbage collector runs as usual");
        }
    }
}
