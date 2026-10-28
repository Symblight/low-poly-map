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

    /// <summary>
    /// Host opt-in for the landmark layer (api 1.11, contract 9b), default true: adds <c>"landmarks"</c> to <c>layers</c> on every configure and
    /// reconfigure when the library has it, and draws the <c>l/z/x/y</c> tiles it then delivers (buildings of 60 m or more of a two-level archive, one
    /// tile per overview tile, flat set, visible while uploaded, never faded). Off, or on a library below 1.11, the landmarks stay inside the vector
    /// chunks of every band, exactly as before and no <c>l/</c> key ever arrives. Set before <see cref="Start"/>; change it later through <see cref="ApplyProfileOverrides"/>.
    /// </summary>
    [Export] public bool LandmarksEnabled { get; set; } = true;

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
    /// <summary>Landmark layer tile map (api 1.11): a flat set keyed by the "l/z/x/y" key (zoom = the archive's overview zoom). Empty unless <see cref="LandmarksActive"/>.</summary>
    public LandmarkCover LandmarkCover { get; } = new();
    /// <summary>True when this view asked for the landmark layer in the last configure (<see cref="LandmarksEnabled"/> and a library with api 1.11). Whether tiles come depends on the archive (a compatibility archive or the seed world has none).</summary>
    public bool LandmarksActive { get; private set; }
    /// <summary>Landmark ready deliveries, unloads, and "l/" keys seen in poll_ready, poll_fades or poll_unload since start (the library must never put one in poll_fades, nor send any to a host that did not opt in).</summary>
    public int LandmarkReadyTotal { get; private set; }
    public int LandmarkReplacedTotal { get; private set; }
    public int LandmarkUnloadedTotal { get; private set; }
    public int LandmarkKeysSeen { get; private set; }
    public int LandmarkFadeKeysSeen { get; private set; }
    /// <summary>Per landmark tile upload cost in microseconds: last, max, sum and count.</summary>
    public long LandmarkUploadUsLast { get; private set; }
    public long LandmarkUploadUsMax { get; private set; }
    public long LandmarkUploadUsSum { get; private set; }
    public int LandmarkUploadCount { get; private set; }
    /// <summary>Landmark tile node sets handed out by the pool (held).</summary>
    public int LandmarkNodesHeld => LandmarkCover.Loaded;
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
    /// <summary>Profile <c>terrain.tau_px</c> (the terrain cover's screen-space-error threshold before the <see cref="TerrainDetail"/> scale; 0 on a library without it) and the effective tau = tau_px x scale.</summary>
    public double TerrainTauPx { get; private set; }
    public double TerrainTauEff => TerrainTauPx * TerrainDetail;
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
    /// <summary>Tiles held in all layers.</summary>
    public int TotalLoaded => Cover.Loaded + TerrainCover.Loaded + LandmarkCover.Loaded;

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
    /// <summary>Idle node sets the pools destroyed (<see cref="PoolTrimmer"/>); <see cref="PoolCreated"/> counts the sets that exist now.</summary>
    public int PoolTrimmed => _pool?.Trimmed ?? 0;
    public int TerrainPoolTrimmed => _terrainPool?.Trimmed ?? 0;
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
    /// <summary>Effective band edges for the overlay, e.g. "edges 0.6/2.5/7/25 km": the live screen-space refine distances (api 1.10), or on an older library the profile's fixed edges (empty until readable).</summary>
    public string BandsText => EdgesText();
    private string _legacyBandsText = "";
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
    /// <summary>Highest number of pooled tile node sets in use at once (vector tiles plus landmark tiles, since both come from the same pool; checked against the pool size by the settings self-test).</summary>
    public int PeakActiveTiles { get; private set; }
    /// <summary>Node sets (tile roots) under the pool container: created sets, in use or free.</summary>
    public int TileRootCount => _tileRoot?.GetChildCount() ?? 0;
    /// <summary>Every node under the vector and terrain pool containers (roots and their children). Diagnostic walk (self-tests): scene node count minus this = nodes that do not belong to the tile pools.</summary>
    public int PooledNodeCount()
    {
        int n = 0;
        if (_tileRoot != null) n += CountDescendants(_tileRoot);
        if (_terrainRoot != null) n += CountDescendants(_terrainRoot);
        return n;
    }

    private static int CountDescendants(Node node)
    {
        int n = 0;
        foreach (var c in node.GetChildren()) n += 1 + CountDescendants(c);
        return n;
    }

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
    private bool _loggedNoLandmarks, _loggedMemoryBudget;
    private readonly HashSet<long> _pendingRootsT = new();
    private readonly HashSet<long> _suspectRootsT = new();
    private float _emissionLevel = 1f;
    private Vector3 _lightFog = new(1500f, 9000f, 1f); // NightLook fog defaults: begin, end, curve
    private Node3D? _tileRoot;
    private AirportMarkings? _markings;
    private MaterialPrewarm? _prewarm;


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

        // Curved vertices sink below the flat tile AABBs, so tiles are culled with a margin: sized per tile from its distance (CullMargins, refreshed 4 times a
        // second). The static margin below only serves the helpers that are not tiles (the runway markings).
        float far = System.Math.Min(Camera?.Far ?? 30000f, 32000f);
        TilePool.CurveCullMargin = (float)(far * (double)far * Curvature / (2.0 * EarthRadiusM));

        _tileRoot = new Node3D { Name = "Tiles" };
        AddChild(_tileRoot);
        _pool = new TilePool(_tileRoot, _settings);
        _pool.SetEmissionLevel(_emissionLevel);
        _pool.SetLightFog(_lightFog.X, _lightFog.Y, _lightFog.Z);
        _pool.Prewarm(_settings.PrewarmTiles);
        UpdateSpriteSwitch(NightFactor); // a host that set the time before the view started
        _terrainRoot = new Node3D { Name = "TerrainTiles" };
        AddChild(_terrainRoot);
        _terrainPool = new TerrainPool(_terrainRoot, _settings);
        if (_settings.PrewarmMaterials && Camera != null && DisplayServer.GetName() != "headless")
        {
            _prewarm = new MaterialPrewarm(Camera, _pool, _terrainPool);
            GD.Print($"[WorldView] material prewarm: {_prewarm.MaterialCount} materials drawn for {MaterialPrewarm.Frames} frames (shader and pipeline compile before the first fade)");
        }
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

    /// <summary>The band edge keys of a library before api 1.10 (gone since: <c>bands.r3_edge_m</c> is now <c>bands.max_distance_m</c>); read only to describe an older library.</summary>
    private static readonly string[] BandEdgeKeys = { "r0_edge_m", "r1_edge_m", "r2_edge_m", "r3_edge_m" };
    private static readonly double[] LegacyEdgesM = { 600.0, 2500.0, 7000.0, 25000.0 };

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
            var retry = ComposeOverrides(null);
            if (retry.Count > 0) options["profile_overrides"] = retry;
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
        if (MemoryBudget.Apply(merged, _settings.MemoryBudgetMb, ProfilePath, Streamer.HasSetView) && !_loggedMemoryBudget)
        {
            _loggedMemoryBudget = true;
            GD.Print($"[WorldView] memory.budget_mb {_settings.MemoryBudgetMb:0.#} (WorldRenderSettings.MemoryBudgetMb; the library default is 512)");
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

    /// <summary>Adds the <c>layers</c> opt-in: <c>terrain</c> when the host wants it and the library has it (api 1.5; warns once when it has not) and <c>landmarks</c> when <see cref="LandmarksEnabled"/> and the library has the layer (api 1.11).</summary>
    private void AddTerrainLayerOption(Godot.Collections.Dictionary options)
    {
        bool want = EnableTerrain;
        if (want && !Streamer.HasTerrainLayer)
        {
            if (!_warnedNoTerrainLayer) GD.PushWarning($"[WorldView] EnableTerrain ignored: library api {Streamer.ApiVersionText} has no terrain layer (needs 1.5); the world stays flat");
            _warnedNoTerrainLayer = true;
            want = false;
        }
        bool landmarks = LandmarksEnabled && Streamer.HasLandmarkLayer;
        if (LandmarksEnabled && !landmarks && !_loggedNoLandmarks)
        {
            _loggedNoLandmarks = true;
            GD.Print($"[WorldView] landmark layer off: library api {Streamer.ApiVersionText} has none (needs 1.{WorldStreamerFacade.LandmarksMinApiMinor}); landmarks stay inside the vector tiles");
        }
        LandmarksActive = landmarks;
        WorldStreamerFacade.AddLayers(options, want, landmarks);
        // Keep the flat zone (and so the relief) where the first configure put it: a reconfigure may move the origin (api 1.6).
        WorldStreamerFacade.AddTerrainCenter(options, want && Streamer.HasTerrainActiveFlag, OriginLat, OriginLon);
    }

    /// <summary>After a configure: reads the profile's terrain section, resets the terrain tile map to its zoom range and the pool to its shading.</summary>
    private void ConfigureTerrainLayer(Godot.Collections.Dictionary profile)
    {
        TerrainActive = false;
        TerrainReachM = 0f;
        TerrainTauPx = 0.0;
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
                TerrainTauPx = t.TryGetValue("tau_px", out var tau) ? tau.AsDouble() : 0.0;
                TerrainActive = true;
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
    /// set_terrain_detail: a scale of the terrain cover's screen-space-error threshold (tau = <c>terrain.tau_px</c> x scale; the library clamps to
    /// [0.25, terrain.detail_scale_max]). Above 1 is coarser (fewer, larger tiles), below 1 finer and much more expensive (0.25 is about 2300 tiles).
    /// Since api 1.10 the cover follows through ordinary refines / coarsens (new keys, fades, unloads): held tiles are not re-delivered. No teleport, the
    /// vector world is untouched. Returns the scale in effect (NaN on a library without the method).
    /// </summary>
    public double SetTerrainDetail(double scale)
    {
        double r = Streamer.SetTerrainDetail(scale);
        if (!double.IsNaN(r)) TerrainDetail = r;
        return r;
    }

    private void AfterConfigure()
    {
        _viewForce = true; // a new world starts with the library's default view: send ours with the next frame
        if (LandmarkCover.Loaded != 0)
        {
            GD.PushError($"[WorldView] landmark tile map still holds {LandmarkCover.Loaded} tiles at configure; releasing them");
            _dropScratch.Clear();
            foreach (var n in LandmarkCover.All) _dropScratch.Add(n.Packed);
            for (int i = 0; i < _dropScratch.Count; i++) ReleaseLandmarkTile(_dropScratch[i]);
            _dropScratch.Clear();
        }
        Status = Streamer.Status();
        if (Attributions.Length == 0) Attributions = Streamer.Attributions();
        if (Attributions.Length > 0) GD.Print($"[WorldView] attribution: {string.Join(" | ", Attributions)}");
        else if (Streamer.HasHostApi) GD.Print("[WorldView] attribution: none required (synthetic city)");

        var profile = Streamer.WorldProfile();
        ApplyWindowProfile(profile); ApplySpriteProfile(profile);
        ConfigureTerrainLayer(profile);
        _legacyBandsText = "";
        if (profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary)
        {
            var b = bv.AsGodotDictionary();
            if (b.TryGetValue("hysteresis", out var hv)) _hysteresis = (float)hv.AsDouble();
            var sb = new StringBuilder("bands ");
            for (int i = 0; i < BandEdgeKeys.Length; i++)
            {
                if (!b.TryGetValue(BandEdgeKeys[i], out var e)) { sb.Clear(); break; }
                if (i > 0) sb.Append('/');
                sb.Append((e.AsDouble() / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            }
            if (sb.Length > 0) _legacyBandsText = sb.Append(" km").ToString();
        }
        _lodBlendD = 0; _lodMaxDist = 0; _lodRingEdge0 = 0; // a new world: write the cross-fade ranges again
        ReadLod();
        if (BandsText.Length > 0) GD.Print($"[WorldView] effective {BandsText}{(HasLodStats ? $", tau_eff {TauEff:0.##} px (quality x{LodQuality:0.##}), library view {LibraryViewFovYDeg:0.#} deg / {LibraryViewHeightPx} px" : "")}");
        RefreshSeed(true);
    }

    // ---- screen-space LOD (api 1.10) ----------------------------------------------------------

    /// <summary>The band edges the cover uses now in metres, R0..R3 (library <c>lod_edges_m</c>; R0..R2 are the refine distances of the screen-space error, R3 is <c>bands.max_distance_m</c>). All 0 until read or on a library before api 1.10. Refreshed 4 times a second and by <see cref="ReadLod"/>.</summary>
    public readonly double[] LodEdgesM = new double[4];
    /// <summary>True once the library has reported the screen-space LOD fields (<c>lod_edges_m</c>, <c>tau_eff</c>).</summary>
    public bool HasLodStats { get; private set; }
    /// <summary>tau_eff in pixels (profile <c>lod.tau_px</c> x <see cref="LodQuality"/>), the set_lod_quality scale in effect, and the view the library works with (echo of set_view).</summary>
    public double TauEff { get; private set; }
    public double LodQuality { get; private set; } = 1.0;
    public double LibraryViewFovYDeg { get; private set; }
    public int LibraryViewHeightPx { get; private set; }
    /// <summary>The R2 to R1 edge D the cross-fade ranges were last computed from (0 = not written yet), and how often they were written.</summary>
    public double BlendEdgeM => _lodBlendD;
    public int BlendUpdates { get; private set; }

    private readonly StreamerStats _lodStats = new();
    private double _lodBlendD, _lodMaxDist, _lodRingEdge0, _lodReadT;
    private readonly double[] _lodMarkEdges = new double[3];
    private float _hysteresis = 1.15f;
    private const double LodReadIntervalS = 0.25, LodBlendRelChange = 0.05;

    /// <summary>
    /// set_lod_quality (api 1.10): live multiplier of the pixel error threshold, clamped by the library to [0.25, 4]; 2 halves every refine distance, 0.5
    /// doubles them. No reconfigure; the library keeps it across <see cref="ApplyProfileOverrides"/>. Returns the scale in effect (NaN on an older library).
    /// The edges change with the next camera update; <see cref="ReadLod"/> then shows them.
    /// </summary>
    public double SetLodQuality(double scale)
    {
        double r = Streamer.SetLodQuality(scale);
        if (!double.IsNaN(r)) LodQuality = r;
        return r;
    }

    /// <summary>Reads <c>lod_edges_m</c>, <c>tau_eff</c>, <c>lod_quality</c> and the view from lod_state() (debug_stats() before api 1.11) into the properties above and, when the live R2 to R1 edge moved by more than 5 %, rewrites the cross-fade ranges.</summary>
    public void ReadLod()
    {
        if (!Streamer.IsAvailable || !Streamer.HasSetView || !Streamer.LodState(_lodStats) || !_lodStats.HasLodStats) return;
        HasLodStats = true;
        TauEff = _lodStats.TauEff;
        LodQuality = _lodStats.LodQuality;
        LibraryViewFovYDeg = _lodStats.ViewFovYDeg;
        LibraryViewHeightPx = _lodStats.ViewHeightPx;
        System.Array.Copy(_lodStats.LodEdgesM, LodEdgesM, 4);
        if (_settings.BlendFollowsLod && _pool != null) FollowLodEdges();
        FollowMarkingEdges();
    }

    /// <summary>Contract-time part of S3: the window / sprite blend and the street pool fade follow D = the R2 to R1 edge, the sprites' far fade follows the maximum distance.</summary>
    private void FollowLodEdges()
    {
        double d = LodEdgesM[1];
        if (d > 1 && (_lodBlendD <= 0 || System.Math.Abs(d - _lodBlendD) > _lodBlendD * LodBlendRelChange))
        {
            _lodBlendD = d;
            BlendUpdates++;
            _pool!.SetLodRanges((float)(d * WorldRenderSettings.BlendStartShare), (float)(d * WorldRenderSettings.BlendEndShare),
                (float)(d * WorldRenderSettings.PoolStartShare), PoolFadeEndM(d));
        }
        double far = LodEdgesM[3];
        if (far > 1 && far != _lodMaxDist)
        {
            _lodMaxDist = far;
            _pool!.SetLightFar((float)(far * WorldRenderSettings.LightFadeShare), (float)far);
        }
    }

    /// <summary>The runway markings choose the lift of the ground ring by view distance: their ring edges follow the live edges x (2 - hysteresis).</summary>
    private void FollowMarkingEdges()
    {
        if (_markings == null || LodEdgesM[2] <= 1) return;
        bool moved = _lodRingEdge0 <= 0;
        for (int i = 0; i < 3 && !moved; i++) moved = System.Math.Abs(LodEdgesM[i] - _lodMarkEdges[i]) > _lodMarkEdges[i] * 0.01;
        if (!moved) return;
        for (int i = 0; i < 3; i++) _lodMarkEdges[i] = LodEdgesM[i];
        _lodRingEdge0 = LodEdgesM[0];
        float k = 2f - _hysteresis;
        _markings.SetRingLifts(RunwayLift, new Vector3((float)LodEdgesM[0] * k, (float)LodEdgesM[1] * k, (float)LodEdgesM[2] * k));
    }

    private string EdgesText()
    {
        if (HasLodStats && LodEdgesM[3] > 0)
        {
            var sb = new StringBuilder("edges ");
            for (int i = 0; i < 4; i++) sb.Append(i > 0 ? "/" : "").Append((LodEdgesM[i] / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            return sb.Append(" km").ToString();
        }
        return _legacyBandsText;
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
    /// <c>max_error_m</c> of the terrain bands whose range starts below <c>rN_edge_m x hysteresis</c> (a generator-1 library has the old fixed edges), times <c>terrain.detail_scale_max</c>, plus
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
                // Generator 1 only: the band reach of the old drape replay. The edges are the old fixed ones (a generator-1 library has no screen-space LOD).
                double rEdge = !levelRule && bands.TryGetValue(BandEdgeKeys[n], out var rev) ? rev.AsDouble() : LegacyEdgesM[n];
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
            }
        }
        catch (System.Exception e)
        {
            GD.PushWarning($"[WorldView] runway marking lifts: cannot read the profile ({e.Message}); using the default lifts");
        }
        RunwayLift = new Vector4(lift[0], lift[1], lift[2], lift[3]);
        RunwayLiftRule = levelRule ? "level" : "drape replay";
        // Ring edges (view distance beyond which ring 1, 2, 3 applies): the live edges x (2 - hysteresis); before the first read the old fixed 600 / 2500 / 7000 m.
        _lodRingEdge0 = 0;
        float ek = 2f - _hysteresis;
        _markings.SetRingLifts(RunwayLift, new Vector3((float)LegacyEdgesM[0] * ek, (float)LegacyEdgesM[1] * ek, (float)LegacyEdgesM[2] * ek));
        FollowMarkingEdges();
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






    // ---- reconfigure (settings panel) ---------------------------------------

    /// <summary>
    /// Queues a world reconfigure with <paramref name="panelOverrides"/> (nested like the profile JSON, e.g. bands.max_distance_m) merged
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
        _dropScratch.Clear();
        foreach (var n in LandmarkCover.All) _dropScratch.Add(n.Packed);
        for (int i = 0; i < _dropScratch.Count; i++) ReleaseLandmarkTile(_dropScratch[i]);
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

    /// <summary>The values the library is running with (world_profile), e.g. "tau 4 px (quality x1, fov 60 deg, H 1080 px), edges 0.6/2.5/7/25 km, max distance 25 km, min building height R0/R1/R2 0/0/0 m".</summary>
    public string EffectiveProfileText()
    {
        var p = Streamer.WorldProfile();
        var sb = new StringBuilder();
        ReadLod();
        if (HasLodStats)
        {
            sb.Append("tau ").Append(TauEff.ToString("0.##", System.Globalization.CultureInfo.InvariantCulture)).Append(" px (quality x").Append(LodQuality.ToString("0.##", System.Globalization.CultureInfo.InvariantCulture))
              .Append(", fov ").Append(LibraryViewFovYDeg.ToString("0.#", System.Globalization.CultureInfo.InvariantCulture)).Append(" deg, H ").Append(LibraryViewHeightPx).Append(" px), ").Append(EdgesText());
        }
        else sb.Append(EdgesText());
        if (p.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary && bv.AsGodotDictionary().TryGetValue("max_distance_m", out var mdv))
            sb.Append(sb.Length > 0 ? ", " : "").Append("max distance ").Append((mdv.AsDouble() / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture)).Append(" km");
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

}
