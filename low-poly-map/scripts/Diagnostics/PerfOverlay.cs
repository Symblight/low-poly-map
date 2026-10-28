using System;
using System.Globalization;
using System.Text;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// F4 performance overlay, like Unreal's <c>stat</c> and the SimPlane game's F3 overlay: F4 cycles off, FPS + frame ms, + frame-time
/// graph (last 300 frames, 33.3 ms budget line, 16.7 ms line), + the full table (percentiles since the run started or the benchmark's
/// warm-up ended, GPU, markers, memory, library numbers). <c>--perf-overlay=fps|graph|table</c> starts with it shown. Text refreshes at
/// 4 Hz; its own update and the graph draw are the <see cref="PerfCat.Overlay"/> marker.
/// </summary>
public partial class PerfOverlay : CanvasLayer
{
    public enum Level { Off, Fps, Graph, Table }

    const float GraphWidth = 300f, GraphHeight = 90f;
    const string StartArg = "--perf-overlay=";
    static readonly CultureInfo Ci = CultureInfo.InvariantCulture;

    readonly DiagnosticsNode _diagnostics;
    readonly StringBuilder _sb = new(1024);
    Level _level = Level.Off;
    PanelContainer _panel = null!;
    Label _text = null!;
    PerfGraph _graph = null!;
    double _textAge = 1e9;

    public PerfOverlay(DiagnosticsNode diagnostics)
    {
        _diagnostics = diagnostics;
        Name = "PerfOverlay";
        Layer = 126;
        ProcessMode = ProcessModeEnum.Always;
    }

    public override void _Ready()
    {
        _panel = new PanelContainer
        {
            Name = "Panel",
            MouseFilter = Control.MouseFilterEnum.Ignore,
            AnchorLeft = 1f, AnchorRight = 1f,
            OffsetLeft = -360f, OffsetRight = -8f, OffsetTop = 24f,
            GrowHorizontal = Control.GrowDirection.Begin,
        };
        var style = new StyleBoxFlat { BgColor = new Color(0f, 0f, 0f, 0.6f) };
        style.SetContentMarginAll(6f);
        _panel.AddThemeStyleboxOverride("panel", style);
        var box = new VBoxContainer { MouseFilter = Control.MouseFilterEnum.Ignore };
        _text = new Label { MouseFilter = Control.MouseFilterEnum.Ignore };
        _text.AddThemeFontSizeOverride("font_size", 12);
        _text.AddThemeFontOverride("font", new SystemFont { FontNames = new[] { "Menlo", "Monaco", "Courier New", "monospace" } });
        _graph = new PerfGraph { CustomMinimumSize = new Vector2(GraphWidth, GraphHeight), MouseFilter = Control.MouseFilterEnum.Ignore };
        box.AddChild(_text);
        box.AddChild(_graph);
        _panel.AddChild(box);
        AddChild(_panel);
        SetLevel(StartLevel());
    }

    static Level StartLevel()
    {
        foreach (var arg in OS.GetCmdlineUserArgs())
            if (arg.StartsWith(StartArg, StringComparison.Ordinal) && Enum.TryParse(arg.Substring(StartArg.Length), true, out Level level) && Enum.IsDefined(level))
                return level;
        return Level.Off;
    }

    void SetLevel(Level level)
    {
        _level = level;
        _panel.Visible = level != Level.Off;
        _graph.Visible = level >= Level.Graph;
        _textAge = 1e9;   // refresh the text now
        _panel.ResetSize();
    }

    public override void _UnhandledInput(InputEvent @event)
    {
        if (@event is InputEventKey { Pressed: true, Echo: false, PhysicalKeycode: Key.F4 })
        {
            SetLevel((Level)(((int)_level + 1) % 4));
            GetViewport().SetInputAsHandled();
        }
    }

    public override void _Process(double delta)
    {
        if (_level == Level.Off) return;
        using var _ = PerfMarker.Scope(PerfCat.Overlay);
        _textAge += delta;
        if (_textAge >= 0.25)
        {
            _textAge = 0;
            _text.Text = BuildText();
        }
        if (_level >= Level.Graph) _graph.QueueRedraw();
    }

    string BuildText()
    {
        var w = PerfMonitor.Window;
        _sb.Clear();
        _sb.Append(string.Format(Ci, "{0,5:F1} fps  {1,6:F2} ms  (p95 {2:F1})", w.AvgFps, w.MeanFrameMs, w.P95Ms));
        if (w.GpuFrames > 0) _sb.Append(string.Format(Ci, "  gpu {0:F2}", w.MeanGpuMs));
        if (_level >= Level.Table) AppendTable();
        _sb.Append("  [F4]");
        return _sb.ToString();
    }

