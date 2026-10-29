using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// <c>--hole-check[=&lt;seconds&gt;]</c>: finds tile-shaped holes of the vector layer (ground and buildings missing where the terrain shows through) while the world
/// runs. Every frame, for each vector tile the host calls visible (<see cref="TileCover.Vis.Visible"/>) it checks that the tile can really draw: the node is
/// visible in the tree, the child meshes agree with the delivery (a mesh with triangles has a surface and a visible instance), a dither fade is not stuck
/// at ~0, a freshly uploaded tile is not held back in AwaitingFade, and the tile is not empty (no building, ground or light) although it is close to the camera
/// or an earlier delivery of the same key had content (an empty re-delivery). Every 0.5 s it also compares the host's tile map with <c>debug_tiles</c> (same keys,
/// same rect centre as the node's position) and the cover consistency counter. A tile failing for more than <see cref="FrameLimit"/> frames (or the set / position
/// checks for 3 samples) is reported once as <c>[HOLE]</c> with its delivery history; a summary line <c>HOLE CHECK PASS/FAIL</c> is printed on exit (exit code 0 / 1).
/// Pair it with <see cref="HoleFlight"/> (<c>--hole-flight</c>) for an unattended mixed flight.
/// </summary>
public partial class HoleDetector : Node
{
    public const int FrameLimit = 10;
    /// <summary>A failure must also last this long (s): a headless run renders 150 frames a second.</summary>
    public const double TimeLimitS = 0.4;
    private const double SampleEveryS = 0.5;
    /// <summary>Empty tiles of the first rings within this horizontal distance (m) of the camera are suspect.</summary>
    private const float NearEmptyM = 1500f;

    public static bool Requested(out double seconds)
    {
        seconds = 0;
        foreach (var a in OS.GetCmdlineUserArgs())
        {
            if (a == "--hole-check") return true;
            if (a.StartsWith("--hole-check=", System.StringComparison.Ordinal))
            {
                double.TryParse(a.Substring(13), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out seconds);
                return true;
            }
        }
        return false;
    }

    public static HoleDetector Attach(WorldView view, Node? parent, double seconds)
    {
        var d = new HoleDetector { Name = "HoleDetector", _view = view, _quitAfterS = seconds };
        (parent ?? view).CallDeferred(Node.MethodName.AddChild, d);
        return d;
    }

    private sealed class Hist
    {
        public readonly List<string> Events = new(8);
        public int LastTris = -1, LastGround = -1, LastLights = -1;
        public int Deliveries, Redeliveries;
        public void Add(string e) { if (Events.Count >= 12) Events.RemoveAt(0); Events.Add(e); }
    }

    private sealed class Episode { public int Frames; public double StartT; public bool Reported; public string Kind = ""; }

    private WorldView _view = null!;
    private double _quitAfterS;
    private double _t, _sample;
    private long _frame;
    private readonly Dictionary<string, Hist> _hist = new();
    private readonly Dictionary<long, Episode> _episodes = new();
    private readonly Dictionary<string, int> _setBad = new();
    private readonly List<DebugTile> _dbg = new(512);
    private readonly Dictionary<long, DebugTile> _dbgByKey = new();
    private readonly List<long> _scratch = new();
    private readonly HashSet<long> _seen = new();
    private int _holes, _emptyRedeliveries, _lastConsistency;
    private readonly Dictionary<string, int> _kinds = new();
    private long _checkedTiles, _emptyVisibleFrames;
    private bool _done;
    private HoleMetrics _metrics = null!;
    private readonly StringBuilder _events = new();
    private readonly Queue<string> _recent = new();
    private double _lastSlotLog;

    public override void _Ready()
    {
        ProcessPriority = 100; // after the view and the camera
        _metrics = new HoleMetrics(_view);
        _view.VectorTileUploaded += OnUploaded;
        _view.VectorTileUnloading += OnUnloading;
        GD.Print($"[HoleCheck] on; limit {FrameLimit} frames{(_quitAfterS > 0 ? $", quits after {_quitAfterS:0} s" : "")}");
    }

    private Hist H(string key)
    {
        if (!_hist.TryGetValue(key, out var h)) _hist[key] = h = new Hist();
        return h;
    }

