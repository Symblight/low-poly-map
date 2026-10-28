using System.Collections.Generic;
using Godot;
using Godot.Collections;

namespace SimWorld;

public enum TileState { Queued, Fetching, Meshing, Ready, Failed, Unloading, Unknown,
    /// <summary>Landmark layer only (api 1.11): an overview tile in range without landmarks; built, never delivered.</summary>
    Empty }

/// <summary>One entry of debug_tiles(): ring, state and the tile rect in local x/z meters.</summary>
public struct DebugTile
{
    public string Key;
    /// <summary>Packed key (see <see cref="TileKey"/>), 0 when the key could not be parsed.</summary>
    public long Packed;
    public int Ring;
    /// <summary>True for a terrain-layer tile (key "t/z/x/y", api 1.5); <see cref="Packed"/> is then a key of the terrain tile map.</summary>
    public bool Terrain;
    /// <summary>The layer of the key: world "z/x/y", terrain "t/z/x/y" or landmarks "l/z/x/y" (api 1.11, at the overview zoom, ring 0). A landmark tile has its own tile map: it is neither a vector nor a terrain tile.</summary>
    public TileLayer Layer;
    public TileState State;
    public Vector2 Min;
    public Vector2 Max;
    /// <summary>Where the tile's data comes from (api 1.7): "seed", "archive", "synthetic" or "terrain"; empty on an older library.</summary>
    public string Source;
}

/// <summary>One end of a runway (api 1.7 <c>airports()</c>). Positions are in the local frame of the call (they follow the floating origin).</summary>
public sealed record RunwayEnd(string Ident, double Lat, double Lon, Vector3 Position, double ThresholdLat, double ThresholdLon, Vector3 ThresholdPosition,
    double DisplacedThresholdM, Vector3 TouchdownPosition, double HeadingDeg);

/// <summary>One runway of an airport (api 1.7). <see cref="Lights"/> / <see cref="LightColors"/> are the same points the tiles draw (ground + the light height).</summary>
public sealed record RunwayInfo(string Designator, RunwayEnd Le, RunwayEnd He, double LengthM, double WidthM, string Surface, bool Paved, bool Lighted,
    bool Approximate, Vector3[] Lights, Color[] LightColors);

/// <summary>One airport of a seed world (api 1.7 <c>airports()</c>), sorted by ident in the array the facade returns.</summary>
public sealed record AirportInfo(string Ident, string Name, string Type, double Lat, double Lon, Vector3 Position, double? ElevationFt, RunwayInfo[] Runways)
{
    /// <summary>The longest runway (the one a spot or a game would use first); null without runways.</summary>
    public RunwayInfo? MainRunway
    {
        get
        {
            RunwayInfo? best = null;
            foreach (var r in Runways) if (best == null || r.LengthM > best.LengthM) best = r;
            return best;
        }
    }
}

/// <summary>One city of the seed world manifest (api 1.7 <c>seed_world_info()</c>).</summary>
public sealed record SeedCityInfo(string Id, string Name, double Lat, double Lon, double RadiusKm, int Version, string Source, string[] Airports);

/// <summary>Summary of the loaded seed world (api 1.7 <c>seed_world_info()</c>); <see cref="Phases"/> lists the built phases ("S0", "S1", ...).</summary>
public sealed record SeedWorldInfo(string Manifest, int GeneratorVersion, string[] Phases, int WorldVersion, string WorldSeed, SeedCityInfo[] Cities,
    int Airports, int WaterPolygons, string[] Warnings, string MemoryText, double LoadMs)
{
    public bool HasPhase(string phase) => System.Array.IndexOf(Phases, phase) >= 0;

    /// <summary>One line for the startup log and the overlay.</summary>
    public string Summary =>
        $"manifest {Manifest}, generator v{GeneratorVersion}, phases [{string.Join(", ", Phases)}], world v{WorldVersion} seed \"{WorldSeed}\", "
        + $"{Cities.Length} cities, {Airports} airports, {WaterPolygons} water polygons, {Warnings.Length} warnings, {MemoryText}";
}

/// <summary>Data-source health from status() / the status_changed signal (contract 3.5). Unknown states count as degraded.</summary>
public sealed class StreamerStatus
{
    public string State = "ok";
    public string Message = "";
    public int FailedFetches;
    public string LastError = "";
    /// <summary>The terrain is active in this configuration (api 1.6; false on an older library, see <see cref="WorldStreamerFacade.HasTerrainActiveFlag"/>).</summary>
    public bool TerrainActive;

    public bool IsOk => State == "ok";

    public static StreamerStatus From(Dictionary d)
    {
        var st = new StreamerStatus();
        if (d.TryGetValue(WorldStreamerFacade.KState, out var s)) st.State = s.AsString();
        if (d.TryGetValue(WorldStreamerFacade.KMessage, out var m)) st.Message = m.AsString();
        if (d.TryGetValue(WorldStreamerFacade.KFailedFetches, out var f)) st.FailedFetches = (int)f.AsInt64();
        if (d.TryGetValue(WorldStreamerFacade.KLastError, out var e)) st.LastError = e.AsString();
        if (d.TryGetValue(WorldStreamerFacade.KTerrainActive, out var ta)) st.TerrainActive = ta.AsBool();
        return st;
    }

    public override string ToString() => Message.Length > 0 ? $"{State} - {Message}" : State;
}

/// <summary>Result of configure_world() (contract 3.1). <see cref="Supported"/> is false when the library predates 0.6.</summary>
public sealed class ConfigureResult
{
    public bool Supported = true;
    public bool Ok;
    public string Error = "";
    public string[] Attributions = System.Array.Empty<string>();
}

