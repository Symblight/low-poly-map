using System.Collections.Generic;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Test-bench launch arguments for the <see cref="WorldView"/> (host side: the addon itself reads no command line). Applies
/// <c>--tile-url=</c>, <c>--no-landmarks</c>, <c>--curvature=</c>, <c>--light-alpha=</c>, <c>--fake-fade=</c>, <c>--profile=</c>, <c>--area=</c>,
/// <c>--set=</c>, <c>--lod-selftest</c>, <c>--view=</c> / <c>--cam-fov=</c> (screen-space LOD view), <c>--render-scale=</c> (3D render scale), <c>--memory-budget-mb=</c>, <c>--pool-trim-hold=</c>, <c>--world=</c> (seed world), <c>--seed-spot=</c>, <c>--seed-selftest</c>, <c>--cover-selftest</c>, <c>--latency-log=</c> and the terrain arguments (<c>--terrain</c>, <c>--terrain-detail=</c>,
/// <c>--set=</c>, <c>--benchmark=</c> / <c>--soak=</c> / <c>--perf-selftest</c> (see <c>Diagnostics/</c>), <c>--lod-selftest</c>, <c>--view=</c> / <c>--cam-fov=</c> (screen-space LOD view), <c>--world=</c> (seed world), <c>--seed-spot=</c>, <c>--seed-selftest</c>, <c>--cover-selftest</c>, <c>--latency-log=</c> and the terrain arguments (<c>--terrain</c>, <c>--terrain-detail=</c>,
/// <c>--terrain-reach-km=</c>, <c>--terrain-amplitude=</c>, <c>--far=</c>, <c>--terrain-selftest</c>, <c>--terrain-collision</c>,
/// <c>--terrain-collision-selftest</c>), then starts the view (the scene turns
/// <c>AutoStart</c> off). See low-poly-map/CLAUDE.md.
/// </summary>
public partial class BenchArgs : Node
{
    private const string TileUrlArg = "--tile-url=";
    private const string ProfileArg = "--profile=";
    private const string SetArg = "--set=";
    private const string AreaArg = "--area=";
    private const double DefaultMaxDistanceM = 25000.0;
    /// <summary>--area=&lt;a&gt;: the LOD quality set after start (1 / a); 0 = not given.</summary>
    private double _areaQuality;

    [Export] public WorldView? View { get; set; }

    /// <summary>Scene fallback plane height while no terrain runs / while it does (the plane then only shows through missing tiles).</summary>
    private const float FallbackY = -0.3f, FallbackYTerrain = -30f;
    private MeshInstance3D? _fallback;
    private double _terrainDetail = double.NaN;

    private static bool TryNumber(string text, out double v) =>
        double.TryParse(text, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out v);

