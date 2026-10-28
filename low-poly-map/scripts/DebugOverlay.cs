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

    [Export] public WorldView? Bridge { get; set; }
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
    private bool _detail;
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
        _timer -= delta;
        if (_timer > 0) return;
        _timer = RefreshInterval;
        Refresh();
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
            if (Bridge.SeedWorldActive) AppendSeedLine(sb);
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
            if (_stats.HasTerrain)
            {
                AppendRings(sb, "terrain queued  ", _stats.TerrainQueued, _stats.TerrainRings, "T");
                AppendRings(sb, "terrain meshing ", _stats.TerrainMeshing, _stats.TerrainRings, "T");
                AppendRings(sb, "terrain loaded  ", _stats.TerrainLoaded, _stats.TerrainRings, "T");
                sb.Append("terrain tris ").Append(_stats.TerrainTris.ToString("N0")).Append("  tau ").Append(TerrainTauText(_stats.TerrainDetail)).Append('\n');
            }
            sb.Append("cache ").Append(_stats.CacheMb.ToString("0.0")).Append(" MB\n");
            AppendViewLine(sb, streamer);
            AppendDataLine(sb);
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
            if (Bridge.TerrainActive)
            {
                var tc = Bridge.TerrainCover;
                int drawn = Camera != null ? Bridge.CountTerrainInFrustum(Camera, out int byRule, out long drawnTris) : 0;
                int visTotal = 0;
                foreach (var v in tc.VisibleByRing) visTotal += v;
                sb.Append("terrain meshes ").Append(tc.Loaded).Append(" held, ").Append(visTotal).Append(" visible by rule (T");
                for (int r = 0; r < tc.VisibleByRing.Length; r++) sb.Append(r > 0 ? "/" : "").Append(tc.VisibleByRing[r]);
                sb.Append("), ").Append(drawn).Append(" in frustum (= terrain draw calls, of ").Append(DrawCallBudget).Append(" cap)  tris ")
                  .Append(tc.TrianglesVisible.ToString("N0")).Append(" visible / ").Append(tc.TrianglesLoaded.ToString("N0")).Append(" held  fading ").Append(tc.FadingTiles)
                  .Append("  replaced ").Append(Bridge.TerrainReplacedTotal).Append('\n');
                sb.Append("terrain upload ms (last/max/avg) per ring:");
                for (int r = 0; r < tc.Rings; r++)
                {
                    int n = Bridge.TerrainUploadCount[r];
                    if (n == 0) continue;
                    sb.Append("  T").Append(r).Append(' ').Append((Bridge.TerrainUploadUsLast[r] / 1000.0).ToString("0.0")).Append('/').Append((Bridge.TerrainUploadUsMax[r] / 1000.0).ToString("0.0"))
                      .Append('/').Append((Bridge.TerrainUploadUsSum[r] / 1000.0 / n).ToString("0.0"));
                }
                sb.Append('\n');
            }
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
            if (_haveStats && _stats.Latency.Count > 0)
                sb.Append("streaming latency ms p50/p95/max (n), last 128 tiles per band (swap = coarse tile standing in):\n").Append(_stats.LatencyText());
        }

        if (Camera != null)
        {
            sb.Append("cam alt ").Append(Camera.AltitudeAgl.ToString("0")).Append(" m AGL (ground ")
              .Append((Bridge?.CameraGroundHeight ?? 0.0).ToString("0.0")).Append(" m)  speed ")
              .Append(Camera.Velocity.Length().ToString("0")).Append('/').Append(Camera.Speed.ToString("0"))
              .Append(" m/s").Append(Camera.AltitudeAgl < 0 && Bridge != null && Bridge.TerrainActive ? "  [UNDER THE TERRAIN: back faces are culled]" : "").Append(Camera.Autopilot ? " [AUTOPILOT]" : "").Append(Camera.ClimbRate > 0f ? " [CLIMB " + Camera.ClimbRate.ToString("0") + " m/s]" : "").Append("  pos ").Append(_camPos.X.ToString("0")).Append(", ").Append(_camPos.Z.ToString("0")).Append('\n');
        }

        if (streamer != null && streamer.IsAvailable)
        {
            if (streamer.HasDebugTiles) sb.Append("debug_tiles: ").Append(_tileCount).Append(" tiles\n");
            else sb.Append("debug_tiles: not exported by this Rust build\n");
        }
        sb.Append("[F1] overlay  [F2] daylight  [F3] settings  [F] autopilot  [PgUp/PgDn] alt x2 / x0.5  [H] hidden tiles: ").Append(_showHidden ? "outlined" : "off").Append("  [M] map zoom (").Append(ZoomSpansM[_zoom] / 1000f).Append(" km)");
        if (Bridge != null && Bridge.Attributions.Length > 0)
            sb.Append('\n').Append(string.Join(" | ", Bridge.Attributions));
    }

    /// <summary>Screen-space LOD (api 1.10): the view the host sends with set_view (fov_y, viewport height), call count, the library's effective threshold and the data tile / skipped request counters.</summary>
    private void AppendViewLine(StringBuilder sb, WorldStreamerFacade streamer)
    {
        if (Bridge == null) return;
        sb.Append("view: fov_y ").Append(Bridge.ViewFovYDeg.ToString("0.0")).Append(" deg  H ").Append(Bridge.ViewHeightPx).Append(" px  set_view ");
        if (!streamer.HasSetView) { sb.Append("unavailable (api ").Append(streamer.ApiVersionText).Append("): fixed bands\n"); return; }
        sb.Append(streamer.SetViewCalls).Append(" calls");
        if (!_stats.HasLodStats) { sb.Append("  (no LOD stats in debug_stats)\n"); return; }
        sb.Append("  tau_eff ").Append(_stats.TauEff.ToString("0.0")).Append(" px (quality x").Append(_stats.LodQuality.ToString("0.##")).Append(")  edges ")
          .Append(_stats.LodEdgesM[0].ToString("0")).Append('/').Append(_stats.LodEdgesM[1].ToString("0")).Append('/').Append(_stats.LodEdgesM[2].ToString("0")).Append('/').Append(_stats.LodEdgesM[3].ToString("0"))
          .Append(" m  lib view ").Append(_stats.ViewFovYDeg.ToString("0.#")).Append(" deg / ").Append(_stats.ViewHeightPx).Append(" px  blend D ").Append(Bridge.BlendEdgeM.ToString("0")).Append(" m\n  data tiles ").Append(_stats.DataTilesLoaded).Append(" held / ").Append(_stats.DataTilesDecoded)
          .Append(" decoded  requests skipped: empty ").Append(_stats.RequestsSkippedEmpty).Append(", moving ").Append(_stats.RequestsSkippedMoving).Append('\n');
    }

    /// <summary>"0.9 px (tau_px 0.9 x scale 1)": the terrain cover's effective screen-space-error threshold (api 1.10: set_terrain_detail is a tau scale, above 1 coarser).</summary>
    private string TerrainTauText(double scale)
    {
        double px = Bridge?.TerrainTauPx ?? 0.0;
        return px > 0
            ? $"{(px * scale).ToString("0.##")} px (terrain.tau_px {px.ToString("0.##")} x scale {scale.ToString("0.##")}; higher = coarser)"
            : $"scale x{scale.ToString("0.##")} (higher = coarser)";
    }

    /// <summary>Data levels and landmark layer (api 1.11): the archive's detail / overview zoom and whether R2 / R3 come from the overview, landmark tiles loaded / pending / empty and the nodes the host holds for them.</summary>
    private void AppendDataLine(StringBuilder sb)
    {
        if (Bridge == null) return;
        sb.Append("data levels: ");
        if (_stats.HasDataLevels)
            sb.Append(_stats.TwoLevel ? "two-level" : "compat").Append(" detail z").Append(_stats.DetailZoom).Append(" / overview z").Append(_stats.OverviewZoom)
              .Append(_stats.LandmarksInArchive ? ", landmarks in archive" : ", no landmarks layer");
        else sb.Append("n/a (library ").Append(Bridge.Streamer.ApiVersionText).Append(Bridge.SeedWorldActive ? ", seed world" : ", archive not open yet or api < 1.11").Append(')');
        sb.Append("\nlandmarks: ");
        if (!Bridge.LandmarksActive) sb.Append(Bridge.LandmarksEnabled ? "layer unavailable (api < 1.11)" : "layer off (--no-landmarks: folded into the vector tiles)");
        else
        {
            if (_stats.HasLandmarkStats)
                sb.Append("loaded ").Append(_stats.LandmarksLoaded).Append("  pending ").Append(_stats.LandmarksPending).Append("  empty ").Append(_stats.LandmarksEmpty).Append("  ");
            else sb.Append("layer requested, none reported  ");
            var lc = Bridge.LandmarkCover;
            sb.Append("| nodes held ").Append(lc.Loaded).Append("  tris ").Append(lc.TrianglesLoaded.ToString("N0")).Append("  lights ").Append(lc.LightsLoaded.ToString("N0"))
              .Append("  beacons ").Append(lc.BeaconsLoaded.ToString("N0"));
            if (Bridge.LandmarkUploadCount > 0)
                sb.Append("  upload ms last/max/avg ").Append((Bridge.LandmarkUploadUsLast / 1000.0).ToString("0.0")).Append('/').Append((Bridge.LandmarkUploadUsMax / 1000.0).ToString("0.0"))
                  .Append('/').Append((Bridge.LandmarkUploadUsSum / 1000.0 / Bridge.LandmarkUploadCount).ToString("0.0"));
            if (Bridge.LandmarkFadeKeysSeen > 0) sb.Append("  [l/ KEYS IN poll_fades: ").Append(Bridge.LandmarkFadeKeysSeen).Append(']');
        }
        sb.Append('\n');
    }

    /// <summary>World seed (api 1.7): generator and phases, the data source of the finest vector tile under the camera, airports near, load warnings.</summary>
    private void AppendSeedLine(StringBuilder sb)
    {
        var info = Bridge!.SeedInfo;
        if (info == null) { sb.Append("seed: no seed_world_info()\n"); return; }
        string under = "-";
        if (Camera != null)
        {
            var p = Camera.GlobalPosition;
            int bestRing = 99;
            for (int i = 0; i < _tiles.Count; i++)
            {
                var t = _tiles[i];
                if (t.Terrain || t.Layer == TileLayer.Landmarks || t.Ring >= bestRing || t.State != TileState.Ready) continue;
                if (p.X < t.Min.X || p.X > t.Max.X || p.Z < t.Min.Y || p.Z > t.Max.Y) continue;
                bestRing = t.Ring;
                under = $"{t.Source} (R{t.Ring} {t.Key})";
            }
        }
        string near = "-";
        double bestD = double.MaxValue;
        if (Camera != null)
            foreach (var a in Bridge.Airports)
            {
                double dx = a.Position.X - Camera.GlobalPosition.X, dz = a.Position.Z - Camera.GlobalPosition.Z;
                double d = System.Math.Sqrt(dx * dx + dz * dz);
                if (d < bestD) { bestD = d; near = $"{a.Ident} {d / 1000.0:0.0} km"; }
            }
        var m = Bridge.Markings;
        sb.Append("seed: ").Append(under).Append("  generator v").Append(info.GeneratorVersion).Append(" phases ").Append(string.Join("/", info.Phases))
          .Append("  airports ").Append(info.Airports).Append(" (nearest ").Append(near).Append(")  markings ").Append(m?.DrawnAirports ?? 0).Append(" airports ").Append(m?.Quads ?? 0).Append(" quads")
          .Append("  warnings ").Append(info.Warnings.Length).Append('\n');
        for (int i = 0; i < info.Warnings.Length && i < 3; i++) sb.Append("  seed warning: ").Append(info.Warnings[i]).Append('\n');
    }

    private static void AppendRings(StringBuilder sb, string name, int[] v, int count = -1, string prefix = "R")
    {
        sb.Append(name);
        if (count < 0 || count > v.Length) count = v.Length;
        for (int i = 0; i < count; i++) sb.Append("  ").Append(prefix).Append(i).Append(' ').Append(v[i]);
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
                if (t.Terrain || t.Layer == TileLayer.Landmarks || t.Ring != ring) continue; // landmark tiles (api 1.11, overview-sized, flat set) and terrain tiles (api 1.5) live in their own tile map and are 22 km wide: not on this minimap
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
