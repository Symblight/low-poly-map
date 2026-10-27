using System.Collections.Generic;
using System.Text;
using Godot;

namespace SimWorld;

/// <summary>
/// The library's entry node: owns the Rust WorldStreamer (via the facade), drives it every frame, uploads ready tiles into
/// pooled nodes and returns unloaded tiles to the pool. Visibility follows the quadtree-cover sibling-swap rule (see
/// <see cref="TileCover"/>). Put it in a scene, set <see cref="OriginLat"/> / <see cref="OriginLon"/> and a <see cref="Camera"/>
/// (or a <see cref="CameraSource"/>), and the city streams around the camera. The host sets the time of day through
/// <see cref="SetTime"/>, shifts its own nodes on <see cref="OriginShifted"/>, and owns the environment, the sun and the exposure.
/// Exposes timing and counters for a debug overlay.
/// </summary>
[GlobalClass]
public partial class WorldView : Node3D
{
    /// <summary>Geodetic origin of the world frame (WGS84 degrees). No default: the host chooses where the world starts.</summary>
    [Export] public double OriginLat { get; set; } = double.NaN;
    [Export] public double OriginLon { get; set; } = double.NaN;
    /// <summary>Tile source (PMTiles URL). Empty = synthetic city.</summary>
    [Export] public string TileUrl { get; set; } = "";
    /// <summary>Disk cache folder (<c>user://</c> or an OS path). Must be unique per instance (contract 11a). Empty = no disk cache.</summary>
    [Export] public string CacheDir { get; set; } = "user://simworld_cache";
    /// <summary>World profile JSON file (<c>res://</c>, <c>user://</c> or an OS path); empty = compiled-in defaults. Needs api 0.6.</summary>
    [Export(PropertyHint.File, "*.json")] public string ProfilePath { get; set; } = "";
    /// <summary>Camera used for the field of view (sprite size) and the far plane. Also the pose source unless <see cref="CameraSource"/> is set.</summary>
    [Export] public Camera3D? Camera { get; set; }
    /// <summary>Tuning of the rendering side; null = defaults.</summary>
    [Export] public WorldRenderSettings? Settings { get; set; }
    /// <summary>Start in <c>_Ready</c>. Turn off to set properties from code first and call <see cref="Start"/> yourself.</summary>
    [Export] public bool AutoStart { get; set; } = true;
    /// <summary>Visual earth curvature (shaders/earth_curve.gdshaderinc): 0 = flat, 1 = real Earth, larger exaggerates.</summary>
    [Export(PropertyHint.Range, "0,100,0.1")] public float Curvature { get; set; } = 1f;

    /// <summary>Pose source (position, velocity, AGL, origin shifts). Null = <see cref="Camera"/> itself when it implements <see cref="IWorldCameraSource"/>, else an adapter around it.</summary>
    public IWorldCameraSource? CameraSource { get; set; }
    /// <summary>Profile overrides (nested like the profile JSON) merged over <see cref="ProfilePath"/> at every configure; set before <see cref="Start"/>.</summary>
    public Godot.Collections.Dictionary BaseProfileOverrides { get; set; } = new();

    /// <summary>0 = day, 1 = night, as last given to <see cref="SetTime"/>.</summary>
    public float NightFactor { get; private set; } = 1f;
    /// <summary>Local solar hour as last given to <see cref="SetTime"/>.</summary>
    public float Hour { get; private set; } = 12f;

    /// <summary>Raised from <c>_Process</c> after the view applied a re-center: the vector to subtract from every host node in the world frame.</summary>
    public event System.Action<Vector3>? OriginShifted;

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
    /// <summary>The effective profile overrides passed to configure_world (base + panel; empty = none), for logs and tests.</summary>
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
    /// take_origin_shift). The hook may move the camera (in the pre-shift frame; the view subtracts the shift
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
    private bool _started;
    private bool _initialized;
    private WorldRenderSettings _settings = new();
    private IWorldCameraSource? _source;
    private Camera3DSource? _adapter;
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

    public override void _EnterTree()
    {
        if (GetScript().As<Script>() is { } script) AddonPaths.SetFromScriptPath(script.ResourcePath);
        ShaderGlobals.EnsureRegistered();
    }

    public override void _Ready()
    {
        if (AutoStart) Start();
    }

