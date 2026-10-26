using System.Collections.Generic;
using System.Text;
using Godot;

namespace LowPolyMap;

/// <summary>
/// Owns the Rust WorldStreamer (via the facade), drives it every frame, uploads ready tiles
/// into pooled nodes and returns unloaded tiles to the pool. Visibility follows the quadtree-cover
/// sibling-swap rule (see <see cref="TileCover"/>). Exposes timing/counters for the debug overlay.
/// </summary>
public partial class StreamerBridge : Node3D
{
    private const long UploadBudgetUs = 2000;
    private const int MaxTilesPerFrame = 2;
    private const int PrewarmTiles = 24;
    private const double VerifyInterval = 5.0;
    private const string TileUrlArg = "--tile-url=";

    [Export] public double OriginLat { get; set; } = 54.6872;
    [Export] public double OriginLon { get; set; } = 25.2797;
    /// <summary>Tile source. Empty = synthetic city. Overridden by the user arg --tile-url=&lt;url&gt;.</summary>
    [Export] public string TileUrl { get; set; } = "";
    [Export] public FreeFlyCamera? Camera { get; set; }
    /// <summary>0 = day, 1 = night: lights and lit-road emission. Plain value until M6's TimeOfDay owns it as a global.</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float NightFactor { get; set; } = 1f;

    public WorldStreamerFacade Streamer { get; private set; } = new();
    /// <summary>Human readable tile source actually in use.</summary>
    public string TileSource { get; private set; } = "";
    public TileCover Cover { get; } = new();

    // Overlay counters.
    public long UploadUsLastFrame { get; private set; }
    public long UploadUsMaxLastSecond { get; private set; }
    public int ReadyLastSecond { get; private set; }
    public int UnloadedLastSecond { get; private set; }
    public int ReadyTotal { get; private set; }
    public int UnloadedTotal { get; private set; }
    /// <summary>Re-deliveries under an existing key (light clustering, coarsening) replaced in place.</summary>
    public int ReplacedTotal { get; private set; }
    public int ActiveTiles => Cover.Loaded;
    public int PoolFree => _pool?.FreeCount ?? 0;
    public int PoolCreated => _pool?.TotalCreated ?? 0;
    /// <summary>Frames that polled at least one tile / frames whose poll+upload exceeded the 2 ms budget (one big tile can).</summary>
    public int UploadFrames { get; private set; }
    public int UploadFramesOverBudget { get; private set; }
    public int ConsistencyErrors { get; private set; }
    public int ConsistencyChecks { get; private set; }
    /// <summary>Per tile upload cost in microseconds, indexed by ring 0..3.</summary>
    public readonly long[] TileUploadUsLast = new long[4];
    public readonly long[] TileUploadUsMax = new long[4];
    public readonly long[] TileUploadUsSum = new long[4];
    public readonly int[] TileUploadCount = new int[4];
    /// <summary>Part of the tile upload cost spent on the ground mesh (microseconds), all rings together.</summary>
    public long GroundUploadUsLast { get; private set; }
    public long GroundUploadUsMax { get; private set; }
    public long GroundUploadUsSum { get; private set; }
    public int GroundUploadCount { get; private set; }
    /// <summary>Terrain height under the camera in meters (Rust ground_height_at; 0 until a DEM exists).</summary>
    public double CameraGroundHeight { get; private set; }

    private TilePool? _pool;
    private Node3D? _tileRoot;

    /// <summary>Tiles in a poll_fades transition (below 1) with their last poll stamp; allocation-free in steady state.</summary>
    private readonly List<TileNodes> _fading = new(64);
    private int _fadeFrame;
    public int FadeEntriesLastPoll { get; private set; }
    /// <summary>Test aid (user arg --fake-fade=&lt;0..1&gt;): pretends poll_fades lists every R1 tile with this fade (checks the dither path without Rust).</summary>
    private float _fakeFade = -1f;
    /// <summary>True when the loaded Rust build exports poll_fades(); false = no transitions, instant swaps (old behaviour).</summary>
    public bool FadesSupported => Cover.FadesEnabled;

