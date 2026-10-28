using System.Globalization;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--terrain-collision-selftest</c> (implies <c>--terrain</c> and <c>--terrain-collision</c>; headless, <c>--tile-url=</c> for the synthetic city):
/// 49 rays straight down over +-400 m around the camera must hit the collision patch within <see cref="Bound"/> of <c>ground_height_at</c>; the same after the camera
/// jumped 3 km, and after a second 3 km jump that crosses an origin shift (the patch moved with it); with the feature off there is no body in the tree and
/// no hit; switching it on again brings the patch back. Prints the measured deviation and fill cost, <c>TERRAIN COLLISION SELFTEST PASS/FAIL: ...</c>, exit 0 / 1.
/// </summary>
public partial class TerrainCollisionSelfTest : Node
{
    private const string Arg = "--terrain-collision-selftest";
    /// <summary>Allowed |hit.y - ground_height_at| in metres: linear interpolation between the 40 m samples of the default patch.</summary>
    private const double Bound = 0.1;

    public static bool Requested() => System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), Arg);

    public static void Attach(WorldView view, Node? parent) => (parent ?? view).CallDeferred(Node.MethodName.AddChild, new TerrainCollisionSelfTest { Name = "TerrainCollisionSelfTest", _view = view });

    private WorldView _view = null!;
    private FreeFlyCamera? _cam;
    private readonly System.Collections.Generic.List<string> _fails = new();
    private string _summary = "";
    private Vector3 _jump;
    private double _maxDev;

    public override void _Ready()
    {
        if (Engine.MaxFps == 0) Engine.MaxFps = 60;
        _cam = _view.Camera as FreeFlyCamera;
        RunAll();
    }

    private async void RunAll()
    {
        try { await Body(); }
        catch (System.Exception e) { _fails.Add("exception: " + e); }
        _view.ExternalCameraUpdate = null;
        GD.Print(_fails.Count == 0 ? $"TERRAIN COLLISION SELFTEST PASS: {_summary}" : "TERRAIN COLLISION SELFTEST FAIL: " + string.Join("; ", _fails) + " | " + _summary);
        GetTree().Quit(_fails.Count == 0 ? 0 : 1);
    }

    private async Task Frames(int n) { for (int i = 0; i < n; i++) await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame); }
    private async Task Physics(int n) { for (int i = 0; i < n; i++) await ToSignal(GetTree(), SceneTree.SignalName.PhysicsFrame); }

    private async Task<bool> WaitFor(System.Func<bool> cond, double timeoutS)
    {
        ulong end = Time.GetTicksMsec() + (ulong)(timeoutS * 1000);
        while (!cond()) { if (Time.GetTicksMsec() > end) return false; await Frames(1); }
        return true;
    }

    /// <summary>49 rays over +-400 m around the camera; returns hits, records the largest deviation.</summary>
    private async Task<int> Rays(string tag, bool expectHits)
    {
        await Physics(3);
        var space = _view.GetWorld3D().DirectSpaceState;
        var cam = _cam!.GlobalPosition;
        int hits = 0, bad = 0;
        double dev = 0;
        for (int i = -3; i <= 3; i++)
            for (int j = -3; j <= 3; j++)
            {
                float x = cam.X + i * 133f + 17f, z = cam.Z + j * 133f - 11f;
                var r = space.IntersectRay(PhysicsRayQueryParameters3D.Create(new Vector3(x, 3000f, z), new Vector3(x, -800f, z)));
                if (r.Count == 0) continue;
                hits++;
                if (r["collider"].AsGodotObject() is not StaticBody3D b || b.Name != "TerrainCollision") { bad++; continue; }
                double d = System.Math.Abs(r["position"].AsVector3().Y - _view.Streamer.GroundHeightAt(x, z));
                if (d > dev) dev = d;
            }
        if (dev > _maxDev) _maxDev = dev;
        GD.Print($"[TerrainCollisionSelfTest] {tag}: {hits}/49 rays hit, max |hit.y - ground_height_at| {dev:0.000} m, camera {cam}");
        if (expectHits)
        {
            if (hits != 49) _fails.Add($"{tag}: {hits} of 49 rays hit");
            if (bad > 0) _fails.Add($"{tag}: {bad} hits on a collider that is not the terrain patch");
            if (dev > Bound) _fails.Add($"{tag}: deviation {dev:0.000} m above {Bound} m");
        }
        else if (hits > 0) _fails.Add($"{tag}: {hits} rays hit although the feature is off");
        return hits;
    }

    private int BodiesInTree() => _view.FindChildren("*", "StaticBody3D", true, false).Count;

    private async Task<bool> Settled(string tag)
    {
        bool ok = await WaitFor(() => _view.TerrainCollision is { Ready: true } cc && System.Math.Abs(cc.Center.X - _cam!.GlobalPosition.X) < 100 && System.Math.Abs(cc.Center.Y - _cam.GlobalPosition.Z) < 100, 20);
        if (!ok) _fails.Add($"{tag}: the patch did not settle under the camera");
        return ok;
    }

    private void Hold(double dt)
    {
        if (_jump != Vector3.Zero)
        {
            var p = _cam!.GlobalPosition + _jump;
            _jump = Vector3.Zero;
            _cam.GlobalPosition = new Vector3(p.X, (float)(_view.Streamer.GroundHeightAt(p.X, p.Z) + 300.0), p.Z);
        }
        _view.Streamer.UpdateCamera(_cam!.GlobalPosition, Vector3.Zero, _cam.AltitudeAgl);
    }

    private async Task Body()
    {
        var view = _view;
        if (_cam == null || !view.WorldRunning) { _fails.Add("no running world: " + view.ConfigureError); return; }
        if (!view.TerrainCollisionEnabled) { _fails.Add("TerrainCollisionEnabled is off: launch with --terrain-collision-selftest"); return; }
        if (!await WaitFor(() => view.TerrainReadyTotal > 0, 90)) { _fails.Add("no terrain tile arrived"); return; }
        _cam.SetProcess(false);
        _cam.SetProcessUnhandledInput(false);
        view.ExternalCameraUpdate = Hold;
        if (!await Settled("start")) return;
        int shifts0 = view.OriginShiftCount;
        await Rays("at the start", true);

        _jump = new Vector3(3000f, 0f, 0f);
        await Frames(2);
        if (!await Settled("after 3 km")) return;
        await Rays("after a 3 km jump", true);

        _jump = new Vector3(3000f, 0f, 0f);
        await Frames(2);
        if (!await Settled("after 6 km")) return;
        await Rays("after 6 km, across an origin shift", true);
        int shifts = view.OriginShiftCount - shifts0;
        if (shifts < 1) _fails.Add("no origin shift during the test");

        var c = view.TerrainCollision!;
        string cost = $"{c.Fills} fills, patch fill {c.LastFillUs / 1000.0:0.00} ms total over frames, worst frame {c.FillFrameUsMax / 1000.0:0.00} ms, commit (Jolt) max {c.CommitUsMax / 1000.0:0.00} ms";
        view.TerrainCollisionEnabled = false;
        await Frames(3);
        int bodies = BodiesInTree();
        GD.Print($"[TerrainCollisionSelfTest] off: collision object {(view.TerrainCollision == null ? "null" : "present")}, StaticBody3D nodes in the world view {bodies}");
        if (view.TerrainCollision != null || bodies != 0) _fails.Add($"off: {bodies} bodies left / helper still present");
        await Rays("off", false);

        view.TerrainCollisionEnabled = true;
        if (await Settled("on again")) await Rays("switched on again", true);
        _summary = $"origin shifts {shifts}, max deviation {_maxDev:0.000} m (bound {Bound} m), {cost}";
    }
}
