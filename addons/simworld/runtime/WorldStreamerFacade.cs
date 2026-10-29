#nullable enable
using System.Collections.Generic;
using Godot;
using Godot.Collections;

namespace SimWorld;

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
}
