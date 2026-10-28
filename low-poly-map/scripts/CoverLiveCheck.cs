using System.Collections.Generic;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Live half of <c>--cover-selftest</c> (api 1.11, landmark layer): once the running world has settled, the host's landmark tile map must equal the
/// landmark entries in <c>debug_tiles</c> (state ready) and <c>landmarks_loaded</c>, every held landmark tile must sit at its rect centre with a mesh and be
/// plainly visible, the pool must hold exactly the vector plus the landmark nodes, and no <c>l/</c> key may ever have appeared in <c>poll_fades</c>. On the synthetic
/// city the camera then jumps 45 km (origin shift, the old landmark tiles must leave through <c>poll_unload</c>) and the check repeats. Without the layer
/// (<c>--no-landmarks</c>, a compatibility archive, a seed world) it asserts that no landmark key reached the host at all. Prints
/// <c>COVER LIVE CHECK PASS/FAIL: ...</c> and quits with exit code 0 / 1 (combined with the simulation result of <see cref="CoverSelfTest"/>).
/// </summary>
public partial class CoverLiveCheck : Node
{
    public static void Attach(WorldView view, Node? parent, bool simulationOk) =>
        (parent ?? view).CallDeferred(Node.MethodName.AddChild, new CoverLiveCheck { Name = "CoverLiveCheck", _view = view, _simOk = simulationOk });

    private WorldView _view = null!;
    private bool _simOk;
    private readonly List<string> _fails = new();
    private readonly List<DebugTile> _tiles = new(512);
    private readonly StreamerStats _stats = new();
    private string _summary = "";

    public override void _Ready()
    {
        ProcessPriority = 1;
        if (Engine.MaxFps == 0) Engine.MaxFps = 60;
        Run();
    }

    private async void Run()
    {
        try { await Body(); }
        catch (System.Exception e) { _fails.Add("exception: " + e); }
        if (!_simOk) _fails.Insert(0, "tile map simulation failed (see CoverSelfTest lines)");
        GD.Print(_fails.Count == 0 ? $"COVER LIVE CHECK PASS: {_summary}" : "COVER LIVE CHECK FAIL: " + string.Join("; ", _fails) + (_summary.Length > 0 ? " | " + _summary : ""));
        GetTree().Quit(_fails.Count == 0 ? 0 : 1);
    }