    private float _appliedNight = -1f;
    private double _secTimer;
    private int _secReady;
    private int _secUnloaded;
    private long _secUploadMax;
    private bool _loggedFirstTile;
    private double _sinceCheck;
    private int _slowLogs;
    private readonly List<DebugTile> _verifyTiles = new(256);
    private readonly HashSet<long> _pendingRoots = new();
    private readonly HashSet<long> _suspectRoots = new();
    private readonly StringBuilder _report = new();

    public override void _Ready()
    {
        _tileRoot = new Node3D { Name = "Tiles" };
        AddChild(_tileRoot);
        _pool = new TilePool(_tileRoot);
        _pool.Prewarm(PrewarmTiles);

        foreach (var arg in OS.GetCmdlineUserArgs())
            if (arg.StartsWith("--light-alpha=", System.StringComparison.Ordinal)
                && float.TryParse(arg.Substring(14), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var la))
                TilePool.DebugLightAlpha = la;
        if (CoverSelfTest.Requested()) CoverSelfTest.Run(this);

        var url = ResolveTileUrl();
        TileSource = url.Length == 0 ? "synthetic city" : url;
        GD.Print($"[StreamerBridge] tile source: {TileSource}");

        Streamer = WorldStreamerFacade.Create();
        foreach (var arg in OS.GetCmdlineUserArgs())
            if (arg.StartsWith("--fake-fade=", System.StringComparison.Ordinal)
                && float.TryParse(arg.Substring(12), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var ff))
                _fakeFade = ff;
        Cover.FadesEnabled = Streamer.HasPollFades || _fakeFade >= 0f;
        GD.Print($"[StreamerBridge] poll_fades {(Cover.FadesEnabled ? "available: dithered LOD fades on" : "missing in this Rust build: instant sibling swaps")}");
        if (Streamer.Node is { } node)
        {
            node.Name = "WorldStreamer";
            AddChild(node);
            var cacheDir = ProjectSettings.GlobalizePath("user://tile_cache");
            DirAccess.MakeDirRecursiveAbsolute(cacheDir);
            Streamer.Configure(OriginLat, OriginLon, url, cacheDir);
            GD.Print($"[StreamerBridge] WorldStreamer created, cache_dir={cacheDir}, debug_tiles={Streamer.HasDebugTiles}");
        }
    }