    public override void _Ready()
    {
        var view = View;
        if (view == null) { GD.PushError("[BenchArgs] no WorldView assigned"); return; }
        var settings = view.Settings ??= new WorldRenderSettings();
        _sceneFar = view.Camera?.Far ?? -1f;
        settings.DebugChecks = WantsDebugChecks();

        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(TileUrlArg, System.StringComparison.Ordinal)) view.TileUrl = arg.Substring(TileUrlArg.Length); // may be empty -> synthetic
            else if (arg.StartsWith("--world=", System.StringComparison.Ordinal)) view.SeedWorld = arg.Substring(8).Trim(); // seed world manifest (api 1.7); clears the tile URL below
            else if (arg == "--no-landmarks") view.LandmarksEnabled = false; // api 1.11: do not opt in to the landmark layer; the landmarks stay folded into the vector tiles
            else if (arg.StartsWith("--curvature=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(12), out var cv)) view.Curvature = Mathf.Max((float)cv, 0f);
            else if (arg.StartsWith("--light-alpha=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(14), out var la)) settings.DebugLightAlpha = (float)la;
            else if (arg == "--no-building-lights") settings.BuildingLightSprites = false;
            else if (arg == "--no-upload-burst") settings.UploadBudgetBurstUs = 0;
            else if (arg.StartsWith("--pool-max-ring=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(16), out var pmr)) settings.StreetPoolMaxRing = (int)pmr;
            else if (arg.StartsWith("--fake-fade=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(12), out var ff)) settings.DebugFakeFade = (float)ff;
            else if (arg.StartsWith("--cam-fov=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(10), out var cf) && cf > 1 && cf < 170 && view.Camera != null)
            {
                if (view.Camera is FreeFlyCamera ffc) ffc.BaseFov = (float)cf; // the camera re-applies its base fov every frame (hold Z = zoom)
                view.Camera.Fov = (float)cf;
            }
            else if (arg.StartsWith("--view=", System.StringComparison.Ordinal)) ParseViewOverride(view, arg.Substring(7));
            else if (arg.StartsWith("--render-scale=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(15), out var rs) && rs >= 0.25 && rs <= 2) // like rendering/scaling_3d/scale: set_view then carries the 3D render height
            {
                view.GetViewport().Scaling3DScale = (float)rs;
                GD.Print($"[BenchArgs] 3D render scale {rs:0.##} (project setting rendering/scaling_3d/scale)");
            }
            else if (arg.StartsWith("--memory-budget-mb=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(19), out var mb)) settings.MemoryBudgetMb = (float)mb; // e.g. 224 = the mobile default
            else if (arg.StartsWith("--pool-trim-hold=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(17), out var th)) settings.PoolTrimHoldS = th; // seconds before idle pool sets are freed; 0 = never
            else if (arg.StartsWith(ProfileArg, System.StringComparison.Ordinal)) view.ProfilePath = arg.Substring(ProfileArg.Length).Trim();
            // A second instance (e.g. a screenshot run while the bench is open) needs its own cache folder: the cache is locked per folder.
            else if (arg.StartsWith("--cache-dir=", System.StringComparison.Ordinal)) view.CacheDir = arg.Substring(12).Trim();
        }
        // A seed world generates every tile: no archive, whatever --tile-url= or the scene said (the view would only warn and ignore it).
        if (view.SeedWorld.Length > 0)
        {
            view.TileUrl = "";
            GD.Print($"[BenchArgs] seed world {view.SeedWorld}: tile URL cleared, terrain layer on");
        }
        // Relative profile paths are relative to the cwd; the view globalizes res:// and user:// itself.
        // The view sizes the cull margin of curved meshes from the camera's far plane when it initializes (first DefaultProfile() call):
        // with terrain a provisional far plane of 160 km covers the default reach until ApplyTerrainArgs sets the final one.
        if (view.Camera != null && (TerrainDefaultOn() || view.SeedWorld.Length > 0 || System.Linq.Enumerable.Any(OS.GetCmdlineUserArgs(), a => a == "--terrain" || a == "--terrain-selftest" || a == "--terrain-collision" || a == "--terrain-collision-selftest" || a.StartsWith("--terrain-detail=", System.StringComparison.Ordinal))))
            view.Camera.Far = 160000f;
        view.BaseProfileOverrides = ParseOverrides(view);
        PinViewForTests(view);
        ApplyTerrainArgs(view);
        // Terrain collision (addon helper, default off): --terrain-collision turns it on (implies --terrain).
        view.TerrainCollisionEnabled = System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--terrain-collision") || TerrainCollisionSelfTest.Requested();

        bool coverSimOk = true;
        if (CoverSelfTest.Requested()) coverSimOk = CoverSelfTest.Run(view);
        view.Start();
        if (CoverSelfTest.Requested()) CoverLiveCheck.Attach(view, View?.GetParent(), coverSimOk);
        if (View?.GetParent() is { } sceneRoot) Diagnostics.Benchmark.Attach(view, Diagnostics.DiagnosticsNode.Attach(view, sceneRoot), sceneRoot);
        if (_areaQuality > 0) GD.Print($"[BenchArgs] --area: set_lod_quality({_areaQuality:0.###}) -> in effect {view.SetLodQuality(_areaQuality):0.###} (band edges are screen-space since api 1.10; bands.max_distance_m scaled by the area)");
        double latencyEvery = LatencyLog.Requested();
        if (latencyEvery > 0) LatencyLog.Attach(view, latencyEvery);
        if (WindowSelfTest.Requested()) GetTree().Quit(WindowSelfTest.Run(view) ? 0 : 1);
        if (Diagnostics.PerfSelfTest.Requested()) GetTree().Quit(Diagnostics.PerfSelfTest.Run() ? 0 : 1);
        if (!double.IsNaN(_terrainDetail) && view.TerrainActive)
            GD.Print($"[BenchArgs] --terrain-detail={_terrainDetail}: library scale in effect {view.SetTerrainDetail(_terrainDetail):0.##}");
        if (System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--hide-vector")) view.VectorVisible = false;
        if (System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--terrain-log")) view.TerrainDebugLog = true;
        if (System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--churn-log")) { view.ChurnLog = true; view.OriginShifted += sh => GD.Print($"[Churn] t={Time.GetTicksMsec() / 1000.0:0.00} origin shift {sh}"); view.Reconfigured += (ok, e) => GD.Print($"[Churn] t={Time.GetTicksMsec() / 1000.0:0.00} RECONFIGURE ok={ok} {e}"); }
        if (UploadMicroBench.Requested()) UploadMicroBench.Attach(View?.GetParent() ?? view);
        if (FadeSelfTest.Requested()) { (View?.GetParent() ?? view).CallDeferred(Node.MethodName.AddChild, new FadeSelfTest { Name = "FadeSelfTest" }); return; }
        if (HoleDetector.Requested(out double holeSecs)) HoleDetector.Attach(view, View?.GetParent(), holeSecs);
        if (HoleFlight.Requested(out int holeSeed)) HoleFlight.Attach(view, View?.GetParent(), holeSeed);
        if (HoleInject.Requested(out string holeInject)) HoleInject.Attach(view, View?.GetParent(), holeInject);
        if (LodSelfTest.Requested()) LodSelfTest.Attach(view, View?.GetParent());
        if (SeedSpots.Requested() is { } spot) SeedSpotDriver.Attach(view, View?.GetParent(), spot);
        if (SeedSelfTest.Requested()) SeedSelfTest.Attach(view, View?.GetParent());
        if (TerrainSelfTest.Requested()) TerrainSelfTest.Attach(view, View?.GetParent());
        if (TerrainCollisionSelfTest.Requested()) TerrainCollisionSelfTest.Attach(view, View?.GetParent());
        _fallback = GetParent()?.GetNodeOrNull<MeshInstance3D>("Ground");
        if (_fallback != null && System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--hide-fallback")) { _fallback.Visible = false; _fallback = null; }
    }

    /// <summary>
    /// The addon's debug checks (periodic cover verification, the first-tile <c>debug_stats</c> print) cost frame time, so they run only where something is
    /// checked: headless smoke runs, every <c>*-selftest</c>, <c>--geodetic-test</c>, <c>--hole-check</c>, or <c>--debug-checks</c>. A benchmark or soak run never
    /// has them (unless <c>--debug-checks</c> is given): its numbers are the frame cost without them.
    /// </summary>
    private static bool WantsDebugChecks()
    {
        bool want = DisplayServer.GetName() == "headless";
        foreach (var a in OS.GetCmdlineUserArgs())
        {
            if (a == "--debug-checks") return true;
            if (a.StartsWith("--benchmark", System.StringComparison.Ordinal) || a.StartsWith("--soak", System.StringComparison.Ordinal)) return false;
            if (a.EndsWith("-selftest", System.StringComparison.Ordinal) || a == "--geodetic-test" || a.StartsWith("--hole-check", System.StringComparison.Ordinal)) want = true;
        }
        return want;
    }

    /// <summary>
    /// A headless window is about 64 px tall, which would make the screen-space edges tiny, and a self-test must not depend on the window of the machine
    /// it runs on: headless runs and every <c>*-selftest</c> / <c>--geodetic-test</c> / <c>--shot</c> run pin the view to 60 deg / 1080 px (the
    /// reference view of the default profile) unless <c>--view=</c> was given. Interactive runs use the real window.
    /// </summary>
    private static void PinViewForTests(WorldView view)
    {
        var args = OS.GetCmdlineUserArgs();
        bool test = DisplayServer.GetName() == "headless";
        foreach (var a in args)
        {
            if (a.StartsWith("--view=", System.StringComparison.Ordinal)) return;
            if (a.EndsWith("-selftest", System.StringComparison.Ordinal) || a.StartsWith("--shot=", System.StringComparison.Ordinal) || a == "--geodetic-test") test = true;
        }
        if (!test) return;
        view.SetViewOverride(60f, 1080);
        GD.Print("[BenchArgs] test/headless run: view pinned to 60 deg / 1080 px");
    }

    /// <summary><c>--view=&lt;fovdeg&gt;,&lt;H&gt;</c>: overrides what the view sends with set_view (api 1.10); either part may be empty (<c>--view=,540</c>, <c>--view=30</c>).</summary>
    private static void ParseViewOverride(WorldView view, string text)
    {
        var parts = text.Split(',');
        float? fov = null;
        int? h = null;
        if (parts.Length > 0 && parts[0].Trim().Length > 0)
        {
            if (TryNumber(parts[0].Trim(), out var f) && f > 0.1 && f < 170) fov = (float)f;
            else GD.PushWarning($"[BenchArgs] bad --view={text}; expected --view=<fov_y deg>,<H px>");
        }
        if (parts.Length > 1 && parts[1].Trim().Length > 0)
        {
            if (TryNumber(parts[1].Trim(), out var hv) && hv >= 1 && hv <= 16384) h = (int)System.Math.Round(hv);
            else GD.PushWarning($"[BenchArgs] bad --view={text}; expected --view=<fov_y deg>,<H px>");
        }
        view.SetViewOverride(fov, h);
        GD.Print($"[BenchArgs] --view: fov_y {(fov.HasValue ? fov.Value.ToString("0.##") : "camera")} deg, H {(h.HasValue ? h.Value.ToString() : "viewport")} px");
    }

    private bool _wasTerrain;
    private float _sceneFar = -1f;

    public override void _Process(double delta)
    {
        UpdateTerrainCamera();
        // The big fallback plane drops to -30 m while the terrain layer runs, so it only shows through missing tiles.
        if (_fallback == null || View == null) return;
        float y = View.TerrainActive ? FallbackYTerrain : FallbackY;
        if (_fallback.Position.Y != y) _fallback.Position = new Vector3(_fallback.Position.X, y, _fallback.Position.Z);
    }

    /// <summary>
    /// While the terrain layer runs: the free-fly camera cannot go below the terrain (its back faces are culled, so the world seems to vanish)
    /// and the far plane covers the terrain reach (x 1.15). Follows the layer when the settings panel switches it on or off at run time.
    /// </summary>
    private void UpdateTerrainCamera()
    {
        var view = View;
        if (view?.Camera == null) return;
        if (view.Camera is FreeFlyCamera ff) ff.ClampToGround = view.TerrainActive;
        if (view.TerrainActive == _wasTerrain) return;
        if (_sceneFar < 0f) _sceneFar = view.Camera.Far;
        if (view.TerrainActive) view.Camera.Far = Mathf.Max(view.Camera.Far, view.TerrainReachM * 1.15f);
        else view.Camera.Far = _sceneFar;
        _wasTerrain = view.TerrainActive;
    }

    /// <summary>
    /// Terrain arguments: <c>--terrain</c> (opt in to the layer and enable it in the profile), <c>--terrain-detail=&lt;x&gt;</c> (set_terrain_detail after start: a scale of the terrain cover's error threshold, above 1 coarser, below 1 finer and expensive),
    /// <c>--terrain-amplitude=&lt;m&gt;</c> (profile terrain.amplitude_m), <c>--terrain-reach-km=&lt;x&gt;</c> (truncate terrain.bands: bands whose inner edge is below
    /// the reach are kept, the last one is clamped to it), <c>--far=&lt;m&gt;</c> (camera far plane; with terrain on it defaults to the last band edge x 1.15).
    /// The layer is on by default (<c>--no-terrain</c> = flat world; self-tests and <c>--geodetic-test</c> stay flat unless <c>--terrain</c> is given). <c>--terrain-selftest</c> implies <c>--terrain</c>. A visual curvature below 1 turns the real-size horizon cull off. Explicit <c>--set=terrain.*</c> values win.
    /// </summary>
    private void ApplyTerrainArgs(WorldView view)
    {
        bool terrain = TerrainDefaultOn() || view.SeedWorld.Length > 0, reachGiven = false; // a seed world requires the terrain layer
        double reachKm = 0, amplitude = double.NaN, far = double.NaN;
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg == "--no-terrain") terrain = view.SeedWorld.Length > 0;
            else if (arg == "--terrain" || arg == "--terrain-selftest" || arg == "--terrain-collision" || arg == "--terrain-collision-selftest") terrain = true;
            else if (arg.StartsWith("--terrain-detail=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(17), out var td)) { _terrainDetail = td; terrain = true; }
            else if (arg.StartsWith("--terrain-amplitude=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(20), out var ta)) amplitude = ta;
            else if (arg.StartsWith("--terrain-reach-km=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(19), out var tr)) { reachKm = tr; reachGiven = true; }
            else if (arg.StartsWith("--far=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(6), out var f)) far = f;
        }
        view.EnableTerrain = view.TerrainEnabled = terrain;
        var o = view.BaseProfileOverrides;
        Godot.Collections.Dictionary T()
        {
            if (o.TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary) return tv.AsGodotDictionary();
            var d = new Godot.Collections.Dictionary();
            o["terrain"] = d;
            return d;
        }
        if (terrain)
        {
            if (!double.IsNaN(amplitude) && !T().ContainsKey("amplitude_m")) T()["amplitude_m"] = amplitude;
            if (view.Curvature < 1f && !T().ContainsKey("horizon_cull")) T()["horizon_cull"] = false;
            if (reachGiven && reachKm > 0 && !T().ContainsKey("bands")) TruncateBands(view, T(), reachKm * 1000.0);
        }

        // Camera far plane: --far=, else the terrain reach x 1.15 when the layer is on (the default 30 km shows nothing of T3/T4).
        if (view.Camera == null) return;
        if (!double.IsNaN(far)) view.Camera.Far = (float)far;
        else if (terrain)
        {
            double reach = TerrainReachOf(view);
            if (reach > 0) view.Camera.Far = (float)(reach * 1.15);
        }
        if (terrain) GD.Print($"[BenchArgs] terrain on: camera far {view.Camera.Far / 1000f:0.#} km, overrides {Godot.Json.Stringify(view.BaseProfileOverrides)}");
    }

    /// <summary>The bench shows the terrain by default (<c>--no-terrain</c> turns it off); self-tests and the geodetic test keep their flat default and need <c>--terrain</c>.</summary>
    private static bool TerrainDefaultOn()
    {
        foreach (var arg in OS.GetCmdlineUserArgs())
            if (arg == "--no-terrain" || arg.EndsWith("-selftest", System.StringComparison.Ordinal) || arg == "--geodetic-test") return false;
        return true;
    }

    /// <summary>Last terrain band edge in metres from the overrides or else the library defaults (0 when unknown).</summary>
    private static double TerrainReachOf(WorldView view)
    {
        Godot.Collections.Array? bands = null;
        if (view.BaseProfileOverrides.TryGetValue("terrain", out var ov) && ov.VariantType == Variant.Type.Dictionary
            && ov.AsGodotDictionary().TryGetValue("bands", out var ob) && ob.VariantType == Variant.Type.Array) bands = ob.AsGodotArray();
        else if (view.DefaultProfile().TryGetValue("terrain", out var dv) && dv.VariantType == Variant.Type.Dictionary
            && dv.AsGodotDictionary().TryGetValue("bands", out var db) && db.VariantType == Variant.Type.Array) bands = db.AsGodotArray();
        if (bands == null || bands.Count == 0 || bands[bands.Count - 1].VariantType != Variant.Type.Dictionary) return 0;
        return bands[bands.Count - 1].AsGodotDictionary().TryGetValue("edge_m", out var e) ? e.AsDouble() : 0;
    }

    /// <summary>
    /// Shortens the terrain layer to <paramref name="reachM"/>: keeps the default bands whose inner edge is below the reach (finest first),
    /// clamps the last kept edge to it and moves terrain.far_zoom to the coarsest kept zoom (the bands array replaces the default as a whole).
    /// </summary>
    private static void TruncateBands(WorldView view, Godot.Collections.Dictionary t, double reachM)
    {
        if (!view.DefaultProfile().TryGetValue("terrain", out var dv) || dv.VariantType != Variant.Type.Dictionary) return;
        var def = dv.AsGodotDictionary();
        if (!def.TryGetValue("bands", out var bv) || !def.TryGetValue("near_zoom", out var nz)) return;
        var src = bv.AsGodotArray();
        var kept = new Godot.Collections.Array();
        double inner = 0;
        for (int i = 0; i < src.Count; i++)
        {
            var b = src[i].AsGodotDictionary();
            double edge = b["edge_m"].AsDouble();
            if (i > 0 && inner >= reachM) break;
            var nb = (Godot.Collections.Dictionary)b.Duplicate(true);
            if (edge > reachM) nb["edge_m"] = System.Math.Max(reachM, inner + 100.0);
            kept.Add(nb);
            inner = edge;
        }
        t["bands"] = kept;
        t["far_zoom"] = nz.AsInt64() - (kept.Count - 1);
        GD.Print($"[BenchArgs] --terrain-reach-km: {kept.Count} terrain bands (z{nz.AsInt64()}..z{nz.AsInt64() - (kept.Count - 1)}), last edge {kept[kept.Count - 1].AsGodotDictionary()["edge_m"].AsDouble() / 1000.0:0.#} km");
    }

    /// <summary>
    /// --area=&lt;0.05..1&gt; (scales the loaded area: <c>bands.max_distance_m</c> x area, applied first, and after start <c>set_lod_quality(1 / area)</c>, which scales the refine distances) and repeated --set=&lt;dotted.key&gt;=&lt;number&gt; (applied last, wins).
    /// Returns the nested overrides (may be empty).
    /// </summary>
    private Godot.Collections.Dictionary ParseOverrides(WorldView view)
    {
        double area = 0, quality = 0;
        var sets = new List<(string Key, Variant Value)>();
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(AreaArg, System.StringComparison.Ordinal))
            {
                if (!TryNumber(arg.Substring(AreaArg.Length), out var a)) { GD.PushWarning($"[BenchArgs] bad {arg}; expected --area=<0.05..1>"); continue; }
                area = System.Math.Clamp(a, 0.05, 1.0);
                if (area != a) GD.Print($"[BenchArgs] --area={a} clamped to {area}");
            }
            else if (arg.StartsWith(SetArg, System.StringComparison.Ordinal))
            {
                var kv = arg.Substring(SetArg.Length);
                int eq = kv.IndexOf('=');
                if (eq <= 0 || eq == kv.Length - 1) { GD.PushWarning($"[BenchArgs] bad {arg}; expected --set=<dotted.key>=<number>"); continue; }
                var key = kv.Substring(0, eq).Trim();
                var text = kv.Substring(eq + 1).Trim();
                // Numbers always go as float (like Godot's JSON parser): Rust accepts integral floats for integer keys.
                if (TryNumber(text, out var num)) sets.Add((key, num));
                else if (text == "true" || text == "false") sets.Add((key, text == "true"));
                else sets.Add((key, text));
            }
        }

        var overrides = new Godot.Collections.Dictionary();
        if (area > 0)
        {
            var profile = view.DefaultProfile();
            double maxDist = profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary && bv.AsGodotDictionary().TryGetValue("max_distance_m", out var md) ? md.AsDouble() : DefaultMaxDistanceM;
            overrides["bands"] = new Godot.Collections.Dictionary { ["max_distance_m"] = System.Math.Max(50.0, maxDist * area) };
            quality = 1.0 / area;
        }
        foreach (var (key, value) in sets) SetDotted(overrides, key, value);
        _areaQuality = quality;
        return overrides;
    }

    private static void SetDotted(Godot.Collections.Dictionary root, string dotted, Variant value)
    {
        var parts = dotted.Split('.');
        var d = root;
        for (int i = 0; i < parts.Length - 1; i++)
        {
            if (!(d.TryGetValue(parts[i], out var sub) && sub.VariantType == Variant.Type.Dictionary))
            {
                var nd = new Godot.Collections.Dictionary();
                d[parts[i]] = nd;
                d = nd;
            }
            else d = sub.AsGodotDictionary();
        }
        d[parts[^1]] = value;
    }
}
