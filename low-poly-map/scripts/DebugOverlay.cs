using System.Collections.Generic;
using System.Text;
using Godot;

namespace LowPolyMap;

/// <summary>
/// F3-toggled debug overlay: a text block (FPS, draw calls, per-ring streamer stats, upload time,
/// camera) plus a top-down minimap around the camera. The map shows only tiles that are currently visible
/// (the quadtree cover), coloured by band; H also outlines hidden tiles (hidden by their children / waiting for
/// siblings) and tiles still in flight. Refreshed 4x/s. M cycles minimap zoom.
/// </summary>
public partial class DebugOverlay : Control
{
    private const double RefreshInterval = 0.25;
    private const float MapPx = 320f;
    private const float Margin = 12f;
    private const int DrawCallBudget = 300;
    private const long TriangleBudget = 1_500_000;

    // Total minimap extent in meters (map is square, camera in the center).
    private static readonly float[] ZoomSpansM = { 16000f, 4000f, 1000f };

    private static readonly Color[] RingColors =
    {
        new(1f, 1f, 1f),        // R0 white
        new(0.2f, 0.9f, 1f),    // R1 cyan
        new(1f, 0.4f, 0.9f),    // R2 magenta
        new(1f, 0.65f, 0.15f),  // R3 orange
    };

    private static readonly Color[] StateColors =
    {
        new(0.5f, 0.5f, 0.55f, 0.35f),  // queued   gray
        new(0.25f, 0.45f, 1f, 0.5f),    // fetching blue
        new(1f, 0.9f, 0.2f, 0.5f),      // meshing  yellow
        new(0.2f, 0.85f, 0.3f, 0.5f),   // ready    green
        new(1f, 0.15f, 0.15f, 0.6f),    // failed   red
        new(0.6f, 0.25f, 0.8f, 0.5f),   // unloading purple
        new(0.3f, 0.3f, 0.3f, 0.3f),    // unknown
    };

    private static readonly string[] StateNames =
        { "queued", "fetching", "meshing", "ready", "failed", "unloading" };