    void AppendTable()
    {
        var s = PerfMonitor.Session;
        var last = PerfMonitor.Last;
        var m = _diagnostics.Sampler.Values;
        string status = _diagnostics.StatusLine?.Invoke() ?? "";
        if (status.Length > 0) _sb.Append('\n').Append(status);
        _sb.Append(string.Format(Ci, "\nsince start {0:F0} s, {1} frames", s.DurationS, s.Frames));
        _sb.Append(string.Format(Ci, "\nframe p50 {0:F2}  p95 {1:F2}  p99 {2:F2}  max {3:F1}", s.P50Ms, s.P95Ms, s.P99Ms, s.MaxFrameMs));
        _sb.Append(string.Format(Ci, "\n1% low {0:F1} fps  hitches {1}  >33.3ms {2}", s.OnePercentLowFps, s.Hitches, s.OverBudget));
        _sb.Append(string.Format(Ci, "\nframe split (1 s mean ms): script {0:F2}  render cpu {1:F2}  wait/other {2:F2}  of {3:F2}",
            m[(int)Metric.ProcessWallMs], m[(int)Metric.RenderCpuMs], m[(int)Metric.OtherWaitMs], m[(int)Metric.FrameMs]));
        _sb.Append(string.Format(Ci, "\ncpu % (100 = 1 core): process {0:F0}  main {1:F0}  other threads {2:F0}",
            m[(int)Metric.CpuProcessPct], m[(int)Metric.CpuMainPct], m[(int)Metric.CpuOtherPct]));
        _sb.Append(string.Format(Ci, "\ngodot monitors: process {0:F2}  physics {1:F2}", last.ProcessMs, last.PhysicsMs));
        _sb.Append(s.GpuFrames == 0 ? "\ngpu  not measured"
            : string.Format(Ci, "\ngpu mean {0:F2}  p95 {1:F2}  p99 {2:F2}", s.MeanGpuMs, s.P95GpuMs, s.P99GpuMs));
        _sb.Append(string.Format(Ci, "\ndraw calls {0}  primitives {1:N0}", last.DrawCalls, last.Primitives));
        _sb.Append(string.Format(Ci, "\nvideo {0:F0} MB  godot static {1:F0} MB", last.VideoMemBytes / 1048576.0, last.StaticMemBytes / 1048576.0));
        _sb.Append(string.Format(Ci, "\nprocess {0:F0} MB  gc heap {1:F0} MB  alloc {2:F2} MB/s",
            m[(int)Metric.ProcessMb], m[(int)Metric.GcHeapMb], m[(int)Metric.GcAllocMbPerS]));
        _sb.Append(string.Format(Ci, "\nlibrary {0:F0} MB (data tiles {1:F0})  jobs {2:F0} queued {3:F0} running",
            m[(int)Metric.LibMemoryMb], m[(int)Metric.LibDataTilesMb], m[(int)Metric.JobsQueued], m[(int)Metric.JobsRunning]));
        _sb.Append(string.Format(Ci, "\ntiles v {0:F0}  t {1:F0}  l {2:F0}  queued {3:F0}  nodes {4:F0}",
            m[(int)Metric.TilesVector], m[(int)Metric.TilesTerrain], m[(int)Metric.TilesLandmarks], m[(int)Metric.TilesQueued], m[(int)Metric.PoolNodes]));
        _sb.Append(string.Format(Ci, "\nper s shown/hidden: vector {0:F0}/{1:F0}  terrain {2:F0}/{3:F0}  quick toggles {4:F0}+{5:F0}",
            m[(int)Metric.ShownVectorPerS], m[(int)Metric.HiddenVectorPerS], m[(int)Metric.ShownTerrainPerS], m[(int)Metric.HiddenTerrainPerS],
            m[(int)Metric.QuickTogglesVectorPerS], m[(int)Metric.QuickTogglesTerrainPerS]));
        _sb.Append(string.Format(Ci, "\nfades started {0:F0}/s ended {1:F0}/s active {2:F0}  redeliveries {3:F0}/s",
            m[(int)Metric.FadesStartedPerS], m[(int)Metric.FadesEndedPerS], m[(int)Metric.FadesActive], m[(int)Metric.RedeliveriesPerS]));
        _sb.Append(string.Format(Ci, "\nuploads {0:F0} tiles/s, over budget {1:F0} frames/s",
            m[(int)Metric.UploadedTilesPerS], m[(int)Metric.UploadFramesOverBudgetPerS]));
        _sb.Append(string.Format(Ci, "\nin frustum: vector {0:F0} tiles {1:N0} tris  terrain {2:F0} tiles {3:N0} tris",
            m[(int)Metric.VectorInFrustum], m[(int)Metric.VectorTrisInFrustum], m[(int)Metric.TerrainInFrustum], m[(int)Metric.TerrainTrisInFrustum]));
        _sb.Append("\nmarker            last   mean    p99");
        for (int i = 0; i < PerfMarker.Count; i++)
        {
            var cat = (PerfCat)i;
            _sb.Append(string.Format(Ci, "\n{0,-16}{1,6:F3}{2,7:F3}{3,7:F3}", PerfMarker.Name(cat), PerfMarker.LastFrameMs(cat),
                PerfMarker.MeanMs(cat), PerfMarker.P99Ms(cat)));
        }
        _sb.Append(string.Format(Ci, "\nmeasuring: frame {0:F3} ms, second {1:F3} ms", _diagnostics.FrameCost.Mean, _diagnostics.SecondCost.Mean));
    }
}