/// <summary>Typed, reusable result of debug_stats(). Per-ring arrays have length <see cref="Rings"/>.</summary>
public sealed class StreamerStats
{
    public const int Rings = 4;
    /// <summary>Capacity of the terrain per-ring arrays (the library sends one entry per terrain.bands item, 5 by default).</summary>
    public const int TerrainRingsMax = 8;
    /// <summary>True while the library reports the terrain layer (debug_stats terrain_* keys, api 1.5).</summary>
    public bool HasTerrain;
    /// <summary>Entries the library sent for the terrain arrays (= number of terrain rings).</summary>
    public int TerrainRings;
    public readonly int[] TerrainQueued = new int[TerrainRingsMax];
    public readonly int[] TerrainFetching = new int[TerrainRingsMax];
    public readonly int[] TerrainMeshing = new int[TerrainRingsMax];
    public readonly int[] TerrainLoaded = new int[TerrainRingsMax];
    /// <summary>Triangles (surface + skirts) of every terrain tile the host holds, from the library.</summary>
    public long TerrainTris;
    /// <summary>The set_terrain_detail scale in effect.</summary>
    public double TerrainDetail = 1.0;
    public readonly int[] Queued = new int[Rings];
    public readonly int[] Fetching = new int[Rings];
    public readonly int[] Meshing = new int[Rings];
    public readonly int[] Loaded = new int[Rings];
    /// <summary>True while the library reports the screen-space LOD fields below (api 1.10; false on an older library, where they stay 0).</summary>
    public bool HasLodStats;
    /// <summary>The effective screen-space error threshold in pixels (debug_stats "tau_eff", api 1.10).</summary>
    public double TauEff;
    /// <summary>The set_lod_quality scale in effect (debug_stats "lod_quality", api 1.10; 1 on an older library).</summary>
    public double LodQuality = 1.0;
    /// <summary>The band edges the cover uses now in metres, R0..R3 (debug_stats "lod_edges_m", api 1.10): R0..R2 are the screen-space refine distances, R3 = bands.max_distance_m. All 0 without the key.</summary>
    public readonly double[] LodEdgesM = new double[Rings];
    /// <summary>The view the library works with (debug_stats "view_fov_y_deg" / "view_height_px", api 1.10; 0 without the keys).</summary>
    public double ViewFovYDeg;
    public int ViewHeightPx;
    /// <summary>Data tiles held / decoded so far (detail z14 + overview z11, api 1.10) and requests the scheduler did not send, because the PMTiles directory has no entry / because the camera moves too fast.</summary>
    public long DataTilesLoaded, DataTilesDecoded, RequestsSkippedEmpty, RequestsSkippedMoving;
    /// <summary>True when debug_stats carries <c>requests_skipped_empty</c> / <c>requests_skipped_moving</c>. The contract (1.10/1.11) does not define them, so a current library never sends them: <see cref="RequestsSkippedEmpty"/> / <see cref="RequestsSkippedMoving"/> stay 0 and the overlay hides the field.</summary>
    public bool HasRequestsSkipped;
    /// <summary>Other <c>data_tiles_*</c> keys the library sends beyond loaded / decoded (e.g. a future <c>data_tiles_evicted</c>), as (key, value); reused, rebuilt on every read. Empty when there are none.</summary>
    public readonly List<KeyValuePair<string, long>> DataTilesExtra = new();
    /// <summary>True when debug_stats carries the shared job pool counters (<c>jobs_queued</c> / <c>jobs_running</c> / <c>jobs_cancelled</c>, api 1.10; display only).</summary>
    public bool HasJobs;
    public long JobsQueued, JobsRunning, JobsCancelled;
    /// <summary>The library's memory accounting (debug_stats "memory_mb", api 1.10; MiB): every entry the library sends (budget, total, data_tiles, height_tiles, collision, free_buffers, over_budget; names may change in a minor), reused, rebuilt on every read. Empty when the key is absent.</summary>
    public readonly List<KeyValuePair<string, double>> MemoryMb = new();
    /// <summary>"queued 42, running 4, cancelled 0", or "" when the library sends no job counters.</summary>
    public string JobsText() => HasJobs ? $"queued {JobsQueued}, running {JobsRunning}, cancelled {JobsCancelled}" : "";

    /// <summary>"budget 512, total 30.5 (data_tiles 30.5, height_tiles 0) over_budget 0" in MiB, or "" when the library sends no memory accounting. Known names first, other pools after them.</summary>
    public string MemoryText()
    {
        if (MemoryMb.Count == 0) return "";
        var sb = new System.Text.StringBuilder();
        foreach (var name in MemoryHead)
        {
            double v = Memory(name);
            if (!double.IsNaN(v)) sb.Append(sb.Length > 0 ? ", " : "").Append(name).Append(' ').Append(v.ToString("0.#"));
        }
        foreach (var kv in MemoryMb)
            if (System.Array.IndexOf(MemoryHead, kv.Key) < 0) sb.Append(sb.Length > 0 ? ", " : "").Append(kv.Key).Append(' ').Append(kv.Value.ToString("0.#"));
        return sb.ToString();
    }

    private static readonly string[] MemoryHead = { "budget", "total", "data_tiles", "height_tiles", "over_budget" };

    /// <summary>"loaded 13, decoded 126, evicted 4": the data_tiles_* counters (known ones, then any other the library sends), "" when none.</summary>
    public string DataTilesText()
    {
        var sb = new System.Text.StringBuilder();
        sb.Append("loaded ").Append(DataTilesLoaded).Append(", decoded ").Append(DataTilesDecoded);
        foreach (var kv in DataTilesExtra) sb.Append(", ").Append(kv.Key).Append(' ').Append(kv.Value);
        if (HasRequestsSkipped) sb.Append(", requests skipped: empty ").Append(RequestsSkippedEmpty).Append(", moving ").Append(RequestsSkippedMoving);
        return sb.ToString();
    }

    /// <summary>One entry of <see cref="MemoryMb"/> by name, or NaN when the library does not report it.</summary>
    public double Memory(string name)
    {
        foreach (var kv in MemoryMb) if (kv.Key == name) return kv.Value;
        return double.NaN;
    }
    /// <summary>True once the library reports the data levels of the tile source (debug_stats "data_levels", api 1.11; absent before the archive opened, for a seed world and on an older library).</summary>
    public bool HasDataLevels;
    /// <summary>Detail and overview zoom of the data tiles; <see cref="TwoLevel"/> = R2/R3 come from the overview level (false = a compatibility archive without simworld_levels); <see cref="LandmarksInArchive"/> = the overview level has a landmarks layer.</summary>
    public int DetailZoom, OverviewZoom;
    public bool TwoLevel, LandmarksInArchive;
    /// <summary>True while the landmark layer runs (debug_stats landmarks_* keys, api 1.11): tiles delivered and held, in range waiting / being meshed, in range without landmarks.</summary>
    public bool HasLandmarkStats;
    public int LandmarksLoaded, LandmarksPending, LandmarksEmpty;
    public double CacheMb;
    public double OriginLat;
    public double OriginLon;
    public string Version = "?";
    /// <summary>debug_stats()["latency"] (api 1.3): one entry per stage that has samples, in library order. Empty on older libraries.</summary>
    public readonly List<LatencyStage> Latency = new();

    /// <summary>The latency table as text, one line per stage: "name  R0 p50/p95/max ms (n) | R1 ...". Empty without data.</summary>
    public string LatencyText()
    {
        var sb = new System.Text.StringBuilder();
        foreach (var st in Latency)
        {
            sb.Append(st.Name.PadRight(10));
            for (int r = 0; r < Rings; r++)
            {
                sb.Append(" | R").Append(r).Append(' ');
                if (st.N[r] == 0) { sb.Append('-'); continue; }
                sb.Append(st.P50[r].ToString("0")).Append('/').Append(st.P95[r].ToString("0")).Append('/').Append(st.Max[r].ToString("0"))
                  .Append(" (").Append(st.N[r]).Append(')');
            }
            sb.Append('\n');
        }
        return sb.ToString();
    }
}

/// <summary>One stage of debug_stats()["latency"]: milliseconds by ring over the library's last fresh deliveries.</summary>
public sealed class LatencyStage
{
    public string Name = "";
    public readonly float[] P50 = new float[StreamerStats.Rings];
    public readonly float[] P95 = new float[StreamerStats.Rings];
    public readonly float[] Max = new float[StreamerStats.Rings];
    public readonly int[] N = new int[StreamerStats.Rings];
}

/// <summary>
/// The single typed C# wrapper over the Rust "WorldStreamer" node (GDExtension).
/// Nothing else in the project touches method-name strings. If the class is not
/// registered (Rust library not built / not loaded) the facade stays unavailable
/// and every method is a harmless no-op returning an empty value.
/// </summary>
public sealed partial class WorldStreamerFacade
{
    public const string ClassNameText = "WorldStreamer";

    /// <summary>Time the main thread spends inside the library calls, summed per frame (see <see cref="StreamerTimings"/>).</summary>
    public StreamerTimings Timings { get; } = new();

