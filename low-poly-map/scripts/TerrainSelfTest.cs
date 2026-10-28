using System.Collections.Generic;
using System.Globalization;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--terrain-selftest</c> (implies <c>--terrain</c>; run it with <c>--tile-url=</c> against the synthetic city, headless works):
/// the host side acceptance of the terrain layer (api 1.5, docs/spec-terrain.md section 11 bench tests).
/// <list type="number">
/// <item>Pass 1, layer on: flies a scripted path over the generated hills (level at 300 m AGL, climb to 3000 m, hold, descend; 250 m/s, so
/// origin shifts happen) and checks every frame that no terrain tile is visible together with a visible ancestor (never a parent and its
/// four children at once; fading tiles excepted), that every held terrain tile has a mesh, and every 20 frames the cover consistency
/// (<see cref="WorldView.VerifyCover"/>: visibility rule per layer with the layer's root zoom, double cover, settled holes). Every 30 frames
/// the host's terrain tile map must be a subset of what the library reports in <c>debug_tiles</c>; after the flight and a settle the two
/// sets are equal and <c>terrain_loaded</c> equals the host's count.</item>
/// <item>Detail: <c>set_terrain_detail(0.1)</c> must come back clamped to 0.25, every held terrain key is re-delivered once and replaced in place
/// (no tile unloaded, no tile without a mesh, no double cover at any frame), the triangle count rises (finer); 1.0 restores the baseline exactly.</item>
/// <item>Pass 2, layer off: reconfigure with the profile's <c>terrain.enabled</c> true but the host not opting in (no <c>layers</c> option): the
/// library must stay flat: no <c>t/</c> key in poll_ready / poll_fades / poll_unload, no terrain entry in debug_tiles or debug_stats, no terrain node,
/// <c>ground_height_at</c> 0, while the vector world streams.</item>
/// </list>
/// Prints <c>TERRAIN SELFTEST PASS/FAIL: ...</c>, exit code 0 / 1. Options: <c>--terrain-selftest-speed=&lt;m/s&gt;</c> (default 250),
/// <c>--terrain-selftest-timeout=&lt;s&gt;</c> (real time failsafe, default 420).
/// </summary>
public partial class TerrainSelfTest : Node
{
    private const string Arg = "--terrain-selftest";

