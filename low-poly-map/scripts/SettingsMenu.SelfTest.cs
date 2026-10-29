using System.Collections.Generic;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

// --settings-selftest: drives the widgets and the apply path without any input (see RunSelfTest).
public partial class SettingsMenu
{
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

    private async Task Delay(double s) => await ToSignal(GetTree().CreateTimer(s), SceneTreeTimer.SignalName.Timeout);

    private string Snapshot()
    {
        var b = Bridge!;
        b.Streamer.DebugStats(_stats);
        string Ring(int[] a) => $"{a[0]}/{a[1]}/{a[2]}/{a[3]}";
        var busy = new int[4];
        for (int i = 0; i < 4; i++) busy[i] = _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
        string terrain = "";
        if (b.TerrainActive)
        {
            int tb = 0;
            for (int i = 0; i < _stats.TerrainRings; i++) tb += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i];
            terrain = $", terrain busy {tb}, library holds {TerrainLoadedTotal()}, host cover {b.TerrainCover.Loaded} (pool in use {b.TerrainPoolCreated - b.TerrainPoolFree})";
        }
        return $"queued+fetching+meshing R0..R3 {Ring(busy)}, loaded {Ring(_stats.Loaded)}, cover {b.Cover.Loaded}{(b.LandmarksActive ? $" + landmarks {b.LandmarkCover.Loaded}" : "")}, tile roots {b.TileRootCount} (in use {b.PoolCreated - b.PoolFree}){terrain}";
    }

    private int LoadedTotal() { int n = 0; foreach (var v in _stats.Loaded) n += v; return n; }
    private int TerrainLoadedTotal() { int n = 0; for (int i = 0; i < _stats.TerrainRings; i++) n += _stats.TerrainLoaded[i]; return n; }

    /// <summary>Idle = nothing queued, fetching or meshing for 1.5 s in a row and no reconfigure pending (finished tiles can still wait for their poll_ready).</summary>
    private async Task<bool> WaitIdle(double timeoutS)
    {
        ulong calmSince = 0;
        return await WaitFor(() =>
        {
            Bridge!.Streamer.DebugStats(_stats);
            int busy = 0;
            for (int i = 0; i < 4; i++) busy += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
            for (int i = 0; i < _stats.TerrainRings; i++) busy += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i];
            if (busy != 0 || Bridge.ReconfigurePending || ApplyInFlight) { calmSince = 0; return false; }
            if (calmSince == 0) calmSince = Time.GetTicksMsec();
            return Time.GetTicksMsec() - calmSince >= 1500;
        }, timeoutS);
    }

    private const int NodeGrowthSlack = 8;

    /// <summary>Number of alternating A/B applies of <c>--settings-selftest</c>: 11 by default, <c>--settings-selftest-applies=&lt;n&gt;</c> (4..500) for a leak run.</summary>
    private static int SelfTestApplies()
    {
        foreach (var a in OS.GetCmdlineUserArgs())
            if (a.StartsWith("--settings-selftest-applies=", System.StringComparison.Ordinal) && int.TryParse(a.Substring(28), out var n))
                return System.Math.Clamp(n, 4, 500);
        return 11;
    }

    private async void RunSelfTest()
    {
        var fails = new List<string>();
        try { await SelfTestBody(fails); }
        catch (System.Exception e) { fails.Add("exception: " + e.Message); }
        GD.Print(fails.Count == 0
            ? $"SETTINGS SELFTEST PASS: {1 + SelfTestApplies()} reconfigures (1 via widgets, {SelfTestApplies()} alternating), one rejected profile, no pool growth, origin kept, tiles released and reloaded each time, no scene node growth outside the pools"
            : "SETTINGS SELFTEST FAIL: " + string.Join("; ", fails));
        GetTree().Quit(fails.Count == 0 ? 0 : 1);
    }

    private async Task SelfTestBody(List<string> fails)
    {
        var bridge = Bridge;
        var cam = Camera;
        if (bridge == null || cam == null || !bridge.WorldRunning) { fails.Add("bridge/camera missing or world not running: " + bridge?.ConfigureError); return; }
        AutoApply = false;
        int atEventLoaded = -1;
        bridge.Reconfigured += (_, _) => atEventLoaded = bridge.TotalLoaded; // both layers

        if (!await WaitFor(() => bridge.ReadyTotal > 0, 90)) { fails.Add("no tile arrived within 90 s"); return; }
        await Delay(3.0);
        GD.Print($"[SelfTest] start: {Snapshot()}; effective {bridge.EffectiveProfileText()}");

        // Widget path first: the same signals a mouse or the keyboard would raise, with auto-apply on (0.6 s timer).
        AutoApply = true;
        int cw = bridge.ReconfigureCount;
        _speedSlider.Value = System.Math.Log10(500);
        if (System.Math.Abs(cam.Speed - 500) > 1) fails.Add($"speed slider -> camera speed {cam.Speed}, expected 500");
        _speedBox.Value = 2500; // beyond the slider's 2000 m/s end: no cap
        if (System.Math.Abs(cam.Speed - 2500) > 1) fails.Add($"speed box -> camera speed {cam.Speed}, expected 2500");
        cam.Speed = 123f; // the mouse wheel path: the panel must follow
        bool wasVisible = Visible;
        Visible = true;
        await Delay(0.3);
        if (System.Math.Abs(_speedBox.Value - 123) > 0.2 || System.Math.Abs(_speedSlider.Value - System.Math.Log10(123)) > 0.01) fails.Add($"panel did not follow the camera speed (box {_speedBox.Value}, slider {_speedSlider.Value})");
        Visible = wasVisible;
        cam.Speed = 40f;
        _maxBox.Value = 12000;
        _minSlider[1].Value = 4;
        await Delay(0.2);
        if (ApplyInFlight || bridge.ReconfigureCount != cw) fails.Add("auto-apply fired before its 0.6 s delay");
        if (!await WaitFor(() => bridge.ReconfigureCount == cw + 1 && !ApplyInFlight, 10)) { fails.Add("auto-apply did not run after the widget changes"); return; }
        if (System.Math.Abs(_appliedMaxDist - 12000) > 0.5 || System.Math.Abs(_appliedMinH[1] - 4) > 0.01) fails.Add($"widget apply: effective max distance {_appliedMaxDist}, R1 min {_appliedMinH[1]}");
        GD.Print($"[SelfTest] widgets: speed slider/box/wheel ok, auto-apply ran once; effective {_effective}");
        AutoApply = false;
        await WaitIdle(150);

        // Live quality (set_lod_quality through the slider): changes the refine distances without a reconfigure, and survives one.
        if (!HasLodQuality) fails.Add("library has no set_lod_quality (api < 1.10)");
        else
        {
            await Delay(0.8);
            bridge.ReadLod();
            double[] e0 = (double[])bridge.LodEdgesM.Clone();
            int rc0 = bridge.ReconfigureCount;
            _qualitySlider.Value = System.Math.Log10(0.5); // detail 0.5 = scale 2 = coarser
            await Delay(1.0);
            bridge.ReadLod();
            for (int i = 0; i < 3; i++)
                if (e0[i] <= 0 || System.Math.Abs(bridge.LodEdgesM[i] / e0[i] - 0.5) > 0.01) fails.Add($"quality x2 (slider): R{i} edge {e0[i]:0.#} -> {bridge.LodEdgesM[i]:0.#} m, expected half");
            if (System.Math.Abs(bridge.LodEdgesM[3] - e0[3]) > 0.5) fails.Add($"quality changed the max distance {e0[3]} -> {bridge.LodEdgesM[3]}");
            if (System.Math.Abs(bridge.LodQuality - 2.0) > 1e-9 || System.Math.Abs(bridge.TauEff - 8.0) > 0.01) fails.Add($"quality x2: library quality {bridge.LodQuality}, tau_eff {bridge.TauEff} (expected 2 and 8 px)");
            if (bridge.ReconfigureCount != rc0 || ApplyInFlight) fails.Add("the quality slider caused a reconfigure");
            GD.Print($"[SelfTest] quality x2 live: edges {e0[0]:0}/{e0[1]:0}/{e0[2]:0}/{e0[3]:0} -> {bridge.LodEdgesM[0]:0}/{bridge.LodEdgesM[1]:0}/{bridge.LodEdgesM[2]:0}/{bridge.LodEdgesM[3]:0} m, tau_eff {bridge.TauEff:0.##} px, no reconfigure");
            if (System.Math.Abs(Detail - 0.5) > 1e-9 || System.Math.Abs(_qualityBox.Value - 0.5) > 1e-6) fails.Add($"detail slider 0.5: panel detail {Detail}, box {_qualityBox.Value}");
            _qualityBox.Value = 1.0;
            await Delay(1.0);
            bridge.ReadLod();
            for (int i = 0; i < 3; i++)
                if (System.Math.Abs(bridge.LodEdgesM[i] / e0[i] - 1.0) > 0.01) fails.Add($"quality x1 (box): R{i} edge {bridge.LodEdgesM[i]:0.#} m, expected back to {e0[i]:0.#}");
            // Right = more detail: detail 2 (box) = scale 0.5 = every refine distance doubles (clamped to the max distance), tau_eff halves.
            _qualityBox.Value = 2.0;
            await Delay(1.0);
            bridge.ReadLod();
            if (System.Math.Abs(bridge.LodQuality - 0.5) > 1e-9 || System.Math.Abs(bridge.TauEff - 2.0) > 0.01) fails.Add($"detail 2 (box): library quality {bridge.LodQuality}, tau_eff {bridge.TauEff} (expected 0.5 and 2 px)");
            for (int i = 0; i < 3; i++)
                if (bridge.LodEdgesM[i] < e0[i] * 1.5 && bridge.LodEdgesM[i] < bridge.LodEdgesM[3] - 1) fails.Add($"detail 2 (box): R{i} edge {bridge.LodEdgesM[i]:0.#} m, expected about double {e0[i]:0.#}");
            GD.Print($"[SelfTest] detail 2 (finer) live: edges -> {bridge.LodEdgesM[0]:0}/{bridge.LodEdgesM[1]:0}/{bridge.LodEdgesM[2]:0}/{bridge.LodEdgesM[3]:0} m, tau_eff {bridge.TauEff:0.##} px");
            _qualitySlider.Value = 0.0;
            await Delay(1.0);
            bridge.ReadLod();
            if (System.Math.Abs(bridge.LodQuality - 1.0) > 1e-9) fails.Add($"detail 1 (slider): library quality {bridge.LodQuality}, expected 1");
            await WaitIdle(150);
        }

        // Config A: smaller ranges plus a height filter. Config B: the library defaults.
        double maxA = 6000, qualityA = 2.0, minA8 = 8;
        double maxB = MaxDistDefaultM, qualityB = 1.0;
        double[] minA = { 0, minA8, 0 }, minB = { 0, 0, 0 };
        const int ringA = 1, ringB = LightRingDefault;
        int nodesBase = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
        long loadedA = -1, loadedB = -1;
        int rootsWarm = 0;

        // Fly off the origin first so the re-centering is a real test (1.5 km east, 0.8 km north).
        cam.GlobalPosition = new Vector3(1500f, cam.GlobalPosition.Y, -800f);
        await Delay(0.5);

        int applies = SelfTestApplies();
        var nodeSeries = new List<int>();
        var otherSeries = new List<int>();
        for (int k = 0; k < applies; k++)
        {
            bool a = k % 2 == 0;
            var minH = a ? minA : minB;
            if (k > 0) cam.GlobalPosition += new Vector3(250f, 0f, -150f); // keep moving between applies
            string before = Snapshot();
            var geo0 = bridge.Streamer.LocalToGeodetic(cam.GlobalPosition);
            float y0 = cam.GlobalPosition.Y;
            int count0 = bridge.ReconfigureCount;

            SetMaxDistance(a ? maxA : maxB);
            SetQuality(a ? qualityA : qualityB);
            ApplyQualityNow(); // live, before the restart: the library keeps the scale across configure_world
            for (int i = 0; i < 3; i++) SetMinHeight(i, minH[i]);
            int lightRing = a ? ringA : ringB;
            SetLightMinRing(lightRing);
            Apply();
            if (!await WaitFor(() => !ApplyInFlight, 30)) { fails.Add($"apply {k}: bridge never answered"); return; }
            if (!LastApplyOk || bridge.ReconfigureCount != count0 + 1) { fails.Add($"apply {k}: rejected: {LastError}"); return; }
            if (atEventLoaded != 0) fails.Add($"apply {k}: {atEventLoaded} tiles still held when the reconfigure finished");
            var p = cam.GlobalPosition;
            if (System.Math.Abs(p.X) > 0.01f || System.Math.Abs(p.Z) > 0.01f || System.Math.Abs(p.Y - y0) > 0.01f) fails.Add($"apply {k}: camera at {p}, expected (0, {y0}, 0)");
            var geo1 = bridge.Streamer.LocalToGeodetic(p);
            if (System.Math.Abs(geo1.X - geo0.X) > 3e-5 || System.Math.Abs(geo1.Y - geo0.Y) > 3e-5) fails.Add($"apply {k}: geodetic position jumped from {geo0} to {geo1}");

            if (!await WaitIdle(150)) { fails.Add($"apply {k}: queues not idle after 150 s: {Snapshot()}"); return; }
            string after = Snapshot();
            GD.Print($"[SelfTest] apply {k} ({(a ? "A" : "B")}): origin moved to camera, tiles held at finish {atEventLoaded}\n  before: {before}\n  after:  {after}");

            // Applied values: what the library reports must be what was sent.
            double wantMax = a ? maxA : maxB, wantQ = a ? qualityA : qualityB;
            if (System.Math.Abs(_appliedMaxDist - wantMax) > 0.5) fails.Add($"apply {k}: effective max distance {_appliedMaxDist}, sent {wantMax}");
            bridge.ReadLod();
            if (System.Math.Abs(bridge.LodQuality - wantQ) > 1e-9) fails.Add($"apply {k}: library LOD quality {bridge.LodQuality} after the restart, expected {wantQ} (kept across configure)");
            if (System.Math.Abs(bridge.LodEdgesM[3] - wantMax) > 0.5) fails.Add($"apply {k}: lod_edges_m R3 {bridge.LodEdgesM[3]}, expected the max distance {wantMax}");
            if (!HasLightMinRing) fails.Add("profile has no lights.min_ring key");
            else if (_appliedLightRing != lightRing) fails.Add($"apply {k}: effective lights.min_ring {_appliedLightRing}, sent {lightRing}");
            if (!HasMinHeight) fails.Add("profile has no buildings.rN.min_height_m keys");
            else for (int i = 0; i < 3; i++)
                if (System.Math.Abs(_appliedMinH[i] - minH[i]) > 0.01) fails.Add($"apply {k}: effective min height R{i} {_appliedMinH[i]}, sent {minH[i]}");

            // Leak check: every pooled set in use is a tile the cover knows, and the host holds what the library says it delivered.
            await WaitFor(() => { bridge.Streamer.DebugStats(_stats); return LoadedTotal() == bridge.Cover.Loaded; }, 5);
            bridge.Streamer.DebugStats(_stats);
            if (LoadedTotal() != bridge.Cover.Loaded) fails.Add($"apply {k}: library holds {LoadedTotal()} tiles, host cover {bridge.Cover.Loaded}");
            if (bridge.PoolCreated - bridge.PoolFree != bridge.Cover.Loaded + bridge.LandmarkCover.Loaded) fails.Add($"apply {k}: pool in use {bridge.PoolCreated - bridge.PoolFree} != cover {bridge.Cover.Loaded} + landmarks {bridge.LandmarkCover.Loaded}");
            if (bridge.TileRootCount != bridge.PoolCreated) fails.Add($"apply {k}: tile roots {bridge.TileRootCount} != pool created {bridge.PoolCreated}");
            if (bridge.Cover.Loaded == 0) fails.Add($"apply {k}: nothing loaded after the restart");
            if (bridge.TerrainActive)
            {
                // The terrain layer is torn down and rebuilt with every restart too: nothing leaks, the host holds what the library delivered.
                await WaitFor(() => { bridge.Streamer.DebugStats(_stats); return TerrainLoadedTotal() == bridge.TerrainCover.Loaded; }, 5);
                bridge.Streamer.DebugStats(_stats);
                if (TerrainLoadedTotal() != bridge.TerrainCover.Loaded) fails.Add($"apply {k}: library holds {TerrainLoadedTotal()} terrain tiles, host cover {bridge.TerrainCover.Loaded}");
                if (bridge.TerrainPoolCreated - bridge.TerrainPoolFree != bridge.TerrainCover.Loaded) fails.Add($"apply {k}: terrain pool in use {bridge.TerrainPoolCreated - bridge.TerrainPoolFree} != cover {bridge.TerrainCover.Loaded}");
                if (bridge.TerrainCover.Loaded == 0) fails.Add($"apply {k}: no terrain tile after the restart");
            }
            if (k == 1) rootsWarm = bridge.TileRootCount;
            int nodesNow = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
            nodeSeries.Add(nodesNow);
            otherSeries.Add(nodesNow - bridge.PooledNodeCount());
            GD.Print($"[SelfTest] apply {k} nodes: scene {nodesNow}, objects {(int)Performance.GetMonitor(Performance.Monitor.ObjectCount)}, orphans {(int)Performance.GetMonitor(Performance.Monitor.ObjectOrphanNodeCount)}, "
                + $"outside pools {otherSeries[^1]}, tile roots {bridge.TileRootCount}, pool created {bridge.PoolCreated} free {bridge.PoolFree}, terrain pool created {bridge.TerrainPoolCreated} free {bridge.TerrainPoolFree}");
            if (a) loadedA = bridge.Cover.Loaded; else loadedB = bridge.Cover.Loaded;
        }

        // Rejection path: an out-of-range tau sent behind the sliders' back must leave the running world alone and revert the panel.
        int loadedBefore = bridge.Cover.Loaded, countBefore = bridge.ReconfigureCount, unloadedBefore = bridge.UnloadedTotal;
        bridge.Streamer.DebugStats(_stats);
        double originLat = _stats.OriginLat, originLon = _stats.OriginLon;
        var posBefore = cam.GlobalPosition;
        bridge.ApplyProfileOverrides(new Godot.Collections.Dictionary { ["lod"] = new Godot.Collections.Dictionary { ["tau_px"] = 1000.0 } });
        ApplyInFlight = true;
        if (!await WaitFor(() => !ApplyInFlight, 30)) { fails.Add("rejection test: bridge never answered"); return; }
        await Delay(1.0);
        bridge.Streamer.DebugStats(_stats);
        GD.Print($"[SelfTest] rejection: ok={LastApplyOk}, error text '{LastError}', ReconfigureCount {countBefore} -> {bridge.ReconfigureCount}, "
            + $"tiles held {loadedBefore} -> {bridge.Cover.Loaded} ({bridge.UnloadedTotal - unloadedBefore} unloaded meanwhile by normal streaming), library origin unchanged: {_stats.OriginLat == originLat && _stats.OriginLon == originLon}");
        if (LastApplyOk || LastError.Length == 0) fails.Add("rejection test: an out-of-range lod.tau_px was not rejected with an error text");
        if (bridge.ReconfigureCount != countBefore) fails.Add("rejection test: counted as a reconfigure");
        if (bridge.TerrainActive && bridge.TerrainCover.Loaded == 0) fails.Add("rejection test: the terrain layer was dropped");
        if (bridge.UnloadedTotal - unloadedBefore > loadedBefore / 3 || bridge.Cover.Loaded < loadedBefore / 2) fails.Add($"rejection test: the world was dropped ({loadedBefore} -> {bridge.Cover.Loaded} tiles); the previous world must keep running");
        if (_stats.OriginLat != originLat || _stats.OriginLon != originLon) fails.Add("rejection test: library origin changed");
        if (cam.GlobalPosition != posBefore) fails.Add("rejection test: camera moved");
        if (System.Math.Abs(_maxDist - _appliedMaxDist) > 0.01 || System.Math.Abs(_maxBox.Value - _appliedMaxDist) > 0.5) fails.Add("rejection test: sliders not reverted");

        int roots = bridge.TileRootCount;
        int peak = System.Math.Max(24, bridge.PeakActiveTiles);
        int nodesEnd = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
        GD.Print($"[SelfTest] tile pool: {roots} node sets at the end ({bridge.PoolTrimmed} trimmed; after first A+B: {rootsWarm}, peak tiles held {bridge.PeakActiveTiles}, prewarmed 24), "
            + $"{bridge.PoolCreated - bridge.PoolFree} in use; scene nodes {nodesBase} -> {nodesEnd}; last A loaded {loadedA}, last B loaded {loadedB}");
        // Scene nodes outside the tile pools: pooled tiles may grow to a high-water mark during the first applies (that is not a leak), but nothing else may be created per reconfigure.
        if (nodeSeries.Count > 4)
        {
            int hw = 0;
            for (int i = 0; i < 4; i++) hw = System.Math.Max(hw, otherSeries[i]);
            int worst = hw;
            for (int i = 4; i < otherSeries.Count; i++) worst = System.Math.Max(worst, otherSeries[i]);
            GD.Print($"[SelfTest] scene nodes per apply: {string.Join(" ", nodeSeries)}; outside the tile pools: {string.Join(" ", otherSeries)}; high-water of the first 4 applies {hw}, max afterwards {worst} (allowed +{NodeGrowthSlack})");
            if (worst > hw + NodeGrowthSlack) fails.Add($"scene nodes outside the tile pools grew after the warm-up: {worst} vs {hw} in the first four applies (leak of {worst - hw} nodes over {nodeSeries.Count} applies)");
        }
        if (roots > rootsWarm * 1.15 + 8) fails.Add($"tile pool grew from {rootsWarm} (after the first A+B) to {roots} node sets");
        // Without trimming the pool holds exactly the peak (no leak, no double allocation). With it (PoolTrimmer, e.g. --pool-trim-hold=3) sets that were destroyed
        // and re-allocated make "ever allocated" larger, so only the sets that exist now are bounded by the peak.
        if (bridge.PoolTrimmed == 0 ? roots != peak : roots > peak) fails.Add($"tile pool has {roots} node sets ({bridge.PoolTrimmed} trimmed), expected {(bridge.PoolTrimmed == 0 ? "exactly" : "at most")} the peak {peak} (leak or double allocation)");
        if (loadedA >= loadedB) fails.Add($"config A (max distance 6000, quality x2) loaded {loadedA} tiles, not fewer than config B's {loadedB}: the ranges had no effect");
        if (bridge.ConsistencyErrors > 0) fails.Add($"{bridge.ConsistencyErrors} cover consistency errors");
    }
}
