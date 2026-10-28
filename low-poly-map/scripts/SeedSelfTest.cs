using System.Collections.Generic;
using System.Globalization;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--seed-selftest</c> (docs/spec-world-seed.md 11a.3; run it in <c>scenes/SeedTwoCities.tscn</c> or with <c>--world=</c>; headless works):
/// <c>$G --headless --path . res://scenes/SeedTwoCities.tscn --quit-after 3000 -- --seed-selftest</c>. The checks are grouped by phase and a group
/// runs only when <c>seed_world_info()["phases"]</c> lists it. Phase S1:
/// <list type="bullet">
/// <item><c>status()</c> is ok (the terrain layer is active, no degraded message) and <c>airports()</c> contains EYVI and EYKA with both runway ends.</item>
/// <item>At the spots <c>eyvi</c> / <c>eyka</c>: along the main runway's centreline 20 points of <c>ground_height_at</c> are linear between the two landing
/// threshold heights within 1 cm and equal across the strip within 1 cm; the runway's own light list is not empty; tiles over it carry
/// <c>airport_lights</c> (the key is non-empty regardless of the hour; the spot is flown at the clock of the bench, 22:00 by default).</item>
/// <item>At <c>coast</c>: ten offshore points of <c>ground_height_at</c> equal <c>terrain.sea_level_m</c>, and every surface vertex of every held terrain
/// tile in the open sea is at sea level. At <c>lake</c>: 25 points over the lake are flat.</item>
/// <item>At every spot (<see cref="SeedSpots.Names"/>): the first vector tile arrives within 5 s of the teleport (<c>update_camera_geodetic</c>), an R3 ground
/// mesh is held, every vector tile in <c>debug_tiles</c> says <c>source: seed</c> and every terrain tile <c>terrain</c>.</item>
/// <item>Determinism: after each spot settles, <c>SEED HASHES &lt;spot&gt; &lt;hash&gt;</c> prints a hash of every held tile (keys, rings, quantized mesh and
/// light content, not the frame-dependent origins); two runs must print the same lines.</item>
/// </list>
/// Prints <c>SEED SELFTEST PASS</c>, or one <c>FAIL &lt;check&gt;: &lt;detail&gt;</c> line per failure and <c>SEED SELFTEST FAIL: n</c>; exit code 0 / 1.
/// Options: <c>--seed-selftest-spots=a,b,c</c> (subset), <c>--seed-selftest-timeout=&lt;s&gt;</c> (real-time failsafe, default 240).
/// </summary>
public partial class SeedSelfTest : Node
{
    private const string Arg = "--seed-selftest";

