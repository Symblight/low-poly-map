using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Shows only an FPS label by default. F1 toggles the detailed debug overlay (off at startup): a text block (FPS, draw
/// calls, per-ring streamer stats, upload time, camera) plus a top-down minimap around the camera. The map shows only tiles that are currently visible
/// (the quadtree cover), coloured by band; H also outlines hidden tiles (hidden by their children / waiting for
/// siblings) and tiles still in flight. Refreshed 4x/s. M cycles minimap zoom (detail mode only).
/// </summary>
public partial class DebugOverlay : Control
{
    private const double RefreshInterval = 0.25;
    private const float Margin = 12f;

    [Export] public WorldView? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }
    [Export] public NightRenderController? Render { get; set; }

    private readonly StreamerStats _stats = new();
    private readonly List<DebugTile> _tiles = new(256);
    private readonly StringBuilder _sb = new(1024);
    private Label _label = null!;
    private Label _head = null!;
    private DebugMinimap _map = null!;
    private double _timer;
    private bool _detail;
    private int _tileCount;
    private bool _haveStats;
    private Vector2 _heading = new(0, -1);
    private Vector3 _camPos;

    public override void _Ready()
    {
        MouseFilter = MouseFilterEnum.Ignore;
        SetAnchorsPreset(LayoutPreset.FullRect);

        // Headline (FPS, draw calls, prims) is its own label so it can turn red when over budget without
        // recolouring the whole block.
        var column = new VBoxContainer { Name = "Column", Position = new Vector2(Margin, 8f), MouseFilter = MouseFilterEnum.Ignore };
        column.AddThemeConstantOverride("separation", 0);
        AddChild(column);
        _head = MakeLabel("HeadLabel");
        _label = MakeLabel("StatsLabel");
        column.AddChild(_head);
        column.AddChild(_label);

        _map = new DebugMinimap
        {
            Name = "Minimap",
            ClipContents = true,
            MouseFilter = MouseFilterEnum.Ignore,
            AnchorLeft = 1f, AnchorRight = 1f, AnchorTop = 0f, AnchorBottom = 0f,
            OffsetLeft = -DebugMinimap.SizePx - Margin, OffsetRight = -Margin,
            OffsetTop = Margin, OffsetBottom = Margin + DebugMinimap.SizePx,
            Tiles = _tiles,
        };
        AddChild(_map);

        // Detail mode (F1) is off at startup (launch arg --overlay-detail starts with it on): only the FPS headline is on screen.
        _detail = System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--overlay-detail");
        ApplyDetailVisibility();
        Refresh();
    }

    private void ApplyDetailVisibility()
    {
        _label.Visible = _detail;
        _map.Visible = _detail;
    }

    private static Label MakeLabel(string name)
    {
        var l = new Label { Name = name, MouseFilter = MouseFilterEnum.Ignore };
        l.AddThemeColorOverride("font_shadow_color", Colors.Black);
        l.AddThemeConstantOverride("shadow_offset_x", 1);
        l.AddThemeConstantOverride("shadow_offset_y", 1);
        return l;
    }

    public override void _UnhandledInput(InputEvent @event)
    {
        if (@event is InputEventKey { Pressed: true, Echo: false } k)
        {
            if (k.PhysicalKeycode == Key.F1)
            {
                _detail = !_detail;
                ApplyDetailVisibility();
                Refresh();
                GetViewport().SetInputAsHandled();
            }
            else if (!_detail) return; // M / H only act on the detail overlay
            else if (k.PhysicalKeycode == Key.M)
            {
                _map.CycleZoom();
            }
            else if (k.PhysicalKeycode == Key.H)
            {
                _map.ToggleHidden();
            }
        }
    }

    public override void _Process(double delta)
    {
        _timer -= delta;
        if (_timer > 0) return;
        _timer = RefreshInterval;
        using (Diagnostics.PerfMarker.Scope(Diagnostics.PerfCat.Overlay)) Refresh();
    }

    private void Refresh()
    {
        if (!_detail)
        {
            // Only the FPS headline; the rest (and its per-refresh debug_stats / debug_tiles calls) is skipped.
            _head.Text = "FPS " + Engine.GetFramesPerSecond().ToString("0");
            _head.AddThemeColorOverride("font_color", Colors.White);
            return;
        }
        var streamer = Bridge?.Streamer;
        if (Camera != null)
        {
            _camPos = Camera.GlobalPosition;
            var fwd = -Camera.GlobalTransform.Basis.Z;
            var h = new Vector2(fwd.X, fwd.Z);
            if (h.LengthSquared() > 1e-4f) _heading = h.Normalized();
        }

        _haveStats = streamer != null && streamer.DebugStats(_stats);
        _tileCount = streamer != null ? streamer.DebugTiles(_tiles) : 0;

        BuildText(streamer);
        _label.Text = _sb.ToString();
        _map.Cover = Bridge?.Cover;
        _map.CamPos = _camPos;
        _map.Heading = _heading;
        _map.QueueRedraw();
    }
}
