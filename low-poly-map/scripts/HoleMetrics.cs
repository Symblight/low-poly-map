using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// The flicker half of <see cref="HoleDetector"/> (one instance, called once per frame): (1) per-tile drawn toggles of the vector and terrain layer (a change less than
/// 1 s after the previous change of the same key is a quick toggle; counted per second, worst keys listed), (2) frames in which the ground point under the screen
/// centre is covered by no vector tile that has ground (duration histogram), (3) heights at a grid around the camera once a second: the drawn terrain surface
/// against <c>ground_height_at</c> (terrain above the ground would hide the city: hypothesis B) and against the drawn vector ground (z-fighting when they are within depth
/// precision), and (4) with <c>--hole-pixels</c> a pixel test: the background is magenta, the terrain and fallback plane are off, so any magenta below the horizon
/// is a pixel where no vector tile was drawn, whatever the host state says (it catches what the GPU did, e.g. dither materials that discard everything).
/// </summary>
public sealed partial class HoleMetrics
{
    private sealed class Tog { public bool Drawn; public double Last = double.NegativeInfinity; public int Quick, Changes; public long Seen; public readonly List<string> Hist = new(); }

    private readonly WorldView _view;
    private readonly List<string> _quickLog = new();
    private readonly Dictionary<long, Tog> _vec = new(), _ter = new();
    private readonly Dictionary<int, float> _halfX = new(), _halfZ = new();
    private long _frame;
    private int _quickSec, _quickTotal, _quickPeakSec, _quickSteady;
    private double _secStart;

    // centre coverage
    private int _gapRun, _gapEvents, _gapFrames, _gapNoTile, _gapNoGround, _gapFadeLow;
    private readonly int[] _gapHist = new int[4];
    private int _gapMax;
    public int CenterFrames;

    // heights
    private int _hSamples, _hTerrainAbove, _hZ005, _hZ025, _hZ1, _hBothMissing;
    private float _hMaxAbove = float.NegativeInfinity, _hMinSep = float.PositiveInfinity;
    private readonly List<float> _sep = new();

    // pixels
    public bool Pixels { get; }
    public double IgnoreUntilT;

    public int Problems => PixelProblems;

    public HoleMetrics(WorldView view)
    {
        _view = view;
        Pixels = System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--hole-pixels");
        InitPixels();
    }

    public void LearnRects(List<DebugTile> dbg)
    {
        foreach (var d in dbg)
        {
            if (d.Layer != TileLayer.World || d.Packed == 0) continue;
            int z = TileKey.Z(d.Packed);
            _halfX[z] = (d.Max.X - d.Min.X) * 0.5f;
            _halfZ[z] = (d.Max.Y - d.Min.Y) * 0.5f;
        }
    }

    // ---- (1) toggles ---------------------------------------------------------------------------

    private string _events = "";

    private void Track(TileCover c, Dictionary<long, Tog> map, string layer, double t)
    {
        foreach (var n in c.All)
        {
            bool drawn = n.Vis == TileCover.Vis.Visible && n.Root.Visible && (!n.Fading || n.Fade > 0.02f); // a tile at fade ~0 draws nothing: its showing / hiding is not a flicker
            if (!map.TryGetValue(n.Packed, out var g)) { map[n.Packed] = g = new Tog { Drawn = drawn }; }
            g.Seen = _frame;
            if (g.Drawn == drawn) continue;
            g.Drawn = drawn;
            g.Changes++;
            if (t - g.Last < 1.0)
            {
                g.Quick++;
                _quickSec++;
                _quickTotal++;
                // The first fill after the start, a teleport or a reconfigure shows tiles and takes them back within a second by design: only steady flight counts.
                if (t > 15 && t > IgnoreUntilT) _quickSteady++;
                if (t > 15 && t > IgnoreUntilT && _quickLog.Count < 12) _quickLog.Add($"{layer} {(layer == "terrain" ? TileKey.ToTerrainText(n.Packed) : TileKey.ToText(n.Packed))} ring {n.Ring} now {(drawn ? "shown" : "hidden")} vis={n.Vis} fading={n.Fading} fade={n.Fade:0.00}; before: {string.Join(" | ", g.Hist)}; frame {_frame} events: {_events}");
            }
            g.Last = t;
            if (g.Hist.Count >= 8) g.Hist.RemoveAt(0);
            g.Hist.Add($"t={t:0.00} {(drawn ? "shown" : "hidden")} vis={n.Vis} fading={n.Fading} fade={n.Fade:0.00}");
        }
    }

