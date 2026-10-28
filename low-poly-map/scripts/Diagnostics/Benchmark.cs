using System;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Benchmark mode: <c>$G --path . -- --benchmark=&lt;name&gt; [--seconds=S | --frames=N] [--soak=S] [--out=path.json] [--quit]</c> (see
/// <see cref="BenchmarkArgs"/>, <see cref="BenchmarkDef"/>). The camera flies a scripted path that is a pure function of the simulated
/// time (<see cref="IFlightPath"/>), so every run covers the same ground. The run has three phases: <b>settle</b> (the camera holds the
/// start pose until the library has no tiles queued, so a cold cache does not decide the result), <b>warm-up</b> (5 s of flight,
/// not measured) and <b>measure</b>. The result is one JSON file (<see cref="BenchmarkReport"/>) compared with the machine's baseline
/// (<see cref="BenchmarkBaseline"/>). With <c>--quit</c> the process exits 0 when the file is written (1 on an error, 2 for an unknown
/// benchmark). A soak run measures for the given seconds and reports the first and the last minute separately.
/// </summary>
public partial class Benchmark : Node
{
    public const double WarmupSeconds = 5.0, SettleMinSeconds = 3.0, SettleMaxSeconds = 90.0;
    const double MinuteSeconds = 60.0;
    const float PinnedFovDeg = 60f;
    const int PinnedHeightPx = 1080;
    static readonly Vector2I WindowSize = new(1600, 900);
    const double EarthRadiusM = 6371008.8;

    public enum Phase { Settle, Warmup, Measure, Done }

    readonly BenchmarkDef _def;
    readonly BenchmarkArgs _args;
    readonly IFlightPath _path;
    readonly WorldView _view;
    readonly DiagnosticsNode _diagnostics;
    FreeFlyCamera _camera = null!;
    Phase _phase = Phase.Settle;
    double _settleSeconds, _simSeconds, _measureStartSim, _nextReconfigure, _nextProgress = MinuteSeconds;
    int _quietSamples;
    FlightPose _pose, _lastPose;
    bool _havePose;

    public BenchmarkDef Def => _def;
    public BenchmarkArgs Args => _args;
    public IFlightPath Path => _path;
    public WorldView View => _view;
    public DiagnosticsNode Diagnostics => _diagnostics;
    public Phase Current => _phase;
    public bool Settled { get; private set; }
    public double SettleSeconds => _settleSeconds;
    /// <summary>Cold fill: seconds into the settle phase of the last tile upload (all layers) and how many tiles were uploaded by then. The camera holds the start pose, so this is how long the world takes to appear after a start.</summary>
    public double FillSeconds { get; private set; }
    public int FillTiles { get; private set; }
    int _lastUploaded;
    public FlightPose Pose => _pose;
    public int Reconfigures { get; private set; }
    public int ReconfigureFailures { get; private set; }

    /// <summary>The measured part: seconds of simulated flight since the warm-up ended.</summary>
    public double MeasuredSeconds => _phase >= Phase.Measure ? _simSeconds - _measureStartSim : 0;

    public PerfStats FirstMinute { get; } = new();
    public PerfStats LastMinute { get; } = new();
    /// <summary>Once-a-second series: the whole measurement; for a soak also the first minute, the last minute and everything after the first minute.</summary>
    public MetricSet AllSeries { get; } = new();
    public MetricSet FirstSeries { get; } = new();
    public MetricSet LastSeries { get; } = new();
    public MetricSet SettledSeries { get; } = new();
    public int GcGen0 { get; private set; }
    public int GcGen1 { get; private set; }
    public int GcGen2 { get; private set; }
    public long AllocatedBytes { get; private set; }
    /// <summary>The view's churn / upload counters when the measurement started (the report subtracts them from the end values).</summary>
    public ViewTotals StartTotals { get; private set; }
    /// <summary>How busy the machine was before the run and after it (load average, other busy processes): numbers from a loaded machine are noisy.</summary>
    public string NoiseStart { get; private set; } = "";
    public string NoiseEnd { get; private set; } = "";
    /// <summary>The library's version string ("simworld-godot 0.5.0 (build release @...)") and its build flavour, read when the measurement starts.</summary>
    public string LibraryVersion { get; private set; } = "";
    public string LibraryFlavour => MachineInfo.LibraryFlavour(LibraryVersion);

    Benchmark(BenchmarkDef def, BenchmarkArgs args, WorldView view, DiagnosticsNode diagnostics)
    {
        Name = "Benchmark";
        ProcessMode = ProcessModeEnum.Always;
        ProcessPriority = -500;   // after the diagnostics sample, before the camera and the view read the pose
        _def = def;
        _args = args;
        _path = def.Path();
        _view = view;
        _diagnostics = diagnostics;
    }

    /// <summary>Starts the benchmark named on the command line; null when none is asked for (an unknown name is an error, exit code 2 with <c>--quit</c>).</summary>
    public static Benchmark? Attach(WorldView view, DiagnosticsNode diagnostics, Node parent)
    {
        var args = BenchmarkArgs.Parse();
        if (args == null) return null;
        var def = BenchmarkDef.Find(args.Name);
        if (def == null)
        {
            GD.PushError($"[Benchmark] unknown benchmark '{args.Name}'; one of: {BenchmarkDef.Names}");
            if (args.Quit) parent.GetTree().Quit(2);
            return null;
        }
        if (view.Camera is not FreeFlyCamera)
        {
            GD.PushError("[Benchmark] needs the FreeFlyCamera (scripted placement)");
            if (args.Quit) parent.GetTree().Quit(1);
            return null;
        }
        var bench = new Benchmark(def, args, view, diagnostics);
        parent.CallDeferred(Node.MethodName.AddChild, bench);
        return bench;
    }

    /// <summary><c>fullscreen</c> (also the default: a macOS window is paced by the compositor, so windowed frame times vary from run to run) or
    /// <c>WxH</c> for a window of that size (<see cref="WindowSize"/> if unparsable); the result's machine block records the size used.</summary>
    static void ApplyWindow(string spec)
    {
        if (spec == "" || spec == "fullscreen")
        {
            DisplayServer.WindowSetMode(DisplayServer.WindowMode.Fullscreen);
            return;
        }
        var parts = spec.Split('x');
        var size = parts.Length == 2 && int.TryParse(parts[0], out int w) && int.TryParse(parts[1], out int h) && w > 0 && h > 0
            ? new Vector2I(w, h) : WindowSize;
        DisplayServer.WindowSetSize(size);
    }

    public override void _Ready()
    {
        _camera = (FreeFlyCamera)_view.Camera!;
        _camera.Scripted = true;
        if (!_args.Vsync) DisplayServer.WindowSetVsyncMode(DisplayServer.VSyncMode.Disabled);   // frame time must show the headroom, not the display rate
        ApplyWindow(_args.Window);
        // A window hidden behind others is paced by the compositor (about the display rate) whatever the vsync mode says: keep it on top.
        DisplayServer.WindowSetFlag(DisplayServer.WindowFlags.AlwaysOnTop, true);
        DisplayServer.WindowMoveToForeground();
        if (GetTree().Root.FindChild("TimeOfDay", true, false) is TimeOfDay clock)
        {
            clock.TimeSpeed = 0;
            clock.OverrideNight = false;
            clock.LocalHour = _def.Hour;
        }
        // The library's view is pinned like the self-tests (60 deg, 1080 px) unless the path zooms or --view= was given.
        if (!HasArg("--view=")) _view.SetViewOverride(_path.VariesFov ? null : PinnedFovDeg, PinnedHeightPx);
        _nextReconfigure = _def.ReconfigureEveryS;
        NoiseStart = MachineInfo.NoiseJson();
        _diagnostics.Sampled += OnSampled;
        _diagnostics.StatusLine = StatusLine;
        _view.Reconfigured += OnReconfigured;
        GD.Print($"[Benchmark] {_def.Name}: {_path.Describe}; hour {_def.Hour:0.#}; {(_args.SoakSeconds > 0 ? $"soak {_args.SoakSeconds:0} s" : _args.Frames > 0 ? $"{_args.Frames} frames" : $"{_args.Seconds:0} s")}");
    }

    static bool HasArg(string prefix) => Array.Exists(OS.GetCmdlineUserArgs(), a => a.StartsWith(prefix, StringComparison.Ordinal));

    /// <summary>The window was closed (or the process was asked to terminate) before the result was written: say so and exit 3, not a silent 0.</summary>
    public override void _Notification(int what)
    {
        if (what != NotificationWMCloseRequest || _phase == Phase.Done) return;
        GD.PushError($"[Benchmark] aborted by a close request after {MeasuredSeconds:0} s of {(_args.Frames > 0 ? _args.Frames + " frames" : _args.Seconds + " s")} measured; no result written");
        GetTree().Quit(3);
    }

    public override void _ExitTree()
    {
        _diagnostics.Sampled -= OnSampled;
        _diagnostics.StatusLine = null;
        _view.Reconfigured -= OnReconfigured;
    }

    public override void _Process(double delta)
    {
        if (_phase == Phase.Done) return;
        if (_phase == Phase.Settle)
        {
            _settleSeconds += delta;
            int uploaded = _view.ReadyTotal + _view.TerrainReadyTotal + _view.LandmarkReadyTotal;
            if (uploaded != _lastUploaded) { _lastUploaded = uploaded; FillSeconds = _settleSeconds; FillTiles = uploaded; }
        }
        else _simSeconds += delta;
        Place(_path.At(_simSeconds), delta);
        switch (_phase)
        {
            case Phase.Settle when _quietSamples >= 2 && _settleSeconds >= SettleMinSeconds || _settleSeconds >= SettleMaxSeconds:
                Settled = _quietSamples >= 2;
                if (!Settled) GD.PushWarning($"[Benchmark] the world did not settle within {SettleMaxSeconds:0} s; measuring anyway");
                _phase = Phase.Warmup;
                break;
            case Phase.Warmup when _simSeconds >= WarmupSeconds:
                BeginMeasure();
                break;
            case Phase.Measure:
                Measure();
                break;
        }
        if (_phase >= Phase.Warmup) ReconfigureOnTimer();
    }

    /// <summary>Places the camera: the path's east / north through the library's geodetic conversion (so origin shifts and reconfigures need no bookkeeping here), height above the ground under it.</summary>
    void Place(FlightPose pose, double dt)
    {
        double lat = _view.OriginLat + pose.North / EarthRadiusM * (180.0 / Math.PI);
        double lon = _view.OriginLon + pose.East / (EarthRadiusM * Math.Cos(_view.OriginLat * Math.PI / 180.0)) * (180.0 / Math.PI);
        if (!_view.Streamer.IsAvailable) return;   // no library
        var local = _view.Streamer.GeodeticToLocal(lat, lon, 0);   // exactly (0, 0, 0) at the origin itself: that is a valid answer
        var velocity = Vector3.Zero;
        if (_havePose && dt > 1e-6)
            velocity = new Vector3((float)((pose.East - _lastPose.East) / dt), (float)((pose.AglM - _lastPose.AglM) / dt), (float)(-(pose.North - _lastPose.North) / dt));
        _camera.Place(new Vector3(local.X, (float)(_view.CameraGroundHeight + pose.AglM), local.Z), velocity, (float)-pose.HeadingRad, (float)pose.PitchRad, (float)pose.FovDeg);
        _lastPose = _pose = pose;
        _havePose = true;
    }

    void BeginMeasure()
    {
        PerfMonitor.Reset();
        _diagnostics.ResetCosts();
        GcGen0 = GC.CollectionCount(0);
        GcGen1 = GC.CollectionCount(1);
        GcGen2 = GC.CollectionCount(2);
        AllocatedBytes = GC.GetTotalAllocatedBytes();
        var stats = new StreamerStats();
        _view.Streamer.DebugStats(stats);
        LibraryVersion = stats.Version;
        StartTotals = ViewTotals.Read(_view);
        _measureStartSim = _simSeconds;
        _phase = Phase.Measure;
        GD.Print($"[Benchmark] measuring ({(Settled ? "settled" : "not settled")} after {_settleSeconds:0.0} s)");
    }

    void Measure()
    {
        double t = MeasuredSeconds;
        var sample = PerfMonitor.Last;
        if (_args.SoakSeconds > 0)
        {
            if (t < MinuteSeconds) FirstMinute.Add(sample);
            if (t >= _args.SoakSeconds - MinuteSeconds) LastMinute.Add(sample);
        }
        if (t >= _nextProgress)
        {
            _nextProgress += MinuteSeconds;
            GD.Print($"[Benchmark] {t:0} s measured, {PerfMonitor.Session.AvgFps:0} fps, p95 {PerfMonitor.Session.P95Ms:0.0} ms");
        }
        bool done = _args.Frames > 0 ? PerfMonitor.Session.Frames >= _args.Frames : t >= _args.Seconds;
        if (done) Finish();
    }

    /// <summary>One once-a-second reading: settle detection, then the series of the measured part.</summary>
    void OnSampled(double seconds, double[] values)
    {
        if (_phase == Phase.Settle)
        {
            bool quiet = values[(int)Metric.TilesQueued] == 0 && values[(int)Metric.TilesVector] > 0;
            _quietSamples = quiet ? _quietSamples + 1 : 0;
        }
        else if (_phase == Phase.Measure)
        {
            double t = MeasuredSeconds;
            AllSeries.Add(seconds, values);
            if (_args.SoakSeconds <= 0) return;
            if (t < MinuteSeconds) FirstSeries.Add(seconds, values);
            else SettledSeries.Add(seconds, values);
            if (t >= _args.SoakSeconds - MinuteSeconds) LastSeries.Add(seconds, values);
        }
    }

    /// <summary>Applies the reconfigure benchmark's two settings alternately, every <see cref="BenchmarkDef.ReconfigureEveryS"/> seconds of flight.</summary>
    void ReconfigureOnTimer()
    {
        if (_def.ReconfigureEveryS <= 0 || _simSeconds < _nextReconfigure || _view.ReconfigurePending) return;
        _nextReconfigure = _simSeconds + _def.ReconfigureEveryS;
        double maxDistance = Reconfigures % 2 == 0 ? 15000.0 : 25000.0;
        _view.ApplyProfileOverrides(new Godot.Collections.Dictionary { ["bands"] = new Godot.Collections.Dictionary { ["max_distance_m"] = maxDistance } });
        Reconfigures++;
    }

    void OnReconfigured(bool ok, string error)
    {
        if (ok) return;
        ReconfigureFailures++;
        GD.PushError($"[Benchmark] reconfigure rejected: {error}");
    }

    string StatusLine() => $"bench {_def.Name} {_phase} {(_phase == Phase.Settle ? _settleSeconds : MeasuredSeconds):0} s";

    void Finish()
    {
        _phase = Phase.Done;
        NoiseEnd = MachineInfo.NoiseJson();
        string path = BenchmarkFiles.OutputPath(_args.Out, _def.Name);
        try
        {
            string json = BenchmarkReport.Build(this, path);
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.WriteAllText(path, json + "\n");
            if (_args.UpdateBaseline)
                GD.Print("[Benchmark] baseline written: " + BenchmarkFiles.Update(BenchmarkFiles.BaselineKey(_def.Name, LibraryFlavour), BenchmarkReport.BaselineEntry(this)));
        }
        catch (Exception e) when (e is System.IO.IOException or UnauthorizedAccessException)
        {
            GD.PushError($"[Benchmark] result not written: {e.Message}");
            if (_args.Quit) GetTree().Quit(1);
            return;
        }
        var s = PerfMonitor.Session;
        GD.Print($"[Benchmark] {_def.Name}: {s.Frames} frames, avg {s.AvgFps:F1} fps, p95 {s.P95Ms:F2} ms, 1% low {s.OnePercentLowFps:F1} fps, " +
                 $"gpu {(s.GpuFrames == 0 ? "not measured" : s.MeanGpuMs.ToString("F2") + " ms")} -> {path}");
        if (_args.Quit) GetTree().Quit(0);
    }
}
