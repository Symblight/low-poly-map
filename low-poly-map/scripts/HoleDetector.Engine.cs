using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

// Two checks that see what the host state cannot, without pixels (the dither mask runs headless too; the engine cull query needs a real renderer):
//  - root-dither-gap: the stack of tiles over a root zoom tile must keep all 16 pixels of the Bayer cell in every frame (the shader's rule, HolePixelProbe.KeepMask), not
//    only weights that sum to 1: a pair whose parities or fades do not complement leaves pixels nobody draws;
//  - engine-culled-visible-ground (windowed only): the engine's own frustum query (RenderingServer.InstancesCullConvex, AABB + extra cull margin) against sample points of the tile's drawn
//    surface (lowered by the earth curvature like the shader does): a tile the host shows whose surface is inside the frustum must not be culled.
public partial class HoleDetector
{
    private const double CullEveryS = 0.25;
    private double _cullT;
    private readonly HashSet<ulong> _engineIds = new();
    private readonly Dictionary<long, int> _cullBad = new();
    private readonly List<long> _cullGone = new();
    private long _cullChecked, _maskChecks;
    private bool _planesOutward = true, _orientationKnown;

    /// <summary>The 16-pixel dither mask the stack of visible tiles at and under <paramref name="k"/> keeps.</summary>
    private int Mask(long k)
    {
        int m = 0;
        if (_view.Cover.TryGet(k, out var t) && t.Vis == TileCover.Vis.Visible) m = HolePixelProbe.KeepMask(t);
        if (TileKey.Z(k) < TileKey.LeafZ)
            for (int q = 0; q < 4; q++) m |= Mask(TileKey.Child(k, q));
        return m;
    }

    private void CheckEngineCulling(double delta)
    {
        if (DisplayServer.GetName() == "headless") return; // the dummy renderer keeps no instance AABBs: its cull query is meaningless
        _cullT += delta;
        if (_cullT < CullEveryS) return;
        _cullT = 0;
        var cam = _view.Camera;
        if (cam == null || !cam.IsInsideTree()) return;
        var planes = cam.GetFrustum();
        if (planes.Count < 6) return;
        if (!_orientationKnown)
        {
            // Outward normals: a point in front of the camera is behind every plane (negative side). Settled once.
            var probe = cam.GlobalPosition - cam.GlobalTransform.Basis.Z * 50f;
            int outward = 0;
            for (int i = 0; i < planes.Count; i++) if (planes[i].DistanceTo(probe) < 0) outward++;
            _planesOutward = outward >= 5;
            _orientationKnown = true;
        }
        _engineIds.Clear();
        foreach (var id in RenderingServer.InstancesCullConvex(planes, _view.GetWorld3D().Scenario)) _engineIds.Add((ulong)id);
        var camPos = cam.GlobalPosition;
        float invTwoR = (float)(_view.Curvature / (2.0 * 6_371_000.0));
        _seenCull.Clear();
        foreach (var n in _view.Cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible || !n.HasGround || !n.Root.IsVisibleInTree() || !n.GroundInstance.Visible) continue;
            if (n.Fading && n.Fade <= 0.02f) continue;
            var rp = n.Root.Position;
            var ab = n.GroundInstance.GetAabb();
            // Cheap candidate test: the AABB (its floor lowered by the curvature at the farthest corner) is not wholly outside one frustum plane. The box need not hold geometry in the part
            // that is in view, so a candidate is confirmed against the real vertices below.
            float farX = System.Math.Max(System.Math.Abs(rp.X + ab.Position.X - camPos.X), System.Math.Abs(rp.X + ab.End.X - camPos.X));
            float farZ = System.Math.Max(System.Math.Abs(rp.Z + ab.Position.Z - camPos.Z), System.Math.Abs(rp.Z + ab.End.Z - camPos.Z));
            float lowY = rp.Y + ab.Position.Y - (farX * farX + farZ * farZ) * invTwoR;
            bool candidate = true;
            for (int pl = 0; pl < planes.Count && candidate; pl++)
            {
                bool allOutside = true;
                for (int k8 = 0; k8 < 8 && allOutside; k8++)
                {
                    var corner = new Vector3(rp.X + ((k8 & 1) == 0 ? ab.Position.X : ab.End.X), (k8 & 2) == 0 ? lowY : rp.Y + ab.End.Y, rp.Z + ((k8 & 4) == 0 ? ab.Position.Z : ab.End.Z));
                    float d = planes[pl].DistanceTo(corner);
                    if (_planesOutward ? d <= 0 : d >= 0) allOutside = false;
                }
                if (allOutside) candidate = false;
            }
            if (!candidate || _engineIds.Contains(n.GroundInstance.GetInstanceId())) { if (candidate) _cullChecked++; continue; }
            _cullChecked++;
            int inside = VerticesInside(n, planes, camPos, invTwoR);
            if (inside == 0) continue;
            _seenCull.Add(n.Packed);
            _cullBad.TryGetValue(n.Packed, out int c);
            _cullBad[n.Packed] = c + 1;
            if (c + 1 == 2)
            {
                _holes++;
                Count("engine-culled-visible-ground");
                GD.Print($"[HOLE] {Stamp()} engine-culled-visible-ground {n.Key} ring={n.Ring}: the host shows it, {inside} of its ground vertices (lowered by the curvature) are inside the frustum, but the engine's cull query does not return its ground instance; "
                    + $"cullMargin={n.GroundInstance.ExtraCullMargin:0.0} (written {n.CullMargin:0.0}) halfSpan={n.HalfSpanM:0} dist={new Vector2(rp.X - camPos.X, rp.Z - camPos.Z).Length():0} aabb y {ab.Position.Y:0.0}..{ab.End.Y:0.0} size=({ab.Size.X:0},{ab.Size.Z:0}) cam=({camPos.X:0},{camPos.Y:0},{camPos.Z:0}) fov={cam.Fov:0.0}");
            }
        }
        if (_cullBad.Count > 0)
        {
            _cullGone.Clear();
            foreach (var k in _cullBad.Keys) if (!_seenCull.Contains(k)) _cullGone.Add(k);
            foreach (var k in _cullGone) _cullBad.Remove(k);
        }
    }

    private readonly HashSet<long> _seenCull = new();

    private int VerticesInside(TileNodes n, Godot.Collections.Array<Plane> planes, Vector3 camPos, float invTwoR)
    {
        var verts = n.GroundMesh.SurfaceGetArrays(0)[(int)Mesh.ArrayType.Vertex].AsVector3Array();
        var rp = n.Root.Position;
        int inside = 0;
        foreach (var v in verts)
        {
            float x = rp.X + v.X, z = rp.Z + v.Z, dx = x - camPos.X, dz = z - camPos.Z;
            if (InsideFrustum(planes, new Vector3(x, rp.Y + v.Y - (dx * dx + dz * dz) * invTwoR, z), 3f)) inside++;
        }
        return inside;
    }

    private bool InsideFrustum(Godot.Collections.Array<Plane> planes, Vector3 p, float inset)
    {
        for (int i = 0; i < planes.Count; i++)
        {
            float d = planes[i].DistanceTo(p);
            if (_planesOutward ? d > -inset : d < inset) return false;
        }
        return true;
    }
}
