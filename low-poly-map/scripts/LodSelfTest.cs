using System.Collections.Generic;
using System.Globalization;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--lod-selftest</c> (screen-space LOD, api 1.10; synthetic city, headless: <c>--tile-url=</c>). The view is set with
/// <see cref="WorldView.SetViewOverride"/> and the library's live edges (<c>debug_stats()["lod_edges_m"]</c>) are checked against the contract formula
/// edge = e_r * H / (2 tan(fov/2) * tau_eff): after settling at 60 deg / 1080 px the edges and the finest-tile count are recorded; fov 30 multiplies every
/// refine distance by tan 30 / tan 15 (within 1 %) and holds more R0 tiles; fov 90 fewer; H 540 halves the edges; set_lod_quality(2) halves them and
/// tau_eff doubles; the window / sprite blend follows the R2 to R1 edge. The cover must stay consistent (<see cref="WorldView.VerifyCover"/>, 0 errors) after every
/// phase. Prints <c>LOD SELFTEST PASS/FAIL</c>, exit code 0 / 1.
/// </summary>
public partial class LodSelfTest : Node
{
    private const string Arg = "--lod-selftest";
    public static bool Requested() => System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), Arg);

    public static void Attach(WorldView view, Node? parent) => (parent ?? view).CallDeferred(Node.MethodName.AddChild, new LodSelfTest { Name = "LodSelfTest", _view = view });

    private WorldView _view = null!;
    private readonly List<string> _fails = new();
    private readonly StreamerStats _stats = new();
    private string _summary = "";

    public override void _Ready()
    {
        if (Engine.MaxFps == 0) Engine.MaxFps = 60;
        RunAll();
    }

    private async void RunAll()
    {
        try { await Body(); }
        catch (System.Exception e) { _fails.Add("exception: " + e); }
        _view.SetViewOverride(60f, 1080);
        GD.Print(_fails.Count == 0 ? $"LOD SELFTEST PASS: {_summary}" : "LOD SELFTEST FAIL: " + string.Join("; ", _fails) + " | " + _summary);
        GetTree().Quit(_fails.Count == 0 ? 0 : 1);
    }

    private async Task Frames(int n) { for (int i = 0; i < n; i++) await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame); }
    private async Task Delay(double s) => await ToSignal(GetTree().CreateTimer(s), SceneTreeTimer.SignalName.Timeout);

    private async Task<bool> WaitFor(System.Func<bool> cond, double timeoutS)
    {
        ulong end = Time.GetTicksMsec() + (ulong)(timeoutS * 1000);
        while (!cond()) { if (Time.GetTicksMsec() > end) return false; await Frames(1); }
        return true;
    }

    /// <summary>Nothing queued, fetching or meshing for 1.5 s in a row, and every finished tile uploaded (library holds == host holds).</summary>
    private async Task<bool> Settle(double timeoutS)
    {
        ulong calmSince = 0;
        return await WaitFor(() =>
        {
            _view.Streamer.DebugStats(_stats);
            int busy = 0, held = 0;
            for (int i = 0; i < StreamerStats.Rings; i++) { busy += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i]; held += _stats.Loaded[i]; }
            if (busy != 0 || held != _view.Cover.Loaded) { calmSince = 0; return false; }
            if (calmSince == 0) calmSince = Time.GetTicksMsec();
            return Time.GetTicksMsec() - calmSince >= 1500;
        }, timeoutS);
    }

    private sealed record Sample(double[] Edges, int R0Held, int Held, double Tau, double Quality, double Fov, int H);

    /// <summary>Sets the view / quality, waits for the library to report it, lets the cover settle, verifies the cover and returns the live edges and counts.</summary>
    private async Task<Sample> Phase(string tag, float fov, int h, double quality)
    {
        _view.SetViewOverride(fov, h);
        if (System.Math.Abs(_view.LodQuality - quality) > 1e-9) _view.SetLodQuality(quality);
        bool echoed = await WaitFor(() =>
        {
            _view.ReadLod();
            return System.Math.Abs(_view.LibraryViewFovYDeg - fov) < 0.01 && _view.LibraryViewHeightPx == h && System.Math.Abs(_view.LodQuality - quality) < 1e-9;
        }, 10);
        if (!echoed) _fails.Add($"{tag}: the library never reported fov {fov} / H {h} / quality {quality}");
        await Frames(5); // the edges follow the next update_camera
        if (!await Settle(120)) _fails.Add($"{tag}: the cover did not settle within 120 s");
        int errors0 = _view.ConsistencyErrors;
        _view.VerifyCover();
        await Frames(2);
        _view.VerifyCover(); // a hole must persist over two checks to be reported
        if (_view.ConsistencyErrors != errors0) _fails.Add($"{tag}: cover consistency errors {_view.ConsistencyErrors - errors0}");
        _view.ReadLod();
        _view.Streamer.DebugStats(_stats);
        var s = new Sample((double[])_view.LodEdgesM.Clone(), _stats.Loaded[0], _view.Cover.Loaded, _view.TauEff, _view.LodQuality, _view.LibraryViewFovYDeg, _view.LibraryViewHeightPx);
        GD.Print($"[LodSelfTest] {tag}: fov {s.Fov:0.#} H {s.H} quality x{s.Quality:0.##} tau_eff {s.Tau:0.##} px | edges {F(s.Edges[0])}/{F(s.Edges[1])}/{F(s.Edges[2])}/{F(s.Edges[3])} m | R0 held {s.R0Held}, visible R0..R3 "
            + $"{_view.Cover.VisibleByRing[0]}/{_view.Cover.VisibleByRing[1]}/{_view.Cover.VisibleByRing[2]}/{_view.Cover.VisibleByRing[3]}, held {s.Held} | blend D {_view.BlendEdgeM:0} m");
        return s;
    }

    private static string F(double v) => v.ToString("0.#", CultureInfo.InvariantCulture);

    private void ExpectRatio(string tag, Sample cur, Sample baseline, double ratio, double tol = 0.01)
    {
        for (int i = 0; i < 3; i++)
        {
            double got = cur.Edges[i] / baseline.Edges[i];
            if (baseline.Edges[i] <= 0 || System.Math.Abs(got / ratio - 1.0) > tol)
                _fails.Add($"{tag}: edge R{i} {F(baseline.Edges[i])} -> {F(cur.Edges[i])} m (x{got:0.0000}), expected x{ratio:0.0000} +-{tol * 100:0.#} %");
        }
        if (System.Math.Abs(cur.Edges[3] - baseline.Edges[3]) > 0.5) _fails.Add($"{tag}: max distance changed {F(baseline.Edges[3])} -> {F(cur.Edges[3])} m");
    }

    /// <summary>The materials hold 0.36 D .. 0.8 D (window / sprite blend), 0.48 D .. min(D, D / hysteresis) (street pools: no pool beyond where R1 ends) and 0.84 M .. M (sprite far fade) within the 5 % the view lets D drift.</summary>
    private void CheckBlendMaterials(string tag, Sample s)
    {
        var r = _view.ReadMaterialBlendRanges();
        if (r.Length != 8) { _fails.Add($"{tag}: cannot read the material ranges"); return; }
        double d = _view.BlendEdgeM, m = s.Edges[3];
        (string, double, double)[] want =
        {
            ("window avg start", r[0], d * WorldRenderSettings.BlendStartShare), ("window avg end", r[1], d * WorldRenderSettings.BlendEndShare),
            ("sprite blend start", r[2], d * WorldRenderSettings.BlendStartShare), ("sprite blend end", r[3], d * WorldRenderSettings.BlendEndShare),
            ("pool start", r[4], d * WorldRenderSettings.PoolStartShare), ("pool end", r[5], System.Math.Min(d * WorldRenderSettings.PoolEndShare, d / _view.Hysteresis)),
            ("sprite fade start", r[6], m * WorldRenderSettings.LightFadeShare), ("sprite far end", r[7], m),
        };
        foreach (var (name, got, exp) in want)
            if (System.Math.Abs(got - exp) > exp * 0.001 + 0.5) _fails.Add($"{tag}: {name} {got:0.#} m, expected {exp:0.#} m");
        if (System.Math.Abs(d - s.Edges[1]) > s.Edges[1] * 0.05) _fails.Add($"{tag}: blend D {d:0} m is more than 5 % off the live R2 to R1 edge {s.Edges[1]:0} m");
        if (tag == "baseline" && (System.Math.Abs(r[0] - 900) > 0.5 || System.Math.Abs(r[1] - 2000) > 0.5 || System.Math.Abs(r[4] - 1200) > 0.5 || System.Math.Abs(r[5] - 2500 / _view.Hysteresis) > 0.5 || System.Math.Abs(r[6] - 21000) > 0.5 || System.Math.Abs(r[7] - 25000) > 0.5))
            _fails.Add($"baseline: the ranges are not today's 900/2000, 1200/(2500 / hysteresis), 21000/25000: {string.Join("/", r)}");
    }

    private async Task Body()
    {
        var view = _view;
        if (!view.WorldRunning) { _fails.Add("no running world: " + view.ConfigureError); return; }
        if (!view.Streamer.HasSetView) { _fails.Add($"library api {view.Streamer.ApiVersionText} has no set_view / lod_edges_m (needs 1.10)"); return; }
        if (!await WaitFor(() => view.ReadyTotal > 0, 90)) { _fails.Add("no tile arrived within 90 s"); return; }
        await Delay(1.0);
        var b = await Phase("baseline 60/1080", 60f, 1080, 1.0);
        int blendUpdates0 = view.BlendUpdates;
        double[] want = { 600, 2500, 7000, 25000 };
        for (int i = 0; i < 4; i++)
            if (System.Math.Abs(b.Edges[i] / want[i] - 1.0) > 0.005) _fails.Add($"baseline: edge R{i} {F(b.Edges[i])} m, the default view should give {want[i]} m");
        if (b.R0Held == 0) _fails.Add("baseline: no R0 tile held (is the camera over the city?)");
        if (view.BlendUpdates == 0) _fails.Add("baseline: the window / sprite blend was never written from the live edges");
        CheckBlendMaterials("baseline", b);
        if (System.Math.Abs(view.BlendEdgeM - b.Edges[1]) > b.Edges[1] * 0.05) _fails.Add($"baseline: blend D {view.BlendEdgeM:0} m does not follow the R2 to R1 edge {F(b.Edges[1])} m");

        double t30 = System.Math.Tan(Mathf.DegToRad(30f)), t15 = System.Math.Tan(Mathf.DegToRad(15f)), t45 = System.Math.Tan(Mathf.DegToRad(45f));
        var f30 = await Phase("fov 30", 30f, 1080, 1.0);
        ExpectRatio("fov 30", f30, b, t30 / t15);
        if (f30.R0Held <= b.R0Held) _fails.Add($"fov 30: {f30.R0Held} R0 tiles held, not more than the baseline's {b.R0Held}");
        if (view.BlendUpdates <= blendUpdates0) _fails.Add("fov 30: the blend ranges did not follow the moved edge");
        CheckBlendMaterials("fov 30", f30);
        if (System.Math.Abs(view.BlendEdgeM - f30.Edges[1]) > f30.Edges[1] * 0.05) _fails.Add($"fov 30: blend D {view.BlendEdgeM:0} m, live R2 to R1 edge {F(f30.Edges[1])} m");

        var f90 = await Phase("fov 90", 90f, 1080, 1.0);
        ExpectRatio("fov 90", f90, b, t30 / t45);
        if (f90.R0Held >= b.R0Held) _fails.Add($"fov 90: {f90.R0Held} R0 tiles held, not fewer than the baseline's {b.R0Held}");

        var h540 = await Phase("H 540", 60f, 540, 1.0);
        ExpectRatio("H 540", h540, b, 0.5);

        var q2 = await Phase("quality x2", 60f, 1080, 2.0);
        ExpectRatio("quality x2", q2, b, 0.5);
        if (System.Math.Abs(q2.Tau - 8.0) > 0.01) _fails.Add($"quality x2: tau_eff {q2.Tau}, expected 8");
        if (q2.R0Held >= b.R0Held) _fails.Add($"quality x2: {q2.R0Held} R0 tiles held, not fewer than the baseline's {b.R0Held}");

        CheckBlendMaterials("quality x2", q2);
        var back = await Phase("back to 60/1080 x1", 60f, 1080, 1.0);
        ExpectRatio("back", back, b, 1.0);
        if (back.R0Held != b.R0Held) GD.Print($"[LodSelfTest] note: R0 held {b.R0Held} at the start, {back.R0Held} after the round trip (the camera does not move, hysteresis can differ)");

        _summary = $"edges 60/1080 {F(b.Edges[0])}/{F(b.Edges[1])}/{F(b.Edges[2])}/{F(b.Edges[3])} m, R0 tiles 90deg {f90.R0Held} < 60deg {b.R0Held} < 30deg {f30.R0Held}, "
            + $"fov 30 x{f30.Edges[1] / b.Edges[1]:0.000} (tan30/tan15 = {t30 / t15:0.000}), H 540 x{h540.Edges[1] / b.Edges[1]:0.000}, quality 2 x{q2.Edges[1] / b.Edges[1]:0.000}, "
            + $"blend updates {view.BlendUpdates - blendUpdates0}, cover errors {view.ConsistencyErrors}";
    }
}
