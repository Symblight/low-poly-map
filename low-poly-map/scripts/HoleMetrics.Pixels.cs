using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

// The pixel test of --hole-pixels (see HoleMetrics): the background is magenta, the terrain and the fallback plane are off, so magenta below the horizon is a
// pixel no vector tile drew. The image it reads is the last frame the RENDERER finished, which is not the frame being processed:
//   - the camera that rendered it is looked up in a ring of per-draw snapshots (HolePixelProbe), keyed by Engine.GetFramesDrawn();
//   - a process frame without a new draw (macOS does not draw an occluded window, the scene may be unchanged) reads the same stale image: it is skipped and counted,
//     never compared against the moved camera (that read the sky as ground and produced hundreds of "hole" frames with a window behind another one).
public sealed partial class HoleMetrics
{
    private int _pxFrames, _pxBad, _pxWorst, _pxStale, _pxNoSnap, _pxEpisodes, _pxProcessed, _pxGaps, _pxDataGap;
    private long _lastDrawn = -1;
    private bool _lastBad, _snapOk;
    private HolePixelProbe.Snap _snap;
    private Image? _full, _reduced;
    private int _legacyScanned, _legacyBad;
    private readonly List<int> _legacyRecent = new();
    private readonly List<string> _pxLog = new();
    private readonly List<int> _pxRecent = new();
    private string _pxDir = "";
    private HolePixelProbe _probe = null!;

    /// <summary>Magenta frames, or too few drawn frames to say anything (a minimized / occluded window is not drawn by macOS: the run proves nothing and must not pass).</summary>
    public int PixelProblems => Pixels && (_pxBad > 0 || _pxFrames < MinScannedFrames) ? 1 : 0;
    public const int MinScannedFrames = 100;

    private void InitPixels()
    {
        _probe = new HolePixelProbe(_view);
        _pxDir = ProjectSettings.GlobalizePath("user://hole_pixels");
        if (Pixels) DirAccess.MakeDirRecursiveAbsolute(_pxDir);
    }

    public void SetupPixels()
    {
        if (!Pixels) return;
        var env = _view.GetViewport().FindWorld3D()?.Environment;
        if (env == null) return;
        env.BackgroundMode = Godot.Environment.BGMode.Color;
        env.BackgroundColor = new Color(1f, 0f, 1f);
        env.GlowEnabled = false;
    }

    private void PixelCheck(double t, string events)
    {
        SetupPixels();
        var cam = _view.Camera;
        if (cam == null) return;
        long drawn = (long)Engine.GetFramesDrawn();
        _pxProcessed++;
        try
        {
            bool fresh = drawn != _lastDrawn; // false: nothing was drawn since the last check, the image is the one already read
            if (fresh)
            {
                if (_lastDrawn >= 0 && drawn - _lastDrawn > 1) _pxGaps++; // several draws between two process frames: the image is the newest one, its snapshot is still exact
                _lastDrawn = drawn;
                _snapOk = _probe.TryGet(drawn - 1, out _snap);
                if (!_snapOk) _pxNoSnap++;
                _full = _view.GetViewport().GetTexture().GetImage();
                _reduced = (Image)_full.Duplicate();
                _reduced.Resize(_full.GetWidth() / 4, _full.GetHeight() / 4, Image.Interpolation.Nearest);
                if (_reduced.GetFormat() != Image.Format.Rgba8) _reduced.Convert(Image.Format.Rgba8);
            }
            else _pxStale++;
            if (_reduced == null) return;
            if (t < 15 || t < IgnoreUntilT) return;
            // After an abrupt change of direction / fov the picture changes wholesale (data edge, horizon): restart the baselines instead of calling it a hole.
            if (HoleFlight.ViewChangeClock > 0 && t - HoleFlight.ViewChangeClock < 1.5 || _view.Camera is FreeFlyCamera && Input.IsPhysicalKeyPressed(Key.Z)) { _pxRecent.Clear(); _legacyRecent.Clear(); return; }
            Legacy(cam);
            if (fresh && _snapOk) Checked(t, events, cam, drawn);
        }
        finally { _probe.Record(drawn, cam); }
    }