    private string ResolveTileUrl()
    {
        var url = TileUrl;
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(TileUrlArg, System.StringComparison.Ordinal))
                url = arg.Substring(TileUrlArg.Length); // may be empty -> synthetic
        }
        return url.Trim();
    }

    public override void _Process(double delta)
    {
        if (_pool != null && NightFactor != _appliedNight)
        {
            _appliedNight = NightFactor;
            _pool.SetNightFactor(NightFactor);
        }

        if (Streamer.IsAvailable && Camera != null)
        {
            // Height above ground = camera y - terrain height under it (0 until the DEM spike).
            CameraGroundHeight = Streamer.GroundHeightAt(Camera.GlobalPosition.X, Camera.GlobalPosition.Z);
            Camera.GroundY = (float)CameraGroundHeight;
            Streamer.UpdateCamera(Camera.GlobalPosition, Camera.Velocity, Camera.AltitudeAgl);

            // Shift first so tiles polled below (already in the new frame) are not shifted twice.
            var shift = Streamer.TakeOriginShift();
            if (shift != Vector3.Zero)
            {
                Camera.GlobalPosition -= shift;
                Cover.Shift(shift);
            }

            // Poll one tile at a time so a single heavy tile cannot drag a second one into the same
            // frame: stop as soon as the budget is spent (Rust keeps the rest queued).
            long t0 = (long)Time.GetTicksUsec();
            int n0 = ReadyTotal;
            for (int n = 0; n < MaxTilesPerFrame; n++)
            {
                var ready = Streamer.PollReady(1);
                if (ready.Count == 0) break;
                foreach (var d in ready)
                {
                    if (WorldStreamerFacade.TryParseReady(d, out var tile)) UploadTile(in tile);
                }
                if ((long)Time.GetTicksUsec() - t0 >= UploadBudgetUs) break;
            }
            PollFades();
            var unload = Streamer.PollUnload();
            for (int i = 0; i < unload.Length; i++) UnloadTile(unload[i]);
            UploadUsLastFrame = (long)Time.GetTicksUsec() - t0;
            if (n0 != ReadyTotal) { UploadFrames++; if (UploadUsLastFrame > UploadBudgetUs) UploadFramesOverBudget++; }
            if (UploadUsLastFrame > _secUploadMax) _secUploadMax = UploadUsLastFrame;
        }

        UpdateSpriteScale();

        _sinceCheck += delta;
        if (_sinceCheck >= VerifyInterval)
        {
            _sinceCheck = 0;
            VerifyCover();
        }

        _secTimer += delta;
        if (_secTimer >= 1.0)
        {
            _secTimer -= 1.0;
            ReadyLastSecond = _secReady;
            UnloadedLastSecond = _secUnloaded;
            UploadUsMaxLastSecond = _secUploadMax;
            _secReady = 0;
            _secUnloaded = 0;
            _secUploadMax = 0;
        }
    }

    /// <summary>
    /// poll_fades step (after poll_ready, before poll_unload): applies each listed tile's fade to its pooled nodes, ends the
    /// transition of tiles that were listed before but are not any more (or listed with 1.0), then lets tiles uploaded
    /// this frame become visible.
    /// </summary>
    private void PollFades()
    {
        var pool = _pool!;
        var fades = Streamer.PollFades();
        _fadeFrame++;
        int entries = 0;
        if (fades != null)
        {
            foreach (var kv in fades)
            {
                entries++;
                if (!TileKey.TryParse(kv.Key.AsString(), out long packed)) continue;
                // A tile that is listed but not uploaded yet is picked up on the call after its upload.
                if (!Cover.TryGet(packed, out var nodes)) continue;
                nodes.FadeStamp = _fadeFrame;
                float f = Mathf.Clamp((float)kv.Value.AsDouble(), 0f, 1f);
                if (f >= 1f)
                {
                    EndTileFade(nodes);
                    continue;
                }
                if (!nodes.Fading) _fading.Add(nodes);
                if (!nodes.Fading || nodes.Fade != f || !nodes.FadeMaterialActive) pool.SetFade(nodes, f);
                Cover.SetFade(nodes, true, f);
            }
        }
        if (_fakeFade >= 0f)
        {
            foreach (var nodes in Cover.All)
            {
                if (nodes.Ring != 1) continue;
                entries++;
                nodes.FadeStamp = _fadeFrame;
                if (!nodes.Fading) _fading.Add(nodes);
                pool.SetFade(nodes, _fakeFade);
                Cover.SetFade(nodes, true, _fakeFade);
            }
        }
        FadeEntriesLastPoll = entries;
        // Transitions that disappeared from the dictionary without a 1.0 entry (e.g. force-completed) end here.
        for (int i = _fading.Count - 1; i >= 0; i--)
        {
            var n = _fading[i];
            if (!n.Fading || Cover.TryGet(n.Packed, out var live) && live != n)
            {
                _fading.RemoveAt(i); // unloaded in the meantime
                continue;
            }
            if (n.FadeStamp != _fadeFrame) EndTileFade(n);
        }
        Cover.ReleaseAwaiting();
    }

    private void EndTileFade(TileNodes nodes)
    {
        if (nodes.Fading || nodes.FadeMaterialActive)
        {
            _pool!.EndFade(nodes);
            Cover.SetFade(nodes, false, 1f);
        }
        _fading.Remove(nodes);
    }

    private static string Describe(Godot.Collections.Array? a)
    {
        if (a == null) return "no mesh";
        var sb = new StringBuilder();
        for (int i = 0; i < a.Count; i++)
        {
            var v = a[i];
            if (v.VariantType == Variant.Type.Nil) continue;
            sb.Append(i).Append(':').Append(v.VariantType);
            if (v.VariantType == Variant.Type.PackedVector3Array) sb.Append('[').Append(v.AsVector3Array().Length).Append(']');
            else if (v.VariantType == Variant.Type.PackedInt32Array) sb.Append('[').Append(v.AsInt32Array().Length).Append(']');
            sb.Append(' ');
        }
        return sb.ToString();
    }

    private float _lastFov = -1f, _lastHeight = -1f;

    /// <summary>Tells the sprite shader how many metres one pixel spans per metre of depth (changes with fov / window size).</summary>
    private void UpdateSpriteScale()
    {
        if (Camera == null || _pool == null) return;
        float fov = Camera.Fov;
        float h = GetViewport().GetWindow().Size.Y;
        if (h < 1f) h = 720f;
        if (fov == _lastFov && h == _lastHeight) return;
        _lastFov = fov;
        _lastHeight = h;
        float k = 2f * Mathf.Tan(Mathf.DegToRad(fov) * 0.5f) / h;
        _pool.LightMaterial.SetShaderParameter("px_per_depth", k);
    }

    private void UploadTile(in ReadyTile tile)
    {
        long t0 = (long)Time.GetTicksUsec();
        if (!TileKey.TryParse(tile.Key, out long packed))
        {
            GD.PushError($"[StreamerBridge] ready tile with unparsable key '{tile.Key}'");
            return;
        }
        var pool = _pool!;

        // Same key delivered again (light clustering, coarsening): replace content in place, keep the node.
        bool replace = Cover.TryGet(packed, out var nodes);
        if (replace)
        {
            Cover.BeginReplace(nodes);
            nodes.Ring = tile.Ring;
        }
        else
        {
            nodes = pool.Acquire(tile.Key, packed, tile.Ring);
        }
        nodes.Root.Position = tile.Origin;

        long tm0 = (long)Time.GetTicksUsec();
        pool.SetMesh(nodes, tile.Mesh);
        long tm1 = (long)Time.GetTicksUsec();
        // Ground (R0..R2): same root, so it is shown / hidden with the buildings. A delivery without a "ground" key
        // (older Rust build) leaves an existing ground mesh alone; an empty Array (R3) clears it.
        if (tile.HasGroundKey)
        {
            pool.SetGround(nodes, tile.Ground);
            long gus = (long)Time.GetTicksUsec() - tm1;
            GroundUploadUsLast = gus;
            if (gus > GroundUploadUsMax) GroundUploadUsMax = gus;
            GroundUploadUsSum += gus;
            GroundUploadCount++;
        }
        long tm1b = (long)Time.GetTicksUsec();
        // Lights only exist on the coarse bands (R2/R3); R0/R1 get windows instead.
        if (tile.Lights.Length > 0) pool.SetLights(nodes, tile.Lights, tile.LightColors);
        else pool.SetLights(nodes, System.Array.Empty<Vector3>(), System.Array.Empty<Color>());

        long tm2 = (long)Time.GetTicksUsec();
        if (replace)
        {
            Cover.EndReplace(nodes);
            ReplacedTotal++;
        }
        else
        {
            Cover.Add(nodes); // sibling-swap re-evaluation of the affected keys
        }

        _secReady++;
        ReadyTotal++;

        long us = (long)Time.GetTicksUsec() - t0;
        int r = System.Math.Clamp(tile.Ring, 0, 3);
        TileUploadUsLast[r] = us;
        if (us > TileUploadUsMax[r]) TileUploadUsMax[r] = us;
        TileUploadUsSum[r] += us;
        TileUploadCount[r]++;
        if (!_loggedFirstTile)
        {
            _loggedFirstTile = true;
            GD.Print($"[StreamerBridge] first tile {tile.Key} ring={tile.Ring} origin={tile.Origin} lights={tile.Lights.Length} tris={nodes.Tris} groundTris={nodes.GroundTris}");
            // One-time stats block so the headless smoke test shows the debug_stats contract is live.
            GD.Print($"[StreamerBridge] debug_stats {Godot.Json.Stringify(Streamer.DebugStats())}");
        }
        if (us > 2000 && _slowLogs < 40)
        {
            _slowLogs++;
            GD.Print($"[StreamerBridge] slow upload {tile.Key} ring={tile.Ring} tris={nodes.Tris} groundTris={nodes.GroundTris} lights={tile.Lights.Length} {us / 1000.0:0.00} ms (mesh {(tm1 - tm0) / 1000.0:0.00}, ground {(tm1b - tm1) / 1000.0:0.00}, lights {(tm2 - tm1b) / 1000.0:0.00}) {Describe(tile.Mesh)}");
        }
    }

    private void UnloadTile(string key)
    {
        if (!TileKey.TryParse(key, out long packed)) return;
        var nodes = Cover.Remove(packed);
        if (nodes == null) return;
        if (nodes.FadeMaterialActive) _fading.Remove(nodes);
        _pool!.Release(nodes);
        _secUnloaded++;
        UnloadedTotal++;
    }

    /// <summary>One-line cover summary (also callable from GDScript probes).</summary>
    public string DescribeCover()
    {
        var c = Cover;
        return $"loaded={c.Loaded} visible R0..R3={c.VisibleByRing[0]}/{c.VisibleByRing[1]}/{c.VisibleByRing[2]}/{c.VisibleByRing[3]} "
            + $"hiddenByChildren={c.HiddenByChildren} waitingSiblings={c.WaitingSiblings} tris={c.TrianglesVisible}/{c.TrianglesLoaded} "
            + $"ground={c.GroundTilesVisible}/{c.GroundTilesLoaded} tiles {c.GroundTrianglesVisible}/{c.GroundTrianglesLoaded} tris "
            + $"lights={c.LightsVisible}/{c.LightsLoaded} clusteredTiles={c.ClusteredTilesVisible}/{c.LightTilesVisible} "
            + $"uploadMaxMs={UploadUsMaxLastSecond / 1000.0:0.00} replaced={ReplacedTotal} cover errors={ConsistencyErrors}";
    }

    /// <summary>
    /// Periodic consistency check of the quadtree cover (every 5 s): stored visibility vs the rule, double cover,
    /// holes in settled roots, counters. Roots that still have pending tiles in Rust are skipped, and a hole must
    /// persist over two consecutive checks before it is reported. Pushes an error on any finding.
    /// </summary>
    public void VerifyCover()
    {
        _pendingRoots.Clear();
        if (Streamer.IsAvailable)
        {
            if (Streamer.HasDebugTiles)
            {
                Streamer.DebugTiles(_verifyTiles);
                for (int i = 0; i < _verifyTiles.Count; i++)
                {
                    var t = _verifyTiles[i];
                    if (t.Packed == 0) continue;
                    if (t.State is TileState.Queued or TileState.Fetching or TileState.Meshing)
                        _pendingRoots.Add(TileKey.Ancestor(t.Packed, TileKey.RootZ));
                }
            }
        }
        _report.Clear();
        int errors = Cover.Verify(_pendingRoots, _suspectRoots, _report);
        ConsistencyChecks++;
        if (errors > 0)
        {
            ConsistencyErrors += errors;
            GD.PushError($"[StreamerBridge] cover consistency: {errors} problem(s)\n{_report}");
        }
        else if (ConsistencyChecks == 1 || Cover.Loaded > 0 && ConsistencyChecks % 12 == 0)
        {
            GD.Print($"[StreamerBridge] cover consistent: {Cover.Loaded} tiles loaded, visible R0..R3 = "
                + $"{Cover.VisibleByRing[0]}/{Cover.VisibleByRing[1]}/{Cover.VisibleByRing[2]}/{Cover.VisibleByRing[3]}, "
                + $"hidden by children {Cover.HiddenByChildren}, waiting siblings {Cover.WaitingSiblings}");
        }
    }
}
