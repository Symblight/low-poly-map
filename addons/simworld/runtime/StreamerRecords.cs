#nullable enable
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
    private readonly List<LatencyStage> _latencySpare = new();
    private LatencyStage? _latencyCurrent;

    // The stage objects are reused by every read (the list is rebuilt, the objects are not), so a periodic read allocates none.
    internal void RecycleLatency() { _latencySpare.AddRange(Latency); Latency.Clear(); }

    internal LatencyStage NextLatencyStage(string name)
    {
        int last = _latencySpare.Count - 1;
        var st = last >= 0 ? _latencySpare[last] : new LatencyStage();
        if (last >= 0) _latencySpare.RemoveAt(last);
        st.Name = name;
        return _latencyCurrent = st;
    }

    /// <summary>Keeps the stage handed out last when it has samples, else takes it back.</summary>
    internal void KeepLatencyStageIfSampled()
    {
        var st = _latencyCurrent!;
        bool any = false;
        foreach (int n in st.N) any |= n > 0;
        if (any) Latency.Add(st);
        else _latencySpare.Add(st);
    }

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