    private string Stamp() => $"t={_t:0.00}s f={_frame}";

    private void OnUploaded(TileNodes n, bool replaced)
    {
        var h = H(n.Key);
        if (replaced) h.Redeliveries++; else h.Deliveries++;
        string ev = $"{Stamp()} {(replaced ? "REDELIVER" : "deliver")} tris={n.Tris} ground={n.GroundTris} lights={n.LightCount} street={n.StreetLightCount}";
        h.Add(ev);
        Note($"{(replaced ? "re" : "+")}{n.Key}");
        bool hadContent = h.LastTris > 0 || h.LastGround > 0;
        bool empty = n.Tris == 0 && n.GroundTris == 0;
        if (replaced && empty && hadContent)
        {
            _emptyRedeliveries++;
            Report(n, "empty-redelivery", $"previous delivery had tris={h.LastTris} ground={h.LastGround}, this one is empty");
        }
        h.LastTris = n.Tris;
        h.LastGround = n.GroundTris;
        h.LastLights = n.LightCount;
    }

    private void Note(string e) { _recent.Enqueue($"{_frame}:{e}"); while (_recent.Count > 24) _recent.Dequeue(); }

    private void OnUnloading(string key, TileNodes? n) { Note("-" + key); H(key).Add($"{Stamp()} unload held={(n != null)} vis={n?.Vis} fading={n?.Fading}"); }

    public override void _Process(double delta)
    {
        if (_done) return;
        _t += delta;
        _frame++;
        if (!_view.Streamer.IsAvailable) return;
        CheckTiles();
        CheckEngineCulling(delta);
        if (HoleFlight.DisruptionClock > 0) _metrics.IgnoreUntilT = System.Math.Max(_metrics.IgnoreUntilT, HoleFlight.DisruptionClock + 12.0); // a teleport or reconfigure starts a first fill
        _metrics.Frame(_t, _frame, string.Join(' ', _recent));
        _sample += delta;
        if (_sample >= SampleEveryS) { _sample = 0; CheckSets(); _metrics.LearnRects(_dbg); }
        if (_t - _lastSlotLog >= 5) { _lastSlotLog = _t; GD.Print($"[HoleCheck] t={_t:0}s vector pool {_view.PoolCreated} sets, terrain pool {_view.TerrainPoolCreated} sets"); }
        if (_quitAfterS > 0 && _t >= _quitAfterS) Finish();
    }

    /// <summary>How much of tile <paramref name="k"/>'s area is drawn by it and what lies below it: a visible tile counts its fade (1 when not fading), otherwise the mean of its children.</summary>
    private float Drawn(long k)
    {
        if (_view.Cover.TryGet(k, out var t) && t.Vis == TileCover.Vis.Visible) return t.Fading ? t.Fade : 1f;
        if (TileKey.Z(k) >= TileKey.LeafZ) return 0f;
        float sum = 0f;
        for (int q = 0; q < 4; q++) sum += Drawn(TileKey.Child(k, q));
        return sum * 0.25f;
    }

    private float Cov(long k)
    {
        float c = 0f;
        if (_view.Cover.TryGet(k, out var t) && t.Vis == TileCover.Vis.Visible) c = t.Fading ? t.Fade : 1f;
        if (TileKey.Z(k) < TileKey.LeafZ)
        {
            float sum = 0f;
            for (int q = 0; q < 4; q++) sum += Cov(TileKey.Child(k, q));
            c += sum * 0.25f;
        }
        return c;
    }

    private readonly HashSet<long> _roots = new();

    /// <summary>Every root zoom tile that has anything uploaded below it must be drawn completely in every frame (complementary fades sum to 1).</summary>
    private void CheckRoots()
    {
        _roots.Clear();
        foreach (var n in _view.Cover.All) _roots.Add(TileKey.Ancestor(n.Packed, TileKey.RootZ));
        foreach (long r in _roots)
        {
            float c = Cov(r);
            int mask = Mask(r);
            _maskChecks++;
            if (c >= 0.97f && mask == 0xFFFF) { _rootGap.Remove(r); continue; }
            _rootGap.TryGetValue(r, out int f);
            _rootGap[r] = f + 1;
            string kind = c < 0.97f ? "root-not-covered" : "root-dither-gap";
            if (f == 0) { _holes++; Count(kind); GD.Print($"[HOLE] {Stamp()} {kind} {TileKey.ToText(r)}: drawn {c * 100f:0}% of its area, dither mask 0x{mask:X4}; recent events: {string.Join(' ', _recent)}"); }
        }
    }