    /// <summary>Contract major this facade is written against (docs/contract.md). A different library major is never called into.
    /// 1.0 is the first breaking change (wall UV in window units, profile ids 6 and 7); every 0.x library is refused.</summary>
    public const int ExpectedApiMajor = 1;
    /// <summary>Lowest contract minor of <see cref="ExpectedApiMajor"/> the facade fully supports. 1.0 contains every feature of 0.5-0.7
    /// (poll_fades, ground_height_at, debug_tiles, configure_world / world_profile, the host API), so nothing is probed.</summary>
    public const int MinApiMinor = 0;
    /// <summary>Highest contract minor this facade knows. A library above it gets one info line.</summary>
    public const int LatestKnownMinor = 11;
    /// <summary>First contract minor of <see cref="ExpectedApiMajor"/> with the terrain layer (layers option, "t/" keys, set_terrain_detail).</summary>
    public const int TerrainMinApiMinor = 5;
    /// <summary>First contract minor with the <c>terrain_center</c> option and the status key <c>terrain_active</c> (api 1.6).</summary>
    public const int TerrainCenterMinApiMinor = 6;
    /// <summary>First contract minor with the world seed phase S1: <c>seed_world</c> option, <c>airports()</c>, <c>seed_world_info()</c>, airport light keys (api 1.7).</summary>
    public const int SeedWorldMinApiMinor = 7;
    /// <summary>First contract minor with <c>set_view(fov_y_deg, viewport_h_px, forward)</c> (screen-space LOD) and the <c>tau_eff</c> / data tile debug_stats fields (api 1.10; 1.8 and 1.9 are reserved).</summary>
    public const int SetViewMinApiMinor = 10;
    /// <summary>First contract minor with two-level archives and the landmark layer: <c>layers</c> value <c>"landmarks"</c>, "l/z/x/y" keys, <c>data_levels</c> / <c>landmarks_*</c> in debug_stats (api 1.11).</summary>
    public const int LandmarksMinApiMinor = 11;

    private static readonly StringName ClassNameSn = new(ClassNameText);
    private static readonly StringName MConfigure = new("configure");
    private static readonly StringName MUpdateCamera = new("update_camera");
    private static readonly StringName MPollReady = new("poll_ready");
    private static readonly StringName MPollFades = new("poll_fades");
    private static readonly StringName MPollUnload = new("poll_unload");
    private static readonly StringName MTakeOriginShift = new("take_origin_shift");
    private static readonly StringName MDebugStats = new("debug_stats");
    private static readonly StringName MDebugTiles = new("debug_tiles");
    private static readonly StringName MGroundHeightAt = new("ground_height_at");
    private static readonly StringName MSetTerrainDetail = new("set_terrain_detail");
    private static readonly StringName MSetView = new("set_view");

    private static readonly StringName MConfigureWorld = new("configure_world");
    private static readonly StringName MWorldProfile = new("world_profile");
    private static readonly StringName MUpdateCameraGeodetic = new("update_camera_geodetic");
    private static readonly StringName MGeodeticToLocal = new("geodetic_to_local");
    private static readonly StringName MLocalToGeodetic = new("local_to_geodetic");
    private static readonly StringName MAttributions = new("attributions");
    private static readonly StringName MStatus = new("status");
    private static readonly StringName MAirports = new("airports");
    private static readonly StringName MSeedWorldInfo = new("seed_world_info");
    private static readonly StringName MShutdown = new("shutdown");
    private static readonly StringName SigOriginShifted = new("origin_shifted");
    private static readonly StringName SigStatusChanged = new("status_changed");

    private static readonly StringName KOk = new("ok");
    private static readonly StringName KError = new("error");
    private static readonly StringName KAttributions = new("attributions");
    private static readonly StringName KOptOriginLat = new("origin_lat");
    private static readonly StringName KOptOriginLon = new("origin_lon");
    private static readonly StringName KOptTileUrl = new("tile_url");
    private static readonly StringName KOptCacheDir = new("cache_dir");
    private static readonly StringName KOptLayers = new("layers");
    private static readonly StringName KLayer = new("layer");
    private static readonly StringName KTerrain = new("terrain");
    private static readonly StringName KHMin = new("h_min");
    private static readonly StringName KHMax = new("h_max");
    private static readonly StringName KTerrainQueued = new("terrain_queued");
    private static readonly StringName KTerrainFetching = new("terrain_fetching");
    private static readonly StringName KTerrainMeshing = new("terrain_meshing");
    private static readonly StringName KTerrainLoaded = new("terrain_loaded");
    private static readonly StringName KTerrainTris = new("terrain_tris");
    private static readonly StringName KTerrainDetail = new("terrain_detail");
    internal static readonly StringName KState = new("state");
    internal static readonly StringName KMessage = new("message");
    internal static readonly StringName KFailedFetches = new("failed_fetches");
    internal static readonly StringName KLastError = new("last_error");
    internal static readonly StringName KTerrainActive = new("terrain_active");
    private static readonly StringName KOptTerrainCenter = new("terrain_center");
    private static readonly StringName KOptSeedWorld = new("seed_world");
    private static readonly StringName KAirportLights = new("airport_lights");
    private static readonly StringName KAirportLightColors = new("airport_light_colors");
    private static readonly StringName KSource = new("source");

    private static readonly StringName KKey = new("key");
    private static readonly StringName KOrigin = new("origin");
    private static readonly StringName KMesh = new("mesh");
    private static readonly StringName KGround = new("ground");
    private static readonly StringName KLights = new("lights");
    private static readonly StringName KLightColors = new("light_colors");
    private static readonly StringName KStreetLights = new("street_lights");
    private static readonly StringName KStreetLightColors = new("street_light_colors");
    private static readonly StringName KBeaconLights = new("beacon_lights");
    private static readonly StringName KBeaconLightColors = new("beacon_light_colors");
    private static readonly StringName KRing = new("ring");
    private static readonly StringName KMin = new("min");
    private static readonly StringName KMax = new("max");
    private static readonly StringName KQueued = new("queued");
    private static readonly StringName KFetching = new("fetching");
    private static readonly StringName KMeshing = new("meshing");
    private static readonly StringName KLoaded = new("loaded");
    private static readonly StringName KCacheMb = new("cache_mb");
    private static readonly StringName KOriginLat = new("origin_lat");
    private static readonly StringName KOriginLon = new("origin_lon");
    private static readonly StringName KVersion = new("version");
    private static readonly StringName KLatency = new("latency");
    private static readonly StringName KTauEff = new("tau_eff");
    private static readonly StringName KLodQuality = new("lod_quality");
    private static readonly StringName KLodEdges = new("lod_edges_m");
    private static readonly StringName KViewFov = new("view_fov_y_deg");
    private static readonly StringName KViewHeight = new("view_height_px");
    private static readonly StringName MSetLodQuality = new("set_lod_quality");
    private static readonly StringName KDataTilesLoaded = new("data_tiles_loaded");
    private static readonly StringName KDataLevels = new("data_levels");
    private static readonly StringName KLandmarksLoaded = new("landmarks_loaded");
    private static readonly StringName KLandmarksPending = new("landmarks_pending");
    private static readonly StringName KLandmarksEmpty = new("landmarks_empty");
    private static readonly StringName KDataTilesDecoded = new("data_tiles_decoded");
    private static readonly StringName KRequestsSkippedEmpty = new("requests_skipped_empty");
    private static readonly StringName KRequestsSkippedMoving = new("requests_skipped_moving");
    private static readonly StringName KJobsQueued = new("jobs_queued");
    private static readonly StringName KJobsRunning = new("jobs_running");
    private static readonly StringName KJobsCancelled = new("jobs_cancelled");
    private static readonly StringName KMemoryMb = new("memory_mb");
    private static readonly StringName MApiVersion = new("api_version");
    private static readonly StringName KApiMajor = new("api_major");
    private static readonly StringName KApiMinor = new("api_minor");

    private static readonly Array<Dictionary> EmptyReady = new();
    private static readonly string[] EmptyUnload = System.Array.Empty<string>();

    private Node? _node;

    /// <summary>The Rust node to add to the scene tree; null when unavailable.</summary>
    public Node? Node => IsAvailable ? _node : null;