    public static bool Requested() => System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), Arg);

    public static void Attach(WorldView view, Node? parent) => (parent ?? view).CallDeferred(Node.MethodName.AddChild, new TerrainSelfTest { Name = "TerrainSelfTest", _view = view }); // the scene root is still setting up its children

    private WorldView _view = null!;
    private FreeFlyCamera? _cam;
    private readonly List<string> _fails = new();
    private readonly List<DebugTile> _tiles = new(512);
    private readonly StreamerStats _stats = new();

    // Flight.
    private double _speed = 250.0;
    private bool _flying;
    private double _flightT;
    private float _agl;
    private bool _flightDone;
    private readonly Vector3 _dir = new Vector3(0.6f, 0f, -0.8f).Normalized();
    private double _timeout = 420.0;
    private ulong _startMs;
    private bool _abort;

    // Per-frame checks.
    private bool _checking;
    private long _frames;
    private int _doubleCover;
    private int _meshless;
    private int _subsetErrors;
    private int _maxHeld;
    private int _maxFading;
    private int _lostDuringDetail;
    private int _detailHeldExpected = -1;
    private int _checkErrors0;
    private int _shifts0;

    public override void _Ready()
    {
        ProcessPriority = 1; // after WorldView (0): its uploads, fades and unloads of this frame are done
        foreach (var a in OS.GetCmdlineUserArgs())
        {
            if (a.StartsWith("--terrain-selftest-speed=", System.StringComparison.Ordinal) && double.TryParse(a.Substring(25), NumberStyles.Float, CultureInfo.InvariantCulture, out var sp)) _speed = System.Math.Max(sp, 1.0);
            else if (a.StartsWith("--terrain-selftest-timeout=", System.StringComparison.Ordinal) && double.TryParse(a.Substring(27), NumberStyles.Float, CultureInfo.InvariantCulture, out var to)) _timeout = to;
        }
        if (Engine.MaxFps == 0) Engine.MaxFps = 60; // leave the workers real time to deliver tiles
        _cam = _view.Camera as FreeFlyCamera;
        _startMs = Time.GetTicksMsec();
        RunAll();
    }

    private async void RunAll()
    {
        try { await Body(); }
        catch (System.Exception e) { _fails.Add("exception: " + e); }
        _view.ExternalCameraUpdate = null;
        GD.Print(_fails.Count == 0
            ? $"TERRAIN SELFTEST PASS: {_summary}"
            : "TERRAIN SELFTEST FAIL: " + string.Join("; ", _fails) + (_summary.Length > 0 ? " | " + _summary : ""));
        GetTree().Quit(_fails.Count == 0 ? 0 : 1);
    }

    private string _summary = "";

    private async Task<bool> WaitFor(System.Func<bool> cond, double timeoutS)
    {
        ulong end = Time.GetTicksMsec() + (ulong)(timeoutS * 1000);
        while (!cond())
        {
            if (Time.GetTicksMsec() > end || _abort) return false;
            if ((Time.GetTicksMsec() - _startMs) / 1000.0 > _timeout) { _abort = true; _fails.Add($"real-time timeout {_timeout} s"); return false; }
            await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame);
        }
        return true;
    }

    private int TerrainLibLoaded()
    {
        int n = 0;
        for (int i = 0; i < _stats.TerrainRings; i++) n += _stats.TerrainLoaded[i];
        return n;
    }

    private bool Busy()
    {
        _view.Streamer.DebugStats(_stats);
        int b = 0;
        for (int i = 0; i < 4; i++) b += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
        for (int i = 0; i < _stats.TerrainRings; i++) b += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i];
        return b != 0 || _view.ReconfigurePending;
    }

    private async Task<bool> WaitIdle(double timeoutS)
    {
        ulong calm = 0;
        return await WaitFor(() =>
        {
            if (Busy()) { calm = 0; return false; }
            if (calm == 0) calm = Time.GetTicksMsec();
            return Time.GetTicksMsec() - calm >= 1500;
        }, timeoutS);
    }

    private async Task Body()
    {
        var view = _view;
        if (_cam == null || !view.WorldRunning || !view.Streamer.HasTerrainLayer) { _fails.Add("no running world with a terrain-capable library: " + view.ConfigureError); return; }
        if (!view.TerrainActive) { _fails.Add("terrain layer not active (EnableTerrain / terrain.enabled): launch with --terrain-selftest, not --terrain=0"); return; }

        // ---- pass 1: layer on, fly ----
        if (!await WaitFor(() => view.TerrainReadyTotal > 0, 90)) { _fails.Add("no terrain tile arrived within 90 s"); return; }
        GD.Print($"[TerrainSelfTest] pass 1: layer on (zoom {view.TerrainCover.RootZ}..{view.TerrainCover.LeafZ}, reach {view.TerrainReachM / 1000f:0.#} km), flying at {_speed} m/s");
        view.ExternalCameraUpdate = Step;
        _cam.SetProcess(false);
        _cam.SetProcessUnhandledInput(false);
        _cam.Rotation = new Vector3(-0.1f, -Mathf.Asin(_dir.X), 0f);
        _checking = true;
        _flying = true;
        _shifts0 = view.OriginShiftCount;
        int uploadsAtStart = view.TerrainReadyTotal;
        if (!await WaitFor(() => _flightDone, 240)) { _fails.Add("flight did not finish"); _flying = false; }
        _flying = false;
        int shifts = view.OriginShiftCount - _shifts0;
        int uploadsFlight = view.TerrainReadyTotal - uploadsAtStart;
        int unloadsFlight = view.TerrainUnloadedTotal;
        view.ExternalCameraUpdate = (_) => view.Streamer.UpdateCamera(_cam.GlobalPosition, Vector3.Zero, _cam.AltitudeAgl); // hover
        GD.Print($"[TerrainSelfTest] flight done: {_frames} frames, origin shifts {shifts}, terrain uploads {uploadsFlight}, unloads {unloadsFlight}, peak held {_maxHeld}, peak fading {_maxFading}, double-cover frames {_doubleCover}");
        if (shifts < 1) _fails.Add("no origin shift during the flight (path too short)");
        if (_doubleCover > 0) _fails.Add($"{_doubleCover} frames had a terrain tile visible together with a visible ancestor");
        if (_meshless > 0) _fails.Add($"{_meshless} tile-frames with a held terrain tile without a mesh");

        if (!await WaitIdle(180)) { _fails.Add("terrain / vector queues not idle 180 s after the flight"); return; }
        await Delay(0.5);
        CompareSets("settled", exact: true);
        view.Streamer.DebugStats(_stats);
        if (TerrainLibLoaded() != view.TerrainCover.Loaded) _fails.Add($"terrain_loaded {TerrainLibLoaded()} != host terrain tiles {view.TerrainCover.Loaded}");
        if (view.TerrainPoolCreated - view.TerrainPoolFree != view.TerrainCover.Loaded) _fails.Add($"terrain pool in use {view.TerrainPoolCreated - view.TerrainPoolFree} != held {view.TerrainCover.Loaded}");
        if (view.TerrainCover.Loaded < 100) _fails.Add($"only {view.TerrainCover.Loaded} terrain tiles held after the flight");
        if (_subsetErrors > 0) _fails.Add($"{_subsetErrors} frames where the host held a terrain tile the library did not report");
        int held = view.TerrainCover.Loaded;
        long tris0 = view.TerrainCover.TrianglesLoaded;
        GD.Print($"[TerrainSelfTest] settled: host holds {held} terrain tiles (by ring {string.Join("/", view.TerrainCover.LoadedByRing)}), {tris0:N0} triangles, library terrain_loaded {TerrainLibLoaded()}, visible {string.Join("/", view.TerrainCover.VisibleByRing)}");

        // ---- detail: set_terrain_detail re-delivers every held key, replacing meshes in place ----
        int replaced0 = view.TerrainReplacedTotal, unloaded0 = view.TerrainUnloadedTotal;
        _detailHeldExpected = held;
        double eff = view.SetTerrainDetail(0.1);
        if (double.IsNaN(eff) || System.Math.Abs(eff - 0.25) > 1e-6) _fails.Add($"set_terrain_detail(0.1) returned {eff}, expected the clamp 0.25");
        bool all = await WaitFor(() => view.TerrainReplacedTotal - replaced0 >= held, 240);
        await Delay(1.0);
        int replacedFine = view.TerrainReplacedTotal - replaced0;
        long trisFine = view.TerrainCover.TrianglesLoaded;
        GD.Print($"[TerrainSelfTest] detail 0.25: {replacedFine} re-deliveries for {held} held tiles, triangles {tris0:N0} -> {trisFine:N0}, unloaded meanwhile {view.TerrainUnloadedTotal - unloaded0}");
        if (!all) _fails.Add($"detail 0.25: only {replacedFine} of {held} held tiles were re-delivered within 240 s");
        if (replacedFine > held + held / 10) _fails.Add($"detail 0.25: {replacedFine} re-deliveries for {held} tiles (each held key must come once)");
        if (view.TerrainUnloadedTotal != unloaded0 || view.TerrainCover.Loaded != held) _fails.Add($"detail 0.25 changed the held set ({held} -> {view.TerrainCover.Loaded}, unloads {view.TerrainUnloadedTotal - unloaded0})");
        if (trisFine <= tris0) _fails.Add($"finer detail did not add triangles ({tris0} -> {trisFine})");
        int replaced1 = view.TerrainReplacedTotal;
        eff = view.SetTerrainDetail(1.0);
        if (System.Math.Abs(eff - 1.0) > 1e-6) _fails.Add($"set_terrain_detail(1.0) returned {eff}");
        await WaitFor(() => view.TerrainReplacedTotal - replaced1 >= held, 240);
        await Delay(1.0);
        long trisBack = view.TerrainCover.TrianglesLoaded;
        GD.Print($"[TerrainSelfTest] detail 1.0: triangles {trisBack:N0} (baseline {tris0:N0}), re-deliveries {view.TerrainReplacedTotal - replaced1}");
        if (trisBack != tris0) _fails.Add($"detail 1.0 did not restore the baseline mesh ({trisBack} vs {tris0} triangles)");
        _detailHeldExpected = -1;
        if (_lostDuringDetail > 0) _fails.Add($"{_lostDuringDetail} frames during the detail change where the held terrain set shrank");
        int errorsPass1 = view.ConsistencyErrors;
        _checking = false;

        // ---- pass 2: the profile enables terrain, the host does not opt in ----
        GD.Print("[TerrainSelfTest] pass 2: profile terrain.enabled = true but the host passes no layers option");
        int seen0 = view.TerrainKeysSeen;
        bool done = false, ok = false;
        string err = "";
        view.Reconfigured += (o, e) => { done = true; ok = o; err = e; };
        view.ApplyProfileOverrides(new Godot.Collections.Dictionary { ["terrain"] = new Godot.Collections.Dictionary { ["enabled"] = true } }, false);
        if (!await WaitFor(() => done, 30) || !ok) { _fails.Add("pass 2 reconfigure failed: " + err); return; }
        seen0 = view.TerrainKeysSeen; // the teleport's unload batch (pass 1's 300+ terrain keys) was consumed by the reconfigure itself
        if (view.TerrainActive) _fails.Add("terrain layer active without the layers opt-in");
        if (view.TerrainCover.Loaded != 0) _fails.Add($"{view.TerrainCover.Loaded} terrain tiles after the reconfigure without the opt-in");
        // The world restarted at the camera position: fly again (vector world only) for a while.
        _flightDone = false;
        _flightT = 0;
        _flying = true;
        _checking = true;
        view.ExternalCameraUpdate = Step;
        await WaitFor(() => view.Cover.Loaded > 20, 90);
        await Delay(8.0);
        _flying = false;
        view.ExternalCameraUpdate = (_) => view.Streamer.UpdateCamera(_cam.GlobalPosition, Vector3.Zero, _cam.AltitudeAgl);
        view.Streamer.DebugStats(_stats);
        view.Streamer.DebugTiles(_tiles);
        int terrainEntries = 0;
        foreach (var t in _tiles) if (t.Terrain) terrainEntries++;
        GD.Print($"[TerrainSelfTest] pass 2: vector tiles {view.Cover.Loaded}, terrain keys seen {view.TerrainKeysSeen - seen0}, terrain tiles {view.TerrainCover.Loaded}, debug_tiles terrain entries {terrainEntries}, debug_stats terrain keys {_stats.HasTerrain}, ground_height_at {view.CameraGroundHeight}");
        if (view.TerrainKeysSeen != seen0) _fails.Add($"{view.TerrainKeysSeen - seen0} terrain keys reached a host that did not opt in");
        if (view.TerrainCover.Loaded != 0 || view.TerrainPoolCreated - view.TerrainPoolFree != 0) _fails.Add("terrain nodes exist without the opt-in");
        if (terrainEntries != 0) _fails.Add($"{terrainEntries} terrain entries in debug_tiles without the opt-in");
        if (_stats.HasTerrain) _fails.Add("debug_stats carries terrain_* keys without the opt-in");
        if (view.CameraGroundHeight != 0.0) _fails.Add($"ground_height_at {view.CameraGroundHeight} without the opt-in (must stay 0)");
        if (view.Cover.Loaded < 20) _fails.Add($"vector world did not stream in pass 2 ({view.Cover.Loaded} tiles)");
        _checking = false;

        int errorsTotal = view.ConsistencyErrors;
        if (errorsTotal > 0) _fails.Add($"{errorsTotal} cover consistency error(s) from VerifyCover (visibility rule per layer / double cover / holes)");
        _summary = $"pass 1 flew {_speed} m/s ({shifts} origin shifts, {uploadsFlight} terrain uploads, peak {_maxHeld} tiles held, {_frames} frames checked, 0 double-cover frames), "
            + $"settled set equals the library's ({held} tiles, {tris0:N0} tris), detail 0.25 re-delivered {replacedFine}/{held} in place ({tris0:N0} -> {trisFine:N0} tris) and 1.0 restored the baseline, "
            + $"pass 2 without the opt-in: 0 terrain keys / nodes / entries, ground_height_at 0, {view.Cover.Loaded} vector tiles; cover verify errors pass 1 {errorsPass1}, total {errorsTotal}";
        _checkErrors0 = errorsPass1;
    }

    private async Task Delay(double s) => await ToSignal(GetTree().CreateTimer(s), SceneTreeTimer.SignalName.Timeout);

    // ---- flight ----------------------------------------------------------------------------------

    // Altitude above ground over time: level, climb to 3000 m, hold, descend again.
    private const double LevelS = 18.0, ClimbS = 18.0, HoldS = 8.0, DescendS = 18.0;
    private static float AglAt(double t)
    {
        if (t < LevelS) return 300f;
        t -= LevelS;
        if (t < ClimbS) return 300f + 2700f * (float)(t / ClimbS);
        t -= ClimbS;
        if (t < HoldS) return 3000f;
        t -= HoldS;
        if (t < DescendS) return 3000f - 2700f * (float)(t / DescendS);
        return 300f;
    }

    /// <summary>Replaces update_camera: moves the camera along the path in the current local frame, then sends the pose (before take_origin_shift).</summary>
    private void Step(double delta)
    {
        var cam = _cam!;
        double dt = System.Math.Min(delta, 0.05);
        if (_flying)
        {
            double t0 = _flightT;
            _flightT += dt;
            float agl0 = AglAt(t0), agl1 = AglAt(_flightT);
            var horiz = _dir * (float)(_speed * dt);
            double ground = _view.CameraGroundHeight;
            cam.GlobalPosition = new Vector3(cam.GlobalPosition.X + horiz.X, (float)(ground + agl1), cam.GlobalPosition.Z + horiz.Z);
            _agl = agl1;
            var vel = _dir * (float)_speed + Vector3.Up * (float)((agl1 - agl0) / System.Math.Max(dt, 1e-4));
            cam.SetGroundHeight(ground);
            _view.Streamer.UpdateCamera(cam.GlobalPosition, vel, agl1);
            if (_flightT >= LevelS + ClimbS + HoldS + DescendS) _flightDone = true;
        }
        else _view.Streamer.UpdateCamera(cam.GlobalPosition, Vector3.Zero, cam.AltitudeAgl);
    }

    // ---- per-frame checks --------------------------------------------------------------------------

    public override void _Process(double delta)
    {
        if (!_checking) return;
        _frames++;
        var cover = _view.TerrainCover;
        if (cover.Loaded > _maxHeld) _maxHeld = cover.Loaded;
        if (cover.FadingTiles > _maxFading) _maxFading = cover.FadingTiles;
        if (_detailHeldExpected >= 0 && cover.Loaded < _detailHeldExpected) _lostDuringDetail++;

        // Never a tile and one of its ancestors (the parent in the sibling swap) visible at the same time unless one of them is fading.
        bool bad = false;
        int meshless = 0;
        foreach (var t in cover.All)
        {
            if (t.Tris == 0) meshless++;
            if (t.Vis != TileCover.Vis.Visible || t.Fading) continue;
            for (int z = TileKey.Z(t.Packed) - 1; z >= cover.RootZ; z--)
            {
                if (cover.TryGet(TileKey.Ancestor(t.Packed, z), out var a) && a.Vis == TileCover.Vis.Visible && !a.Fading) { bad = true; break; }
            }
            if (bad) break;
        }
        if (bad) _doubleCover++;
        _meshless += meshless;

        if (_frames % 20 == 0) _view.VerifyCover();
        if (_frames % 30 == 0) CompareSets("flight", exact: false);
    }

    /// <summary>Host terrain tiles against debug_tiles: every held tile is known to the library; with <paramref name="exact"/> the "ready" sets are equal.</summary>
    private void CompareSets(string when, bool exact)
    {
        var view = _view;
        view.Streamer.DebugTiles(_tiles);
        var lib = new HashSet<long>();
        var libReady = new HashSet<long>();
        foreach (var t in _tiles)
        {
            if (!t.Terrain || t.Packed == 0) continue;
            lib.Add(t.Packed);
            if (t.State == TileState.Ready) libReady.Add(t.Packed);
        }
        int notInLib = 0;
        foreach (var n in view.TerrainCover.All) if (!lib.Contains(n.Packed)) notInLib++;
        if (notInLib > 0) _subsetErrors++;
        if (exact)
        {
            int missing = 0;
            foreach (long k in libReady) if (!view.TerrainCover.TryGet(k, out _)) missing++;
            int extra = 0;
            foreach (var n in view.TerrainCover.All) if (!libReady.Contains(n.Packed)) extra++;
            GD.Print($"[TerrainSelfTest] {when}: library 'ready' terrain tiles {libReady.Count}, host tiles {view.TerrainCover.Loaded}, library-only {missing}, host-only {extra}");
            if (missing != 0 || extra != 0) _fails.Add($"{when}: terrain tile sets differ (library-only {missing}, host-only {extra})");
        }
    }
}