    /// <summary>First row (reduced image) of the region 5 degrees under the horizon for a camera with this pitch / fov; -1 when the view is too level.</summary>
    private int ScanRow(float pitch, float fovRad, int imageH, int reducedH)
    {
        if (-pitch - Mathf.DegToRad(5f) <= Mathf.DegToRad(2f)) return -1; // too level: the far edge of the data (about 1 deg under the horizon) would show
        // screen row of the ray 5 degrees under the horizon: y = h/2 - tan(e - pitch) / tan(fov/2) * h/2, e = -5 deg
        float cy = imageH * 0.5f - Mathf.Tan(Mathf.DegToRad(-5f) - pitch) / Mathf.Tan(fovRad * 0.5f) * imageH * 0.5f;
        return Mathf.Clamp((int)cy / 4, 0, reducedH - 1);
    }

    /// <summary>Solid magenta blocks (4 x 4 reduced pixels, all magenta) from row <paramref name="rows0"/>: cracks between ground polygons and road edges are thin slivers and never fill a block, a missing tile fills hundreds.</summary>
    private int CountBlocks(Image img, int rows0, List<Vector2I>? blocks)
    {
        int iw = img.GetWidth(), ih = img.GetHeight(), bad = 0;
        var data = img.GetData();
        bool Mag(int x, int y)
        {
            int o = (y * iw + x) * 4; // RGBA8 (converted when the viewport gives something else)
            return data[o] > 127 && data[o + 2] > 127 && data[o + 1] < 0.3f * data[o];
        }
        for (int by = rows0; by + 4 <= ih; by += 4)
            for (int bx = 0; bx + 4 <= iw; bx += 4)
            {
                bool solid = true;
                for (int yy = 0; yy < 4 && solid; yy++)
                    for (int xx = 0; xx < 4; xx++)
                        if (!Mag(bx + xx, by + yy)) { solid = false; break; }
                if (!solid) continue;
                bad++;
                blocks?.Add(new Vector2I(bx, by));
            }
        return bad;
    }

    /// <summary>A spike over the median of the last second (the data ends somewhere; magenta beyond it is legitimate).</summary>
    private static bool Spike(List<int> recent, int bad)
    {
        recent.Add(bad);
        if (recent.Count > 60) recent.RemoveAt(0);
        var sorted = new List<int>(recent);
        sorted.Sort();
        return recent.Count >= 20 && bad >= sorted[sorted.Count / 2] * 2 + 40;
    }

    /// <summary>What the first version of this test counted, kept for the A/B in the summary: every process frame, the newest image (stale or not) against the CURRENT camera.</summary>
    private void Legacy(Camera3D cam)
    {
        var f = -cam.GlobalTransform.Basis.Z;
        int rows0 = ScanRow(Mathf.Asin(f.Y), Mathf.DegToRad(cam.Fov), _full!.GetHeight(), _reduced!.GetHeight());
        if (rows0 < 0) return;
        _legacyScanned++;
        if (Spike(_legacyRecent, CountBlocks(_reduced, rows0, null))) _legacyBad++;
    }