    /// <summary>True when the node exists and is still a live Godot instance.</summary>
    public bool IsAvailable => _node != null && GodotObject.IsInstanceValid(_node);

    /// <summary>Why the facade is unavailable (empty when it is).</summary>
    public string Error { get; private set; } = "";

    /// <summary>Contract major of the loaded library; meaningful only when <see cref="ApiVersionKnown"/>.</summary>
    public int ApiMajor { get; private set; }
    /// <summary>Contract minor of the loaded library; meaningful only when <see cref="ApiVersionKnown"/>.</summary>
    public int ApiMinor { get; private set; }
    /// <summary>False for a library that predates api_version (such a library is refused: it is a 0.x library).</summary>
    public bool ApiVersionKnown { get; private set; }
    /// <summary>One-line description of the resolved version for the startup log.</summary>
    public string ApiVersionText => ApiVersionKnown ? $"{ApiMajor}.{ApiMinor}" : "unknown (library predates api_version)";

    /// <summary>Known version that satisfies <see cref="MinApiMinor"/>: the facade features are guaranteed, no probing.</summary>
    private bool VersionGuaranteesFeatures => ApiVersionKnown && ApiMajor == ExpectedApiMajor && ApiMinor >= MinApiMinor;

    /// <summary>Known library of the expected major (never true while the version is unknown or the major differs).</summary>
    private bool KnownCompatible => ApiVersionKnown && ApiMajor == ExpectedApiMajor;

    /// <summary>configure_world(), world_profile() available (part of api 1.0, introduced in 0.6).</summary>
    public bool HasConfigureWorld => IsAvailable && KnownCompatible;
    /// <summary>Geodetic input and conversions, signals, attributions, status, shutdown available (part of api 1.0, introduced in 0.7).</summary>
    public bool HasHostApi => IsAvailable && KnownCompatible;

    /// <summary>The library's origin_shifted signal (api 0.7+), raised during UpdateCamera/UpdateCameraGeodetic. It does not consume TakeOriginShift().</summary>
    public event System.Action<Vector3>? OriginShifted;
    /// <summary>The library's status_changed signal (api 0.7+), raised when the state or message of status() changes.</summary>
    public event System.Action<StreamerStatus>? StatusChanged;

    private Callable _originShiftedCallable;
    private Callable _statusChangedCallable;
    private bool _signalsConnected;

    private void ConnectSignals()
    {
        if (_signalsConnected || !HasHostApi) return;
        _originShiftedCallable = Callable.From<Vector3>(v => OriginShifted?.Invoke(v));
        _statusChangedCallable = Callable.From<Dictionary>(d => StatusChanged?.Invoke(StreamerStatus.From(d)));
        _node!.Connect(SigOriginShifted, _originShiftedCallable);
        _node.Connect(SigStatusChanged, _statusChangedCallable);
        _signalsConnected = true;
    }

    private void DisconnectSignals()
    {
        if (!_signalsConnected) return;
        _signalsConnected = false;
        if (!IsAvailable) return;
        if (_node!.IsConnected(SigOriginShifted, _originShiftedCallable)) _node.Disconnect(SigOriginShifted, _originShiftedCallable);
        if (_node.IsConnected(SigStatusChanged, _statusChangedCallable)) _node.Disconnect(SigStatusChanged, _statusChangedCallable);
    }

    public static bool ClassExists() => ClassDB.ClassExists(ClassNameSn);

    /// <summary>Instantiate the Rust class. Returns null (and logs) if it is not registered.</summary>
    public static WorldStreamerFacade Create()
    {
        var facade = new WorldStreamerFacade();
        if (!ClassExists())
        {
            facade.Error = "class 'WorldStreamer' is not registered (Rust library missing, not built, or "
                + "simplane_world.gdextension not loaded). Build ../rust (cargo build) and restart/reload the editor.";
            GD.PushError("[WorldStreamerFacade] " + facade.Error);
            return facade;
        }

        var variant = ClassDB.Instantiate(ClassNameSn);
        var obj = variant.AsGodotObject();
        if (obj is Node node)
        {
            facade._node = node;
        }
        else
        {
            facade.Error = "ClassDB.Instantiate('WorldStreamer') did not return a Node (got "
                + (obj == null ? "null" : obj.GetClass()) + ").";
            GD.PushError("[WorldStreamerFacade] " + facade.Error);
            obj?.Free();
        }
        if (facade._node != null)
        {
            facade.CheckApiVersion();
            if (facade._node != null) facade.ConnectSignals();
        }
        return facade;
    }

    /// <summary>
    /// Reads the library's contract version once (api_version(), else debug_stats api_major/api_minor, else unknown)
    /// and applies the policy: unknown version or a different major (every 0.x library) = unavailable with an error naming the fix,
    /// a higher minor = one info line.
    /// </summary>
    private void CheckApiVersion()
    {
        var node = _node!;
        if (ClassDB.ClassHasMethod(ClassNameSn, MApiVersion))
        {
            var v = node.Call(MApiVersion);
            if (v.VariantType == Variant.Type.Vector2I)
            {
                var vi = v.AsVector2I();
                ApiMajor = vi.X;
                ApiMinor = vi.Y;
                ApiVersionKnown = true;
            }
        }
        else
        {
            var sv = node.Call(MDebugStats);
            if (sv.VariantType == Variant.Type.Dictionary)
            {
                var d = sv.AsGodotDictionary();
                if (d.TryGetValue(KApiMajor, out var maj) && d.TryGetValue(KApiMinor, out var min))
                {
                    ApiMajor = (int)maj.AsInt64();
                    ApiMinor = (int)min.AsInt64();
                    ApiVersionKnown = true;
                }
            }
        }

        if (!ApiVersionKnown)
        {
            Error = $"the Rust library predates api_version (a 0.x library); this facade needs contract {ExpectedApiMajor}.{MinApiMinor}+ (wall UV in window units). "
                + "Rebuild ../rust (cargo build) so the library matches the addon.";
            GD.PushError("[WorldStreamerFacade] " + Error);
            node.Free();
            _node = null;
            return;
        }

        if (ApiMajor != ExpectedApiMajor)
        {
            var hint = ApiMajor < ExpectedApiMajor
                ? "This is an old library (wall UV in meters, profile ids 0-4): rebuild ../rust (cargo build) or update the addon's bin/ libraries to api 1.0+."
                : "The library is newer than this addon: update the addon (addons/simworld) to match.";
            Error = $"Rust library api {ApiMajor}.{ApiMinor} has a different major than this facade expects ({ExpectedApiMajor}.x): "
                + "the contract is breaking across majors, so the library is not used. " + hint;
            GD.PushError("[WorldStreamerFacade] " + Error);
            node.Free();
            _node = null;
            return;
        }

        if (ApiMinor > LatestKnownMinor)
        {
            GD.Print($"[WorldStreamerFacade] Rust library api {ApiMajor}.{ApiMinor} is newer than the facade's {ExpectedApiMajor}.{LatestKnownMinor}; additive changes only, fine.");
        }
        if (!HasSetView)
            GD.Print($"[WorldStreamerFacade] set_view unavailable (library api {ApiMajor}.{ApiMinor} < {ExpectedApiMajor}.{SetViewMinApiMinor}): LOD follows fixed bands");
    }

    // ---- contract -------------------------------------------------------

    public void Configure(double originLat, double originLon, string tileUrl, string cacheDir)
    {
        if (!IsAvailable) return;
        _node!.Call(MConfigure, originLat, originLon, tileUrl, cacheDir);
    }

