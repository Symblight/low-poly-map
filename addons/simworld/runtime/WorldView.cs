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
    /// <summary>
    /// World seed (api 1.7, docs/spec-world-seed.md): path of a <c>world.json</c> manifest (<c>res://</c>, <c>user://</c> or an OS path). Non-empty
    /// turns the view into a seed world host: every vector tile is generated from the seed files, so <see cref="TileUrl"/> is ignored (a
    /// warning is logged if it is set), the terrain layer is switched on (<see cref="EnableTerrain"/> / <see cref="TerrainEnabled"/>), the
    /// profile overrides <c>terrain.flat_radius_m</c> / <c>flat_blend_m</c> = 0 are sent unless the host's overrides set them, and
    /// <see cref="CacheDir"/> is not used (S1 has no URL source; a seed world only needs the cache lock for a city with a real URL, which S1 does not use).
    /// Set before <see cref="Start"/>; the settings panel's reconfigure keeps it. Needs api 1.7.
    /// </summary>
    [Export(PropertyHint.File, "*.json")] public string SeedWorld { get; set; } = "";
    /// <summary>Disk cache folder: <c>cache://</c> (under <c>OS.GetCacheDir()</c>, purgeable and not backed up; on iOS <c>Library/Caches</c>),
    /// <c>user://</c> or an OS path. Never <c>user://</c> on iOS: it maps to <c>Documents</c>, which iCloud backs up.
    /// Must be unique per instance (contract 11a). Empty = no disk cache.</summary>
    [Export] public string CacheDir { get; set; } = "cache://simworld/tiles";
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

    /// <summary>
    /// Host opt-in for the terrain layer (api 1.5): sends <c>layers: ["world", "terrain"]</c> to <c>configure_world</c>, creates the terrain
    /// pool and draws the <c>t/z/x/y</c> tiles the library then delivers. Default false = exactly the flat world of before (no "t/" key
    /// ever arrives). The library also needs the profile key <c>terrain.enabled</c>: set <see cref="TerrainEnabled"/> or put it in
    /// <see cref="BaseProfileOverrides"/> / the profile file. Set before <see cref="Start"/>; change it later through
    /// <see cref="ApplyProfileOverrides"/>.
    /// </summary>
    [Export] public bool EnableTerrain { get; set; }
    /// <summary>Sets the profile override <c>terrain.enabled = true</c> at every configure (unless the overrides already carry the key). Without
    /// <see cref="EnableTerrain"/> the library ignores it and stays flat, with one warning.</summary>
    [Export] public bool TerrainEnabled { get; set; }

    /// <summary>
    /// Optional physics collision for the ground (default false: no body exists). One <c>StaticBody3D</c> with a <c>HeightMapShape3D</c> patch of
    /// <see cref="TerrainCollisionSamples"/>^2 samples at <see cref="TerrainCollisionSpacingM"/> centred under the camera, filled from
    /// <c>ground_height_at</c> and re-centred as the camera moves (see <see cref="SimWorld.TerrainCollision"/>). Buildings and far terrain have no collision.
    /// Works with or without the terrain layer (flat at 0 m without it).
    /// </summary>
    [Export] public bool TerrainCollisionEnabled { get; set; }
    /// <summary>Samples per side of the collision patch (odd, default 33).</summary>
    [Export(PropertyHint.Range, "3,129,2")] public int TerrainCollisionSamples { get; set; } = 33;
    /// <summary>Distance between collision samples in metres (default 40, so the 33 x 33 patch is about 1.3 km wide).</summary>
    [Export] public float TerrainCollisionSpacingM { get; set; } = 40f;

    /// <summary>True when this view runs a seed world (api 1.7): <see cref="SeedWorld"/> was set and the library has the phase S1 API.</summary>
    public bool SeedWorldActive { get; private set; }
    /// <summary>seed_world_info() as read after the last configure; null without a seed world.</summary>
    public SeedWorldInfo? SeedInfo { get; private set; }
    /// <summary>airports() in the current local frame (re-read on configure and on every origin shift); empty without a seed world.</summary>
    public AirportInfo[] Airports { get; private set; } = System.Array.Empty<AirportInfo>();
    /// <summary>The runway marking helper; null until the view is initialized.</summary>
    public AirportMarkings? Markings => _markings;

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
    /// <summary>Terrain layer tile map (api 1.5): its own quadtree, root / leaf zoom from the profile (terrain.far_zoom / near_zoom, default 10..14),
    /// ring statistics for 5 rings. Empty unless <see cref="TerrainActive"/>.</summary>
    public TileCover TerrainCover { get; } = new(10, 14);
    /// <summary>Debug aid: false hides the whole vector layer (buildings, ground, lights) so the terrain can be inspected on its own; the streaming is unchanged.</summary>
    public bool VectorVisible
    {
        get => _tileRoot?.Visible ?? true;
        set { if (_tileRoot != null) _tileRoot.Visible = value; }
    }
    /// <summary>True when the terrain layer runs: the host asked for it, the library has it (api 1.5) and the profile enables it.</summary>
    public bool TerrainActive { get; private set; }
    /// <summary>Slant reach of the terrain layer in metres (last band edge), 0 while inactive: the camera far plane and the day fog follow it.</summary>
    public float TerrainReachM { get; private set; }
    /// <summary>Rings of the terrain layer (terrain.bands entries), 0 while inactive.</summary>
    public int TerrainRingCount => TerrainActive ? TerrainCover.Rings : 0;
    /// <summary>The set_terrain_detail scale in effect (1 = profile).</summary>
    public double TerrainDetail { get; private set; } = 1.0;
    /// <summary>Terrain ready deliveries (first and re-deliveries), re-deliveries replaced in place, and unloads.</summary>
    public int TerrainReadyTotal { get; private set; }
    public int TerrainReplacedTotal { get; private set; }
    public int TerrainUnloadedTotal { get; private set; }
    /// <summary>Terrain keys ("t/z/x/y") seen in poll_ready, poll_fades or poll_unload since start. Must stay 0 for a host that did not opt in.</summary>
    public int TerrainKeysSeen { get; private set; }
    /// <summary>Per tile upload cost in microseconds for the terrain layer, indexed by ring (up to 8).</summary>
    public readonly long[] TerrainUploadUsLast = new long[StreamerStats.TerrainRingsMax];
    public readonly long[] TerrainUploadUsMax = new long[StreamerStats.TerrainRingsMax];
    public readonly long[] TerrainUploadUsSum = new long[StreamerStats.TerrainRingsMax];
    public readonly int[] TerrainUploadCount = new int[StreamerStats.TerrainRingsMax];
    /// <summary>The ground collision helper; null while <see cref="TerrainCollisionEnabled"/> is off.</summary>
    public TerrainCollision? TerrainCollision => _collision;
    /// <summary>Tiles held in both layers.</summary>
    public int TotalLoaded => Cover.Loaded + TerrainCover.Loaded;

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
    public int TerrainPoolFree => _terrainPool?.FreeCount ?? 0;
    public int TerrainPoolCreated => _terrainPool?.TotalCreated ?? 0;
    /// <summary>The shared opaque terrain material (tests, debug UI).</summary>
    public ShaderMaterial? TerrainMaterial => _terrainPool?.Material;
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
    /// <summary>Terrain height under the camera in meters (Rust ground_height_at; 0 unless the terrain layer is active).</summary>
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
    private TerrainPool? _terrainPool;
    private Node3D? _terrainRoot;
    private TerrainCollision? _collision;
    private bool _loggedFirstTerrainTile;
    private bool _warnedNoTerrainLayer;
    private readonly HashSet<long> _pendingRootsT = new();
    private readonly HashSet<long> _suspectRootsT = new();
    private float _emissionLevel = 1f;
    private Vector3 _lightFog = new(1500f, 9000f, 1f); // NightLook fog defaults: begin, end, curve
    private Node3D? _tileRoot;
    private AirportMarkings? _markings;

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
        // Vector tiles never reach beyond ~29 km (R3 + hysteresis), whatever the far plane: a margin sized for a 150 km terrain far plane would only
        // keep tiles in the frustum that are not visible (about 2x the vector draw calls). The terrain pool sizes its own margin (TerrainPool.CullMargin).
        float far = System.Math.Min(Camera?.Far ?? 30000f, 32000f);
        TilePool.CurveCullMargin = (float)(far * (double)far * Curvature / (2.0 * EarthRadiusM));

        _tileRoot = new Node3D { Name = "Tiles" };
        AddChild(_tileRoot);
        _pool = new TilePool(_tileRoot, _settings);
        _pool.SetEmissionLevel(_emissionLevel);
        _pool.SetLightFog(_lightFog.X, _lightFog.Y, _lightFog.Z);
        _pool.Prewarm(_settings.PrewarmTiles);
        _terrainRoot = new Node3D { Name = "TerrainTiles" };
        AddChild(_terrainRoot);
        _terrainPool = new TerrainPool(_terrainRoot, _settings);
        _markings = new AirportMarkings(this, _settings, AddonPaths.Root + "/shaders");

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
        SeedWorldActive = false;
        if (SeedWorld.Trim().Length > 0)
        {
            if (!Streamer.HasSeedWorld)
            {
                ConfigureError = $"SeedWorld needs library api 1.{WorldStreamerFacade.SeedWorldMinApiMinor}+ (library {Streamer.ApiVersionText}): rebuild ../rust";
                GD.PushError($"[WorldView] {ConfigureError}; running without a world.");
                return;
            }
            SeedWorldActive = true;
            if (_tileUrl.Length > 0) GD.PushWarning($"[WorldView] SeedWorld is set: TileUrl '{_tileUrl}' is ignored (a seed world generates every tile)");
            _tileUrl = "";
            EnableTerrain = TerrainEnabled = true; // a seed world requires the terrain layer (contract 3.1)
        }
        TileSource = SeedWorldActive ? $"seed world {SeedWorld.Trim()}" : _tileUrl.Length == 0 ? "synthetic city" : _tileUrl;
        GD.Print($"[WorldView] tile source: {TileSource}");
        // A seed world without URL sources needs no disk cache and takes no lock; with a cache_dir and a city that has real.url it would lock it.
        _cacheDir = SeedWorldActive ? "" : ResolveCacheDir(CacheDir.Trim());
        if (_cacheDir.Length > 0) DirAccess.MakeDirRecursiveAbsolute(_cacheDir);
        ConfigureWorld(_tileUrl, _cacheDir);
        GD.Print($"[WorldView] WorldStreamer created, cache_dir={_cacheDir}, debug_tiles={Streamer.HasDebugTiles}");
    }

    private const string CachePrefix = "cache://";

    /// <summary><c>cache://a/b</c> -> <c>OS.GetCacheDir()/&lt;project name&gt;/a/b</c> (the desktop cache folder is shared by all apps);
    /// other <c>://</c> paths through <c>GlobalizePath</c>; OS paths and "" unchanged.</summary>
    private static string ResolveCacheDir(string dir)
    {
        if (!dir.StartsWith(CachePrefix, System.StringComparison.Ordinal))
            return dir.Contains("://") ? ProjectSettings.GlobalizePath(dir) : dir;
        string app = ProjectSettings.GetSetting("application/config/name").AsString().Trim();
        foreach (char c in ":/\\?*\"|%<>") app = app.Replace(c, '_');
        return OS.GetCacheDir().PathJoin(app.Length > 0 ? app : "simworld").PathJoin(dir.Substring(CachePrefix.Length));
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
        UpdateWindowShares(false);
    }

    /// <summary>Hour change (hours, wrapping at 24) that triggers a new <c>window_share</c> upload.</summary>
    private const float WindowShareStepH = 0.01f;
    private readonly float[] _shares = new float[WindowProfile.ProfileCount];
    private float _sharesHour = float.NaN;
    private WindowProfile? _windowProfile;
    /// <summary>Lit-window share per light profile id last written to the building materials (8 entries, ids 3 5 6 7 are 0).</summary>
    public System.ReadOnlySpan<float> WindowShares => _shares;
    /// <summary>The <c>window_share</c> array as read back from the plain / dithering building material (tests).</summary>
    public float[] ReadMaterialWindowShares(bool fadeMaterial) => _pool?.ReadWindowShares(fadeMaterial) ?? System.Array.Empty<float>();
    /// <summary>True when the shares come from the world profile's lit_share tables (false = the TilePool fallback values).</summary>
    public bool WindowSharesFromProfile => _windowProfile != null;
    /// <summary>Number of <c>window_share</c> uploads so far (tests: it must not grow every frame).</summary>
    public int WindowShareUploads { get; private set; }

    /// <summary>
    /// Writes the lit-window share of every profile at the current <see cref="Hour"/> to both building materials: the profile's hourly
    /// table interpolated, or the pool's fallback values when no profile is readable. Skipped while the hour moved by less than
    /// 0.01 h (<paramref name="force"/> overrides, e.g. after the profile changed).
    /// </summary>
    private void UpdateWindowShares(bool force)
    {
        if (_pool == null) return;
        float d = Mathf.Abs(Hour - _sharesHour);
        d = Mathf.Min(d, 24f - d); // the clock wraps at 24 h
        if (!force && !float.IsNaN(_sharesHour) && d <= WindowShareStepH) return;
        _sharesHour = Hour;
        if (_windowProfile != null) _windowProfile.SharesAt(Hour, _shares);
        else System.Array.Copy(TilePool.FallbackWindowShares, _shares, _shares.Length);
        _pool.SetWindowShares(_shares);
        WindowShareUploads++;
    }

    /// <summary>Reads colours, floodlight and lit_share tables from the world profile and pushes them to the pool (after every configure).</summary>
    private void ApplyWindowProfile(Godot.Collections.Dictionary profile)
    {
        _windowProfile = WindowProfile.FromWorldProfile(profile);
        if (_pool == null) return;
        // Street lamp height (api 1.1): the light pools sit this far below each lamp.
        if (profile.TryGetValue("lights", out var lv) && lv.VariantType == Variant.Type.Dictionary
            && lv.AsGodotDictionary().TryGetValue("street", out var sv) && sv.VariantType == Variant.Type.Dictionary
            && sv.AsGodotDictionary().TryGetValue("height_m", out var hv))
            _pool.SetStreetLampHeight((float)hv.AsDouble());
        if (_windowProfile != null)
        {
            _pool.SetProfileColors(_windowProfile.Colors);
            _pool.SetFloodlight(_windowProfile.FloodlightColor, _windowProfile.FloodlightStrength, _windowProfile.FloodlightFadeHeightM);
            GD.Print($"[WorldView] window profile: lit_share tables for {CountTables(_windowProfile)} light profiles, floodlight strength {_windowProfile.FloodlightStrength:0.##} over {_windowProfile.FloodlightFadeHeightM:0.#} m");
        }
        else GD.PushWarning("[WorldView] world profile has no lights.profiles lit_share tables (library older than api 1.0?); using the built-in window shares and colours");
        UpdateWindowShares(true);
    }

    private static int CountTables(WindowProfile wp)
    {
        int n = 0;
        foreach (var t in wp.LitShare) if (t != null) n++;
        return n;
    }

    /// <summary>
    /// HDR level a lit window reaches (windows emit 0.65 of it, light sprites 1.6 x color): the host passes
    /// <see cref="NightLook.EmissionLevel"/> so the glow threshold (0.82 x EmissionLevel) stays above windows. Remembered if the pool does not exist yet.
    /// </summary>
    public void SetEmissionLevel(float level)
    {
        _emissionLevel = level;
        _pool?.SetEmissionLevel(level);
    }

    /// <summary>
    /// The environment's depth fog (<see cref="NightLook.FogBeginM"/> / <see cref="NightLook.FogEndM"/> / <see cref="NightLook.FogCurve"/>)
    /// for the light sprites, which are additive and apply it by hand. Remembered if the pool does not exist yet.
    /// </summary>
    public void SetLightFog(float beginM, float endM, float curve)
    {
        _lightFog = new Vector3(beginM, endM, curve);
        _pool?.SetLightFog(beginM, endM, curve);
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
        var overrides = ComposeOverrides(null);
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
        AddTerrainLayerOption(options);
        AddSeedWorldOption(options);
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
            if (TerrainEnabled) options["profile_overrides"] = ComposeOverrides(null);
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

    /// <summary>The base profile overrides plus <c>terrain.enabled</c> from <see cref="TerrainEnabled"/> (unless the overrides set the key), then <paramref name="panel"/> merged over (panel wins).</summary>
    private Godot.Collections.Dictionary ComposeOverrides(Godot.Collections.Dictionary? panel)
    {
        var merged = (Godot.Collections.Dictionary)BaseProfileOverrides.Duplicate(true);
        if (TerrainEnabled)
        {
            Godot.Collections.Dictionary t;
            if (merged.TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary) t = tv.AsGodotDictionary();
            else merged["terrain"] = t = new Godot.Collections.Dictionary();
            if (!t.ContainsKey("enabled")) t["enabled"] = true;
        }
        if (SeedWorldActive || SeedWorld.Trim().Length > 0)
        {
            // Contract 3.1: a seed world needs terrain.enabled and no flat zone (its runway strips and water rule replace it). Explicit host values win
            // (and fail in the library with its error text, which the panel shows).
            Godot.Collections.Dictionary t;
            if (merged.TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary) t = tv.AsGodotDictionary();
            else merged["terrain"] = t = new Godot.Collections.Dictionary();
            if (!t.ContainsKey("enabled")) t["enabled"] = true;
            if (!t.ContainsKey("flat_radius_m")) t["flat_radius_m"] = 0.0;
            if (!t.ContainsKey("flat_blend_m")) t["flat_blend_m"] = 0.0;
        }
        if (panel != null) MergeInto(merged, panel);
        return merged;
    }

    /// <summary>Adds the <c>seed_world</c> option (api 1.7) for a seed world host. OS paths are made absolute (relative to the cwd); <c>res://</c> and <c>user://</c> go as they are.</summary>
    private void AddSeedWorldOption(Godot.Collections.Dictionary options)
    {
        if (!SeedWorldActive) { WorldStreamerFacade.AddSeedWorld(options, ""); return; }
        var path = SeedWorld.Trim();
        if (!path.Contains("://")) path = System.IO.Path.GetFullPath(path);
        WorldStreamerFacade.AddSeedWorld(options, path);
    }

    /// <summary>Adds the <c>layers</c> opt-in when the host wants the terrain and the library has it (api 1.5); warns once when it has not.</summary>
    private void AddTerrainLayerOption(Godot.Collections.Dictionary options)
    {
        bool want = EnableTerrain;
        if (want && !Streamer.HasTerrainLayer)
        {
            if (!_warnedNoTerrainLayer) GD.PushWarning($"[WorldView] EnableTerrain ignored: library api {Streamer.ApiVersionText} has no terrain layer (needs 1.5); the world stays flat");
            _warnedNoTerrainLayer = true;
            want = false;
        }
        WorldStreamerFacade.AddLayers(options, want);
        // Keep the flat zone (and so the relief) where the first configure put it: a reconfigure may move the origin (api 1.6).
        WorldStreamerFacade.AddTerrainCenter(options, want && Streamer.HasTerrainActiveFlag, OriginLat, OriginLon);
    }

    /// <summary>After a configure: reads the profile's terrain section, resets the terrain tile map to its zoom range and the pool to its shading.</summary>
    private void ConfigureTerrainLayer(Godot.Collections.Dictionary profile)
    {
        TerrainActive = false;
        TerrainReachM = 0f;
        if (EnableTerrain && Streamer.HasTerrainLayer
            && profile.TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary)
        {
            var t = tv.AsGodotDictionary();
            // api 1.6: the library says whether the terrain is active; before that the host re-derives the rule from the profile.
            bool enabled = Streamer.HasTerrainActiveFlag ? Status.TerrainActive : t.TryGetValue("enabled", out var ev) && ev.AsBool();
            if (enabled && t.TryGetValue("near_zoom", out var nz) && t.TryGetValue("far_zoom", out var fz))
            {
                if (TerrainCover.Loaded != 0)
                    GD.PushError($"[WorldView] terrain tile map still holds {TerrainCover.Loaded} tiles at configure; releasing them");
                var stray = new List<long>();
                foreach (var n in TerrainCover.All) stray.Add(n.Packed);
                foreach (long k in stray) ReleaseTerrainTile(k);
                TerrainCover.Configure((int)fz.AsInt64(), (int)nz.AsInt64());
                _terrainPool!.SetSmoothShading(t.TryGetValue("shading", out var sh) && sh.AsString() == "smooth");
                if (t.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Array)
                {
                    var bands = bv.AsGodotArray();
                    if (bands.Count > 0 && bands[bands.Count - 1].VariantType == Variant.Type.Dictionary
                        && bands[bands.Count - 1].AsGodotDictionary().TryGetValue("edge_m", out var edge))
                        TerrainReachM = (float)edge.AsDouble();
                }
                TerrainActive = true;
                // Curved terrain sinks below its flat AABB by d^2/2R up to the far plane / the terrain reach: widen the cull box accordingly.
                double farM = System.Math.Max(Camera?.Far ?? 0f, TerrainReachM * 1.15);
                _terrainPool!.CullMargin = (float)(farM * farM * Curvature / (2.0 * EarthRadiusM));
            }
        }
        if (!TerrainActive && TerrainCover.Loaded == 0) TerrainCover.Configure(TerrainCover.RootZ, TerrainCover.LeafZ);
        _collision?.Invalidate(); // new world: the patch was filled from the old one
        GD.Print(TerrainActive
            ? $"[WorldView] terrain layer on: zoom {TerrainCover.RootZ}..{TerrainCover.LeafZ} ({TerrainCover.Rings} rings), reach {TerrainReachM / 1000f:0.#} km, shading {(_terrainPool != null ? "per profile" : "-")}"
            : EnableTerrain ? "[WorldView] terrain layer requested but the profile has terrain.enabled = false (or the library lacks it): no terrain tiles"
                            : "[WorldView] terrain layer off");
    }

    /// <summary>
    /// terrain detail (api 1.5): multiplies every terrain band's max_error_m by <paramref name="scale"/> (the library clamps to [0.25, terrain.detail_scale_max]).
    /// Held terrain tiles are re-delivered under the same keys and replaced in place; no teleport, the vector world is untouched.
    /// Returns the scale in effect (NaN on a library without the method).
    /// </summary>
    public double SetTerrainDetail(double scale)
    {
        double r = Streamer.SetTerrainDetail(scale);
        if (!double.IsNaN(r)) TerrainDetail = r;
        return r;
    }

    private void AfterConfigure()
    {
        Status = Streamer.Status();
        if (Attributions.Length == 0) Attributions = Streamer.Attributions();
        if (Attributions.Length > 0) GD.Print($"[WorldView] attribution: {string.Join(" | ", Attributions)}");
        else if (Streamer.HasHostApi) GD.Print("[WorldView] attribution: none required (synthetic city)");

        var profile = Streamer.WorldProfile();
        ApplyWindowProfile(profile);
        ConfigureTerrainLayer(profile);
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
        RefreshSeed(true);
    }

    /// <summary>The drawn runway height above ground_height_at per ring R0..R3 as the host computes it (set by <see cref="ComputeRunwayLifts"/>; tests, overlay).</summary>
    public Vector4 RunwayLift { get; private set; } = new(1.2f, 3.6f, 9.5f, 25f);
    /// <summary>Which rule <see cref="RunwayLift"/> follows: "level" (generator 2: runway level x lift_step_m, no drape lift) or "drape replay" (generator 1 or unknown).</summary>
    public string RunwayLiftRule { get; private set; } = "drape replay";

    /// <summary>
    /// How far the drawn runway is above ground_height_at, per ring (the markings are laid on it). Generator 2 and later (<c>seed_world_info()["generator_version"]</c>,
    /// contract section 9 "Airports"): inside an airport fence the ground has no drape lift, the runway is at stacking level
    /// <see cref="WorldRenderSettings.MarkingRunwayLevel"/> (3) x <c>ground.rN.lift_step_m</c> and nothing else (0.06 / 0.18 / 0.45 / 1.5 m by default). It
    /// does NOT depend on the general drape lift. Generator 1 or a library without the key (older libraries): replays the old rule, lift(band N) = the largest
    /// <c>max_error_m</c> of the terrain bands whose range starts below <c>rN_edge_m x hysteresis</c>, times <c>terrain.detail_scale_max</c>, plus
    /// <c>MarkingRunwayLevel</c> x step; there the level must be set back to 10 (the old runway level), which this method does by itself.
    /// </summary>
    private void ComputeRunwayLifts(Godot.Collections.Dictionary profile)
    {
        if (_markings == null) return;
        int generator = SeedInfo?.GeneratorVersion ?? 0;
        bool levelRule = generator >= 2;
        // The old library drew the runway at level 10; the setting is the generator-2 level (3) and must not be applied to it.
        int level = levelRule ? _settings.MarkingRunwayLevel : LegacyRunwayLevel;
        var lift = new float[4] { 1.2f, 3.6f, 9.5f, 25f };
        var edge = new float[3] { 510f, 2125f, 5950f };
        if (levelRule) lift = new float[4] { 0.06f, 0.18f, 0.45f, 1.5f };
        try
        {
            var bands = profile["bands"].AsGodotDictionary();
            double hyst = bands["hysteresis"].AsDouble();
            var ground = profile["ground"].AsGodotDictionary();
            Godot.Collections.Array? tb = null;
            double detail = 1;
            if (!levelRule)
            {
                var terrain = profile["terrain"].AsGodotDictionary();
                detail = terrain["detail_scale_max"].AsDouble();
                tb = terrain["bands"].AsGodotArray();
            }
            for (int n = 0; n < 4; n++)
            {
                double rEdge = bands[BandEdgeKeys[n]].AsDouble();
                double step = ground["r" + n].AsGodotDictionary()["lift_step_m"].AsDouble();
                double worst = 0;
                if (tb != null)
                {
                    double reach = rEdge * hyst, prev = 0;
                    for (int k = 0; k < tb.Count; k++)
                    {
                        var b = tb[k].AsGodotDictionary();
                        if (prev < reach) worst = System.Math.Max(worst, b["max_error_m"].AsDouble());
                        prev = b["edge_m"].AsDouble();
                    }
                }
                lift[n] = (float)(worst * detail + level * step);
                if (n < 3) edge[n] = (float)(rEdge * (2.0 - hyst));
            }
        }
        catch (System.Exception e)
        {
            GD.PushWarning($"[WorldView] runway marking lifts: cannot read the profile ({e.Message}); using the default lifts");
        }
        RunwayLift = new Vector4(lift[0], lift[1], lift[2], lift[3]);
        RunwayLiftRule = levelRule ? "level" : "drape replay";
        _markings.SetRingLifts(RunwayLift, new Vector3(edge[0], edge[1], edge[2]));
        GD.Print($"[WorldView] runway lift per ring (generator {generator}, {(levelRule ? $"level {level} x lift_step_m, no drape lift" : "old drape replay")}): "
            + $"{lift[0]:0.###} / {lift[1]:0.###} / {lift[2]:0.###} / {lift[3]:0.###} m above ground_height_at");
    }

    /// <summary>Stacking level of the runway before generator 2 (contract section 9, the old drape replay).</summary>
    private const int LegacyRunwayLevel = 10;

    /// <summary>
    /// Seed world only: reads seed_world_info() (when <paramref name="full"/>, after a configure; logs the summary and the load-time warnings) and
    /// airports() in the current frame (also after every origin shift) and hands the airports to the marking helper.
    /// </summary>
    private void RefreshSeed(bool full)
    {
        if (!SeedWorldActive)
        {
            SeedInfo = null;
            Airports = System.Array.Empty<AirportInfo>();
            _markings?.SetAirports(Airports);
            return;
        }
        if (full)
        {
            SeedInfo = Streamer.SeedInfo();
            if (SeedInfo == null) GD.PushWarning("[WorldView] seed_world_info() is empty although a seed world was configured");
            else
            {
                GD.Print($"[WorldView] seed world: {SeedInfo.Summary}");
                foreach (var c in SeedInfo.Cities) GD.Print($"[WorldView]   city {c.Id} \"{c.Name}\" r={c.RadiusKm:0.#} km v{c.Version} source={c.Source} airports=[{string.Join(", ", c.Airports)}]");
                // Load-time validation warnings (e.g. approximately placed runways of the OurAirports cut) are data notes, not host problems: print, do not push.
                for (int i = 0; i < SeedInfo.Warnings.Length && i < 5; i++) GD.Print($"[WorldView]   seed warning: {SeedInfo.Warnings[i]}");
                if (SeedInfo.Warnings.Length > 5) GD.Print($"[WorldView]   ... and {SeedInfo.Warnings.Length - 5} more seed warnings (overlay F1 shows the first 3)");
            }
        }
        Airports = Streamer.Airports();
        _markings?.SetAirports(Airports);
        if (full) ComputeRunwayLifts(Streamer.WorldProfile());
        if (full) GD.Print($"[WorldView] airports(): {Airports.Length} with an open runway ({string.Join(" ", System.Linq.Enumerable.Select(Airports, a => $"{a.Ident}[{a.Runways.Length}]"))})");
    }

    public override void _Process(double delta)
    {
        if (_source != null && Streamer.IsAvailable)
        {
            _adapter?.Tick(delta);
            // A queued reconfigure runs first, so this frame's update_camera already carries the new origin and camera position.
            if (_pendingOverrides != null) RunReconfigure();
            // Height above ground = camera y - terrain height under it (0 unless the terrain layer is active).
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
                TerrainCover.Shift(shift);
                _collision?.Shift(shift);
                if (SeedWorldActive) RefreshSeed(false); // airport positions follow the floating origin
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
                    if (!WorldStreamerFacade.TryParseReady(d, out var tile)) continue;
                    if (tile.IsTerrain) UploadTerrainTile(in tile);
                    else UploadTile(in tile);
                }
                if ((long)Time.GetTicksUsec() - t0 >= _settings.UploadBudgetUs) break;
            }
            PollFades();
            var unload = Streamer.PollUnload();
            for (int i = 0; i < unload.Length; i++) UnloadTile(unload[i]);
            UploadUsLastFrame = (long)Time.GetTicksUsec() - t0;
            if (n0 != ReadyTotal) { UploadFrames++; if (UploadUsLastFrame > _settings.UploadBudgetUs) UploadFramesOverBudget++; }
            if (UploadUsLastFrame > _secUploadMax) _secUploadMax = UploadUsLastFrame;
            UpdateCollision();
            _markings?.Update(_source.GlobalPosition, delta);
        }

        UpdateSpriteScale();
        if (TerrainDebugLog && TerrainActive) LogTerrainState(delta);

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
                if (!TileKey.TryParse(kv.Key.AsString(), out long packed, out bool terrainKey)) continue;
                if (terrainKey) TerrainKeysSeen++;
                var cover = terrainKey ? TerrainCover : Cover;
                // A tile that is listed but not uploaded yet is picked up on the call after its upload.
                if (!cover.TryGet(packed, out var nodes)) continue;
                nodes.FadeStamp = _fadeFrame;
                float f = Mathf.Clamp((float)kv.Value.AsDouble(), 0f, 1f);
                if (f >= 1f)
                {
                    EndTileFade(nodes);
                    continue;
                }
                if (!nodes.Fading) _fading.Add(nodes);
                if (!nodes.Fading || nodes.Fade != f || !nodes.FadeMaterialActive)
                {
                    if (nodes is TerrainNodes tn) _terrainPool!.SetFade(tn, f);
                    else pool.SetFade(nodes, f);
                }
                cover.SetFade(nodes, true, f);
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
            if (!n.Fading || CoverOf(n).TryGet(n.Packed, out var live) && live != n)
            {
                _fading.RemoveAt(i); // unloaded in the meantime
                continue;
            }
            if (n.FadeStamp != _fadeFrame) EndTileFade(n);
        }
        Cover.ReleaseAwaiting();
        TerrainCover.ReleaseAwaiting();
    }

    private TileCover CoverOf(TileNodes nodes) => nodes is TerrainNodes ? TerrainCover : Cover;

    private void EndTileFade(TileNodes nodes)
    {
        if (nodes.Fading || nodes.FadeMaterialActive)
        {
            if (nodes is TerrainNodes tn) _terrainPool!.EndFade(tn);
            else _pool!.EndFade(nodes);
            CoverOf(nodes).SetFade(nodes, false, 1f);
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
        _pool.SetSpritePxPerDepth(k);
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
            nodes.LoadedMs = Time.GetTicksMsec();
            ChurnDeliver("world", tile.Key);
        }
        nodes.Root.Position = tile.Origin;

        long tm0 = (long)Time.GetTicksUsec();
        pool.SetMesh(nodes, tile.Mesh);
        long tm1 = (long)Time.GetTicksUsec();
        // Ground (every band; R3 since api 1.3): same root, so it is shown / hidden with the buildings. A delivery without
        // a "ground" key (older Rust build) leaves an existing ground mesh alone; an empty Array clears it.
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
        // Building lights exist on the bands lights.min_ring..R3 (R1..R3 by default); street lamps on every band.
        pool.SetLights(nodes, tile.Lights, tile.LightColors);
        pool.SetStreetLights(nodes, tile.StreetLights ?? System.Array.Empty<Vector3>(), tile.StreetLightColors ?? System.Array.Empty<Color>());
        pool.SetBeacons(nodes, tile.BeaconLights ?? System.Array.Empty<Vector3>(), tile.BeaconLightColors ?? System.Array.Empty<Color>());
        pool.SetAirportLights(nodes, tile.AirportLights ?? System.Array.Empty<Vector3>(), tile.AirportLightColors ?? System.Array.Empty<Color>());

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

    private void UploadTerrainTile(in ReadyTile tile)
    {
        long t0 = (long)Time.GetTicksUsec();
        TerrainKeysSeen++;
        if (!TileKey.TryParse(tile.Key, out long packed, out bool isTerrainKey) || !isTerrainKey)
        {
            GD.PushError($"[WorldView] terrain ready tile with unparsable key '{tile.Key}'");
            return;
        }
        var pool = _terrainPool!;
        TerrainNodes nodes;
        bool replace = TerrainCover.TryGet(packed, out var existing);
        if (replace)
        {
            // Same key delivered again (set_terrain_detail): replace the mesh in place, keep the node and its visibility.
            nodes = (TerrainNodes)existing;
            TerrainCover.BeginReplace(nodes);
            nodes.Ring = tile.Ring;
        }
        else
        {
            nodes = pool.Acquire(tile.Key, packed, tile.Ring);
            nodes.LoadedMs = Time.GetTicksMsec();
            ChurnDeliver("terrain", tile.Key);
        }
        nodes.Root.Position = tile.Origin;
        pool.SetMesh(nodes, tile.Terrain, tile.HMin, tile.HMax);
        if (replace)
        {
            TerrainCover.EndReplace(nodes);
            TerrainReplacedTotal++;
        }
        else TerrainCover.Add(nodes);
        TerrainReadyTotal++;

        long us = (long)Time.GetTicksUsec() - t0;
        int r = System.Math.Clamp(tile.Ring, 0, TerrainUploadUsLast.Length - 1);
        TerrainUploadUsLast[r] = us;
        if (us > TerrainUploadUsMax[r]) TerrainUploadUsMax[r] = us;
        TerrainUploadUsSum[r] += us;
        TerrainUploadCount[r]++;
        if (!_loggedFirstTerrainTile)
        {
            _loggedFirstTerrainTile = true;
            GD.Print($"[WorldView] first terrain tile {tile.Key} ring={tile.Ring} origin={tile.Origin} tris={nodes.Tris} h=[{tile.HMin:0.#}, {tile.HMax:0.#}] m");
        }
        if (nodes.Tris == 0) GD.PushWarning($"[WorldView] terrain tile {tile.Key} produced no surface (unusable arrays)");
        if (us > 2000 && _slowLogs < 40)
        {
            _slowLogs++;
            GD.Print($"[WorldView] slow terrain upload {tile.Key} ring={tile.Ring} tris={nodes.Tris} {us / 1000.0:0.00} ms");
        }
    }

    private void UnloadTile(string key)
    {
        if (!TileKey.TryParse(key, out long packed, out bool terrain)) return;
        if (terrain) { TerrainKeysSeen++; ChurnUnload("terrain", key, TerrainCover.TryGet(packed, out var tn) ? tn : null, "poll_unload"); ReleaseTerrainTile(packed); }
        else { ChurnUnload("world", key, Cover.TryGet(packed, out var vn) ? vn : null, "poll_unload"); ReleaseTile(packed); }
    }

    /// <summary>After the unloads: creates / frees the collision patch with the flag, re-centres it on the camera.</summary>
    private void UpdateCollision()
    {
        if (!TerrainCollisionEnabled)
        {
            if (_collision != null) { _collision.Free(); _collision = null; }
            return;
        }
        _collision ??= new TerrainCollision(this, (x, z) => Streamer.GroundHeightAt(x, z));
        _collision.Samples = TerrainCollisionSamples;
        _collision.SpacingM = TerrainCollisionSpacingM;
        _collision.Update(_source!.GlobalPosition);
    }

    private bool ReleaseTerrainTile(long packed)
    {
        var nodes = TerrainCover.Remove(packed) as TerrainNodes;
        if (nodes == null) return false;
        if (nodes.FadeMaterialActive) _fading.Remove(nodes);
        _terrainPool!.Release(nodes);
        TerrainUnloadedTotal++;
        return true;
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

    /// <summary>Debug (--churn-log): log every unload (layer, key, how long the tile was held, why) and every fresh delivery of a key that was unloaded less than 10 s before.</summary>
    public bool ChurnLog { get; set; }
    private readonly Dictionary<string, ulong> _unloadedAt = new();
    public int ChurnRedeliveries { get; private set; }
    private int _churnLines;

    private void ChurnUnload(string layer, string key, TileNodes? n, string why)
    {
        if (!ChurnLog) return;
        ulong now = Time.GetTicksMsec();
        _unloadedAt[key] = now;
        if (_churnLines++ < 400)
            GD.Print($"[Churn] t={now / 1000.0:0.00} unload {layer} {key} held {(n != null ? (now - n.LoadedMs) / 1000.0 : -1):0.00} s ({why}) fading={n?.Fading} vis={n?.Vis}");
    }

    private void ChurnDeliver(string layer, string key)
    {
        if (!ChurnLog) return;
        ulong now = Time.GetTicksMsec();
        if (_unloadedAt.TryGetValue(key, out ulong at) && now - at < 10000)
        {
            ChurnRedeliveries++;
            if (_churnLines++ < 400) GD.Print($"[Churn] t={now / 1000.0:0.00} RE-DELIVERY {layer} {key} {(now - at) / 1000.0:0.00} s after its unload");
        }
    }

    /// <summary>Debug: log the terrain layer's state once a second (held, visible, holes, camera against the terrain).</summary>
    public bool TerrainDebugLog { get; set; }
    private double _terrainLogT;
    private readonly HashSet<long> _logPending = new();

    private void LogTerrainState(double delta)
    {
        _terrainLogT += delta;
        if (_terrainLogT < 1.0) return;
        _terrainLogT = 0;
        _logPending.Clear();
        if (Streamer.HasDebugTiles)
        {
            Streamer.DebugTiles(_verifyTiles);
            foreach (var t in _verifyTiles)
                if (t.Terrain && t.Packed != 0 && t.State is TileState.Queued or TileState.Fetching or TileState.Meshing) _logPending.Add(TileKey.Ancestor(t.Packed, TerrainCover.RootZ));
        }
        int holes = TerrainCover.CountUncoveredRoots(null, out float worst);
        int holesSettled = TerrainCover.CountUncoveredRoots(_logPending, out _);
        var c = TerrainCover;
        int fadeZero = 0;
        foreach (var n in c.All) if (n.Fading && n.Fade <= 0.01f && n.Vis == TileCover.Vis.Visible) fadeZero++;
        var cam = _source!.GlobalPosition;
        GD.Print($"[Terrain] held {c.Loaded} vis {string.Join("/", c.VisibleByRing)} hiddenByChildren {c.HiddenByChildren} waitingSiblings {c.WaitingSiblings} awaitingFade {c.AwaitingFadeTiles} fading {c.FadingTiles}(at0 {fadeZero}) "
            + $"| uncovered roots {holes} (worst {worst:0.00}, excluding pending {holesSettled}) | cam y {cam.Y:0} ground {CameraGroundHeight:0} agl {cam.Y - CameraGroundHeight:0} far {Camera?.Far ?? 0:0} | uploads {TerrainReadyTotal} unloads {TerrainUnloadedTotal}");
    }

    private readonly HashSet<ulong> _frustumIds = new();

    /// <summary>
    /// Counts what the camera can see (draw call estimate): terrain tiles that are visible by the cover rule and whose mesh AABB (plus the cull margin)
    /// intersects the camera frustum, as the renderer culls them. <paramref name="visibleByRule"/> = tiles shown by the visibility rule,
    /// <paramref name="triangles"/> = their triangles. Debug aid, allocates a little (call a few times per second at most).
    /// </summary>
    public int CountTerrainInFrustum(Camera3D cam, out int visibleByRule, out long triangles) => CountInFrustum(cam, TerrainCover, out visibleByRule, out triangles);

    /// <summary>Same for the vector layer's building meshes (one draw call per tile; ground and sprites not counted).</summary>
    public int CountVectorInFrustum(Camera3D cam, out int visibleByRule, out long triangles) => CountInFrustum(cam, Cover, out visibleByRule, out triangles);

    private int CountInFrustum(Camera3D cam, TileCover cover, out int visibleByRule, out long triangles)
    {
        visibleByRule = 0;
        triangles = 0;
        _frustumIds.Clear();
        foreach (var id in RenderingServer.InstancesCullConvex(cam.GetFrustum(), GetWorld3D().Scenario)) _frustumIds.Add((ulong)id);
        int inFrustum = 0;
        foreach (var n in cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible) continue;
            visibleByRule++;
            if (n.Tris > 0 && _frustumIds.Contains(n.MeshInstance.GetInstanceId()))
            {
                inFrustum++;
                triangles += n.Tris;
            }
        }
        return inFrustum;
    }

    // ---- reconfigure (settings panel) ---------------------------------------

    /// <summary>
    /// Queues a world reconfigure with <paramref name="panelOverrides"/> (nested like the profile JSON, e.g. bands.r3_edge_m) merged
    /// over the launch-arg profile (panel wins). It runs at the start of the next <c>_Process</c>, before update_camera, and ends with
    /// <see cref="Reconfigured"/>. A later call before that replaces the queued one. The world restarts at the camera's current
    /// position: that is the new origin, the camera goes back to local x/z = 0 (altitude and orientation kept), and every tile is
    /// released through the unload batch the library returns for the teleport.
    /// </summary>
    public void ApplyProfileOverrides(Godot.Collections.Dictionary panelOverrides, bool? terrain = null)
    {
        _pendingOverrides = panelOverrides;
        _pendingTerrain = terrain;
    }

    private bool? _pendingTerrain;

    private void RunReconfigure()
    {
        var panel = _pendingOverrides!;
        var terrain = _pendingTerrain;
        _pendingOverrides = null;
        _pendingTerrain = null;
        // The terrain opt-in flags change with the new world; a rejected profile keeps the previous world running with its old flags.
        bool oldEnable = EnableTerrain, oldProfile = TerrainEnabled;
        if (terrain.HasValue) EnableTerrain = TerrainEnabled = terrain.Value || SeedWorldActive; // a seed world cannot run without the terrain layer
        bool ok = TryReconfigure(panel, out var error);
        if (!ok) { EnableTerrain = oldEnable; TerrainEnabled = oldProfile; }
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

        var merged = ComposeOverrides(panel);
        var options = WorldStreamerFacade.MakeWorldOptions(lat, lon, _tileUrl, _cacheDir);
        AddTerrainLayerOption(options);
        AddSeedWorldOption(options);
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
        int strays = _dropScratch.Count;
        _dropScratch.Clear();
        foreach (var n in TerrainCover.All) _dropScratch.Add(n.Packed);
        for (int i = 0; i < _dropScratch.Count; i++) ReleaseTerrainTile(_dropScratch[i]);
        strays += _dropScratch.Count;
        if (strays > 0)
            GD.PushWarning($"[WorldView] reconfigure: {strays} tiles were not in the library's unload batch of {unload.Length}; released anyway");
        _fading.Clear();
        _suspectRoots.Clear();
        _pendingRoots.Clear();
        _suspectRootsT.Clear();
        _pendingRootsT.Clear();

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
        if (TerrainActive) sb.Append(sb.Length > 0 ? ", " : "").Append("terrain z").Append(TerrainCover.RootZ).Append("..z").Append(TerrainCover.LeafZ).Append(" to ").Append((TerrainReachM / 1000.0).ToString("0.#", System.Globalization.CultureInfo.InvariantCulture)).Append(" km");
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
            + $"uploadMaxMs={UploadUsMaxLastSecond / 1000.0:0.00} replaced={ReplacedTotal} cover errors={ConsistencyErrors}"
            + (TerrainActive ? $" terrain loaded={TerrainCover.Loaded} visible={string.Join("/", TerrainCover.VisibleByRing)} tris={TerrainCover.TrianglesVisible}/{TerrainCover.TrianglesLoaded}" : "");
    }

    /// <summary>
    /// Periodic consistency check of the quadtree cover (every 5 s): stored visibility vs the rule, double cover,
    /// holes in settled roots, counters. Roots that still have pending tiles in Rust are skipped, and a hole must
    /// persist over two consecutive checks before it is reported. Pushes an error on any finding.
    /// </summary>
    public void VerifyCover()
    {
        _pendingRoots.Clear();
        _pendingRootsT.Clear();
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
                    {
                        if (t.Terrain) _pendingRootsT.Add(TileKey.Ancestor(t.Packed, TerrainCover.RootZ));
                        else _pendingRoots.Add(TileKey.Ancestor(t.Packed, TileKey.RootZ));
                    }
                }
            }
        }
        _report.Clear();
        int errors = Cover.Verify(_pendingRoots, _suspectRoots, _report);
        if (TerrainActive) errors += TerrainCover.Verify(_pendingRootsT, _suspectRootsT, _report);
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
