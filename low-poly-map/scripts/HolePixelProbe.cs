using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Helper of the pixel test in <see cref="HoleMetrics"/>: (1) remembers the camera of every frame that was drawn, so the image the check reads (the last
/// frame the renderer finished, not the one being processed) is matched with the camera that rendered it, and (2) explains a magenta region: the screen point
/// is cast to the ground, and every vector tile whose rect holds that point is described (host state, the dither pattern the stack of tiles keeps, the engine's
/// own frustum query for the tile's instances, the library's <c>debug_tiles</c> entry).
/// </summary>
public sealed class HolePixelProbe
{
    public struct Snap
    {
        public long Drawn;
        public Vector3 Pos;
        public Basis B;
        public float FovDeg;
        public bool Valid;
    }

    private const int Ring = 8;
    private readonly Snap[] _snaps = new Snap[Ring];
    private readonly WorldView _view;
    private readonly List<DebugTile> _dbg = new(512);

    public HolePixelProbe(WorldView view) { _view = view; }

    /// <summary>Stores the camera as it stands at the end of this process frame under the number of frames drawn so far; the draw that follows renders exactly this state.</summary>
    public void Record(long drawn, Camera3D cam)
    {
        ref var s = ref _snaps[drawn % Ring];
        s.Drawn = drawn;
        s.Pos = cam.GlobalPosition;
        s.B = cam.GlobalTransform.Basis;
        s.FovDeg = cam.Fov;
        s.Valid = true;
    }

    public bool TryGet(long drawn, out Snap snap)
    {
        snap = _snaps[(drawn % Ring + Ring) % Ring];
        return snap.Valid && snap.Drawn == drawn;
    }

    /// <summary>Ray through the pixel (px, py) of a w x h image to the plane y = groundY; false when it points at or above the horizon.</summary>
    public static bool GroundPoint(in Snap s, float px, float py, int w, int h, float groundY, out float x, out float z, out float dist)
    {
        x = z = dist = 0f;
        float t = Mathf.Tan(Mathf.DegToRad(s.FovDeg) * 0.5f);
        float nx = (px / w * 2f - 1f) * t * w / h, ny = (1f - py / h * 2f) * t;
        var d = (s.B * new Vector3(nx, ny, -1f)).Normalized();
        if (d.Y > -0.002f) return false;
        float k = (groundY - s.Pos.Y) / d.Y;
        x = s.Pos.X + d.X * k;
        z = s.Pos.Z + d.Z * k;
        dist = k;
        return true;
    }

    private static int Bayer(int x, int y)
    {
        // same matrix as lod_dither.gdshaderinc
        int[] m = { 0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5 };
        return m[y * 4 + x];
    }

    /// <summary>The 16 pixels of the 4 x 4 Bayer cell a tile keeps (bit i = pixel y*4+x): all of them unless it dithers, then the shader's rule.</summary>
    public static int KeepMask(TileNodes n)
    {
        if (!(n.Fading && n.FadeMaterialActive) || n.Fade >= 1f) return 0xFFFF;
        int parity = TileKey.Z(n.Packed) & 1, mask = 0;
        for (int y = 0; y < 4; y++)
            for (int x = 0; x < 4; x++)
            {
                float p = (Bayer(x, y) + 0.5f) / 16f;
                bool keep = parity == 0 ? p < n.Fade : p >= 1f - n.Fade;
                if (keep) mask |= 1 << (y * 4 + x);
            }
        return mask;
    }

    /// <summary>Every visible tile of the vector layer that holds the local point (x, z) (rect from the learned half extents per zoom).</summary>
    public List<TileNodes> TilesAt(float x, float z, Dictionary<int, float> halfX, Dictionary<int, float> halfZ)
    {
        var list = new List<TileNodes>();
        foreach (var n in _view.Cover.All)
        {
            int tz = TileKey.Z(n.Packed);
            if (!halfX.TryGetValue(tz, out float hx)) continue;
            var rp = n.Root.Position;
            if (System.Math.Abs(rp.X - x) > hx || System.Math.Abs(rp.Z - z) > halfZ[tz]) continue;
            list.Add(n);
        }
        list.Sort((a, b) => TileKey.Z(a.Packed).CompareTo(TileKey.Z(b.Packed)));
        return list;
    }

    /// <summary>Text for one screen point of a magenta region.</summary>
    public string Describe(in Snap s, float px, float py, int w, int h, Dictionary<int, float> halfX, Dictionary<int, float> halfZ, Camera3D cam)
    {
        var sb = new StringBuilder();
        double groundY = _view.CameraGroundHeight;
        if (!GroundPoint(s, px, py, w, h, (float)groundY, out float x, out float z, out float dist))
        {
            sb.Append($"  point ({px:0},{py:0}): ray at or above the horizon (sky; the scan region was wrong for this camera)\n");
            return sb.ToString();
        }
        sb.Append($"  point ({px:0},{py:0}) -> ground x={x:0.0} z={z:0.0} (range {dist:0} m, cam {s.Pos.X:0},{s.Pos.Y:0},{s.Pos.Z:0} fov {s.FovDeg:0.0})\n");
        var tiles = TilesAt(x, z, halfX, halfZ);
        _view.Streamer.DebugTiles(_dbg);
        var inFrustum = new HashSet<ulong>();
        foreach (var id in RenderingServer.InstancesCullConvex(cam.GetFrustum(), _view.GetWorld3D().Scenario)) inFrustum.Add((ulong)id);
        int covered = 0;
        for (int z0 = TileKey.RootZ; z0 <= TileKey.LeafZ; z0++)
        {
            bool held = false;
            foreach (var n in tiles) if (TileKey.Z(n.Packed) == z0) held = true;
            if (!held) sb.Append($"    z{z0}: no tile held here\n");
        }
        foreach (var n in tiles)
        {
            int mask = n.Vis == TileCover.Vis.Visible && n.HasGround && n.Root.IsVisibleInTree() ? KeepMask(n) : 0;
            covered |= mask;
            var gab = n.GroundInstance.GetAabb();
            var rp = n.Root.Position;
            string lib = "-";
            foreach (var d in _dbg) if (d.Layer == TileLayer.World && d.Packed == n.Packed) { lib = $"{d.State}/ring{d.Ring}"; break; }
            sb.Append($"    {n.Key} ring{n.Ring} vis={n.Vis} fading={n.Fading} fade={n.Fade:0.000} fadeMat={n.FadeMaterialActive} await={n.FadeAwait} shown={n.Shown} root={n.Root.Visible}/{n.Root.IsVisibleInTree()} ")
              .Append($"ground inst={n.GroundInstance.Visible} surf={n.GroundMesh.GetSurfaceCount()} tris={n.GroundTris} mesh inst={n.MeshInstance.Visible} surf={n.Mesh.GetSurfaceCount()} tris={n.Tris} ")
              .Append($"margin={n.GroundInstance.ExtraCullMargin:0.0} aabb=({rp.X + gab.Position.X:0},{rp.Z + gab.Position.Z:0})+({gab.Size.X:0},{gab.Size.Z:0}) y {gab.Position.Y:0.0}..{gab.End.Y:0.0} ")
              .Append($"engine-frustum ground={inFrustum.Contains(n.GroundInstance.GetInstanceId())} mesh={inFrustum.Contains(n.MeshInstance.GetInstanceId())} keep=0x{mask:X4} library={lib}\n");
        }
        sb.Append($"    dither mask kept by the stack: 0x{covered:X4}{(covered == 0xFFFF ? "" : "  <-- pixels nobody keeps")}\n");
        return sb.ToString();
    }

    /// <summary>What a magenta block means (the pixel is magenta because nothing drew it; why nothing did decides whether it is a bug).</summary>
    public enum Kind
    {
        /// <summary>Ray at or above the horizon, or beyond the data's reach (max distance): not tile ground.</summary>
        Sky,
        /// <summary>A visible tile holds the point but none of its triangles covers it: unmapped land (no landuse / road polygon; the scene's fallback plane shows there). Data, not a bug.</summary>
        Unmapped,
        /// <summary>A visible tile's triangle covers the point, the stack keeps pixels there, and the pixel is still magenta: the GPU did not draw what the host shows (cull, dither, material). A bug.</summary>
        DrawnButMagenta,
        /// <summary>No visible tile's rect holds the point at any zoom: a hole in the cover. A bug (unless the tile is still being fetched; the structural root check says that too).</summary>
        NoTile,
    }

    public sealed class Classified
    {
        public int Sky, Unmapped, DrawnButMagenta, NoTile, Samples;
        public readonly StringBuilder Detail = new();
        public bool Real => DrawnButMagenta > 0 || NoTile > 0;
        public override string ToString() => $"samples {Samples}: unmapped {Unmapped}, drawn-but-magenta {DrawnButMagenta}, no-tile {NoTile}, sky/out of reach {Sky}";
    }

    private readonly Dictionary<long, (Vector3[] v, int[] idx)> _arrays = new();

    private (Vector3[] v, int[] idx) Arrays(TileNodes n)
    {
        if (_arrays.TryGetValue(n.Packed, out var a)) return a;
        var arr = n.GroundMesh.SurfaceGetArrays(0);
        a = (arr[(int)Mesh.ArrayType.Vertex].AsVector3Array(), arr[(int)Mesh.ArrayType.Index].AsInt32Array());
        _arrays[n.Packed] = a;
        return a;
    }

    private static bool Covers((Vector3[] v, int[] idx) a, Vector3 off, float x, float z)
    {
        float px = x - off.X, pz = z - off.Z;
        var v = a.v;
        var idx = a.idx;
        for (int i = 0; i + 2 < idx.Length; i += 3)
        {
            var p0 = v[idx[i]]; var p1 = v[idx[i + 1]]; var p2 = v[idx[i + 2]];
            float d = (p1.Z - p2.Z) * (p0.X - p2.X) + (p2.X - p1.X) * (p0.Z - p2.Z);
            if (System.Math.Abs(d) < 1e-4f) continue;
            float l1 = ((p1.Z - p2.Z) * (px - p2.X) + (p2.X - p1.X) * (pz - p2.Z)) / d;
            float l2 = ((p2.Z - p0.Z) * (px - p2.X) + (p0.X - p2.X) * (pz - p2.Z)) / d;
            if (l1 >= -1e-4f && l2 >= -1e-4f && 1f - l1 - l2 >= -1e-4f) return true;
        }
        return false;
    }

    /// <summary>
    /// Classifies up to <paramref name="maxSamples"/> of the magenta blocks (evenly spread over the list; a block is 16 x 16 pixels, its centre is cast to the ground) against
    /// the host's own geometry: is there a triangle of a visible tile under it, and does the stack of tiles keep any pixel there.
    /// </summary>
    public Classified Classify(List<Vector2I> blocks, in Snap s, int w, int h, Dictionary<int, float> halfX, Dictionary<int, float> halfZ, int maxSamples)
    {
        var res = new Classified();
        _arrays.Clear();
        float reach = (float)(_view.LodEdgesM[3] > 0 ? _view.LodEdgesM[3] * 0.95 : 23000.0);
        int step = System.Math.Max(1, blocks.Count / maxSamples);
        for (int i = 0; i < blocks.Count; i += step)
        {
            var b = blocks[i];
            float px = (b.X + 2) * 4f, py = (b.Y + 2) * 4f;
            res.Samples++;
            if (!GroundPoint(s, px, py, w, h, (float)_view.CameraGroundHeight, out float x, out float z, out float dist) || dist > reach) { res.Sky++; continue; }
            var tiles = TilesAt(x, z, halfX, halfZ);
            int mask = 0, held = 0;
            foreach (var n in tiles)
            {
                if (n.Vis != TileCover.Vis.Visible || !n.Root.IsVisibleInTree()) continue;
                held++;
                if (!n.HasGround || n.GroundMesh.GetSurfaceCount() == 0) continue;
                if (Covers(Arrays(n), n.Root.Position, x, z)) mask |= KeepMask(n);
            }
            if (held == 0) { res.NoTile++; res.Detail.Append($"    no-tile at ({x:0},{z:0}) range {dist:0}\n"); }
            else if (mask != 0) { res.DrawnButMagenta++; res.Detail.Append($"    drawn-but-magenta at ({x:0},{z:0}) range {dist:0}, keep mask 0x{mask:X4}\n"); }
            else res.Unmapped++;
        }
        _arrays.Clear();
        return res;
    }
}