    /// <summary>
    /// configure_world(options) (api 0.6+): origin_lat, origin_lon, tile_url, cache_dir, profile_path, profile_overrides, diagnostics.
    /// Validates first; on an error nothing changes and <see cref="ConfigureResult.Error"/> says why. On older libraries
    /// returns Supported=false, Ok=false without calling anything (use <see cref="Configure"/>).
    /// </summary>
    public ConfigureResult ConfigureWorld(Dictionary options)
    {
        var r = new ConfigureResult();
        if (!IsAvailable) { r.Supported = false; r.Error = "WorldStreamer unavailable"; return r; }
        if (!HasConfigureWorld) { r.Supported = false; r.Error = $"configure_world needs api 0.6 (library {ApiVersionText})"; return r; }
        var v = _node!.Call(MConfigureWorld, options);
        if (v.VariantType != Variant.Type.Dictionary) { r.Error = "configure_world returned no result dictionary"; return r; }
        var d = v.AsGodotDictionary();
        r.Ok = d.TryGetValue(KOk, out var ok) && ok.AsBool();
        if (d.TryGetValue(KError, out var e)) r.Error = e.AsString();
        if (d.TryGetValue(KAttributions, out var a) && a.VariantType == Variant.Type.PackedStringArray) r.Attributions = a.AsStringArray();
        return r;
    }

    /// <summary>Builds the standard configure_world option keys (origin, tile source, cache folder).</summary>
    public static Dictionary MakeWorldOptions(double originLat, double originLon, string tileUrl, string cacheDir)
        => new()
        {
            [KOptOriginLat] = originLat,
            [KOptOriginLon] = originLon,
            [KOptTileUrl] = tileUrl,
            [KOptCacheDir] = cacheDir,
        };

    /// <summary>
    /// Adds the host opt-in <c>layers</c> option (api 1.4+): <c>["world", "terrain"]</c> when <paramref name="terrain"/>, plus <c>"landmarks"</c>
    /// (api 1.11, "I draw the landmark layer") when <paramref name="landmarks"/>; with neither nothing is
    /// added (the library's default is <c>["world"]</c>, exactly the pre-terrain behaviour). The terrain is active only together with the
    /// profile key <c>terrain.enabled</c>; passing the layer means "I draw terrain tiles".
    /// </summary>
    public static void AddLayers(Dictionary options, bool terrain, bool landmarks = false)
    {
        if (terrain && landmarks) options[KOptLayers] = new string[] { "world", "terrain", "landmarks" };
        else if (terrain) options[KOptLayers] = new string[] { "world", "terrain" };
        else if (landmarks) options[KOptLayers] = new string[] { "world", "landmarks" };
        else options.Remove(KOptLayers);
    }

    /// <summary>True when the library has the landmark layer (api 1.11): the <c>layers</c> value <c>"landmarks"</c>, "l/z/x/y" keys, <c>data_levels</c> and <c>landmarks_*</c> in debug_stats. A library below it would reject the value as an unknown layer, so the host does not send it.</summary>
    public bool HasLandmarkLayer => IsAvailable && KnownCompatible && ApiMinor >= LandmarksMinApiMinor;

    /// <summary>Sets (or removes) the <c>terrain_center</c> option [lat, lon] (api 1.6): the centre of the terrain's flat zone, kept across reconfigures.</summary>
    public static void AddTerrainCenter(Dictionary options, bool set, double lat, double lon)
    {
        if (set) options[KOptTerrainCenter] = new double[] { lat, lon };
        else options.Remove(KOptTerrainCenter);
    }

    /// <summary>True when the library has <c>terrain_center</c> and <c>status()["terrain_active"]</c> (api 1.6+).</summary>
    public bool HasTerrainActiveFlag => IsAvailable && KnownCompatible && ApiMinor >= TerrainCenterMinApiMinor;

    /// <summary>True when the library has the terrain layer (api 1.5+): the <c>layers</c> option, "t/" keys and set_terrain_detail.</summary>
    public bool HasTerrainLayer => IsAvailable && KnownCompatible && ApiMinor >= TerrainMinApiMinor;

    /// <summary>True when the library has the world seed phase S1 (api 1.7): <c>seed_world</c>, <c>airports()</c>, <c>seed_world_info()</c>.</summary>
    public bool HasSeedWorld => IsAvailable && KnownCompatible && ApiMinor >= SeedWorldMinApiMinor;

    /// <summary>Sets (or removes, for an empty path) the <c>seed_world</c> option (api 1.7): the path of a world.json (<c>res://</c>, <c>user://</c> or an OS path).</summary>
    public static void AddSeedWorld(Dictionary options, string path)
    {
        if (path.Length > 0) options[KOptSeedWorld] = path;
        else options.Remove(KOptSeedWorld);
    }

    /// <summary>world_profile() (api 0.6+): the resolved world profile (defaults before a configure with a profile). Empty when unavailable.</summary>
    public Dictionary WorldProfile()
    {
        if (!HasConfigureWorld) return new Dictionary();
        var v = _node!.Call(MWorldProfile);
        return v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : new Dictionary();
    }

    /// <summary>
    /// update_camera_geodetic (api 0.7+): WGS84 degrees, meters above MSL, velocity as (north, east, down) m/s. Use instead of
    /// <see cref="UpdateCamera"/> in a frame. Returns false when unavailable (nothing was sent).
    /// </summary>
    public bool UpdateCameraGeodetic(double lat, double lon, double altMslM, Vector3 velNed)
    {
        if (!HasHostApi) return false;
        using var timing = Timings.Time(StreamerCall.UpdateCamera);
        _node!.Call(MUpdateCameraGeodetic, lat, lon, altMslM, velNed);
        return true;
    }

    /// <summary>geodetic_to_local (api 0.7+): WGS84 point to the current local frame; Vector3.Zero when unavailable or invalid.</summary>
    public Vector3 GeodeticToLocal(double lat, double lon, double altMsl)
    {
        if (!HasHostApi) return Vector3.Zero;
        var v = _node!.Call(MGeodeticToLocal, lat, lon, altMsl);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }

    /// <summary>local_to_geodetic (api 0.7+): current local point to Vector3(lat, lon, alt_msl) (f32 resolution, about 0.1-0.4 m); Vector3.Zero when unavailable.</summary>
    public Vector3 LocalToGeodetic(Vector3 local)
    {
        if (!HasHostApi) return Vector3.Zero;
        var v = _node!.Call(MLocalToGeodetic, local);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }

    /// <summary>attributions() (api 0.7+): strings to show while map data is visible; empty for the synthetic city or when unavailable.</summary>
    public string[] Attributions()
    {
        if (!HasHostApi) return EmptyUnload;
        var v = _node!.Call(MAttributions);
        return v.VariantType == Variant.Type.PackedStringArray ? v.AsStringArray() : EmptyUnload;
    }

    private static readonly StreamerStatus OkStatus = new();

    /// <summary>status() (api 0.7+): data-source health. A shared "ok" instance (do not modify) when unavailable.</summary>
    public StreamerStatus Status()
    {
        if (!HasHostApi) return OkStatus;
        var v = _node!.Call(MStatus);
        return v.VariantType == Variant.Type.Dictionary ? StreamerStatus.From(v.AsGodotDictionary()) : OkStatus;
    }

    /// <summary>
    /// shutdown() (api 0.7+): stops the workers (bounded 1 s wait), releases the cache_dir lock; the node is inert until configured again.
    /// Also disconnects the signals. Safe to call twice or when unavailable.
    /// </summary>
    public void Shutdown()
    {
        if (!HasHostApi) return;
        DisconnectSignals();
        _node!.Call(MShutdown);
    }

    // ---- world seed (api 1.7) ---------------------------------------------

    private static readonly AirportInfo[] NoAirports = System.Array.Empty<AirportInfo>();

