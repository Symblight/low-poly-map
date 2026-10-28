using System.Diagnostics;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// The session's frame sampler (the same design as the SimPlane game's <c>PerfMonitor</c>). <see cref="DiagnosticsNode"/> calls
/// <see cref="Frame"/> first thing every process frame: it measures the wall time since the previous call, reads Godot's monitors and
/// the root viewport's measured render times into <see cref="Last"/>, feeds <see cref="Session"/> (since start or
/// <see cref="Reset"/>) and the one-second <see cref="Window"/>, keeps the last <see cref="GraphFrames"/> frame times for the overlay
/// graph and closes the <see cref="PerfMarker"/> frame. Nodes that run later in the frame (overlays, the benchmark) read
/// <see cref="Last"/>. No allocation per frame.
/// </summary>
public static class PerfMonitor
{
    public const int GraphFrames = 300;
    const double WindowSeconds = 1.0;

    static readonly float[] Ring = new float[GraphFrames];
    static int _ringHead;
    static int _ringCount;
    static PerfStats _window = new(), _lastWindow = new();
    static long _lastTimestamp;
    static Rid _viewport;
    static bool _measuring;
    static long _processStart;
    static double _processWallMs;

    /// <summary>Everything since the session started or the last <see cref="Reset"/>.</summary>
    public static PerfStats Session { get; } = new();

    /// <summary>The last complete one-second window.</summary>
    public static PerfStats Window => _lastWindow;

    /// <summary>The latest sample (this frame, once <see cref="Frame"/> ran).</summary>
    public static PerfSample Last;

    /// <summary>Turns on render-time measurement for <paramref name="viewport"/> (the root viewport).</summary>
    public static void Attach(Viewport viewport)
    {
        _viewport = viewport.GetViewportRid();
        RenderingServer.ViewportSetMeasureRenderTime(_viewport, true);
        _measuring = true;
    }

    public static void Frame()
    {
        long now = Stopwatch.GetTimestamp();
        if (_lastTimestamp == 0)
        {
            _lastTimestamp = now;
            return;
        }
        var s = new PerfSample
        {
            FrameMs = (now - _lastTimestamp) * 1000.0 / Stopwatch.Frequency,
            ProcessMs = Performance.GetMonitor(Performance.Monitor.TimeProcess) * 1000.0,
            PhysicsMs = Performance.GetMonitor(Performance.Monitor.TimePhysicsProcess) * 1000.0,
            DrawCalls = (int)Performance.GetMonitor(Performance.Monitor.RenderTotalDrawCallsInFrame),
            Primitives = (long)Performance.GetMonitor(Performance.Monitor.RenderTotalPrimitivesInFrame),
            VideoMemBytes = (long)Performance.GetMonitor(Performance.Monitor.RenderVideoMemUsed),
            StaticMemBytes = (long)Performance.GetMonitor(Performance.Monitor.MemoryStatic),
            ProcessWallMs = _processWallMs,
        };
        _lastTimestamp = now;
        if (_measuring)
        {
            s.GpuMs = RenderingServer.ViewportGetMeasuredRenderTimeGpu(_viewport);
            s.RenderCpuMs = RenderingServer.ViewportGetMeasuredRenderTimeCpu(_viewport);
        }
        Add(s);
    }

    /// <summary>Marks the start of the frame's process callbacks (the head node, after <see cref="Frame"/>).</summary>
    public static void BeginProcess() => _processStart = Stopwatch.GetTimestamp();

    /// <summary>Marks the end of the frame's process callbacks (the tail node); the next <see cref="Frame"/> reports it.</summary>
    public static void EndProcess() => _processWallMs = (Stopwatch.GetTimestamp() - _processStart) * 1000.0 / Stopwatch.Frequency;

    /// <summary>Feeds one sample to the session, the window, the graph and closes the marker frame.</summary>
    static void Add(in PerfSample s)
    {
        Last = s;
        Session.Add(s);
        _window.Add(s);
        if (_window.DurationS >= WindowSeconds)
        {
            (_window, _lastWindow) = (_lastWindow, _window);
            _window.Reset();
        }
        Ring[_ringHead] = (float)s.FrameMs;
        _ringHead = (_ringHead + 1) % GraphFrames;
        if (_ringCount < GraphFrames) _ringCount++;
        PerfMarker.EndFrame();
    }

    /// <summary>Frame time <paramref name="age"/> frames ago (0 = the latest), ms; 0 beyond what is stored.</summary>
    public static float GraphSample(int age)
    {
        if (age < 0 || age >= _ringCount) return 0f;
        return Ring[(_ringHead - 1 - age + GraphFrames * 2) % GraphFrames];
    }

    public static int GraphCount => _ringCount;

    /// <summary>Clears the session stats and the marker histograms (the benchmark does it when the warm-up ends).</summary>
    public static void Reset()
    {
        Session.Reset();
        PerfMarker.Reset();
    }
}