    /// <summary>Builds the tile pool and the WorldStreamer node once (idempotent). Needs the node in the tree.</summary>
    private void Initialize()
    {
        if (_initialized) return;
        _initialized = true;
        _settings = Settings ?? new WorldRenderSettings();
        TilePool.DebugLightAlpha = _settings.DebugLightAlpha;

        if (CameraSource != null) _source = CameraSource;
        else if (Camera is IWorldCameraSource s) _source = s;
        else if (Camera != null) _source = _adapter = new Camera3DSource(Camera);

        // Curved vertices sink below the flat tile AABBs: widen the cull box by the drop at the camera's far plane.
        float far = Camera?.Far ?? 30000f;
        TilePool.CurveCullMargin = (float)(far * (double)far * Curvature / (2.0 * EarthRadiusM));

        _tileRoot = new Node3D { Name = "Tiles" };
        AddChild(_tileRoot);
        _pool = new TilePool(_tileRoot, _settings);
        _pool.Prewarm(_settings.PrewarmTiles);

        Streamer = WorldStreamerFacade.Create();
        if (Streamer.IsAvailable) GD.Print($"[WorldView] library api {Streamer.ApiVersionText}");
        _fakeFade = _settings.DebugFakeFade;
        Cover.FadesEnabled = Streamer.HasPollFades || _fakeFade >= 0f;
        GD.Print($"[WorldView] poll_fades {(Cover.FadesEnabled ? "available: dithered LOD fades on" : "missing in this Rust build: instant sibling swaps")}");
        if (Streamer.Node is { } node)
        {
            node.Name = "WorldStreamer";
            AddChild(node);
            Streamer.StatusChanged += OnStatusChanged;
            Streamer.OriginShifted += OnOriginShifted;
        }
    }

    /// <summary>
    /// Builds what the view needs and configures the world from the properties (origin, tile source, cache folder, profile).
    /// Runs from <c>_Ready</c> when <see cref="AutoStart"/> is on. Failures are logged and exposed as <see cref="ConfigureError"/>; the
    /// view then runs without a world.
    /// </summary>
    public void Start()
    {
        if (_started) return;
        _started = true;
        Initialize();
        if (!Streamer.IsAvailable || Streamer.Node == null) return;
        if (double.IsNaN(OriginLat) || double.IsNaN(OriginLon))
        {
            ConfigureError = "OriginLat / OriginLon are not set";
            GD.PushError("[WorldView] OriginLat / OriginLon are not set; running without a world.");
            return;
        }
        _tileUrl = TileUrl.Trim();
        TileSource = _tileUrl.Length == 0 ? "synthetic city" : _tileUrl;
        GD.Print($"[WorldView] tile source: {TileSource}");
        _cacheDir = CacheDir.Length == 0 ? "" : CacheDir.Contains("://") ? ProjectSettings.GlobalizePath(CacheDir) : CacheDir;
        if (_cacheDir.Length > 0) DirAccess.MakeDirRecursiveAbsolute(_cacheDir);
        ConfigureWorld(_tileUrl, _cacheDir);
        GD.Print($"[WorldView] WorldStreamer created, cache_dir={_cacheDir}, debug_tiles={Streamer.HasDebugTiles}");
    }

    /// <summary>The library's default world profile (world_profile() of an unconfigured streamer), for hosts that derive overrides from it.</summary>
    public Godot.Collections.Dictionary DefaultProfile()
    {
        Initialize();
        return Streamer.IsAvailable ? Streamer.WorldProfile() : new Godot.Collections.Dictionary();
    }

    /// <summary>
    /// Time of day for the shaders: writes the global uniforms <c>simworld_hour</c> and <c>simworld_night_factor</c> (light sprites
    /// fade in with the night factor). The host owns the clock and calls this whenever it changes.
    /// </summary>
    public void SetTime(float localHour, float nightFactor)
    {
        Hour = localHour;
        NightFactor = nightFactor;
        ShaderGlobals.EnsureRegistered();
        RenderingServer.GlobalShaderParameterSet(ShaderGlobals.Hour, localHour);
        RenderingServer.GlobalShaderParameterSet(ShaderGlobals.NightFactor, nightFactor);
    }

    public override void _ExitTree()
    {
        // Deterministic stop before the node is freed: waits (bounded 1 s) for cache I/O and releases the cache_dir lock.
        if (Streamer.IsAvailable)
        {
            Streamer.StatusChanged -= OnStatusChanged;
            Streamer.OriginShifted -= OnOriginShifted;
            if (Streamer.HasHostApi) GD.Print("[WorldView] shutdown (releasing the cache lock)");
            Streamer.Shutdown();
        }
    }

    private void OnStatusChanged(StreamerStatus st)
    {
        Status = st;
        var line = $"status: {st.State} - {st.Message}";
        if (line == _lastLoggedStatus) return;
        _lastLoggedStatus = line;
        GD.Print($"[WorldView] {line}");
    }

    // Diagnostic only: the bridge shifts from take_origin_shift() (same-frame ordering); never consume the shift here.
    private void OnOriginShifted(Vector3 shift)
    {
        SignalShiftCount++;
        LastSignalShift = shift;
    }

    // ---- configuration ---------------------------------------------------