    /// <summary>
    /// airports() (api 1.7): the seed world's airports sorted by ident, positions in the CURRENT local frame (call again after an origin shift).
    /// Empty without a seed world, on an older library or when unavailable. Allocates (parses a few hundred light points): call it on
    /// configure and on origin shifts, not every frame.
    /// </summary>
    public AirportInfo[] Airports()
    {
        if (!HasSeedWorld) return NoAirports;
        var v = _node!.Call(MAirports);
        if (v.VariantType != Variant.Type.Array) return NoAirports;
        var arr = v.AsGodotArray();
        var list = new List<AirportInfo>(arr.Count);
        foreach (var item in arr)
        {
            if (item.VariantType != Variant.Type.Dictionary) continue;
            var d = item.AsGodotDictionary();
            var runways = new List<RunwayInfo>();
            if (d.TryGetValue("runways", out var rv) && rv.VariantType == Variant.Type.Array)
                foreach (var r in rv.AsGodotArray())
                    if (r.VariantType == Variant.Type.Dictionary) runways.Add(ParseRunway(r.AsGodotDictionary()));
            list.Add(new AirportInfo(Str(d, "ident"), Str(d, "name"), Str(d, "type"), Num(d, "lat"), Num(d, "lon"), Vec(d, "position"),
                d.TryGetValue("elevation_ft", out var el) && (el.VariantType == Variant.Type.Float || el.VariantType == Variant.Type.Int) ? el.AsDouble() : null,
                runways.ToArray()));
        }
        return list.ToArray();
    }

    private static RunwayInfo ParseRunway(Dictionary d)
    {
        return new RunwayInfo(Str(d, "designator"), ParseEnd(d, "le"), ParseEnd(d, "he"), Num(d, "length_m"), Num(d, "width_m"), Str(d, "surface"),
            Bool(d, "paved"), Bool(d, "lighted"), Bool(d, "approximate"),
            d.TryGetValue("lights", out var l) && l.VariantType == Variant.Type.PackedVector3Array ? l.AsVector3Array() : System.Array.Empty<Vector3>(),
            d.TryGetValue("light_colors", out var c) && c.VariantType == Variant.Type.PackedColorArray ? c.AsColorArray() : System.Array.Empty<Color>());
    }

    private static RunwayEnd ParseEnd(Dictionary runway, string key)
    {
        var e = runway.TryGetValue(key, out var v) && v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : new Dictionary();
        return new RunwayEnd(Str(e, "ident"), Num(e, "lat"), Num(e, "lon"), Vec(e, "position"), Num(e, "threshold_lat"), Num(e, "threshold_lon"),
            Vec(e, "threshold_position"), Num(e, "displaced_threshold_m"), Vec(e, "touchdown_position"), Num(e, "heading_deg"));
    }

    private static string Str(Dictionary d, string k) => d.TryGetValue(k, out var v) && v.VariantType == Variant.Type.String ? v.AsString() : "";
    private static double Num(Dictionary d, string k) => d.TryGetValue(k, out var v) && (v.VariantType == Variant.Type.Float || v.VariantType == Variant.Type.Int) ? v.AsDouble() : 0.0;
    private static bool Bool(Dictionary d, string k) => d.TryGetValue(k, out var v) && v.VariantType == Variant.Type.Bool && v.AsBool();
    private static Vector3 Vec(Dictionary d, string k) => d.TryGetValue(k, out var v) && v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    private static string[] Strs(Dictionary d, string k) => d.TryGetValue(k, out var v)
        ? v.VariantType == Variant.Type.PackedStringArray ? v.AsStringArray()
        : v.VariantType == Variant.Type.Array ? System.Linq.Enumerable.ToArray(System.Linq.Enumerable.Select(v.AsGodotArray(), x => x.AsString()))
        : System.Array.Empty<string>()
        : System.Array.Empty<string>();

    /// <summary>
    /// seed_world_info() (api 1.7): generator version, built phases, cities, counts, load-time warnings, memory. Null without a seed world
    /// (empty dictionary), on an older library or when unavailable.
    /// </summary>
    public SeedWorldInfo? SeedInfo()
    {
        if (!HasSeedWorld) return null;
        var v = _node!.Call(MSeedWorldInfo);
        if (v.VariantType != Variant.Type.Dictionary) return null;
        var d = v.AsGodotDictionary();
        if (d.Count == 0) return null;
        var cities = new List<SeedCityInfo>();
        if (d.TryGetValue("cities", out var cv) && cv.VariantType == Variant.Type.Array)
            foreach (var c in cv.AsGodotArray())
            {
                if (c.VariantType != Variant.Type.Dictionary) continue;
                var cd = c.AsGodotDictionary();
                cities.Add(new SeedCityInfo(Str(cd, "id"), Str(cd, "name"), Num(cd, "lat"), Num(cd, "lon"), Num(cd, "radius_km"), (int)Num(cd, "version"), Str(cd, "source"), Strs(cd, "airports")));
            }
        string mem = "";
        double loadMs = 0;
        if (d.TryGetValue("memory", out var mv) && mv.VariantType == Variant.Type.Dictionary)
        {
            var md = mv.AsGodotDictionary();
            loadMs = Num(md, "load_ms");
            mem = $"water index {Num(md, "water_index_mb"):0.##} MB + rasters {Num(md, "water_raster_mb"):0.##} MB, strips {Num(md, "strip_index_kb"):0.#} KB, airports {Num(md, "airports_kb"):0.#} KB, load {loadMs:0} ms";
        }
        return new SeedWorldInfo(Str(d, "manifest"), (int)Num(d, "generator_version"), Strs(d, "phases"), (int)Num(d, "world_version"), Str(d, "world_seed"),
            cities.ToArray(), (int)Num(d, "airports"), (int)Num(d, "water_polygons"), Strs(d, "warnings"), mem, loadMs);
    }

    public void UpdateCamera(Vector3 localPos, Vector3 velocity, double altAglM)
    {
        if (!IsAvailable) return;
        using var timing = Timings.Time(StreamerCall.UpdateCamera);
        _argsCamera[0] = localPos; _argsCamera[1] = velocity; _argsCamera[2] = altAglM; // reused: a params array per call is the per-frame garbage
        _node!.Call(MUpdateCamera, _argsCamera);
    }

    private readonly Variant[] _argsCamera = new Variant[3], _argsGround = new Variant[2];
    private bool? _hasGroundHeight;

