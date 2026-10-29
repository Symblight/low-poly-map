#nullable enable
using System;

namespace SimWorld;

/// <summary>The places of the per-frame path whose managed allocation <see cref="AllocProbe"/> attributes (exclusive of nested sites).</summary>
public enum AllocSite
{
    UpdateCamera,
    GroundHeight,
    UpdateView,
    TakeOriginShift,
    PollReadyCall,
    ReadyParse,
    /// <summary>Copying the four light arrays of a ready tile into managed arrays (part of the parse, booked separately).</summary>
    ReadyLights,
    UploadVector,
    UploadTerrain,
    UploadLandmark,
    SetSprites,
    PollFades,
    FadeApply,
    PollUnload,
    UnloadApply,
    LodRead,
    CullRefresh,
    PoolMaintenance,
    SpriteScale,
    DebugStats,
    ShaderGlobals,
    /// <summary>Host nodes of the bench, booked by the host: its UI nodes, the benchmark driver and the once-a-second sampler.</summary>
    HostBench,
    HostUi,
    FadeRead,
    HostSampler,
    /// <summary>What <c>WorldView._Process</c> allocates outside the sites above.</summary>
    ViewFrame,
    Count,
}

/// <summary>
/// Attributes the main thread's managed allocation to the sites of the frame loop, to rank what feeds the GC. A <see cref="Scope"/> reads
/// <c>GC.GetAllocatedBytesForCurrentThread</c> on entry and exit and books the difference minus what nested scopes booked, so the sites add up without
/// double counting. Off by default (a scope is then one static read and a branch); the bench's diagnostics node turns it on. Main thread only. Allocation-free.
/// </summary>
public static class AllocProbe
{
    private static readonly long[] Bytes = new long[(int)AllocSite.Count];
    private static readonly long[] Calls = new long[(int)AllocSite.Count];
    private static long _nested;

    /// <summary>When false every scope is a no-op.</summary>
    public static bool Enabled { get; set; }

    public static Scope Measure(AllocSite site)
    {
        if (!Enabled) return default;
        long saved = _nested;
        _nested = 0;
        return new Scope(site, GC.GetAllocatedBytesForCurrentThread(), saved, true);
    }

    /// <summary>Bytes the site allocated itself (nested sites excluded) since the last <see cref="Reset"/>.</summary>
    public static long BytesOf(AllocSite site) => Bytes[(int)site];

    /// <summary>Times the site ran since the last <see cref="Reset"/>.</summary>
    public static long CallsOf(AllocSite site) => Calls[(int)site];

    public static void Reset()
    {
        Array.Clear(Bytes);
        Array.Clear(Calls);
        _nested = 0;
    }

    public readonly struct Scope : IDisposable
    {
        private readonly AllocSite _site;
        private readonly long _start, _savedNested;
        private readonly bool _on;

        internal Scope(AllocSite site, long start, long savedNested, bool on)
        {
            _site = site;
            _start = start;
            _savedNested = savedNested;
            _on = on;
        }

        public void Dispose()
        {
            if (!_on) return;
            long total = GC.GetAllocatedBytesForCurrentThread() - _start;
            Bytes[(int)_site] += total - _nested;
            Calls[(int)_site]++;
            _nested = _savedNested + total;
        }
    }
}