    [Export] public StreamerBridge? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }
    [Export] public NightRenderController? Render { get; set; }

    private readonly StreamerStats _stats = new();
    private readonly List<DebugTile> _tiles = new(256);
    private readonly StringBuilder _sb = new(1024);
    private readonly Vector2[] _arrow = new Vector2[3];
    private readonly List<Vector2> _fillV = new(1024);
    private readonly List<Color> _fillC = new(1024);
    private readonly List<int> _fillI = new(2048);
    private readonly List<Vector2> _lineP = new(2048);
    private readonly List<Color> _lineC = new(1024);
    private readonly ArrayMesh _fillMesh = new();
    private Label _label = null!;
    private Label _head = null!;
    private Control _map = null!;
    private Font _font = null!;
    private double _timer;
    private int _zoom;
    private bool _showHidden;
    private int _tileCount;
    private bool _haveStats;
    private Vector2 _heading = new(0, -1);
    private Vector3 _camPos;

    public override void _Ready()
    {
        MouseFilter = MouseFilterEnum.Ignore;
        SetAnchorsPreset(LayoutPreset.FullRect);
        _font = ThemeDB.FallbackFont;

        // Headline (FPS, draw calls, prims) is its own label so it can turn red when over budget without
        // recolouring the whole block.
        var column = new VBoxContainer { Name = "Column", Position = new Vector2(Margin, 8f), MouseFilter = MouseFilterEnum.Ignore };
        column.AddThemeConstantOverride("separation", 0);
        AddChild(column);
        _head = MakeLabel("HeadLabel");
        _label = MakeLabel("StatsLabel");
        column.AddChild(_head);
        column.AddChild(_label);

        _map = new Control
        {
            Name = "Minimap",
            ClipContents = true,
            MouseFilter = MouseFilterEnum.Ignore,
            AnchorLeft = 1f, AnchorRight = 1f, AnchorTop = 0f, AnchorBottom = 0f,
            OffsetLeft = -MapPx - Margin, OffsetRight = -Margin,
            OffsetTop = Margin, OffsetBottom = Margin + MapPx,
        };
        _map.Draw += DrawMap;
        AddChild(_map);

        Refresh();
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
            if (k.PhysicalKeycode == Key.F3)
            {
                Visible = !Visible;
                if (Visible) Refresh();
                GetViewport().SetInputAsHandled();
            }
            else if (k.PhysicalKeycode == Key.M)
            {
                _zoom = (_zoom + 1) % ZoomSpansM.Length;
                _map.QueueRedraw();
            }
            else if (k.PhysicalKeycode == Key.H)
            {
                _showHidden = !_showHidden;
                _map.QueueRedraw();
            }
        }
    }

    public override void _Process(double delta)
    {
        if (!Visible) return;
        _timer -= delta;
        if (_timer > 0) return;
        _timer = RefreshInterval;
        Refresh();
    }

    private void Refresh()
    {
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
        _map.QueueRedraw();
    }

    private void BuildText(WorldStreamerFacade? streamer)
    {
        var sb = _sb;
        sb.Clear();
        double fps = Engine.GetFramesPerSecond();
        long draws = (long)RenderingServer.GetRenderingInfo(RenderingServer.RenderingInfo.TotalDrawCallsInFrame);
        long prims = (long)RenderingServer.GetRenderingInfo(RenderingServer.RenderingInfo.TotalPrimitivesInFrame);
        long vpDraws = (long)RenderingServer.ViewportGetRenderInfo(GetViewport().GetViewportRid(), RenderingServer.ViewportRenderInfoType.Visible, RenderingServer.ViewportRenderInfo.DrawCallsInFrame);
        // The viewport counter is the 3D scene (incl. shadow passes); the engine-wide total also counts this overlay.
        bool overDraws = vpDraws > DrawCallBudget, overPrims = prims > TriangleBudget;
        sb.Append("FPS ").Append(fps.ToString("0")).Append(" (").Append((1000.0 / System.Math.Max(fps, 1.0)).ToString("0.0")).Append(" ms)\n");
        sb.Append("draw calls ").Append(vpDraws).Append(" (+").Append(draws - vpDraws).Append(" HUD) / ").Append(DrawCallBudget).Append(overDraws ? "  OVER BUDGET" : "")
          .Append("   prims ").Append(prims.ToString("N0")).Append(" / 1.5M").Append(overPrims ? "  OVER BUDGET" : "")
          .Append("   objs ").Append((long)RenderingServer.GetRenderingInfo(RenderingServer.RenderingInfo.TotalObjectsInFrame));
        _head.Text = sb.ToString();
        _head.AddThemeColorOverride("font_color", overDraws || overPrims ? new Color(1f, 0.3f, 0.25f) : Colors.White);
        sb.Clear();

        if (Bridge != null)
        {
            sb.Append("source: ").Append(Bridge.TileSource);
            if (Bridge.BandsText.Length > 0) sb.Append("  ").Append(Bridge.BandsText);
            sb.Append('\n');
            if (Bridge.ConfigureError.Length > 0)
                sb.Append("WORLD NOT CONFIGURED: ").Append(Bridge.ConfigureError).Append('\n');
            else if (!Bridge.Status.IsOk)
            {
                var st = Bridge.Status;
                sb.Append("STATUS ").Append(st.State.ToUpperInvariant()).Append(": ").Append(st.Message)
                  .Append("  (failed fetches ").Append(st.FailedFetches).Append(st.LastError.Length > 0 ? ", last: " + st.LastError : "").Append(")\n");
            }
        }

        if (streamer == null || !streamer.IsAvailable)
        {
            sb.Append("WorldStreamer: UNAVAILABLE\n");
            sb.Append(streamer != null && streamer.Error.Length > 0 ? streamer.Error : "instance freed (extension reload?)").Append('\n');
        }
        else if (_haveStats)
        {
            sb.Append("WorldStreamer ").Append(_stats.Version).Append("  origin ")
              .Append(_stats.OriginLat.ToString("0.0000")).Append(", ").Append(_stats.OriginLon.ToString("0.0000")).Append('\n');
            AppendRings(sb, "queued  ", _stats.Queued);
            AppendRings(sb, "fetching", _stats.Fetching);
            AppendRings(sb, "meshing ", _stats.Meshing);
            AppendRings(sb, "loaded  ", _stats.Loaded);
            sb.Append("cache ").Append(_stats.CacheMb.ToString("0.0")).Append(" MB\n");
        }

        if (Bridge != null)
        {
            var c = Bridge.Cover;
            sb.Append("tiles uploaded ").Append(c.Loaded).Append("  pool free ").Append(Bridge.PoolFree).Append('/').Append(Bridge.PoolCreated).Append('\n');
            sb.Append("visible  R0 ").Append(c.VisibleByRing[0]).Append("  R1 ").Append(c.VisibleByRing[1])
              .Append("  R2 ").Append(c.VisibleByRing[2]).Append("  R3 ").Append(c.VisibleByRing[3]).Append('\n');
            sb.Append("hidden by children ").Append(c.HiddenByChildren).Append("  waiting siblings ").Append(c.WaitingSiblings).Append('\n');
            sb.Append("LOD fades: ").Append(Bridge.FadesSupported ? "on" : "off (no poll_fades in this Rust build)")
              .Append("  fading tiles ").Append(c.FadingTiles).Append("  poll entries ").Append(Bridge.FadeEntriesLastPoll)
              .Append("  awaiting first poll ").Append(c.AwaitingFadeTiles).Append('\n');
            sb.Append("lights ").Append(c.LightsVisible.ToString("N0")).Append(" visible / ").Append(c.LightsLoaded.ToString("N0"))
              .Append(" loaded  clustered tiles ").Append(c.ClusteredTilesVisible).Append('/').Append(c.LightTilesVisible)
              .Append(c.ClusteredTilesVisible > 0 ? "  [CLUSTERED]" : "").Append('\n');
            sb.Append("building tris ").Append(c.TrianglesVisible.ToString("N0")).Append(" visible / ").Append(c.TrianglesLoaded.ToString("N0")).Append(" loaded\n");
            sb.Append("ground meshes ").Append(c.GroundTilesVisible).Append(" visible / ").Append(c.GroundTilesLoaded)
              .Append(" loaded (= draw calls, of ").Append(DrawCallBudget).Append(" cap)  tris ")
              .Append(c.GroundTrianglesVisible.ToString("N0")).Append(" / ").Append(c.GroundTrianglesLoaded.ToString("N0")).Append('\n');
            if (Render != null)
                sb.Append("render: tonemap ").Append(Render.Tonemap).Append(" [T]  exposure x").Append(Render.ExposureNow.ToString("0.00"))
                  .Append(Render.ExternalView ? "" : " (off)").Append(" [X]  glow thr ").Append(Render.GlowThresholdNow.ToString("0.00"))
                  .Append(" int ").Append(Render.GlowIntensityNow.ToString("0.00")).Append('\n');
            sb.Append("this second: ready ").Append(Bridge.ReadyLastSecond)
              .Append("  unloaded ").Append(Bridge.UnloadedLastSecond)
              .Append("  replaced ").Append(Bridge.ReplacedTotal)
              .Append("  cover check ").Append(Bridge.ConsistencyErrors == 0 ? "ok" : "ERRORS " + Bridge.ConsistencyErrors).Append('\n');
            sb.Append("upload ms: frame ").Append((Bridge.UploadUsLastFrame / 1000.0).ToString("0.00"))
              .Append("  max/1s ").Append((Bridge.UploadUsMaxLastSecond / 1000.0).ToString("0.00"))
              .Append("  (budget 2.00)  frames over: ").Append(Bridge.UploadFramesOverBudget).Append('/').Append(Bridge.UploadFrames).Append('\n');
            if (Bridge.GroundUploadCount > 0)
                sb.Append("  ground part of upload ms: last ").Append((Bridge.GroundUploadUsLast / 1000.0).ToString("0.00"))
                  .Append("  max ").Append((Bridge.GroundUploadUsMax / 1000.0).ToString("0.00"))
                  .Append("  avg ").Append((Bridge.GroundUploadUsSum / 1000.0 / Bridge.GroundUploadCount).ToString("0.00"))
                  .Append("  n ").Append(Bridge.GroundUploadCount).Append('\n');
            for (int r = 0; r < 4; r++)
            {
                int n = Bridge.TileUploadCount[r];
                if (n == 0) continue;
                sb.Append("  R").Append(r).Append(" tile upload ms: last ").Append((Bridge.TileUploadUsLast[r] / 1000.0).ToString("0.00"))
                  .Append("  max ").Append((Bridge.TileUploadUsMax[r] / 1000.0).ToString("0.00"))
                  .Append("  avg ").Append((Bridge.TileUploadUsSum[r] / 1000.0 / n).ToString("0.00"))
                  .Append("  n ").Append(n).Append('\n');
            }
        }

        if (Camera != null)
        {
            sb.Append("cam alt ").Append(Camera.AltitudeAgl.ToString("0")).Append(" m AGL (ground ")
              .Append((Bridge?.CameraGroundHeight ?? 0.0).ToString("0.0")).Append(" m)  speed ")
              .Append(Camera.Velocity.Length().ToString("0")).Append('/').Append(Camera.Speed.ToString("0"))
              .Append(" m/s").Append(Camera.Autopilot ? " [AUTOPILOT]" : "").Append(Camera.ClimbRate > 0f ? " [CLIMB " + Camera.ClimbRate.ToString("0") + " m/s]" : "").Append("  pos ").Append(_camPos.X.ToString("0")).Append(", ").Append(_camPos.Z.ToString("0")).Append('\n');
        }

        if (streamer != null && streamer.IsAvailable)
        {
            if (streamer.HasDebugTiles) sb.Append("debug_tiles: ").Append(_tileCount).Append(" tiles\n");
            else sb.Append("debug_tiles: not exported by this Rust build\n");
        }
        sb.Append("[F2] daylight  [F3] overlay  [F] autopilot  [PgUp/PgDn] alt x2 / x0.5  [H] hidden tiles: ").Append(_showHidden ? "outlined" : "off").Append("  [M] map zoom (").Append(ZoomSpansM[_zoom] / 1000f).Append(" km)");
        if (Bridge != null && Bridge.Attributions.Length > 0)
            sb.Append('\n').Append(string.Join(" | ", Bridge.Attributions));
    }

    private static void AppendRings(StringBuilder sb, string name, int[] v)
    {
        sb.Append(name);
        for (int i = 0; i < v.Length; i++) sb.Append("  R").Append(i).Append(' ').Append(v[i]);
        sb.Append('\n');
    }

    private void AddFill(Rect2 r, Color c)
    {
        int b = _fillV.Count;
        _fillV.Add(r.Position); _fillV.Add(new Vector2(r.End.X, r.Position.Y)); _fillV.Add(r.End); _fillV.Add(new Vector2(r.Position.X, r.End.Y));
        for (int i = 0; i < 4; i++) _fillC.Add(c);
        _fillI.Add(b); _fillI.Add(b + 1); _fillI.Add(b + 2); _fillI.Add(b); _fillI.Add(b + 2); _fillI.Add(b + 3);
    }

    private void AddLine(Vector2 a, Vector2 b, Color c)
    {
        _lineP.Add(a); _lineP.Add(b); _lineC.Add(c);
    }

    private void AddOutline(Rect2 r, Color c)
    {
        var tr = new Vector2(r.End.X, r.Position.Y);
        var bl = new Vector2(r.Position.X, r.End.Y);
        AddLine(r.Position, tr, c); AddLine(tr, r.End, c); AddLine(r.End, bl, c); AddLine(bl, r.Position, c);
    }

    private void DrawMap()
    {
        float span = ZoomSpansM[_zoom];
        float scale = MapPx / span;
        var center = new Vector2(MapPx, MapPx) * 0.5f;
        var cam2 = new Vector2(_camPos.X, _camPos.Z);

        _map.DrawRect(new Rect2(0, 0, MapPx, MapPx), new Color(0, 0, 0, 0.55f));

        // Distance circles.
        var grid = new Color(1, 1, 1, 0.12f);
        for (float r = 1000f; r * scale < MapPx * 0.75f; r *= 2f)
            _map.DrawArc(center, r * scale, 0f, Mathf.Tau, 48, grid, 1f);

        // Coarse rings first so finer rings stay on top. Visible tiles (= the quadtree cover) are filled with
        // their band colour; with [H], hidden tiles are outlined dimly and tiles in flight use their state colour.
        var cover = Bridge?.Cover;
        _fillV.Clear(); _fillC.Clear(); _fillI.Clear(); _lineP.Clear(); _lineC.Clear();
        for (int ring = 3; ring >= 0; ring--)
        {
            var band = RingColors[ring];
            for (int i = 0; i < _tiles.Count; i++)
            {
                var t = _tiles[i];
                if (t.Ring != ring) continue;
                int vis = cover != null && t.Packed != 0 ? cover.StateOf(t.Packed) : -1;
                bool isVisible = vis == (int)TileCover.Vis.Visible;
                if (!isVisible && !_showHidden) continue;
                var p = center + (t.Min - cam2) * scale;
                var sz = (t.Max - t.Min) * scale;
                if (p.X > MapPx || p.Y > MapPx || p.X + sz.X < 0 || p.Y + sz.Y < 0) continue;
                if (sz.X < 1f) sz.X = 1f;
                if (sz.Y < 1f) sz.Y = 1f;
                var rect = new Rect2(p, sz);
                if (isVisible)
                {
                    var fill = band;
                    fill.A = 0.35f;
                    AddFill(rect, fill);
                    var outline = band;
                    outline.A = 0.95f;
                    AddOutline(rect, outline);
                }
                else if (vis < 0)
                {
                    var sc = StateColors[(int)t.State];
                    sc.A = 0.6f;
                    AddOutline(rect, sc); // not uploaded yet (queued / fetching / meshing / ...)
                }
                else
                {
                    var outline = band;
                    outline.A = vis == (int)TileCover.Vis.HiddenByChildren ? 0.28f : 0.5f;
                    AddOutline(rect, outline);
                    if (vis == (int)TileCover.Vis.WaitingSiblings)
                        AddLine(rect.Position, rect.End, outline); // uploaded, waiting for its siblings
                }
            }
        }
        // Everything above is batched into one mesh + one multiline so the minimap costs ~2 draw calls
        // (hundreds of DrawRect calls would swamp the 3D draw-call count shown in the overlay).
        if (_fillV.Count > 0)
        {
            var arrays = new Godot.Collections.Array();
            arrays.Resize((int)Mesh.ArrayType.Max);
            arrays[(int)Mesh.ArrayType.Vertex] = _fillV.ToArray();
            arrays[(int)Mesh.ArrayType.Color] = _fillC.ToArray();
            arrays[(int)Mesh.ArrayType.Index] = _fillI.ToArray();
            _fillMesh.ClearSurfaces();
            _fillMesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays);
            _map.DrawMesh(_fillMesh, null);
        }
        if (_lineP.Count > 0) _map.DrawMultilineColors(_lineP.ToArray(), _lineC.ToArray(), 1f);

        // Camera marker + heading (north up: screen up = -z).
        var h = _heading;
        var side = new Vector2(-h.Y, h.X);
        _arrow[0] = center + h * 9f;
        _arrow[1] = center - h * 5f + side * 5f;
        _arrow[2] = center - h * 5f - side * 5f;
        _map.DrawColoredPolygon(_arrow, new Color(1f, 0.2f, 0.2f));
        _map.DrawLine(center, center + h * 28f, new Color(1f, 0.2f, 0.2f, 0.8f), 1f);

        // Legend: rings (top-left), states (bottom).
        for (int i = 0; i < RingColors.Length; i++)
        {
            float x = 6f + i * 34f;
            _map.DrawString(_font, new Vector2(x, 14f), "R" + i, HorizontalAlignment.Left, -1, 12, RingColors[i]);
        }
        for (int i = 0; i < StateNames.Length; i++)
        {
            float x = 6f + (i % 3) * 104f;
            float y = MapPx - 24f + (i / 3) * 14f;
            var c = StateColors[i];
            c.A = 1f;
            _map.DrawRect(new Rect2(x, y - 8f, 8f, 8f), c);
            _map.DrawString(_font, new Vector2(x + 12f, y), StateNames[i], HorizontalAlignment.Left, -1, 11, Colors.White);
        }
        _map.DrawRect(new Rect2(0, 0, MapPx, MapPx), new Color(1, 1, 1, 0.5f), false, 1f);
    }
}