    private readonly Dictionary<long, int> _rootGap = new();
    private string _lightsWhy = "";

    private void CheckTiles()
    {
        CheckRoots();
        var cover = _view.Cover;
        var cam = _view.Camera?.GlobalPosition ?? Vector3.Zero;
        _seen.Clear();
        foreach (var n in cover.All)
        {
            if (n.Vis == TileCover.Vis.HiddenByChildren)
            {
                // A parent the rule hides behind its children: they must draw the area (weights of a swap sum to 1). The 1.0 entry of a refine's outgoing parent used to hide it
                // while its children were still at fade 0: the whole tile blank for up to fades.cap_s.
                float cov = 0f;
                for (int q = 0; q < 4; q++) cov += Drawn(TileKey.Child(n.Packed, q));
                if (cov * 0.25f < 0.9f) Flag(n, "children-draw-too-little", $"children draw {cov * 25f:0}% of the area");
                continue;
            }
            if (n.Vis != TileCover.Vis.Visible && n.Vis != TileCover.Vis.AwaitingFade) continue;
            string kind = "";
            if (n.Vis == TileCover.Vis.AwaitingFade)
            {
                kind = "stuck-awaiting-fade";
            }
            else
            {
                _checkedTiles++;
                var p = n.Root.Position;
                float dist = new Vector2(p.X - cam.X, p.Z - cam.Z).Length();
                if (!n.Root.IsVisibleInTree()) kind = "root-not-visible-in-tree";
                else if (n.HasMesh && (!n.MeshInstance.Visible || n.Mesh.GetSurfaceCount() == 0)) kind = "building-mesh-not-drawn";
                else if (n.HasGround && (!n.GroundInstance.Visible || n.GroundMesh.GetSurfaceCount() == 0)) kind = "ground-mesh-not-drawn";
                else if (!_view.LightsDrawn(n, out string lightsWhy)) { kind = "lights-not-drawn"; _lightsWhy = lightsWhy; }
                else if (n.Fading && n.Fade <= 0.02f) kind = "fade-stuck-at-0";
                else if (n.Fading && n.Fade < 1f && n.FadeMaterialActive == false) kind = "fading-without-dither-material";
                else if (!n.Fading && n.FadeMaterialActive) kind = "dither-material-after-fade";
                else if (n.Fading && n.Fade >= 1f && n.FadeMaterialActive) kind = "dither-material-at-fade-1";
                else if (n.Tris == 0 && n.GroundTris == 0 && n.Ring <= 2 && dist < NearEmptyM && NeighboursHaveContent(n)) _emptyVisibleFrames++; // informational: the archive has tiles without any data (fields), both v1 and v2 deliver them empty
            }
            if (kind.Length == 0) continue;
            _seen.Add(n.Packed);
            if (!_episodes.TryGetValue(n.Packed, out var ep) || ep.Kind != kind) _episodes[n.Packed] = ep = new Episode { Kind = kind, StartT = _t };
            ep.Frames++;
            // A visible tile that draws nothing for even one frame is a bug; only the tolerant kinds wait.
            bool strict = kind != "fade-stuck-at-0" && kind != "empty-near-camera" && kind != "stuck-awaiting-fade";
            double limitS = kind == "fade-stuck-at-0" ? 1.5 : TimeLimitS; // the fade cap is 0.75 s
            if (strict ? ep.Frames >= 1 && !ep.Reported : ep.Frames > FrameLimit && _t - ep.StartT > limitS && !ep.Reported)
            {
                ep.Reported = true;
                Report(n, kind, $"for {ep.Frames} frames / {_t - ep.StartT:0.00} s{(kind == "lights-not-drawn" ? ": " + _lightsWhy : "")}");
            }
        }
        if (_episodes.Count > 0)
        {
            _scratch.Clear();
            foreach (var k in _episodes.Keys) if (!_seen.Contains(k)) _scratch.Add(k);
            foreach (var k in _scratch) _episodes.Remove(k);
        }
    }