    private async Task<bool> WaitFor(System.Func<bool> cond, double timeoutS)
    {
        ulong end = Time.GetTicksMsec() + (ulong)(timeoutS * 1000);
        while (!cond())
        {
            if (Time.GetTicksMsec() > end) return false;
            await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame);
        }
        return true;
    }

    private bool Busy()
    {
        _view.Streamer.DebugStats(_stats);
        int b = _stats.LandmarksPending;
        for (int i = 0; i < 4; i++) b += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
        for (int i = 0; i < _stats.TerrainRings; i++) b += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i];
        return b != 0 || _view.ReconfigurePending;
    }

    private async Task<bool> Settle(double timeoutS)
    {
        ulong calm = 0;
        return await WaitFor(() =>
        {
            if (Busy()) { calm = 0; return false; }
            if (calm == 0) calm = Time.GetTicksMsec();
            return Time.GetTicksMsec() - calm >= 2000;
        }, timeoutS);
    }

    /// <summary>Host landmark tile map against debug_tiles and landmarks_loaded; returns the held count.</summary>
    private int Compare(string when)
    {
        var view = _view;
        view.Streamer.DebugTiles(_tiles);
        view.Streamer.DebugStats(_stats);
        var libReady = new HashSet<long>();
        int libEmpty = 0, libOther = 0;
        foreach (var t in _tiles)
        {
            if (t.Layer != TileLayer.Landmarks) continue;
            if (t.State == TileState.Ready) libReady.Add(t.Packed);
            else if (t.State == TileState.Empty) libEmpty++;
            else libOther++;
        }
        int missing = 0, extra = 0, noMesh = 0, hidden = 0, misplaced = 0;
        foreach (long k in libReady) if (!view.LandmarkCover.TryGet(k, out _)) missing++;
        var byKey = new Dictionary<long, DebugTile>();
        foreach (var t in _tiles) if (t.Layer == TileLayer.Landmarks) byKey[t.Packed] = t;
        foreach (var n in view.LandmarkCover.All)
        {
            if (!libReady.Contains(n.Packed)) extra++;
            if (n.Tris == 0 || !n.HasMesh) noMesh++;
            if (!n.Root.Visible || n.Fading) hidden++;
            if (byKey.TryGetValue(n.Packed, out var dt))
            {
                var c = (dt.Min + dt.Max) * 0.5f;
                // The root is the tile centre of the Mercator projection at the tile's latitude, the debug rect is the projected corners: they differ by
                // about 0.1 % of the tile width on a z11 tile (the same Rust-side projection detail the geodetic test tolerates for terrain tiles).
                if (new Vector2(n.Root.Position.X - c.X, n.Root.Position.Z - c.Y).Length() > System.Math.Max(2.0, 0.0015 * (dt.Max.X - dt.Min.X))) misplaced++;
            }
        }
        int host = view.LandmarkCover.Loaded;
        GD.Print($"[CoverLiveCheck] {when}: library ready landmark tiles {libReady.Count} (empty {libEmpty}, other {libOther}), debug_stats landmarks_loaded {_stats.LandmarksLoaded}, host {host}, library-only {missing}, host-only {extra}, no mesh {noMesh}, hidden {hidden}, misplaced {misplaced}");
        if (missing != 0 || extra != 0) _fails.Add($"{when}: landmark sets differ (library-only {missing}, host-only {extra})");
        if (_stats.HasLandmarkStats && _stats.LandmarksLoaded != host) _fails.Add($"{when}: landmarks_loaded {_stats.LandmarksLoaded} != host landmark tiles {host}");
        if (noMesh != 0) _fails.Add($"{when}: {noMesh} landmark tiles without a mesh");
        if (hidden != 0) _fails.Add($"{when}: {hidden} landmark tiles not visible");
        if (misplaced != 0) _fails.Add($"{when}: {misplaced} landmark tiles off their rect centre");
        if (libOther != 0) _fails.Add($"{when}: {libOther} landmark tiles still queued / fetching / meshing after the settle");
        int inUse = view.PoolCreated - view.PoolFree;
        if (inUse != view.Cover.Loaded + host) _fails.Add($"{when}: pool in use {inUse} != vector {view.Cover.Loaded} + landmark {host}");
        int errs = view.ConsistencyErrors;
        view.VerifyCover();
        if (view.ConsistencyErrors != errs) _fails.Add($"{when}: VerifyCover found {view.ConsistencyErrors - errs} problem(s)");
        return host;
    }

    private async Task Body()
    {
        var view = _view;
        if (!view.WorldRunning) { _fails.Add("no running world: " + view.ConfigureError); return; }
        if (!view.LandmarksActive)
        {
            // Layer not requested (--no-landmarks) or not available: nothing may arrive.
            await WaitFor(() => view.Cover.Loaded > 5, 60);
            await Settle(60);
            if (view.LandmarkKeysSeen != 0 || view.LandmarkCover.Loaded != 0) _fails.Add($"landmark keys reached a host that did not opt in ({view.LandmarkKeysSeen} keys, {view.LandmarkCover.Loaded} tiles)");
            _summary = $"landmark layer not requested (enabled {view.LandmarksEnabled}, library {view.Streamer.ApiVersionText}): 0 l/ keys, {view.Cover.Loaded} vector tiles";
            return;
        }
        bool expect = true;
        if (!await WaitFor(() => view.LandmarkReadyTotal > 0, 60))
        {
            expect = false;
            GD.Print("[CoverLiveCheck] no landmark tile within 60 s (compatibility archive, a seed world or an archive without landmarks): checking the empty case");
        }
        if (!await Settle(180)) { _fails.Add("queues not idle 180 s after the start"); return; }
        int held = Compare("settled");
        if (expect && held == 0) _fails.Add("settled with no landmark tile although some arrived");
        int ready0 = view.LandmarkReadyTotal;
        string jump = "";
        if (expect && view.TileSource == "synthetic city" && view.Camera is FreeFlyCamera cam)
        {
            int unloaded0 = view.LandmarkUnloadedTotal;
            cam.GlobalPosition += new Vector3(45000f, 0f, 0f);
            await WaitFor(() => view.LandmarkUnloadedTotal > unloaded0, 60);
            if (!await Settle(180)) { _fails.Add("queues not idle 180 s after the 45 km jump"); return; }
            int after = Compare("after a 45 km jump");
            if (view.LandmarkUnloadedTotal <= unloaded0) _fails.Add("the old landmark tiles were not unloaded after a 45 km jump");
            jump = $", after a 45 km jump {view.LandmarkUnloadedTotal - unloaded0} unloaded / {view.LandmarkReadyTotal - ready0} new, {after} held";
        }
        if (view.LandmarkFadeKeysSeen != 0) _fails.Add($"{view.LandmarkFadeKeysSeen} l/ keys appeared in poll_fades");
        _summary = $"landmark map equals debug_tiles ({held} tiles held{jump}), {view.LandmarkReadyTotal} delivered / {view.LandmarkUnloadedTotal} unloaded, "
            + $"0 l/ keys in poll_fades ({view.FadesSupported}), cover verify errors {view.ConsistencyErrors}";
        if (view.ConsistencyErrors > 0) _fails.Add($"{view.ConsistencyErrors} cover consistency error(s)");
    }
}
