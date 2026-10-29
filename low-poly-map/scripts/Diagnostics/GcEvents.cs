using System;
using System.Runtime;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// One line per garbage collection that fell into the measured part: its generation, kind, pause, how much survived, the heap and what was waiting for finalization
/// (<c>GC.GetGCMemoryInfo</c>, read in the frame after the collection; if two collections fall into one frame only the last is described). Says whether the long pauses are gen0 / gen1
/// collections (too much survives, too many finalizable objects) or gen2 (the large-object heap). The first <see cref="Max"/> events are kept; preallocated, the poll allocates only when a collection happened.
/// </summary>
public sealed class GcEvents
{
    public const int Max = 64;

    struct Event
    {
        public double AtS, PauseMs;
        public long Index, PromotedBytes, HeapBytes, FragmentedBytes, FinalizationPending, Gen0Bytes, LohBytes;
        public int Generation;
        public bool Compacted, Concurrent;
    }

    readonly Event[] _events = new Event[Max];
    int _count, _lastCollections;

    public void Reset()
    {
        _count = 0;
        _lastCollections = GC.CollectionCount(0);
    }

    /// <summary>Call once per frame, outside the frame's allocation accounting.</summary>
    public void Poll(double atSeconds)
    {
        int collections = GC.CollectionCount(0);
        if (collections == _lastCollections) return;
        _lastCollections = collections;
        if (_count >= Max) return;
        var info = GC.GetGCMemoryInfo(GCKind.Any);
        double pause = info.PauseDurations.Length > 0 ? info.PauseDurations[0].TotalMilliseconds : 0;
        if (info.PauseDurations.Length > 1) pause += info.PauseDurations[1].TotalMilliseconds;
        var g = info.GenerationInfo;
        _events[_count++] = new Event
        {
            AtS = atSeconds, PauseMs = pause, Index = info.Index, Generation = info.Generation, Compacted = info.Compacted, Concurrent = info.Concurrent,
            PromotedBytes = info.PromotedBytes, HeapBytes = info.HeapSizeBytes, FragmentedBytes = info.FragmentedBytes, FinalizationPending = info.FinalizationPendingCount,
            Gen0Bytes = g.Length > 0 ? g[0].SizeBeforeBytes : 0, LohBytes = g.Length > 3 ? g[3].SizeBeforeBytes : 0,
        };
    }

    /// <summary><c>"events":[…]</c> as an object of <c>gc_1</c>…: the collections in order.</summary>
    public void WriteFields(JsonText j)
    {
        j.BeginObject("events");
        for (int i = 0; i < _count; i++)
        {
            var e = _events[i];
            j.BeginObject("gc_" + (i + 1));
            j.Field("at_s", Math.Round(e.AtS, 2));
            j.Field("generation", e.Generation);
            j.Field("pause_ms", Math.Round(e.PauseMs, 3));
            j.Field("compacted", e.Compacted);
            j.Field("concurrent", e.Concurrent);
            j.Field("promoted_kb", e.PromotedBytes / 1024);
            j.Field("heap_mb", Math.Round(e.HeapBytes / 1048576.0, 2));
            j.Field("gen0_before_kb", e.Gen0Bytes / 1024);
            j.Field("loh_before_kb", e.LohBytes / 1024);
            j.Field("fragmented_kb", e.FragmentedBytes / 1024);
            j.Field("finalization_pending", e.FinalizationPending);
            j.End();
        }
        j.End();
    }
}