    /// <summary>An empty tile is only suspect inside a built-up area: at least two of its four edge neighbours (same zoom) hold ground or buildings.</summary>
    private bool NeighboursHaveContent(TileNodes n)
    {
        int z = TileKey.Z(n.Packed), x = TileKey.X(n.Packed), y = TileKey.Y(n.Packed), withContent = 0;
        Span4(z, x + 1, y, ref withContent);
        Span4(z, x - 1, y, ref withContent);
        Span4(z, x, y + 1, ref withContent);
        Span4(z, x, y - 1, ref withContent);
        return withContent >= 2;
    }

    private void Span4(int z, int x, int y, ref int count)
    {
        if (_view.Cover.TryGet(TileKey.Pack(z, x, y), out var o) && (o.Tris > 0 || o.GroundTris > 0)) count++;
    }

    private void Flag(TileNodes n, string kind, string detail)
    {
        if (!_episodes.TryGetValue(n.Packed, out var ep) || ep.Kind != kind) _episodes[n.Packed] = ep = new Episode { Kind = kind, StartT = _t };
        _seen.Add(n.Packed);
        ep.Frames++;
        if (ep.Reported) return;
        ep.Reported = true;
        Report(n, kind, detail);
    }

    private void CheckSets()
    {
        var s = _view.Streamer;
        s.DebugTiles(_dbg);
        _dbgByKey.Clear();
        foreach (var d in _dbg) if (d.Layer == TileLayer.World && d.Packed != 0) _dbgByKey[d.Packed] = d;
        var cover = _view.Cover;
        var cam = _view.Camera?.GlobalPosition ?? Vector3.Zero;
        int cons = _view.ConsistencyErrors;
        if (cons > _lastConsistency)
        {
            _lastConsistency = cons;
            Fail("cover-consistency", $"WorldView.ConsistencyErrors rose to {cons} ({Stamp()})");
        }
        // Host holds a key the library does not hold (or has not been told to unload), or the other way round; the library's `ready` set is what it delivered.
        var bad = new HashSet<string>();
        foreach (var n in cover.All)
        {
            if (!_dbgByKey.TryGetValue(n.Packed, out var d) || (d.State != TileState.Ready && d.State != TileState.Unloading))
            {
                bad.Add("host-holds-library-does-not:" + n.Key);
                continue;
            }
            var c = new Vector2((d.Min.X + d.Max.X) * 0.5f, (d.Min.Y + d.Max.Y) * 0.5f);
            var p = n.Root.Position;
            if (new Vector2(p.X, p.Z).DistanceTo(c) > 2f) bad.Add($"position:{n.Key}:node=({p.X:0.#},{p.Z:0.#}) rect-centre=({c.X:0.#},{c.Y:0.#})");
        }
        // (debug_tiles labels a built-but-undelivered tile "ready" too, so the reverse direction, library holds / host does not, cannot be checked from it.)
        foreach (var b in bad)
        {
            _setBad.TryGetValue(b, out int c);
            _setBad[b] = c + 1;
            if (c + 1 == 3)
            {
                string key = b.Substring(b.IndexOf(':') + 1);
                key = key.Split(':')[0];
                _holes++;
                Count(b.Split(':')[0]);
                string lib = "";
                if (TileKey.TryParse(key, out long pk) && _dbgByKey.TryGetValue(pk, out var ld))
                    lib = $"  library: state={ld.State} ring={ld.Ring} rect=({ld.Min.X:0},{ld.Min.Y:0})-({ld.Max.X:0},{ld.Max.Y:0}) parent={StateOf(TileKey.Parent(pk))}\n";
                GD.Print($"[HOLE] {Stamp()} {b.Replace(":", " ")} (3 samples in a row)\n{lib}{HistoryText(key)}");
            }
        }
        if (_setBad.Count > 0)
        {
            var gone = new List<string>();
            foreach (var k in _setBad.Keys) if (!bad.Contains(k)) gone.Add(k);
            foreach (var k in gone) _setBad.Remove(k);
        }
        _ = cam;
    }

