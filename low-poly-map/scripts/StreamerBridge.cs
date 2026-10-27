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
    /// <summary>0 = day, 1 = night: drives the light sprites. Plain value until M6's TimeOfDay owns it as a global.</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float NightFactor { get; set; } = 1f;
    /// <summary>Visual earth curvature (shaders/earth_curve.gdshaderinc): 0 = flat, 1 = real Earth, larger exaggerates. User arg --curvature=&lt;x&gt;.</summary>
    [Export(PropertyHint.Range, "0,100,0.1")] public float Curvature { get; set; } = 1f;

    private const double EarthRadiusM = 6_371_000.0;

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

    /// <summary>Latest data-source status (contract 3.5): from status() after configure and from the status_changed signal.</summary>
    public StreamerStatus Status { get; private set; } = new();
    /// <summary>Non-empty when the world could not be configured (e.g. "cache_dir in use"); the spike then runs without a world.</summary>
    public string ConfigureError { get; private set; } = "";
    /// <summary>True when a configure call succeeded and the world is streaming.</summary>
    public bool WorldRunning { get; private set; }
    /// <summary>Attribution strings to show while map data is visible (empty for the synthetic city).</summary>
    public string[] Attributions { get; private set; } = System.Array.Empty<string>();
    /// <summary>Effective band edges for the overlay, e.g. "bands 0.6/2.5/7/25 km" (empty until the profile is readable, api 0.6+).</summary>
    public string BandsText { get; private set; } = "";
    /// <summary>The resolved profile overrides passed to configure_world (empty = none), for logs and tests.</summary>
    public Godot.Collections.Dictionary ProfileOverrides { get; private set; } = new();

    /// <summary>Re-centers seen through take_origin_shift() (the authoritative path) and the latest / summed shift.</summary>
    public int OriginShiftCount { get; private set; }
    public Vector3 LastShift { get; private set; }
    public Vector3 TotalShift { get; private set; }
    /// <summary>Re-centers seen through the origin_shifted signal (diagnostic; must equal <see cref="OriginShiftCount"/>) and its latest vector.</summary>
    public int SignalShiftCount { get; private set; }
    public Vector3 LastSignalShift { get; private set; }
    /// <summary>
    /// Host hook: when set, replaces the update_camera(...) call in each frame (same place in the call order, before
    /// take_origin_shift). The hook may move <see cref="Camera"/> (in the pre-shift frame; the bridge subtracts the shift
    /// afterwards) and must call one camera update on the Streamer (e.g. UpdateCameraGeodetic).
    /// </summary>
    public System.Action<double>? ExternalCameraUpdate { get; set; }

    /// <summary>Raised from <c>_Process</c> after every <see cref="ApplyProfileOverrides"/> request has run: (ok, library error text).</summary>
    public event System.Action<bool, string>? Reconfigured;
    /// <summary>Successful reconfigures since start (not counting the startup configure).</summary>
    public int ReconfigureCount { get; private set; }
    public bool LastReconfigureOk { get; private set; } = true;
    public string LastReconfigureError { get; private set; } = "";
    /// <summary>True from <see cref="ApplyProfileOverrides"/> until the next frame start runs it.</summary>
    public bool ReconfigurePending => _pendingOverrides != null;
    /// <summary>Highest <see cref="ActiveTiles"/> seen (checked against the pool size by the settings self-test).</summary>
    public int PeakActiveTiles { get; private set; }
    /// <summary>Node sets (tile roots) under the pool container: created sets, in use or free.</summary>
    public int TileRootCount => _tileRoot?.GetChildCount() ?? 0;

    private string _lastLoggedStatus = "";
    private string _tileUrl = "";
    private string _cacheDir = "";
    /// <summary>profile_path and resolved --area/--set overrides from the launch args, kept for <see cref="ApplyProfileOverrides"/> (never re-derived: --area scales the library defaults and would compound).</summary>
    private string _argProfilePath = "";
    private Godot.Collections.Dictionary _argOverrides = new();
    private Godot.Collections.Dictionary? _pendingOverrides;
    private readonly List<long> _dropScratch = new(256);
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
        foreach (var arg in OS.GetCmdlineUserArgs())
            if (arg.StartsWith("--curvature=", System.StringComparison.Ordinal)
                && float.TryParse(arg.Substring(12), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var cv))
                Curvature = Mathf.Max(cv, 0f);
        // Curved vertices sink below the flat tile AABBs: widen the cull box by the drop at the camera's far plane.
        float far = Camera?.Far ?? 30000f;
        TilePool.CurveCullMargin = (float)(far * (double)far * Curvature / (2.0 * EarthRadiusM));

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
        if (Streamer.IsAvailable) GD.Print($"[StreamerBridge] library api {Streamer.ApiVersionText}");
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
            Streamer.StatusChanged += OnStatusChanged;
            Streamer.OriginShifted += OnOriginShifted;
            var cacheDir = ProjectSettings.GlobalizePath("user://tile_cache");
            DirAccess.MakeDirRecursiveAbsolute(cacheDir);
            _tileUrl = url;
            _cacheDir = cacheDir;
            ConfigureWorld(url, cacheDir);
            GD.Print($"[StreamerBridge] WorldStreamer created, cache_dir={cacheDir}, debug_tiles={Streamer.HasDebugTiles}");
        }
    }

    public override void _ExitTree()
    {
        // Deterministic stop before the node is freed: waits (bounded 1 s) for cache I/O and releases the cache_dir lock.
        if (Streamer.IsAvailable)
        {
            Streamer.StatusChanged -= OnStatusChanged;
            Streamer.OriginShifted -= OnOriginShifted;
            if (Streamer.HasHostApi) GD.Print("[StreamerBridge] shutdown (releasing the cache lock)");
            Streamer.Shutdown();
        }
    }

    private void OnStatusChanged(StreamerStatus st)
    {
        Status = st;
        var line = $"status: {st.State} - {st.Message}";
        if (line == _lastLoggedStatus) return;
        _lastLoggedStatus = line;
        GD.Print($"[StreamerBridge] {line}");
    }

    // Diagnostic only: the bridge shifts from take_origin_shift() (same-frame ordering); never consume the shift here.
    private void OnOriginShifted(Vector3 shift)
    {
        SignalShiftCount++;
        LastSignalShift = shift;
    }

    // ---- configuration ---------------------------------------------------

    private const string ProfileArg = "--profile=";
    private const string SetArg = "--set=";
    private const string AreaArg = "--area=";
    private static readonly string[] BandEdgeKeys = { "r0_edge_m", "r1_edge_m", "r2_edge_m", "r3_edge_m" };
    private static readonly double[] BandEdgeDefaults = { 600.0, 2500.0, 7000.0, 25000.0 };

    private static bool TryNumber(string text, out double v) =>
        double.TryParse(text, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out v);

    /// <summary>
    /// Builds the profile options from the user args: --profile=&lt;path&gt;, --area=&lt;0.05..1&gt; (scales the four band edges, applied
    /// first) and repeated --set=&lt;dotted.key&gt;=&lt;number&gt; (applied last, wins). Returns the nested overrides (may be empty).
    /// </summary>
    private Godot.Collections.Dictionary ParseProfileArgs(out string profilePath)
    {
        profilePath = "";
        double area = 0;
        var sets = new List<(string Key, Variant Value)>();
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(ProfileArg, System.StringComparison.Ordinal))
            {
                var path = arg.Substring(ProfileArg.Length).Trim();
                if (path.Length == 0) continue;
                // Rust reads the file itself: hand over an OS path (res:// and user:// resolve through Godot).
                profilePath = path.Contains("://") ? ProjectSettings.GlobalizePath(path) : System.IO.Path.GetFullPath(path);
            }
            else if (arg.StartsWith(AreaArg, System.StringComparison.Ordinal))
            {
                if (!TryNumber(arg.Substring(AreaArg.Length), out var a)) { GD.PushWarning($"[StreamerBridge] bad {arg}; expected --area=<0.05..1>"); continue; }
                area = System.Math.Clamp(a, 0.05, 1.0);
                if (area != a) GD.Print($"[StreamerBridge] --area={a} clamped to {area}");
            }
            else if (arg.StartsWith(SetArg, System.StringComparison.Ordinal))
            {
                var kv = arg.Substring(SetArg.Length);
                int eq = kv.IndexOf('=');
                if (eq <= 0 || eq == kv.Length - 1) { GD.PushWarning($"[StreamerBridge] bad {arg}; expected --set=<dotted.key>=<number>"); continue; }
                var key = kv.Substring(0, eq).Trim();
                var text = kv.Substring(eq + 1).Trim();
                // Numbers always go as float (like Godot's JSON parser): Rust accepts integral floats for integer keys.
                if (TryNumber(text, out var num)) sets.Add((key, num));
                else if (text == "true" || text == "false") sets.Add((key, text == "true"));
                else sets.Add((key, text));
            }
        }

        var overrides = new Godot.Collections.Dictionary();
        if (area > 0)
        {
            Godot.Collections.Dictionary bands = new();
            var profile = Streamer.WorldProfile();
            Godot.Collections.Dictionary? pb = profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary ? bv.AsGodotDictionary() : null;
            for (int i = 0; i < BandEdgeKeys.Length; i++)
            {
                double def = pb != null && pb.TryGetValue(BandEdgeKeys[i], out var e) ? e.AsDouble() : BandEdgeDefaults[i];
                bands[BandEdgeKeys[i]] = def * area;
            }
            overrides["bands"] = bands;
        }
        foreach (var (key, value) in sets) SetDotted(overrides, key, value);
        return overrides;
    }

    private static void SetDotted(Godot.Collections.Dictionary root, string dotted, Variant value)
    {
        var parts = dotted.Split('.');
        var d = root;
        for (int i = 0; i < parts.Length - 1; i++)
        {
            if (!(d.TryGetValue(parts[i], out var sub) && sub.VariantType == Variant.Type.Dictionary))
            {
                var nd = new Godot.Collections.Dictionary();
                d[parts[i]] = nd;
                d = nd;
            }
            else d = sub.AsGodotDictionary();
        }
        d[parts[^1]] = value;
    }

    /// <summary>
    /// configure_world (api 0.6+, with the profile args) or the legacy configure. On a profile error the world is retried with the
    /// default profile; any other failure (e.g. cache_dir in use) leaves the spike running without a world.
    /// </summary>
    private void ConfigureWorld(string url, string cacheDir)
    {
        var overrides = ParseProfileArgs(out var profilePath);
        _argProfilePath = profilePath;
        _argOverrides = overrides;
        ProfileOverrides = overrides;
        bool haveProfileArgs = profilePath.Length > 0 || overrides.Count > 0;
        if (profilePath.Length > 0) GD.Print($"[StreamerBridge] profile file: {profilePath}");
        if (overrides.Count > 0) GD.Print($"[StreamerBridge] profile overrides: {Godot.Json.Stringify(overrides)}");

        if (!Streamer.HasConfigureWorld)
        {
            if (haveProfileArgs) GD.PushWarning($"[StreamerBridge] profile args ignored: library api {Streamer.ApiVersionText} has no configure_world (needs 0.6)");
            Streamer.Configure(OriginLat, OriginLon, url, cacheDir);
            WorldRunning = true; // legacy configure reports errors only in the Godot log
            AfterConfigure();
            return;
        }

        var options = WorldStreamerFacade.MakeWorldOptions(OriginLat, OriginLon, url, cacheDir);
        if (profilePath.Length > 0) options["profile_path"] = profilePath;
        if (overrides.Count > 0) options["profile_overrides"] = overrides;
        var result = Streamer.ConfigureWorld(options);
        if (!result.Ok && haveProfileArgs && result.Error.StartsWith("profile", System.StringComparison.Ordinal))
        {
            GD.PushError($"[StreamerBridge] profile rejected by the library: {result.Error}");
            GD.Print("[StreamerBridge] retrying with the default world profile");
            ProfileOverrides = new Godot.Collections.Dictionary();
            _argProfilePath = "";
            _argOverrides = new Godot.Collections.Dictionary();
            options.Remove("profile_path");
            options.Remove("profile_overrides");
            result = Streamer.ConfigureWorld(options);
        }
        if (!result.Ok)
        {
            ConfigureError = result.Error.Length > 0 ? result.Error : "configure_world failed";
            GD.PushError($"[StreamerBridge] configure_world failed: {ConfigureError}. Running without a world (no tiles will load).");
            return;
        }
        WorldRunning = true;
        Attributions = result.Attributions;
        AfterConfigure();
    }

    private void AfterConfigure()
    {
        Status = Streamer.Status();
        if (Attributions.Length == 0) Attributions = Streamer.Attributions();
        if (Attributions.Length > 0) GD.Print($"[StreamerBridge] attribution: {string.Join(" | ", Attributions)}");
        else if (Streamer.HasHostApi) GD.Print("[StreamerBridge] attribution: none required (synthetic city)");

        var profile = Streamer.WorldProfile();
        if (profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary)
        {
            var b = bv.AsGodotDictionary();
            var sb = new StringBuilder("bands ");
            for (int i = 0; i < BandEdgeKeys.Length; i++)
            {
                if (!b.TryGetValue(BandEdgeKeys[i], out var e)) { sb.Clear(); break; }
                if (i > 0) sb.Append('/');
                sb.Append((e.AsDouble() / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            }
            if (sb.Length > 0) BandsText = sb.Append(" km").ToString();
        }
        if (BandsText.Length > 0) GD.Print($"[StreamerBridge] effective {BandsText}");
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
            // A queued reconfigure runs first, so this frame's update_camera already carries the new origin and camera position.
            if (_pendingOverrides != null) RunReconfigure();
            // Height above ground = camera y - terrain height under it (0 until the DEM spike).
            CameraGroundHeight = Streamer.GroundHeightAt(Camera.GlobalPosition.X, Camera.GlobalPosition.Z);
            Camera.GroundY = (float)CameraGroundHeight;
            if (ExternalCameraUpdate != null) ExternalCameraUpdate(delta);
            else Streamer.UpdateCamera(Camera.GlobalPosition, Camera.Velocity, Camera.AltitudeAgl);

            // Shift first so tiles polled below (already in the new frame) are not shifted twice.
            var shift = Streamer.TakeOriginShift();
            LastShift = shift;
            if (shift != Vector3.Zero)
            {
                OriginShiftCount++;
                TotalShift += shift;
                Camera.GlobalPosition -= shift;
                Cover.Shift(shift);
            }
            RenderingServer.GlobalShaderParameterSet("curve_center", Camera.GlobalPosition);
            RenderingServer.GlobalShaderParameterSet("curve_inv_2r", (float)(Curvature / (2.0 * EarthRadiusM)));

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
        if (Cover.Loaded > PeakActiveTiles) PeakActiveTiles = Cover.Loaded;

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
        ReleaseTile(packed);
    }

    private bool ReleaseTile(long packed)
    {
        var nodes = Cover.Remove(packed);
        if (nodes == null) return false;
        if (nodes.FadeMaterialActive) _fading.Remove(nodes);
        _pool!.Release(nodes);
        _secUnloaded++;
        UnloadedTotal++;
        return true;
    }

    // ---- reconfigure (settings panel) ---------------------------------------

    /// <summary>
    /// Queues a world reconfigure with <paramref name="panelOverrides"/> (nested like the profile JSON, e.g. bands.r3_edge_m) merged
    /// over the launch-arg profile (panel wins). It runs at the start of the next <c>_Process</c>, before update_camera, and ends with
    /// <see cref="Reconfigured"/>. A later call before that replaces the queued one. The world restarts at the camera's current
    /// position: that is the new origin, the camera goes back to local x/z = 0 (altitude and orientation kept), and every tile is
    /// released through the unload batch the library returns for the teleport.
    /// </summary>
    public void ApplyProfileOverrides(Godot.Collections.Dictionary panelOverrides) => _pendingOverrides = panelOverrides;

    private void RunReconfigure()
    {
        var panel = _pendingOverrides!;
        _pendingOverrides = null;
        bool ok = TryReconfigure(panel, out var error);
        LastReconfigureOk = ok;
        LastReconfigureError = error;
        if (ok) ReconfigureCount++;
        else GD.PushError($"[StreamerBridge] reconfigure rejected, previous world keeps running: {error}");
        Reconfigured?.Invoke(ok, error);
    }

    private bool TryReconfigure(Godot.Collections.Dictionary panel, out string error)
    {
        error = "";
        if (!Streamer.HasConfigureWorld) { error = $"configure_world needs api 0.6 (library {Streamer.ApiVersionText})"; return false; }
        if (Camera == null) { error = "no camera assigned"; return false; }
        long t0 = (long)Time.GetTicksUsec();

        // New origin = where the camera is now (f32 geodetic: 0.1-0.4 m). Without a running world there is nothing to convert from.
        var cam = Camera.GlobalPosition;
        bool recenter = WorldRunning;
        double lat = OriginLat, lon = OriginLon;
        if (recenter)
        {
            var geo = Streamer.LocalToGeodetic(cam);
            if (geo == Vector3.Zero) { error = "local_to_geodetic failed (camera position not convertible)"; return false; }
            lat = geo.X;
            lon = geo.Y;
        }

        var merged = (Godot.Collections.Dictionary)_argOverrides.Duplicate(true);
        MergeInto(merged, panel);
        var options = WorldStreamerFacade.MakeWorldOptions(lat, lon, _tileUrl, _cacheDir);
        if (_argProfilePath.Length > 0) options["profile_path"] = _argProfilePath;
        if (merged.Count > 0) options["profile_overrides"] = merged;
        // Same cache_dir, no Shutdown() in between: the lock is kept across the teleport (contract 11a).
        var result = Streamer.ConfigureWorld(options);
        if (!result.Ok)
        {
            error = result.Error.Length > 0 ? result.Error : "configure_world failed";
            return false;
        }

        // Fresh start. Take the unload batch right now (before any new delivery can reuse a key), release stragglers too.
        int unloadedBefore = UnloadedTotal;
        var unload = Streamer.PollUnload();
        for (int i = 0; i < unload.Length; i++) UnloadTile(unload[i]);
        _dropScratch.Clear();
        foreach (var n in Cover.All) _dropScratch.Add(n.Packed);
        for (int i = 0; i < _dropScratch.Count; i++) ReleaseTile(_dropScratch[i]);
        if (_dropScratch.Count > 0)
            GD.PushWarning($"[StreamerBridge] reconfigure: {_dropScratch.Count} tiles were not in the library's unload batch of {unload.Length}; released anyway");
        _fading.Clear();
        _suspectRoots.Clear();
        _pendingRoots.Clear();

        if (recenter) Camera.GlobalPosition = new Vector3(0f, cam.Y, 0f);
        LastShift = Vector3.Zero;
        TotalShift = Vector3.Zero;
        ProfileOverrides = merged;
        WorldRunning = true;
        ConfigureError = "";
        Attributions = result.Attributions;
        AfterConfigure();
        GD.Print($"[StreamerBridge] reconfigured: origin=({lat:0.000000}, {lon:0.000000}) camera -> (0, {cam.Y:0.0}, 0) from ({cam.X:0.0}, {cam.Z:0.0}), "
            + $"{UnloadedTotal - unloadedBefore} tiles released, {((long)Time.GetTicksUsec() - t0) / 1000.0:0.0} ms; effective {EffectiveProfileText()}");
        return true;
    }

    /// <summary>Deep merge: nested dictionaries merge, everything else in <paramref name="over"/> replaces.</summary>
    private static void MergeInto(Godot.Collections.Dictionary into, Godot.Collections.Dictionary over)
    {
        foreach (var kv in over)
        {
            if (kv.Value.VariantType == Variant.Type.Dictionary && into.TryGetValue(kv.Key, out var cur) && cur.VariantType == Variant.Type.Dictionary)
                MergeInto(cur.AsGodotDictionary(), kv.Value.AsGodotDictionary());
            else
                into[kv.Key] = kv.Value.VariantType == Variant.Type.Dictionary ? kv.Value.AsGodotDictionary().Duplicate(true) : kv.Value;
        }
    }

    /// <summary>The values the library is running with (world_profile), e.g. "bands 0.6/2.5/7/25 km, min building height R0/R1/R2 0/0/0 m".</summary>
    public string EffectiveProfileText()
    {
        var p = Streamer.WorldProfile();
        var sb = new StringBuilder();
        if (p.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary)
        {
            var b = bv.AsGodotDictionary();
            sb.Append("bands ");
            for (int i = 0; i < BandEdgeKeys.Length; i++)
                sb.Append(i > 0 ? "/" : "").Append(b.TryGetValue(BandEdgeKeys[i], out var e) ? (e.AsDouble() / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture) : "?");
            sb.Append(" km");
        }
        if (p.TryGetValue("buildings", out var bd) && bd.VariantType == Variant.Type.Dictionary)
        {
            var d = bd.AsGodotDictionary();
            sb.Append(sb.Length > 0 ? ", " : "").Append("min building height R0/R1/R2 ");
            for (int i = 0; i < 3; i++)
            {
                double h = d.TryGetValue("r" + i, out var rv) && rv.VariantType == Variant.Type.Dictionary && rv.AsGodotDictionary().TryGetValue("min_height_m", out var mh) ? mh.AsDouble() : double.NaN;
                sb.Append(i > 0 ? "/" : "").Append(double.IsNaN(h) ? "?" : h.ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            }
            sb.Append(" m");
        }
        return sb.ToString();
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
