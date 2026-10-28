using System;
using System.Diagnostics;

namespace SimWorld;

/// <summary>The library calls <see cref="StreamerTimings"/> measures (every caller of the facade counts, not only <see cref="WorldView"/>).</summary>
public enum StreamerCall
{
    /// <summary><c>update_camera</c> and <c>update_camera_geodetic</c>.</summary>
    UpdateCamera,
    PollReady,
    PollFades,
    PollUnload,
    TakeOriginShift,
    DebugStats,
}

/// <summary>
/// Wall time the main thread spends inside the <c>WorldStreamer</c> calls, summed per frame. The facade wraps each call in
/// <see cref="Time"/> (a struct scope, no allocation); a host that wants per-frame numbers calls <see cref="EndFrame"/> once per
/// frame (<see cref="WorldView.EndTimingFrame"/>) and reads the totals of the frame that just ended through <see cref="FrameMs"/>.
/// Without a reader the counters only add up. Main thread only.
/// </summary>
public sealed class StreamerTimings
{
    public const int Count = 6;
    private static readonly double MsPerTick = 1000.0 / Stopwatch.Frequency;
    private readonly long[] _openTicks = new long[Count];
    private readonly int[] _openCalls = new int[Count];
    private readonly long[] _frameTicks = new long[Count];
    private readonly int[] _frameCalls = new int[Count];

    public Scope Time(StreamerCall call) => new(this, call, Stopwatch.GetTimestamp());

    /// <summary>Closes the frame: the totals collected since the last call become <see cref="FrameMs"/> / <see cref="FrameCalls"/>.</summary>
    public void EndFrame()
    {
        Array.Copy(_openTicks, _frameTicks, Count);
        Array.Copy(_openCalls, _frameCalls, Count);
        Array.Clear(_openTicks);
        Array.Clear(_openCalls);
    }

    /// <summary>Time spent in <paramref name="call"/> during the last closed frame, ms.</summary>
    public double FrameMs(StreamerCall call) => _frameTicks[(int)call] * MsPerTick;

    /// <summary>Number of <paramref name="call"/>s in the last closed frame.</summary>
    public int FrameCalls(StreamerCall call) => _frameCalls[(int)call];

    public readonly struct Scope : IDisposable
    {
        private readonly StreamerTimings _owner;
        private readonly StreamerCall _call;
        private readonly long _start;

        internal Scope(StreamerTimings owner, StreamerCall call, long start)
        {
            _owner = owner;
            _call = call;
            _start = start;
        }

        public void Dispose()
        {
            _owner._openTicks[(int)_call] += Stopwatch.GetTimestamp() - _start;
            _owner._openCalls[(int)_call]++;
        }
    }
}