    /// <summary>True if the loaded Rust build exports ground_height_at() (checked once).</summary>
    public bool HasGroundHeight =>
        _hasGroundHeight ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MGroundHeightAt));

    /// <summary>
    /// Terrain height in meters under a local point (Rust's single ground_height function). Returns 0 when the
    /// facade is unavailable or the Rust build does not export ground_height_at() yet.
    /// </summary>
    public double GroundHeightAt(double localX, double localZ)
    {
        if (!IsAvailable || !HasGroundHeight) return 0.0;
        _argsGround[0] = localX; _argsGround[1] = localZ;
        var v = _node!.Call(MGroundHeightAt, _argsGround);
        return v.VariantType == Variant.Type.Float ? v.AsDouble() : 0.0;
    }

    private bool? _hasSetTerrainDetail;

    /// <summary>
    /// set_terrain_detail(scale) (api 1.5; behaviour since 1.10): a scale of the terrain cover's screen-space-error threshold, tau = terrain.tau_px x scale
    /// (clamped by the library to [0.25, terrain.detail_scale_max]): above 1 is COARSER (fewer, larger tiles), below 1 is FINER and much more expensive
    /// (0.25 is about 2300 tiles). The change arrives as ordinary refines / coarsens (new keys, fades, unloads); held tiles are not re-delivered
    /// (before 1.10 the scale multiplied max_error_m and every held key was re-delivered in place).
    /// Returns the scale in effect, or NaN when the library lacks the method (older than api 1.5) or is unavailable.
    /// </summary>
    public double SetTerrainDetail(double scale)
    {
        if (!IsAvailable) return double.NaN;
        _hasSetTerrainDetail ??= ClassDB.ClassHasMethod(ClassNameSn, MSetTerrainDetail);
        if (!_hasSetTerrainDetail.Value) return double.NaN;
        var v = _node!.Call(MSetTerrainDetail, scale);
        return v.VariantType == Variant.Type.Float ? v.AsDouble() : double.NaN;
    }

    /// <summary>True when the library has <c>set_view</c> (api 1.10+): the LOD then follows screen-space error instead of fixed distance bands.</summary>
    public bool HasSetView => IsAvailable && KnownCompatible && ApiMinor >= SetViewMinApiMinor;

    /// <summary>set_view calls made (always 0 on a library without it) and the values of the last one.</summary>
    public int SetViewCalls { get; private set; }
    public double LastViewFovYDeg { get; private set; }
    public int LastViewHeightPx { get; private set; }
    public Vector3 LastViewForward { get; private set; }

    /// <summary>
    /// set_view(fov_y_deg, viewport_h_px, forward) (api 1.10): vertical field of view in degrees, viewport height in physical pixels and the
    /// camera look direction (normalized, local Godot coordinates). Call before <see cref="UpdateCamera"/>, when one of them changed.
    /// Returns false when the library lacks the method (nothing was sent).
    /// </summary>
    public bool SetView(double fovYDeg, int hPx, Vector3 forward)
    {
        if (!HasSetView) return false;
        _node!.Call(MSetView, fovYDeg, hPx, forward);
        SetViewCalls++;
        LastViewFovYDeg = fovYDeg;
        LastViewHeightPx = hPx;
        LastViewForward = forward;
        return true;
    }

    /// <summary>True when the library has <c>set_lod_quality</c> (api 1.10+).</summary>
    public bool HasLodQuality => HasSetView;

    /// <summary>
    /// set_lod_quality(scale) (api 1.10): live multiplier of the screen-space error threshold, clamped by the library to [0.25, 4]; 2 halves every
    /// refine distance (coarser, fewer tiles), 0.5 doubles them. Kept across configure_world. Returns the scale in effect, NaN without the method.
    /// </summary>
    public double SetLodQuality(double scale)
    {
        if (!HasLodQuality) return double.NaN;
        var v = _node!.Call(MSetLodQuality, scale);
        return v.VariantType == Variant.Type.Float ? v.AsDouble() : double.NaN;
    }

    private bool? _hasPollFades;

    /// <summary>True if the loaded Rust build exports poll_fades() (checked once; false = every tile is fade 1, no transitions).</summary>
    public bool HasPollFades =>
        _hasPollFades ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MPollFades));

    /// <summary>
    /// poll_fades(): key ("z/x/y") -> float fade (0..1) for every tile currently in a LOD transition (changed or not);
    /// a tile that is not listed has fade 1. Returns null when the Rust build does not export the method yet (or the
    /// facade is unavailable), which callers treat as "no tile is fading".
    /// </summary>
    public Dictionary? PollFades()
    {
        if (!IsAvailable || !HasPollFades) return null;
        using var timing = Timings.Time(StreamerCall.PollFades);
        var v = _node!.Call(MPollFades);
        return v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : null;
    }

    /// <summary>Keys ("z/x/y") of tiles Rust wants unloaded.</summary>
    public string[] PollUnload()
    {
        if (!IsAvailable) return EmptyUnload;
        using var timing = Timings.Time(StreamerCall.PollUnload);
        var v = _node!.Call(MPollUnload);
        return v.VariantType == Variant.Type.PackedStringArray ? v.AsStringArray() : EmptyUnload;
    }

    /// <summary>Non-zero when the floating origin moved: subtract it from camera and all tile roots.</summary>
    public Vector3 TakeOriginShift()
    {
        if (!IsAvailable) return Vector3.Zero;
        using var timing = Timings.Time(StreamerCall.TakeOriginShift);
        var v = _node!.Call(MTakeOriginShift);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }

    /// <summary>Raw debug_stats() dictionary (prefer the typed overload).</summary>
    public Dictionary DebugStats()
    {
        if (!IsAvailable) return new Dictionary();
        using var timing = Timings.Time(StreamerCall.DebugStats);
        var v = _node!.Call(MDebugStats);
        return v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : new Dictionary();
    }

    /// <summary>
    /// Fills <paramref name="into"/> from debug_stats(). Returns false if unavailable. <paramref name="lodOnly"/> reads just the ring counters, cache / origin / version and the
    /// screen-space LOD keys (tau_eff, quality, edges, view) and leaves everything else of <paramref name="into"/> untouched: no latency table, memory, jobs, data tile or terrain parsing
    /// (for the host's 4x per second LOD read, which does not need them).
    /// </summary>
    public bool DebugStats(StreamerStats into, bool lodOnly = false)
    {
        if (!IsAvailable) return false;
        using var timing = Timings.Time(StreamerCall.DebugStats);
        var v = _node!.Call(MDebugStats);
        if (v.VariantType != Variant.Type.Dictionary) return false;
        var d = v.AsGodotDictionary();
        ReadRing(d, KQueued, into.Queued);
        ReadRing(d, KFetching, into.Fetching);
        ReadRing(d, KMeshing, into.Meshing);
        ReadRing(d, KLoaded, into.Loaded);
        if (d.TryGetValue(KCacheMb, out var cm)) into.CacheMb = cm.AsDouble();
        if (d.TryGetValue(KOriginLat, out var la)) into.OriginLat = la.AsDouble();
        if (d.TryGetValue(KOriginLon, out var lo)) into.OriginLon = lo.AsDouble();
        if (d.TryGetValue(KVersion, out var ver)) into.Version = ver.AsString();
        if (!lodOnly) ReadLatency(d, into.Latency);
        into.HasLodStats = d.ContainsKey(KTauEff);
        into.TauEff = d.TryGetValue(KTauEff, out var tau) ? tau.AsDouble() : 0.0;
        into.LodQuality = d.TryGetValue(KLodQuality, out var lq) ? lq.AsDouble() : 1.0;
        System.Array.Clear(into.LodEdgesM);
        if (d.TryGetValue(KLodEdges, out var le) && le.VariantType == Variant.Type.PackedFloat64Array)
        {
            var arr = le.AsFloat64Array();
            for (int i = 0; i < arr.Length && i < StreamerStats.Rings; i++) into.LodEdgesM[i] = arr[i];
        }
        into.ViewFovYDeg = d.TryGetValue(KViewFov, out var vf) ? vf.AsDouble() : 0.0;
        into.ViewHeightPx = d.TryGetValue(KViewHeight, out var vh) ? (int)vh.AsInt64() : 0;
        if (lodOnly) return true;
        into.DataTilesLoaded = d.TryGetValue(KDataTilesLoaded, out var dl) ? dl.AsInt64() : 0;
        into.DataTilesDecoded = d.TryGetValue(KDataTilesDecoded, out var dd) ? dd.AsInt64() : 0;
        into.HasRequestsSkipped = d.ContainsKey(KRequestsSkippedEmpty) || d.ContainsKey(KRequestsSkippedMoving);
        into.RequestsSkippedEmpty = d.TryGetValue(KRequestsSkippedEmpty, out var se) ? se.AsInt64() : 0;
        into.RequestsSkippedMoving = d.TryGetValue(KRequestsSkippedMoving, out var sm) ? sm.AsInt64() : 0;
        into.HasJobs = d.ContainsKey(KJobsQueued);
        into.JobsQueued = d.TryGetValue(KJobsQueued, out var jq) ? jq.AsInt64() : 0;
        into.JobsRunning = d.TryGetValue(KJobsRunning, out var jr) ? jr.AsInt64() : 0;
        into.JobsCancelled = d.TryGetValue(KJobsCancelled, out var jc) ? jc.AsInt64() : 0;
        into.MemoryMb.Clear();
        if (d.TryGetValue(KMemoryMb, out var mv) && mv.VariantType == Variant.Type.Dictionary)
            foreach (var kv in mv.AsGodotDictionary())
                if (kv.Value.VariantType is Variant.Type.Float or Variant.Type.Int) into.MemoryMb.Add(new(kv.Key.AsString(), kv.Value.AsDouble()));
        into.DataTilesExtra.Clear();
        foreach (var kv in d)
        {
            if (kv.Key.VariantType != Variant.Type.String && kv.Key.VariantType != Variant.Type.StringName) continue;
            string key = kv.Key.AsString();
            if (key.StartsWith("data_tiles_", System.StringComparison.Ordinal) && key != "data_tiles_loaded" && key != "data_tiles_decoded"
                && kv.Value.VariantType is Variant.Type.Int or Variant.Type.Float)
                into.DataTilesExtra.Add(new(key.Substring(11), kv.Value.AsInt64()));
        }
        into.HasDataLevels = false;
        if (d.TryGetValue(KDataLevels, out var dlv) && dlv.VariantType == Variant.Type.Dictionary)
        {
            var dl2 = dlv.AsGodotDictionary();
            into.HasDataLevels = true;
            into.DetailZoom = dl2.TryGetValue("detail", out var dz) ? (int)dz.AsInt64() : 0;
            into.OverviewZoom = dl2.TryGetValue("overview", out var oz) ? (int)oz.AsInt64() : 0;
            into.TwoLevel = dl2.TryGetValue("two_level", out var tl) && tl.AsBool();
            into.LandmarksInArchive = dl2.TryGetValue("landmarks", out var lm) && lm.AsBool();
        }
        into.HasLandmarkStats = d.ContainsKey(KLandmarksLoaded);
        into.LandmarksLoaded = d.TryGetValue(KLandmarksLoaded, out var lml) ? (int)lml.AsInt64() : 0;
        into.LandmarksPending = d.TryGetValue(KLandmarksPending, out var lmp) ? (int)lmp.AsInt64() : 0;
        into.LandmarksEmpty = d.TryGetValue(KLandmarksEmpty, out var lme) ? (int)lme.AsInt64() : 0;
        into.HasTerrain = d.ContainsKey(KTerrainLoaded);
        System.Array.Clear(into.TerrainQueued); System.Array.Clear(into.TerrainFetching);
        System.Array.Clear(into.TerrainMeshing); System.Array.Clear(into.TerrainLoaded);
        into.TerrainRings = 0;
        into.TerrainTris = 0;
        if (into.HasTerrain)
        {
            ReadRing(d, KTerrainQueued, into.TerrainQueued);
            ReadRing(d, KTerrainFetching, into.TerrainFetching);
            ReadRing(d, KTerrainMeshing, into.TerrainMeshing);
            ReadRing(d, KTerrainLoaded, into.TerrainLoaded);
            if (d[KTerrainLoaded].VariantType == Variant.Type.PackedInt32Array) into.TerrainRings = System.Math.Min(d[KTerrainLoaded].AsInt32Array().Length, StreamerStats.TerrainRingsMax);
            if (d.TryGetValue(KTerrainTris, out var tt)) into.TerrainTris = tt.AsInt64();
            if (d.TryGetValue(KTerrainDetail, out var td)) into.TerrainDetail = td.AsDouble();
        }
        return true;
    }

    private static void ReadLatency(Dictionary d, List<LatencyStage> dst)
    {
        dst.Clear();
        if (!d.TryGetValue(KLatency, out var lv) || lv.VariantType != Variant.Type.Dictionary) return;
        foreach (var kv in lv.AsGodotDictionary())
        {
            if (kv.Value.VariantType != Variant.Type.Dictionary) continue;
            var sd = kv.Value.AsGodotDictionary();
            var st = new LatencyStage { Name = kv.Key.AsString() };
            Floats(sd, "p50", st.P50);
            Floats(sd, "p95", st.P95);
            Floats(sd, "max", st.Max);
            if (sd.TryGetValue("n", out var n) && n.VariantType == Variant.Type.PackedInt32Array)
            {
                var src = n.AsInt32Array();
                System.Array.Copy(src, st.N, System.Math.Min(src.Length, st.N.Length));
            }
            bool any = false;
            foreach (var c in st.N) any |= c > 0;
            if (any) dst.Add(st);
        }
    }

    private static void Floats(Dictionary d, string key, float[] dst)
    {
        if (!d.TryGetValue(key, out var v) || v.VariantType != Variant.Type.PackedFloat32Array) return;
        var src = v.AsFloat32Array();
        System.Array.Copy(src, dst, System.Math.Min(src.Length, dst.Length));
    }

    private static void ReadRing(Dictionary d, StringName key, int[] dst)
    {
        System.Array.Clear(dst);
        if (!d.TryGetValue(key, out var v)) return;
        if (v.VariantType == Variant.Type.PackedInt32Array)
        {
            var src = v.AsInt32Array();
            System.Array.Copy(src, dst, System.Math.Min(src.Length, dst.Length));
        }
        else if (v.VariantType == Variant.Type.Int)
        {
            dst[0] = (int)v.AsInt64(); // legacy scalar form: put everything on ring 0
        }
    }

    private bool? _hasDebugTiles;

    /// <summary>True if the loaded Rust build exports debug_tiles() (checked once).</summary>
    public bool HasDebugTiles =>
        _hasDebugTiles ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MDebugTiles));

    /// <summary>
    /// Clears <paramref name="into"/> and fills it from debug_tiles(). Returns the number of tiles
    /// (0 when the method is missing or the facade is unavailable).
    /// </summary>
    public int DebugTiles(List<DebugTile> into)
    {
        into.Clear();
        if (!IsAvailable || !HasDebugTiles) return 0;
        var v = _node!.Call(MDebugTiles);
        if (v.VariantType != Variant.Type.Array) return 0;
        var arr = v.AsGodotArray<Dictionary>();
        foreach (var d in arr)
        {
            var t = new DebugTile { State = TileState.Unknown, Key = "", Source = "" };
            if (d.TryGetValue(KKey, out var key))
            {
                t.Key = key.AsString();
                if (!TileKey.TryParse(t.Key, out t.Packed, out t.Layer)) t.Packed = 0;
                t.Terrain = t.Layer == TileLayer.Terrain;
            }
            if (d.TryGetValue(KRing, out var r)) t.Ring = (int)r.AsInt64();
            if (d.TryGetValue(KState, out var st)) t.State = ParseState(st.AsString());
            if (d.TryGetValue(KMin, out var mn)) t.Min = mn.AsVector2();
            if (d.TryGetValue(KMax, out var mx)) t.Max = mx.AsVector2();
            if (d.TryGetValue(KSource, out var src)) t.Source = src.AsString();
            into.Add(t);
        }
        return into.Count;
    }

    private static TileState ParseState(string s) => s switch
    {
        "queued" => TileState.Queued,
        "fetching" => TileState.Fetching,
        "meshing" => TileState.Meshing,
        "ready" => TileState.Ready,
        "failed" => TileState.Failed,
        "unloading" => TileState.Unloading,
        "empty" => TileState.Empty,
        _ => TileState.Unknown,
    };
}