    private void Count(string kind) { _kinds.TryGetValue(kind, out int c); _kinds[kind] = c + 1; }

    private void Fail(string kind, string detail)
    {
        _holes++;
        Count(kind);
        GD.Print($"[HOLE] {Stamp()} {kind}: {detail}");
    }

    private string HistoryText(string key)
    {
        var sb = new StringBuilder();
        if (_hist.TryGetValue(key, out var h))
        {
            sb.Append("  history of ").Append(key).Append(": deliveries ").Append(h.Deliveries).Append(" re-deliveries ").Append(h.Redeliveries).Append('\n');
            foreach (var e in h.Events) sb.Append("    ").Append(e).Append('\n');
        }
        else sb.Append("  no delivery history for ").Append(key).Append('\n');
        return sb.ToString();
    }

    private void Report(TileNodes n, string kind, string detail)
    {
        _holes++;
        Count(kind);
        var cam = _view.Camera?.GlobalPosition ?? Vector3.Zero;
        var p = n.Root.Position;
        float dist = new Vector2(p.X - cam.X, p.Z - cam.Z).Length();
        var sb = new StringBuilder();
        sb.Append($"[HOLE] {Stamp()} {kind} {n.Key} ring={n.Ring} {detail}: vis={n.Vis} fading={n.Fading} fade={n.Fade:0.000} fadeMat={n.FadeMaterialActive} fadeAwait={n.FadeAwait} ")
          .Append($"tris={n.Tris} ground={n.GroundTris} lights={n.LightCount} rootVisible={n.Root.Visible} inTree={n.Root.IsVisibleInTree()} meshVisible={n.MeshInstance.Visible} groundVisible={n.GroundInstance.Visible} ")
          .Append($"dist={dist:0} m cam=({cam.X:0},{cam.Y:0},{cam.Z:0}) held {(Time.GetTicksMsec() - n.LoadedMs) / 1000.0:0.0} s\n");
        if (_dbgByKey.TryGetValue(n.Packed, out var d)) sb.Append($"  library: state={d.State} ring={d.Ring} rect=({d.Min.X:0},{d.Min.Y:0})-({d.Max.X:0},{d.Max.Y:0}) source={d.Source}\n");
        else sb.Append("  library: no debug_tiles entry sampled yet\n");
        sb.Append("  parent ").Append(StateOf(TileKey.Parent(n.Packed))).Append("; children ");
        for (int q = 0; q < 4 && TileKey.Z(n.Packed) < TileKey.LeafZ; q++) sb.Append(StateOf(TileKey.Child(n.Packed, q))).Append(' ');
        sb.Append('\n').Append(HistoryText(n.Key));
        GD.Print(sb.ToString());
    }

    private string StateOf(long k)
    {
        int s = _view.Cover.StateOf(k);
        return $"{TileKey.ToText(k)}={(s < 0 ? "-" : ((TileCover.Vis)s).ToString())}";
    }

    private void Finish()
    {
        _done = true;
        var stats = new StreamerStats();
        _view.Streamer.DebugStats(stats);
        var sb = new StringBuilder();
        foreach (var kv in _kinds) sb.Append(kv.Key).Append('=').Append(kv.Value).Append(' ');
        string line = $"{_t:0} s, {_frame} frames, {_checkedTiles} visible-tile checks, {_view.ReadyTotal} deliveries, {_view.ReplacedTotal} re-deliveries ({_emptyRedeliveries} empty), "
            + $"{_view.UnloadedTotal} unloads, {_view.OriginShiftCount} origin shifts, empty tiles among built-up neighbours (informational) frames {_emptyVisibleFrames}, holes {_holes} [{sb}]";
        GD.Print($"[HoleCheck] engine cull checks (tiles with a surface point inside the frustum): {_cullChecked}, root dither-mask checks: {_maskChecks}");
        GD.Print("[HoleCheck] flicker and coverage metrics:\n" + _metrics.Summary(_t));
        GD.Print(_holes == 0 && _metrics.Problems == 0 ? $"HOLE CHECK PASS: {line}" : $"HOLE CHECK FAIL: {line}, pixel holes {_metrics.Problems}");
        GetTree().Quit(_holes == 0 && _metrics.Problems == 0 ? 0 : 1);
    }
}
