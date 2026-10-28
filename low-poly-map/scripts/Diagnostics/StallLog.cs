using System;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Attributes the long frames. For every frame it takes the GC pause time that fell into it (<c>GC.GetTotalPauseDuration</c>), the wall and the thread CPU time of each
/// library call (<see cref="StreamerTimings"/> with <see cref="StreamerTimings.MeasureThreadCpu"/>) and the <see cref="PerfMarker"/> categories, and keeps the
/// <see cref="Top"/> longest frames since the last <see cref="Reset"/> with all of it. A call whose wall time is far above its CPU time was descheduled or suspended (a
/// GC pause stops every managed thread, so a native call in flight shows the pause as wall time too); a call with equal wall and CPU time did the work. Main thread only; the
/// entries are preallocated, so recording allocates nothing.
/// </summary>
public sealed class StallLog
{
    public const int Top = 3;
    const int Calls = StreamerTimings.Count;
    static readonly string[] CallNames = { "update_camera", "poll_ready", "poll_fades", "poll_unload", "take_origin_shift", "debug_stats" };

    sealed class Entry
    {
        public double AtS, FrameMs, ProcessWallMs, GcPauseMs, GpuMs, RenderCpuMs;
        public int DrawCalls;
        public readonly double[] WallMs = new double[Calls], CpuMs = new double[Calls], MarkerMs = new double[PerfMarker.Count];
        public readonly int[] CallCount = new int[Calls];
    }

    readonly Entry[] _worst = new Entry[Top];
    int _used;
    TimeSpan _lastPause;

    /// <summary>GC pause time since the last <see cref="Reset"/>, ms, and the frames in which a pause of at least 1 ms fell.</summary>
    public double TotalGcPauseMs { get; private set; }
    public int GcPauseFrames { get; private set; }

    public StallLog()
    {
        for (int i = 0; i < Top; i++) _worst[i] = new Entry();
    }

    public void Reset()
    {
        _used = 0;
        TotalGcPauseMs = 0;
        GcPauseFrames = 0;
        _lastPause = GC.GetTotalPauseDuration();
    }

    /// <summary>Records the frame that just ended (<paramref name="s"/> = its sample, <paramref name="timings"/> = the view's closed frame). Call it right after <see cref="PerfMonitor.Frame"/>.</summary>
    public void Record(in PerfSample s, StreamerTimings timings, double atSeconds)
    {
        var pause = GC.GetTotalPauseDuration();
        double pauseMs = (pause - _lastPause).TotalMilliseconds;
        _lastPause = pause;
        TotalGcPauseMs += pauseMs;
        if (pauseMs >= 1.0) GcPauseFrames++;

        Entry e;
        if (_used < Top) e = _worst[_used++];
        else
        {
            int min = 0;
            for (int i = 1; i < Top; i++) if (_worst[i].FrameMs < _worst[min].FrameMs) min = i;
            if (s.FrameMs <= _worst[min].FrameMs) return;
            e = _worst[min];
        }
        e.AtS = atSeconds; e.FrameMs = s.FrameMs; e.ProcessWallMs = s.ProcessWallMs; e.GcPauseMs = pauseMs; e.GpuMs = s.GpuMs; e.RenderCpuMs = s.RenderCpuMs; e.DrawCalls = s.DrawCalls;
        for (int c = 0; c < Calls; c++)
        {
            var call = (StreamerCall)c;
            e.WallMs[c] = timings.FrameMs(call);
            e.CpuMs[c] = timings.FrameCpuMs(call);
            e.CallCount[c] = timings.FrameCalls(call);
        }
        for (int m = 0; m < PerfMarker.Count; m++) e.MarkerMs[m] = PerfMarker.LastFrameMs((PerfCat)m);
    }

    /// <summary><c>"stalls":{gc totals, "worst_1":{…}, …}</c>, the longest first.</summary>
    public void WriteFields(JsonText j)
    {
        j.BeginObject("stalls");
        j.Field("gc_pause_total_ms", Math.Round(TotalGcPauseMs, 3));
        j.Field("gc_pause_frames_over_1ms", GcPauseFrames);
        j.Field("thread_cpu_supported", ThreadClock.Supported);
        j.Field("note", "per call: wall_ms far above cpu_ms = the thread was descheduled / suspended (GC pause, busy machine) inside the call; equal = the call worked");
        var order = new int[_used];
        for (int i = 0; i < _used; i++) order[i] = i;
        Array.Sort(order, (a, b) => _worst[b].FrameMs.CompareTo(_worst[a].FrameMs));
        for (int rank = 0; rank < _used; rank++)
        {
            var e = _worst[order[rank]];
            j.BeginObject("worst_" + (rank + 1));
            j.Field("at_s", Math.Round(e.AtS, 2));
            j.Field("frame_ms", Math.Round(e.FrameMs, 3));
            j.Field("process_wall_ms", Math.Round(e.ProcessWallMs, 3));
            j.Field("gc_pause_ms", Math.Round(e.GcPauseMs, 3));
            j.Field("render_cpu_ms", Math.Round(e.RenderCpuMs, 3));
            j.Field("draw_calls", e.DrawCalls);
            j.BeginObject("calls");
            for (int c = 0; c < Calls; c++)
            {
                if (e.CallCount[c] == 0) continue;
                j.BeginObject(CallNames[c]);
                j.Field("wall_ms", Math.Round(e.WallMs[c], 3));
                j.Field("cpu_ms", Math.Round(e.CpuMs[c], 3));
                j.Field("calls", e.CallCount[c]);
                j.End();
            }
            j.End();
            j.BeginObject("markers_ms");
            for (int m = 0; m < PerfMarker.Count; m++)
                if (e.MarkerMs[m] >= 0.05) j.Field(PerfMarker.Name((PerfCat)m), Math.Round(e.MarkerMs[m], 3));
            j.End();
            j.End();
        }
        j.End();
    }
}
