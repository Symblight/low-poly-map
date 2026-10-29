#nullable enable
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
    /// <summary>Camera used for the field of view (sprite size) and the far plane. Also the pose source unless <see cref="CameraSource"/> is set; a source that is an <see cref="IWorldCameraProvider"/> (<see cref="CameraSwitchSource"/>) supplies the camera itself and this export may stay empty.</summary>
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

    public IWorldCameraSource? CameraSource { get; set; }
    /// <summary>Profile overrides (nested like the profile JSON) merged over <see cref="ProfilePath"/> at every configure; set before <see cref="Start"/>.</summary>
    public Godot.Collections.Dictionary BaseProfileOverrides { get; set; } = new();

    private const double EarthRadiusM = 6_371_000.0;

    public WorldStreamerFacade Streamer { get; private set; } = new();
    /// <summary>Human readable tile source actually in use.</summary>
    public string TileSource { get; private set; } = "";
    public TileCover Cover { get; } = new();

    private string _lastLoggedStatus = "";
    private string _tileUrl = "";
    private string _cacheDir = "";
    private bool _started;
    private bool _initialized;
    private WorldRenderSettings _settings = new();
    private IWorldCameraSource? _source;
    private Camera3DSource? _adapter;
    private bool _prewarmPending;

    /// <summary>Starts the material prewarm once a camera exists (at Start, or on the first frame that has one). Without it the first tile draw and the first fade compile the shaders and pipelines inside a frame: measured 4.7 s on Metal Forward+ in SimPlane.</summary>
    private void StartPrewarmIfReady()
    {
        if (!_prewarmPending || _pool == null) return;
        var cam = ViewCamera;
        if (cam == null) return;
        _prewarmPending = false;
        _prewarm = new MaterialPrewarm(cam, _pool, _terrainPool);
        GD.Print($"[WorldView] material prewarm: {_prewarm.MaterialCount} materials drawn for {MaterialPrewarm.Frames} frames (shader and pipeline compile before the first fade)");
    }
    private readonly List<long> _dropScratch = new(256);
    private TilePool? _pool;
    private TerrainPool? _terrainPool;
    private Node3D? _terrainRoot;
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
        float far = System.Math.Min(ViewCamera?.Far ?? 30000f, 32000f);
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
        // The prewarm needs a camera to draw in front of; a host whose cameras enter the tree after this node (SimPlane) has none yet, so it starts on the first frame that has one (StartPrewarmIfReady).
        _prewarmPending = _settings.PrewarmMaterials && DisplayServer.GetName() != "headless";
        StartPrewarmIfReady();
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
        _cacheDir = SeedWorldActive ? "" : CacheDirPath.Resolve(CacheDir.Trim());
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


    public override void _ExitTree()
    {
        GcPacing?.Release();   // the last view ends the no-GC region: the collector is back on its own schedule
        GcPacing = null;
        // Deterministic stop before the node is freed: waits (bounded 1 s) for cache I/O and releases the cache_dir lock.
        if (Streamer.IsAvailable)
        {
            Streamer.StatusChanged -= OnStatusChanged;
            Streamer.OriginShifted -= OnOriginShifted;
            if (Streamer.HasHostApi) GD.Print("[WorldView] shutdown (releasing the cache lock)");
            Streamer.Shutdown();
        }
    }

    public override void _Notification(int what)
    {
        // iOS / Android: the OS wants memory back. The pacer ends its region, collects and stays out for a while (GcPacer.MemoryWarningHoldS).
        if (what == NotificationOsMemoryWarning) GcPacing?.OnMemoryWarning();
    }

    private void OnStatusChanged(StreamerStatus st)
    {
        Status = st;
        var line = $"status: {st.State} - {st.Message}";
        if (line == _lastLoggedStatus) return;
        _lastLoggedStatus = line;
        GD.Print($"[WorldView] {line}");
    }
}