    private void Checked(double t, string events, Camera3D cam, long drawn)
    {
        var snap = _snap;
        int w = _full!.GetWidth(), h = _full.GetHeight();
        float pitch = Mathf.Asin(-snap.B.Z.Y); // the camera that rendered this image, not the current one
        int rows0 = ScanRow(pitch, Mathf.DegToRad(snap.FovDeg), h, _reduced!.GetHeight());
        if (rows0 < 0) return;
        var blocks = new List<Vector2I>();
        int bad = CountBlocks(_reduced, rows0, blocks);
        _pxFrames++;
        int median = _pxRecent.Count > 0 ? Median(_pxRecent) : 0;
        if (!Spike(_pxRecent, bad)) { _lastBad = false; return; }
        // A spike of magenta is not yet a hole: land no polygon covers (parking lots, fields between roads: the scene's fallback plane shows there, and this test hides it)
        // looks the same. The block centres are cast to the ground and checked against the triangles the host holds.
        var cls = _probe.Classify(blocks, snap, w, h, _halfX, _halfZ, 12);
        if (!cls.Real)
        {
            _pxDataGap++;
            if (_pxDataGap <= 3) GD.Print($"[PixelHole] t={t:0.00}s f={_frame} {bad} magenta blocks are unmapped land (no tile triangle under them), not a hole: {cls}");
            _lastBad = false;
            return;
        }
        _pxBad++;
        if (bad > _pxWorst) _pxWorst = bad;
        bool first = !_lastBad;
        _lastBad = true;
        if (!first || _pxEpisodes >= 12) return;
        _pxEpisodes++;
        var sb = new StringBuilder();
        sb.Append($"t={t:0.00}s f={_frame} draw={drawn} solid magenta blocks (4x4 px) {bad} vs median {median}; scan from row {rows0 * 4}/{h}, camera pitch {Mathf.RadToDeg(pitch):0.0} fov {snap.FovDeg:0.0}; events: {events}\n");
        sb.Append($"  classified: {cls}\n").Append(cls.Detail);
        sb.Append(DescribeBlocks(blocks, snap, w, h, cam));
        _pxLog.Add(sb.ToString());
        GD.Print("[PixelHole] " + sb);
        SavePng((Image)_full.Duplicate(), blocks, rows0 * 4, $"{_pxDir}/hole_{_pxEpisodes}_f{_frame}.png");
    }

    private static int Median(List<int> l)
    {
        var s = new List<int>(l);
        s.Sort();
        return s[s.Count / 2];
    }

    private string DescribeBlocks(List<Vector2I> blocks, in HolePixelProbe.Snap snap, int w, int h, Camera3D cam)
    {
        // blocks are in reduced coordinates (1/4); a block is 16 x 16 screen pixels. Samples: nearest (lowest on screen), farthest (highest), and the middle one.
        var near = blocks[0]; var far = blocks[0];
        foreach (var b in blocks) { if (b.Y > near.Y) near = b; if (b.Y < far.Y) far = b; }
        var mid = blocks[blocks.Count / 2];
        var sb = new StringBuilder();
        foreach (var b in new[] { near, far, mid })
            sb.Append(_probe.Describe(snap, (b.X + 2) * 4f, (b.Y + 2) * 4f, w, h, _halfX, _halfZ, cam));
        return sb.ToString();
    }

    private static void SavePng(Image full, List<Vector2I> blocks, int y0, string path)
    {
        var cyan = new Color(0f, 1f, 1f);
        foreach (var b in blocks)
        {
            int x = b.X * 4, y = b.Y * 4;
            for (int i = 0; i < 16 && x + i < full.GetWidth(); i++) { full.SetPixel(x + i, y, cyan); if (y + 15 < full.GetHeight()) full.SetPixel(x + i, y + 15, cyan); }
        }
        for (int x = 0; x < full.GetWidth(); x += 2) full.SetPixel(x, y0, new Color(1f, 1f, 0f));
        full.SavePng(path);
    }

    private void PixelSummary(StringBuilder sb)
    {
        if (!Pixels) return;
        sb.Append($"pixels (legacy test = every process frame against the current camera: {_legacyBad} of {_legacyScanned} frames flagged): {_pxFrames} frames scanned of {_pxProcessed} processed ({_pxStale} without a new draw, {_pxNoSnap} without a camera snapshot, {_pxGaps} with several draws), ")
          .Append($"{_pxDataGap} magenta spikes were unmapped land (no triangle under them, legitimate), {_pxBad} frames with magenta where the host holds a drawn triangle or no tile at all, in {_pxEpisodes} episodes, worst {_pxWorst} blocks; PNGs in {_pxDir}\n");
        if (_pxFrames < MinScannedFrames) sb.Append($"  INVALID RUN: fewer than {MinScannedFrames} drawn frames were scanned (window minimized / occluded, or too short a run)\n");
        foreach (var l in _pxLog) sb.Append("  ").Append(l).Append('\n');
    }
}