    // ---- (2) centre ground ---------------------------------------------------------------------

    private void CenterGround(double t)
    {
        var cam = _view.Camera;
        if (cam == null) return;
        var p = cam.GlobalPosition;
        var f = -cam.GlobalTransform.Basis.Z;
        if (f.Y > -0.05f) return; // looking at or above the horizon: no ground point
        float s = ((float)(p.Y - _view.CameraGroundHeight)) / -f.Y;
        if (s > 20000f) return;
        float x = p.X + f.X * s, z = p.Z + f.Z * s;
        CenterFrames++;
        float weight = 0f;
        int tilesHere = 0, withGround = 0;
        foreach (var n in _view.Cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible) continue;
            int tz = TileKey.Z(n.Packed);
            if (!_halfX.TryGetValue(tz, out float hx)) continue;
            var rp = n.Root.Position;
            if (System.Math.Abs(rp.X - x) > hx || System.Math.Abs(rp.Z - z) > _halfZ[tz]) continue;
            tilesHere++;
            if (!n.HasGround) continue;
            withGround++;
            weight += n.Fading ? n.Fade : 1f;
        }
        bool gap = weight < 0.9f && t > IgnoreUntilT;
        if (gap)
        {
            _gapRun++;
            _gapFrames++;
            if (_gapRun == 1)
            {
                _gapEvents++;
                if (tilesHere == 0) _gapNoTile++; else if (withGround == 0) _gapNoGround++; else _gapFadeLow++;
            }
            if (_gapRun > _gapMax) _gapMax = _gapRun;
        }
        else if (_gapRun > 0)
        {
            _gapHist[_gapRun == 1 ? 0 : _gapRun <= 5 ? 1 : _gapRun <= 30 ? 2 : 3]++;
            _gapRun = 0;
        }
    }

    // ---- (3) heights ---------------------------------------------------------------------------

    internal static bool Hit(Godot.Collections.Array arrays, Vector3 off, float x, float z, out float y)
    {
        y = float.NegativeInfinity;
        if (arrays.Count <= (int)Mesh.ArrayType.Index) return false;
        var v = arrays[(int)Mesh.ArrayType.Vertex].AsVector3Array();
        var idx = arrays[(int)Mesh.ArrayType.Index].AsInt32Array();
        float px = x - off.X, pz = z - off.Z;
        bool hit = false;
        for (int i = 0; i + 2 < idx.Length; i += 3)
        {
            var a = v[idx[i]]; var b = v[idx[i + 1]]; var c = v[idx[i + 2]];
            float d = (b.Z - c.Z) * (a.X - c.X) + (c.X - b.X) * (a.Z - c.Z);
            if (System.Math.Abs(d) < 1e-4f) continue; // vertical (skirt) or degenerate
            float l1 = ((b.Z - c.Z) * (px - c.X) + (c.X - b.X) * (pz - c.Z)) / d;
            float l2 = ((c.Z - a.Z) * (px - c.X) + (a.X - c.X) * (pz - c.Z)) / d;
            float l3 = 1f - l1 - l2;
            if (l1 < -1e-4f || l2 < -1e-4f || l3 < -1e-4f) continue;
            float yy = l1 * a.Y + l2 * b.Y + l3 * c.Y + off.Y;
            if (yy > y) y = yy;
            hit = true;
        }
        return hit;
    }

    private bool TerrainY(float x, float z, out float y)
    {
        y = float.NegativeInfinity;
        foreach (var n in _view.TerrainCover.All)
        {
            if (n.Vis != TileCover.Vis.Visible || !n.HasMesh) continue;
            var ab = n.Mesh.GetAabb();
            var rp = n.Root.Position;
            if (x < rp.X + ab.Position.X || x > rp.X + ab.End.X || z < rp.Z + ab.Position.Z || z > rp.Z + ab.End.Z) continue;
            if (Hit(n.Mesh.SurfaceGetArrays(0), rp, x, z, out float ty) && ty > y) y = ty;
        }
        return y > float.NegativeInfinity;
    }

    private bool VectorGroundY(float x, float z, out float y)
    {
        y = float.NegativeInfinity;
        foreach (var n in _view.Cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible || !n.HasGround) continue;
            int tz = TileKey.Z(n.Packed);
            if (!_halfX.TryGetValue(tz, out float hx)) continue;
            var rp = n.Root.Position;
            if (System.Math.Abs(rp.X - x) > hx || System.Math.Abs(rp.Z - z) > _halfZ[tz]) continue;
            if (Hit(n.GroundMesh.SurfaceGetArrays(0), rp, x, z, out float gy) && gy > y) y = gy;
        }
        return y > float.NegativeInfinity;
    }

    /// <summary>A grid of 11 x 11 points within +-1.5 km of the camera, once a second.</summary>
    private void Heights()
    {
        var cam = _view.Camera?.GlobalPosition ?? Vector3.Zero;
        for (int i = -5; i <= 5; i++)
            for (int j = -5; j <= 5; j++)
            {
                float x = cam.X + i * 300f + 17f, z = cam.Z + j * 300f + 23f;
                if (!TerrainY(x, z, out float ty)) continue;
                float baseY = (float)_view.Streamer.GroundHeightAt(x, z);
                _hSamples++;
                float above = ty - baseY;
                if (above > _hMaxAbove) _hMaxAbove = above;
                if (above > 0.2f) _hTerrainAbove++;
                if (!VectorGroundY(x, z, out float vy)) { _hBothMissing++; continue; }
                float sep = vy - ty;
                _sep.Add(sep);
                float a = System.Math.Abs(sep);
                if (a < _hMinSep) _hMinSep = a;
                if (a < 0.05f) _hZ005++;
                if (a < 0.25f) _hZ025++;
                if (a < 1f) _hZ1++;
            }
    }

    // ---- per frame ----------------------------------------------------------------------------

    public void Frame(double t, long frame, string recentEvents)
    {
        _frame = frame;
        _events = recentEvents;
        if (t - _secStart >= 1.0)
        {
            if (_quickSec > _quickPeakSec) _quickPeakSec = _quickSec;
            _quickSec = 0;
            _secStart = t;
            Heights();
            // forget keys that left the covers
            var gone = new List<long>();
            foreach (var kv in _vec) if (kv.Value.Seen != frame) gone.Add(kv.Key);
            foreach (var k in gone) _vec.Remove(k);
            gone.Clear();
            foreach (var kv in _ter) if (kv.Value.Seen != frame) gone.Add(kv.Key);
            foreach (var k in gone) _ter.Remove(k);
        }
        Track(_view.Cover, _vec, "world", t);
        if (_view.TerrainActive) Track(_view.TerrainCover, _ter, "terrain", t);
        CenterGround(t);
        if (Pixels) PixelCheck(t, recentEvents);
    }

    public string Summary(double seconds)
    {
        var sb = new StringBuilder();
        sb.Append($"quick toggles (a tile drawn/hidden again < 1 s after its last change): {_quickTotal} total ({_quickPeakSec} in the worst second), of which {_quickSteady} in steady flight (not within 15 s of the start or 12 s of a teleport / reconfigure)\n");
        foreach (var q in _quickLog) sb.Append("  quick toggle: ").Append(q).Append('\n');
        sb.Append($"centre ground point (screen centre ray, {CenterFrames} frames): {_gapEvents} uncovered episodes, {_gapFrames} frames (longest {_gapMax}); 1 frame {_gapHist[0]}, 2-5 {_gapHist[1]}, 6-30 {_gapHist[2]}, >30 {_gapHist[3]}; kinds: no visible tile {_gapNoTile}, tile without ground {_gapNoGround}, fade low {_gapFadeLow}\n");
        _sep.Sort();
        string med = _sep.Count > 0 ? $"{_sep[_sep.Count / 2]:0.00}" : "-";
        sb.Append($"heights ({_hSamples} points under the drawn terrain, {_sep.Count} also under a drawn vector ground mesh): terrain above ground_height_at by > 0.2 m at {_hTerrainAbove} points (max {(_hSamples > 0 ? _hMaxAbove.ToString("0.00") : "-")} m); vector ground minus terrain: median {med} m, min |sep| {(_sep.Count > 0 ? _hMinSep.ToString("0.000") : "-")} m, |sep| < 5 cm {_hZ005}, < 25 cm {_hZ025}, < 1 m {_hZ1}\n");
        PixelSummary(sb);
        return sb.ToString();
    }
}
