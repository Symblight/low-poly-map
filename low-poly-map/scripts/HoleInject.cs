using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// <c>--hole-inject=cull|layers|hidden</c> (with <c>--hole-check --hole-pixels --hole-flight</c>): proves the pixel test sees what the host state cannot. From t = 25 s, once the camera
/// looks well down at a tile of ring 0 / 1 that has ground, it breaks that tile's GROUND instance for 2 s, three times: <c>cull</c> = a custom AABB far away (the engine frustum-culls
/// the instance; every host flag stays as it was), <c>layers</c> = render layer mask 0 (no camera draws it; every host flag stays as it was), <c>hidden</c> = the instance made
/// invisible (a control the host check sees as well). The run must FAIL (pixel holes; for <c>cull</c> also <c>engine-culled-visible-ground</c>); without it, it must pass.
/// </summary>
public partial class HoleInject : Node
{
    public static bool Requested(out string mode)
    {
        mode = "";
        foreach (var a in OS.GetCmdlineUserArgs())
            if (a.StartsWith("--hole-inject=", System.StringComparison.Ordinal)) { mode = a.Substring(14); return true; }
        return false;
    }

    public static void Attach(WorldView view, Node? parent, string mode) =>
        (parent ?? view).CallDeferred(Node.MethodName.AddChild, new HoleInject { Name = "HoleInject", _view = view, _mode = mode });

    private WorldView _view = null!;
    private string _mode = "";
    private double _t, _until;
    private int _rounds;
    private TileNodes? _victim;

    public override void _Ready() { ProcessPriority = 90; GD.Print($"[HoleInject] mode {_mode}"); }

    public override void _Process(double delta)
    {
        _t += delta;
        if (_victim != null)
        {
            if (_t >= _until) { Restore(); _victim = null; _rounds++; _until = _t + 8; }
            return;
        }
        if (_t < 25 || _t < _until || _rounds >= 3) return;
        var cam = _view.Camera;
        if (cam == null) return;
        var f = -cam.GlobalTransform.Basis.Z;
        if (f.Y > -0.3f) return; // looks at least 17 degrees down
        float s = (cam.GlobalPosition.Y - (float)_view.CameraGroundHeight) / -f.Y;
        float x = cam.GlobalPosition.X + f.X * s, z = cam.GlobalPosition.Z + f.Z * s;
        TileNodes? best = null;
        foreach (var n in _view.Cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible || !n.HasGround || n.Ring > 1 || n.Fading) continue;
            var rp = n.Root.Position;
            float half = n.Ring == 0 ? 150f : 300f;
            if (System.Math.Abs(rp.X - x) > half || System.Math.Abs(rp.Z - z) > half) continue;
            if (best == null || n.Ring < best.Ring) best = n;
        }
        if (best == null) return;
        _victim = best;
        _until = _t + 2;
        switch (_mode)
        {
            case "cull": best.GroundInstance.CustomAabb = new Aabb(new Vector3(1e6f, 1e6f, 1e6f), new Vector3(1f, 1f, 1f)); break;
            case "layers": best.GroundInstance.Layers = 0; break;
            default: best.GroundInstance.Visible = false; break; // "hidden": the host check sees this one too (control)
        }
        GD.Print($"[HoleInject] t={_t:0.0}s {_mode}: broke the ground of {best.Key} (ring {best.Ring}) for 2 s");
    }

    private void Restore()
    {
        var v = _victim!;
        v.GroundInstance.CustomAabb = new Aabb();
        v.GroundInstance.Layers = 1;
        v.GroundInstance.Visible = v.HasGround;
        GD.Print($"[HoleInject] t={_t:0.0}s restored {v.Key}");
    }
}