    private static readonly string[] BandEdgeKeys = { "r0_edge_m", "r1_edge_m", "r2_edge_m", "r3_edge_m" };

    /// <summary>
    /// configure_world (api 0.6+, with the profile) or the legacy configure. On a profile error the world is retried with the
    /// default profile; any other failure (e.g. cache_dir in use) leaves the view running without a world.
    /// </summary>
    private void ConfigureWorld(string url, string cacheDir)
    {
        var overrides = BaseProfileOverrides;
        var profilePath = ProfilePath.Length == 0 ? "" : ProfilePath.Contains("://") ? ProjectSettings.GlobalizePath(ProfilePath) : System.IO.Path.GetFullPath(ProfilePath);
        ProfileOverrides = overrides;
        bool haveProfileArgs = profilePath.Length > 0 || overrides.Count > 0;
        if (profilePath.Length > 0) GD.Print($"[WorldView] profile file: {profilePath}");
        if (overrides.Count > 0) GD.Print($"[WorldView] profile overrides: {Godot.Json.Stringify(overrides)}");

        if (!Streamer.HasConfigureWorld)
        {
            if (haveProfileArgs) GD.PushWarning($"[WorldView] profile ignored: library api {Streamer.ApiVersionText} has no configure_world (needs 0.6)");
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
            GD.PushError($"[WorldView] profile rejected by the library: {result.Error}");
            GD.Print("[WorldView] retrying with the default world profile");
            ProfileOverrides = new Godot.Collections.Dictionary();
            ProfilePath = "";
            BaseProfileOverrides = new Godot.Collections.Dictionary();
            options.Remove("profile_path");
            options.Remove("profile_overrides");
            result = Streamer.ConfigureWorld(options);
        }
        if (!result.Ok)
        {
            ConfigureError = result.Error.Length > 0 ? result.Error : "configure_world failed";
            GD.PushError($"[WorldView] configure_world failed: {ConfigureError}. Running without a world (no tiles will load).");
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
        if (Attributions.Length > 0) GD.Print($"[WorldView] attribution: {string.Join(" | ", Attributions)}");
        else if (Streamer.HasHostApi) GD.Print("[WorldView] attribution: none required (synthetic city)");

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
        if (BandsText.Length > 0) GD.Print($"[WorldView] effective {BandsText}");
    }

    public override void _Process(double delta)
    {
        if (_source != null && Streamer.IsAvailable)
        {
            _adapter?.Tick(delta);
            // A queued reconfigure runs first, so this frame's update_camera already carries the new origin and camera position.
            if (_pendingOverrides != null) RunReconfigure();
            // Height above ground = camera y - terrain height under it (0 until the DEM spike).
            var pos = _source.GlobalPosition;
            CameraGroundHeight = Streamer.GroundHeightAt(pos.X, pos.Z);
            _source.SetGroundHeight(CameraGroundHeight);
            if (ExternalCameraUpdate != null) ExternalCameraUpdate(delta);
            else Streamer.UpdateCamera(_source.GlobalPosition, _source.Velocity, _source.AltitudeAgl);

            // Shift first so tiles polled below (already in the new frame) are not shifted twice.
            var shift = Streamer.TakeOriginShift();
            LastShift = shift;
            if (shift != Vector3.Zero)
            {
                OriginShiftCount++;
                TotalShift += shift;
                _source.ApplyOriginShift(shift);
                Cover.Shift(shift);
                OriginShifted?.Invoke(shift);
            }
            RenderingServer.GlobalShaderParameterSet(ShaderGlobals.CurveCenter, _source.GlobalPosition);
            RenderingServer.GlobalShaderParameterSet(ShaderGlobals.CurveInv2R, (float)(Curvature / (2.0 * EarthRadiusM)));

            // Poll one tile at a time so a single heavy tile cannot drag a second one into the same
            // frame: stop as soon as the budget is spent (Rust keeps the rest queued).
            long t0 = (long)Time.GetTicksUsec();
            int n0 = ReadyTotal;
            for (int n = 0; n < _settings.MaxTilesPerFrame; n++)
            {
                var ready = Streamer.PollReady(1);
                if (ready.Count == 0) break;
                foreach (var d in ready)
                {
                    if (WorldStreamerFacade.TryParseReady(d, out var tile)) UploadTile(in tile);
                }
                if ((long)Time.GetTicksUsec() - t0 >= _settings.UploadBudgetUs) break;
            }
            PollFades();
            var unload = Streamer.PollUnload();
            for (int i = 0; i < unload.Length; i++) UnloadTile(unload[i]);
            UploadUsLastFrame = (long)Time.GetTicksUsec() - t0;
            if (n0 != ReadyTotal) { UploadFrames++; if (UploadUsLastFrame > _settings.UploadBudgetUs) UploadFramesOverBudget++; }
            if (UploadUsLastFrame > _secUploadMax) _secUploadMax = UploadUsLastFrame;
        }

        UpdateSpriteScale();

        _sinceCheck += delta;
        if (_settings.VerifyIntervalS > 0 && _sinceCheck >= _settings.VerifyIntervalS)
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
            GD.PushError($"[WorldView] ready tile with unparsable key '{tile.Key}'");
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
            GD.Print($"[WorldView] first tile {tile.Key} ring={tile.Ring} origin={tile.Origin} lights={tile.Lights.Length} tris={nodes.Tris} groundTris={nodes.GroundTris}");
            // One-time stats block so the headless smoke test shows the debug_stats contract is live.
            GD.Print($"[WorldView] debug_stats {Godot.Json.Stringify(Streamer.DebugStats())}");
        }
        if (us > 2000 && _slowLogs < 40)
        {
            _slowLogs++;
            GD.Print($"[WorldView] slow upload {tile.Key} ring={tile.Ring} tris={nodes.Tris} groundTris={nodes.GroundTris} lights={tile.Lights.Length} {us / 1000.0:0.00} ms (mesh {(tm1 - tm0) / 1000.0:0.00}, ground {(tm1b - tm1) / 1000.0:0.00}, lights {(tm2 - tm1b) / 1000.0:0.00}) {Describe(tile.Mesh)}");
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
        else GD.PushError($"[WorldView] reconfigure rejected, previous world keeps running: {error}");
        Reconfigured?.Invoke(ok, error);
    }

    private bool TryReconfigure(Godot.Collections.Dictionary panel, out string error)
    {
        error = "";
        if (!Streamer.HasConfigureWorld) { error = $"configure_world needs api 0.6 (library {Streamer.ApiVersionText})"; return false; }
        if (_source == null) { error = "no camera assigned"; return false; }
        long t0 = (long)Time.GetTicksUsec();

        // New origin = where the camera is now (f32 geodetic: 0.1-0.4 m). Without a running world there is nothing to convert from.
        var cam = _source.GlobalPosition;
        bool recenter = WorldRunning;
        double lat = OriginLat, lon = OriginLon;
        if (recenter)
        {
            var geo = Streamer.LocalToGeodetic(cam);
            if (geo == Vector3.Zero) { error = "local_to_geodetic failed (camera position not convertible)"; return false; }
            lat = geo.X;
            lon = geo.Y;
        }

        var merged = (Godot.Collections.Dictionary)BaseProfileOverrides.Duplicate(true);
        MergeInto(merged, panel);
        var options = WorldStreamerFacade.MakeWorldOptions(lat, lon, _tileUrl, _cacheDir);
        var profilePath = ProfilePath.Length == 0 ? "" : ProfilePath.Contains("://") ? ProjectSettings.GlobalizePath(ProfilePath) : System.IO.Path.GetFullPath(ProfilePath);
        if (profilePath.Length > 0) options["profile_path"] = profilePath;
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
            GD.PushWarning($"[WorldView] reconfigure: {_dropScratch.Count} tiles were not in the library's unload batch of {unload.Length}; released anyway");
        _fading.Clear();
        _suspectRoots.Clear();
        _pendingRoots.Clear();

        if (recenter) _source.ApplyOriginShift(new Vector3(cam.X, 0f, cam.Z));
        LastShift = Vector3.Zero;
        TotalShift = Vector3.Zero;
        ProfileOverrides = merged;
        WorldRunning = true;
        ConfigureError = "";
        Attributions = result.Attributions;
        AfterConfigure();
        GD.Print($"[WorldView] reconfigured: origin=({lat:0.000000}, {lon:0.000000}) camera -> (0, {cam.Y:0.0}, 0) from ({cam.X:0.0}, {cam.Z:0.0}), "
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
        if (p.TryGetValue("lights", out var lt) && lt.VariantType == Variant.Type.Dictionary && lt.AsGodotDictionary().TryGetValue("min_ring", out var mr))
            sb.Append(sb.Length > 0 ? ", " : "").Append("lights from R").Append(mr.AsInt64());
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
            GD.PushError($"[WorldView] cover consistency: {errors} problem(s)\n{_report}");
        }
        else if (ConsistencyChecks == 1 || Cover.Loaded > 0 && ConsistencyChecks % 12 == 0)
        {
            GD.Print($"[WorldView] cover consistent: {Cover.Loaded} tiles loaded, visible R0..R3 = "
                + $"{Cover.VisibleByRing[0]}/{Cover.VisibleByRing[1]}/{Cover.VisibleByRing[2]}/{Cover.VisibleByRing[3]}, "
                + $"hidden by children {Cover.HiddenByChildren}, waiting siblings {Cover.WaitingSiblings}");
        }
    }
}