    public static bool Requested() => System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), Arg);

    public static void Attach(WorldView view, Node? parent) =>
        (parent ?? view).CallDeferred(Node.MethodName.AddChild, new SeedSelfTest { Name = "SeedSelfTest", _view = view });

    private WorldView _view = null!;
    private FreeFlyCamera? _cam;
    private readonly List<string> _fails = new();
    private readonly List<DebugTile> _tiles = new(1024);
    private readonly StreamerStats _stats = new();
    private double _timeoutS = 240.0;
    private ulong _startMs;
    private bool _abort;
    private string[] _spots = SeedSpots.Names;
    private double _seaLevel;

    public override void _Ready()
    {
        ProcessPriority = 1; // after WorldView (0): this frame's uploads and unloads are done
        foreach (var a in OS.GetCmdlineUserArgs())
        {
            if (a.StartsWith("--seed-selftest-timeout=", System.StringComparison.Ordinal) && double.TryParse(a.Substring(24), NumberStyles.Float, CultureInfo.InvariantCulture, out var to)) _timeoutS = to;
            else if (a.StartsWith("--seed-selftest-spots=", System.StringComparison.Ordinal)) _spots = a.Substring(22).Split(',', System.StringSplitOptions.RemoveEmptyEntries | System.StringSplitOptions.TrimEntries);
        }
        // Headless frames are free, tile generation is not: cap the rate so --quit-after 3000 (frames) leaves real time for the workers, and deliver
        // faster per frame (no GPU in a headless run, so the 2 ms upload budget protects nothing here).
        if (Engine.MaxFps == 0) Engine.MaxFps = 30;
        if (_view.Settings != null) { _view.Settings.MaxTilesPerFrame = 24; _view.Settings.UploadBudgetUs = 1_000_000; }
        _cam = _view.Camera as FreeFlyCamera;
        _startMs = Time.GetTicksMsec();
        RunAll();
    }

    private async void RunAll()
    {
        try { await Body(); }
        catch (System.Exception e) { Fail("exception", e.ToString()); }
        _view.ExternalCameraUpdate = null;
        if (_fails.Count == 0) GD.Print("SEED SELFTEST PASS");
        else
        {
            foreach (var f in _fails) GD.Print("FAIL " + f);
            GD.Print($"SEED SELFTEST FAIL: {_fails.Count} check(s) failed");
        }
        GetTree().Quit(_fails.Count == 0 ? 0 : 1);
    }

    private void Fail(string check, string detail) => _fails.Add($"{check}: {detail}");

    private void Note(string line) => GD.Print("[SeedSelfTest] " + line);

    private async Task<bool> WaitFor(System.Func<bool> cond, double timeoutS)
    {
        ulong end = Time.GetTicksMsec() + (ulong)(timeoutS * 1000);
        while (!cond())
        {
            if (_abort) return false;
            if (Time.GetTicksMsec() > end) return false;
            if ((Time.GetTicksMsec() - _startMs) / 1000.0 > _timeoutS) { _abort = true; Fail("timeout", $"real-time failsafe {_timeoutS} s"); return false; }
            await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame);
        }
        return true;
    }

    private async Task Body()
    {
        if (_cam == null) { Fail("setup", "the WorldView has no FreeFlyCamera"); return; }
        if (!await WaitFor(() => _view.WorldRunning || _view.ConfigureError.Length > 0, 15)) { Fail("setup", "world did not start"); return; }
        if (!_view.WorldRunning) { Fail("setup", "configure failed: " + _view.ConfigureError); return; }
        var info = _view.SeedInfo;
        if (!_view.SeedWorldActive || info == null) { Fail("setup", "no seed world is active (run scenes/SeedTwoCities.tscn or --world=<manifest>)"); return; }
        Note("seed world: " + info.Summary);
        _view.Streamer.DebugStats(_stats);
        if (_view.DefaultProfile().Count > 0 && _view.Streamer.WorldProfile().TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary
            && tv.AsGodotDictionary().TryGetValue("sea_level_m", out var sl)) _seaLevel = sl.AsDouble();

        bool s1 = info.HasPhase("S1");
        if (!s1) { Fail("phases", $"seed_world_info phases [{string.Join(", ", info.Phases)}] has no S1: nothing to test"); return; }

        // ---- S1: status and airports -----------------------------------------------------
        var st = _view.Streamer.Status();
        if (!st.IsOk) Fail("status", $"state {st.State}: {st.Message}");
        if (st.Message.Contains("degraded", System.StringComparison.OrdinalIgnoreCase)) Fail("status", "degraded message: " + st.Message);
        if (!_view.TerrainActive || !st.TerrainActive) Fail("terrain", $"the terrain layer is not active (host {_view.TerrainActive}, status {st.TerrainActive})");
        foreach (string ident in new[] { "EYVI", "EYKA" })
        {
            AirportInfo? a = null;
            foreach (var x in _view.Airports) if (x.Ident == ident) a = x;
            if (a == null) { Fail("airports", $"{ident} is missing from airports() ({_view.Airports.Length} airports)"); continue; }
            if (a.Runways.Length == 0) { Fail("airports", $"{ident} has no runway"); continue; }
            foreach (var r in a.Runways)
            {
                if (r.Le.Ident.Length == 0 || r.He.Ident.Length == 0) Fail("airports", $"{ident} {r.Designator}: a runway end has no ident");
                if (r.Le.Position.DistanceTo(r.He.Position) < 100f) Fail("airports", $"{ident} {r.Designator}: the two ends are {r.Le.Position.DistanceTo(r.He.Position):0.0} m apart");
                if (r.Lighted && r.Lights.Length == 0) Fail("airports", $"{ident} {r.Designator}: lighted but runway lights are empty");
            }
        }
        Note($"airports(): {_view.Airports.Length}, EYVI/EYKA present, sea level {_seaLevel:0.##} m");

        // ---- spots -------------------------------------------------------------------------
        foreach (var spot in _spots)
        {
            if (_abort) break;
            await VisitSpot(spot);
        }
        var st2 = _view.Streamer.Status();
        if (!st2.IsOk) Fail("status", $"after the flight: state {st2.State}: {st2.Message}");
        if (_view.ConsistencyErrors != 0) Fail("cover", $"{_view.ConsistencyErrors} cover consistency error(s)");
    }

    private bool VectorQuiet()
    {
        _view.Streamer.DebugStats(_stats);
        int busy = 0;
        for (int i = 0; i < StreamerStats.Rings; i++) busy += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
        for (int i = 0; i < _stats.TerrainRings; i++) busy += _stats.TerrainQueued[i] + _stats.TerrainMeshing[i] + _stats.TerrainFetching[i];
        if (busy != 0 || _view.Cover.FadingTiles != 0 || _view.TerrainCover.FadingTiles != 0 || _view.ReconfigurePending) return false;
        // Everything the library tracks is ready AND held by the host (finished tiles can wait for the host's poll budget).
        _view.Streamer.DebugTiles(_tiles);
        foreach (var t in _tiles)
        {
            if (t.Layer == TileLayer.Landmarks) // api 1.11: a flat third map (a seed world delivers none; "empty" = built, never delivered)
            {
                if (t.State is TileState.Queued or TileState.Fetching or TileState.Meshing or TileState.Unloading) return false;
                if (t.State == TileState.Ready && !_view.LandmarkCover.TryGet(t.Packed, out _)) return false;
                continue;
            }
            if (t.State != TileState.Ready) return false;
            if (!(t.Terrain ? _view.TerrainCover : _view.Cover).TryGet(t.Packed, out _)) return false;
        }
        return true;
    }

    private async Task VisitSpot(string name)
    {
        if (!SeedSpots.TryResolve(_view, name, out var pose)) { Fail("spot " + name, "cannot be resolved (airport missing?)"); return; }
        ulong t0 = Time.GetTicksMsec();
        bool teleported = false;
        SeedSpots.Teleport(_view, _cam!, pose, _ => teleported = true);
        if (!await WaitFor(() => teleported, 5)) { Fail("spot " + name, "teleport hook never ran"); return; }
        ulong tTele = Time.GetTicksMsec();
        int origin0 = _view.OriginShiftCount;

        // First tile under the camera within 5 s (tiles of the old place are gone with the teleport; a tile elsewhere does not count).
        bool first = await WaitFor(() => HeldUnderCamera(-1, false), 5.0);
        double firstMs = Time.GetTicksMsec() - tTele;
        if (!first) Fail("first tile " + name, $"no vector tile arrived within 5 s of the teleport");

        // R3 ground present: some R3 tile (7-25 km out; a camera at cruise height has none under it) with a ground mesh.
        bool r3 = await WaitFor(() => R3Tiles(out int withGround, out _) > 0 && withGround > 0, 12.0);
        double r3Ms = Time.GetTicksMsec() - tTele;
        if (!r3) Fail("R3 ground " + name, "no R3 vector tile with a ground mesh after 12 s");

        // Settle: no queued / meshing work and no fade, for 40 consecutive frames.
        int quiet = 0;
        bool settled = await WaitFor(() => { quiet = VectorQuiet() ? quiet + 1 : 0; return quiet >= 40; }, 45.0);
        double settleS = (Time.GetTicksMsec() - tTele) / 1000.0;
        if (!settled) Fail("settle " + name, "the streamer did not go quiet within 45 s");

        // Once settled, every held R3 tile has a ground mesh (open sea is water ground, land is countryside).
        int r3n = R3Tiles(out int r3g, out string r3Missing);
        if (settled && r3n > 0 && r3g < r3n) Fail("R3 ground " + name, $"{r3n - r3g} of {r3n} held R3 tiles have no ground mesh, e.g. {r3Missing}");

        // Sources.
        _view.Streamer.DebugTiles(_tiles);
        int seedTiles = 0, terrainTiles = 0;
        foreach (var t in _tiles)
        {
            if (t.Layer == TileLayer.Landmarks) continue;
            if (t.Terrain) { terrainTiles++; if (t.Source != "terrain") { Fail("source " + name, $"terrain tile {t.Key} has source '{t.Source}'"); break; } }
            else { seedTiles++; if (t.Source != "seed") { Fail("source " + name, $"vector tile {t.Key} has source '{t.Source}'"); break; } }
        }

        string extra = "";
        switch (name)
        {
            case "eyvi": extra = CheckAirport("EYVI", name); break;
            case "eyka": extra = CheckAirport("EYKA", name); break;
            case "coast": extra = CheckCoast(); break;
            case "lake": extra = CheckLake(pose); break;
        }

        var hash = HashTiles();
        GD.Print($"SEED HASHES {name} {hash:x16}");
        Note($"{name}: {pose.AltAglM:0} m AGL at {pose.Lat:0.0000},{pose.Lon:0.0000}; first tile {firstMs:0} ms, R3 ground {r3Ms:0} ms ({r3n} R3 tiles, {r3g} with ground), quiet after {settleS:0.0} s; "
            + $"{_view.Cover.Loaded} vector + {_view.TerrainCover.Loaded} terrain tiles held ({seedTiles}/{terrainTiles} in debug_tiles), origin shifts {_view.OriginShiftCount - origin0}{extra}");
    }

    /// <summary>Held R3 vector tiles; <paramref name="withGround"/> of them have a ground mesh, <paramref name="firstMissing"/> names one without.</summary>
    private int R3Tiles(out int withGround, out string firstMissing)
    {
        int n = 0;
        withGround = 0;
        firstMissing = "";
        foreach (var t in _view.Cover.All)
        {
            if (t.Ring != 3) continue;
            n++;
            if (t.HasGround && t.GroundTris > 0) withGround++;
            else if (firstMissing.Length == 0) firstMissing = t.Key;
        }
        return n;
    }

    /// <summary>True when a vector tile of <paramref name="ring"/> (-1 = any) that the host holds covers the camera's x/z; with <paramref name="needGround"/> it must carry a ground mesh.</summary>
    private bool HeldUnderCamera(int ring, bool needGround)
    {
        var p = _cam!.GlobalPosition;
        _view.Streamer.DebugTiles(_tiles);
        foreach (var t in _tiles)
        {
            if (t.Terrain || t.Layer == TileLayer.Landmarks || t.State != TileState.Ready || (ring >= 0 && t.Ring != ring)) continue;
            if (p.X < t.Min.X || p.X > t.Max.X || p.Z < t.Min.Y || p.Z > t.Max.Y) continue;
            if (!_view.Cover.TryGet(t.Packed, out var n)) continue;
            if (needGround && !(n.HasGround && n.GroundTris > 0)) continue;
            return true;
        }
        return false;
    }

    // ---- airport checks -------------------------------------------------------------------------

    private string CheckAirport(string ident, string spot)
    {
        AirportInfo? a = null;
        foreach (var x in _view.Airports) if (x.Ident == ident) a = x;
        if (a?.MainRunway is not { } rw) { Fail("runway " + spot, $"{ident} has no main runway in airports()"); return ""; }
        // Positions in the frame of airports() now (refreshed after every origin shift).
        Vector3 pa = rw.Le.ThresholdPosition, pb = rw.He.ThresholdPosition;
        var axis = new Vector2(pb.X - pa.X, pb.Z - pa.Z);
        float len = axis.Length();
        var dir = axis / len;
        var side = new Vector2(-dir.Y, dir.X);
        double maxLinear = 0, maxAcross = 0;
        float w = (float)rw.WidthM;
        float[] offs = { -(w * 0.5f + 10f), -w * 0.5f, -w * 0.25f, w * 0.25f, w * 0.5f, w * 0.5f + 10f };
        for (int i = 0; i < 20; i++)
        {
            float f = i / 19f;
            var p = new Vector2(pa.X, pa.Z) + axis * f;
            double expect = pa.Y + (pb.Y - pa.Y) * f;
            double h = _view.Streamer.GroundHeightAt(p.X, p.Y);
            maxLinear = System.Math.Max(maxLinear, System.Math.Abs(h - expect));
            foreach (float t in offs)
            {
                var q = p + side * t;
                double hq = _view.Streamer.GroundHeightAt(q.X, q.Y);
                maxAcross = System.Math.Max(maxAcross, System.Math.Abs(hq - h));
            }
        }
        if (maxLinear > 0.01) Fail("runway linear " + ident, $"ground_height_at departs {maxLinear * 100:0.00} cm from the line between the threshold heights {pa.Y:0.000} and {pb.Y:0.000} (limit 1 cm)");
        if (maxAcross > 0.01) Fail("runway level across " + ident, $"{maxAcross * 100:0.00} cm difference across the strip (limit 1 cm)");

        string drawn = CheckDrawnRunway(ident, a, rw);

        int lightTiles = 0;
        long lights = 0;
        foreach (var n in _view.Cover.All) if (n.AirportLightCount > 0) { lightTiles++; lights += n.AirportLightCount; }
        if (lightTiles == 0) Fail("airport lights " + ident, $"no held tile carries airport_lights (clock {_view.Hour:0.0} h)");
        // The key has colours of the four fixed kinds: blue / white / green / red appear on a lighted airport.
        return $"; {ident} {rw.Designator} {rw.LengthM:0} m x {rw.WidthM:0} m, slope {(pb.Y - pa.Y) / len * 100:0.00} %, linear err {maxLinear * 1000:0.0} mm, across err {maxAcross * 1000:0.0} mm, "
            + $"{lights} airport lights in {lightTiles} tiles (runway list {rw.Lights.Length}){drawn}";
    }

    /// <summary>
    /// The drawn runway height the host uses (generator 2: <c>ground_height_at</c> + <see cref="WorldRenderSettings.MarkingRunwayLevel"/> x <c>ground.rN.lift_step_m</c>, no
    /// drape lift): per ring equal to the formula within 1 mm; and, with the markings drawn for this airport, every mesh vertex of its main runway sits on
    /// <c>ground_height_at</c> (within the 1 cm of the linear check) so that mesh y + the R0 lift is the stacking residual (0.06 m, +-0.015) above it.
    /// </summary>
    private string CheckDrawnRunway(string ident, AirportInfo a, RunwayInfo rw)
    {
        int gen = _view.SeedInfo?.GeneratorVersion ?? 0;
        if (gen < 2) return $"; drawn runway lift: generator {gen}, old drape replay {_view.RunwayLift.X:0.##} m (no level check)";
        var prof = _view.Streamer.WorldProfile();
        var lift = _view.RunwayLift;
        var want = new float[4];
        try
        {
            var ground = prof["ground"].AsGodotDictionary();
            for (int n = 0; n < 4; n++)
            {
                double step = ground["r" + n].AsGodotDictionary()["lift_step_m"].AsDouble();
                want[n] = (float)(3 * step);
                float have = n == 0 ? lift.X : n == 1 ? lift.Y : n == 2 ? lift.Z : lift.W;
                if (System.Math.Abs(have - want[n]) > 0.001) Fail("drawn runway lift " + ident, $"R{n}: host lift {have:0.0000} m, 3 x lift_step_m {step:0.0000} = {want[n]:0.0000} m (1 mm)");
            }
        }
        catch (System.Exception e) { Fail("drawn runway lift " + ident, "cannot read ground.rN.lift_step_m: " + e.Message); return ""; }
        if (_view.RunwayLiftRule != "level") Fail("drawn runway lift " + ident, $"generator {gen} but the host rule is '{_view.RunwayLiftRule}'");
        var m = _view.Markings;
        if (m == null || m.DrawnAirports == 0 || m.Vertices.Count == 0) { Fail("marking mesh " + ident, "no marking mesh is drawn at the airport"); return ""; }
        // Vertices of this airport's runways (the mesh holds every airport within 4.5 km): those within a runway's extent.
        double worstGround = 0, lo = double.MaxValue, hi = double.MinValue;
        int n2 = 0;
        foreach (var v in m.Vertices)
        {
            bool near = false;
            foreach (var r in a.Runways)
            {
                float dx = v.X - r.Le.ThresholdPosition.X, dz = v.Z - r.Le.ThresholdPosition.Z;
                var ax = new Vector2(r.He.ThresholdPosition.X - r.Le.ThresholdPosition.X, r.He.ThresholdPosition.Z - r.Le.ThresholdPosition.Z);
                float len = ax.Length();
                if (len < 100f) continue;
                float s = (dx * ax.X + dz * ax.Y) / len, t = (dx * -ax.Y + dz * ax.X) / len;
                if (s >= -1f && s <= len + 1f && System.Math.Abs(t) <= (float)r.WidthM * 0.5f + 1f) { near = true; break; }
            }
            if (!near) continue;
            double h = _view.Streamer.GroundHeightAt(v.X, v.Z);
            double dy = v.Y - h;
            worstGround = System.Math.Max(worstGround, System.Math.Abs(dy));
            double drawnAbove = dy + lift.X; // R0 ring lift, the shader adds it (and MarkingLiftM as margin)
            lo = System.Math.Min(lo, drawnAbove); hi = System.Math.Max(hi, drawnAbove);
            n2++;
        }
        if (n2 == 0) { Fail("marking mesh " + ident, "no marking vertex lies on this airport's runways"); return ""; }
        if (worstGround > 0.01) Fail("marking mesh " + ident, $"marking mesh y departs {worstGround * 100:0.00} cm from ground_height_at (limit 1 cm)");
        if (lo < want[0] - 0.015 || hi > want[0] + 0.015) Fail("marking mesh " + ident, $"mesh y + R0 lift is {lo:0.000}..{hi:0.000} m above ground_height_at, expected {want[0]:0.000} +- 0.015 m");
        return $"; drawn runway lift (rule {_view.RunwayLiftRule}) {lift.X:0.###} / {lift.Y:0.###} / {lift.Z:0.###} / {lift.W:0.###} m, marking mesh ({n2} vertices) drawn "
            + $"{lo * 1000:0} .. {hi * 1000:0} mm above ground_height_at (+ {_view.Settings?.MarkingLiftM * 1000:0} mm margin)";
    }

    // ---- coast and lake -----------------------------------------------------------------------

    private string CheckCoast()
    {
        double worst = 0;
        int n = 0;
        for (int i = 0; i < 10; i++)
        {
            double lon = 20.0 + 0.09 * i;
            var p = _view.Streamer.GeodeticToLocal(55.85, lon, 0.0);
            double h = _view.Streamer.GroundHeightAt(p.X, p.Z);
            worst = System.Math.Max(worst, System.Math.Abs(h - _seaLevel));
            n++;
        }
        if (worst > 0.001) Fail("sea level", $"ground_height_at 1 mm tolerance: worst offshore departure {worst * 100:0.00} cm from terrain.sea_level_m {_seaLevel}");

        // Every surface vertex in the open sea of every held terrain tile is at sea level: between lon 19.8 (the water file's bbox ends at 19.5, west of
        // it the file says "land") and 20.95 (the coast at this latitude is at 21.06), within 20 km north or south of the camera's latitude.
        float xLim = _view.Streamer.GeodeticToLocal(55.85, 20.95, 0.0).X;
        float xMin = _view.Streamer.GeodeticToLocal(55.85, 19.8, 0.0).X;
        float z0 = _view.Streamer.GeodeticToLocal(55.85, 20.95, 0.0).Z;
        int checkedVerts = 0, bad = 0;
        double worstV = 0;
        string badKey = "";
        foreach (var node in _view.TerrainCover.All)
        {
            if (node is not TerrainNodes tn || !tn.HasMesh) continue;
            var arrays = tn.Mesh.SurfaceGetArrays(0);
            var verts = arrays[(int)Mesh.ArrayType.Vertex].AsVector3Array();
            var org = tn.Root.Position;
            foreach (var v in verts)
            {
                if (v.Y < tn.HMin - 0.001f) continue; // skirt copies hang below the surface
                float x = org.X + v.X, z = org.Z + v.Z;
                if (x >= xLim || x < xMin || System.Math.Abs(z - z0) > 20000f) continue;
                checkedVerts++;
                double d = System.Math.Abs(v.Y - _seaLevel);
                if (d > 0.01) { bad++; if (d > worstV) { worstV = d; badKey = tn.Key; } }
            }
        }
        if (checkedVerts < 50) Fail("sea vertices", $"only {checkedVerts} terrain surface vertices lie in the open sea at the coast spot");
        if (bad > 0) Fail("sea vertices", $"{bad} of {checkedVerts} offshore terrain vertices are off sea level, worst {worstV:0.000} m in tile {badKey}");
        return $"; sea level {n} points worst {worst * 1000:0.0} mm, {checkedVerts} offshore terrain vertices {bad} off";
    }

    private string CheckLake(SpotPose pose)
    {
        double lo = double.MaxValue, hi = double.MinValue;
        int n = 0;
        for (int i = -2; i <= 2; i++)
            for (int j = -2; j <= 2; j++)
            {
                var p = _view.Streamer.GeodeticToLocal(pose.Lat + j * 0.0027, pose.Lon + i * 0.0036, 0.0); // 300 m x 230 m steps: the lake is clear of the shore for 1.1 km
                double h = _view.Streamer.GroundHeightAt(p.X, p.Z);
                lo = System.Math.Min(lo, h);
                hi = System.Math.Max(hi, h);
                n++;
            }
        if (hi - lo > 0.01) Fail("lake flat", $"{n} points over Kauno marios span {(hi - lo) * 100:0.00} cm ({lo:0.000} .. {hi:0.000} m)");
        if (System.Math.Abs(lo - _seaLevel) < 0.5) Fail("lake level", $"the lake lies at sea level ({lo:0.00} m): the lake rule did not run");
        return $"; lake level {lo:0.000} m, spread {(hi - lo) * 1000:0.0} mm over {n} points";
    }

    // ---- determinism hash ---------------------------------------------------------------------

    private static ulong Mix(ulong h, ulong v)
    {
        h ^= v;
        h *= 0x100000001b3UL;
        return h;
    }

    private static ulong MixStr(ulong h, string s)
    {
        foreach (char c in s) h = Mix(h, c);
        return Mix(h, 0xff);
    }

    private static ulong Q(float f) => unchecked((ulong)(long)System.Math.Round(f * 1000.0)); // millimetres

    private static ulong HashMesh(ulong h, ArrayMesh mesh)
    {
        if (mesh.GetSurfaceCount() == 0) return Mix(h, 0);
        var arrays = mesh.SurfaceGetArrays(0);
        var v = arrays[(int)Mesh.ArrayType.Vertex].AsVector3Array();
        h = Mix(h, (ulong)v.Length);
        foreach (var p in v) { h = Mix(h, Q(p.X)); h = Mix(h, Q(p.Y)); h = Mix(h, Q(p.Z)); }
        var idx = arrays[(int)Mesh.ArrayType.Index];
        if (idx.VariantType == Variant.Type.PackedInt32Array) foreach (int i in idx.AsInt32Array()) h = Mix(h, (ulong)i);
        var col = arrays[(int)Mesh.ArrayType.Color];
        if (col.VariantType == Variant.Type.PackedColorArray) foreach (var c in col.AsColorArray()) h = Mix(h, (ulong)c.ToRgba32());
        return h;
    }

    /// <summary>Hashes the first <paramref name="count"/> lights of a tile's points mesh: position, colour, kind and index within the kind (the padding vertices are left out).</summary>
    private static ulong HashSprites(ulong h, ArrayMesh mesh, int count)
    {
        h = Mix(h, (ulong)count);
        if (count == 0 || mesh.GetSurfaceCount() == 0) return h;
        var arrays = mesh.SurfaceGetArrays(0);
        var pos = arrays[(int)Mesh.ArrayType.Vertex].AsVector3Array();
        var col = arrays[(int)Mesh.ArrayType.Color].AsColorArray();
        var custom = arrays[(int)Mesh.ArrayType.Custom0].AsByteArray();
        for (int i = 0; i < count && i < pos.Length; i++)
        {
            h = Mix(h, Q(pos[i].X)); h = Mix(h, Q(pos[i].Y)); h = Mix(h, Q(pos[i].Z));
            h = Mix(h, (ulong)col[i].ToRgba32());
            for (int k = 0; k < 4; k++) h = Mix(h, custom[i * 4 + k]);
        }
        return h;
    }

    /// <summary>FNV-1a over every held tile of both layers in key order: key, ring, quantized meshes, lights. Frame-dependent values (tile origins) are left out.</summary>
    private ulong HashTiles()
    {
        ulong h = 0xcbf29ce484222325UL;
        var list = new List<TileNodes>(_view.Cover.Loaded);
        foreach (var n in _view.Cover.All) list.Add(n);
        list.Sort((a, b) => a.Packed.CompareTo(b.Packed));
        foreach (var n in list)
        {
            h = MixStr(h, n.Key);
            h = Mix(h, (ulong)n.Ring);
            h = HashMesh(h, n.Mesh);
            h = HashMesh(h, n.GroundMesh);
            h = HashSprites(h, n.PointsMesh, n.SpriteCount);
        }
        var tlist = new List<TileNodes>(_view.TerrainCover.Loaded);
        foreach (var n in _view.TerrainCover.All) tlist.Add(n);
        tlist.Sort((a, b) => a.Packed.CompareTo(b.Packed));
        foreach (var n in tlist)
        {
            h = MixStr(h, n.Key);
            h = Mix(h, (ulong)n.Ring);
            h = HashMesh(h, n.Mesh);
        }
        return h;
    }
}
