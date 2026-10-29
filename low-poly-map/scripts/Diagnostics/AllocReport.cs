using System;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// The managed allocation of the main thread over the measured part, by site (<see cref="AllocProbe"/>): the thing that feeds the GC. Started with the measurement,
/// written as <c>gc.main_thread</c> next to the collection counts. "unattributed" is what no site claims (Godot callbacks of the host's own nodes, the diagnostics, the engine's marshalling between sites).
/// </summary>
public sealed class AllocReport
{
    long _threadStart, _processStart;

    public void Begin()
    {
        AllocProbe.Enabled = true;
        AllocProbe.Reset();
        _threadStart = GC.GetAllocatedBytesForCurrentThread();
        _processStart = GC.GetTotalAllocatedBytes();
    }

    /// <summary><c>"main_thread"</c> object: totals, and per site bytes, calls, bytes per call and bytes per second, largest first.</summary>
    public void WriteFields(JsonText j, double seconds, long frames)
    {
        long thread = GC.GetAllocatedBytesForCurrentThread() - _threadStart;
        long process = GC.GetTotalAllocatedBytes() - _processStart;
        long attributed = 0;
        var order = new int[(int)AllocSite.Count];
        for (int i = 0; i < order.Length; i++) { order[i] = i; attributed += AllocProbe.BytesOf((AllocSite)i); }
        Array.Sort(order, (a, b) => AllocProbe.BytesOf((AllocSite)b).CompareTo(AllocProbe.BytesOf((AllocSite)a)));
        double s = Math.Max(seconds, 1e-3), f = Math.Max(frames, 1);
        j.BeginObject("main_thread");
        j.Field("bytes", thread);
        j.Field("mb_per_s", Math.Round(thread / 1048576.0 / s, 4));
        j.Field("bytes_per_frame", Math.Round(thread / f, 1));
        j.Field("other_threads_bytes", process - thread);
        j.Field("unattributed_bytes", thread - attributed);
        j.Field("unattributed_per_frame", Math.Round((thread - attributed) / f, 1));
        j.BeginObject("sites");
        foreach (int i in order)
        {
            var site = (AllocSite)i;
            long bytes = AllocProbe.BytesOf(site), calls = AllocProbe.CallsOf(site);
            if (calls == 0) continue;
            j.BeginObject(site.ToString());
            j.Field("bytes", bytes);
            j.Field("calls", calls);
            j.Field("bytes_per_call", Math.Round(bytes / (double)calls, 1));
            j.Field("kb_per_s", Math.Round(bytes / 1024.0 / s, 2));
            j.End();
        }
        j.End();
        j.End();
    }
}
