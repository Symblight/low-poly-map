using System;

namespace LowPolyMap.Diagnostics;

/// <summary>The numbers sampled once a second (see <see cref="SecondSampler"/>), in the order of <see cref="MetricSet"/>'s arrays.</summary>
public enum Metric
{
    // Memory
    /// <summary>Whole-process memory (<see cref="ProcessMemory"/>), MB.</summary>
    ProcessMb,
    /// <summary><c>OS.GetStaticMemoryUsage</c>: Godot's own allocations, MB.</summary>
    GodotStaticMb,
    /// <summary><c>GC.GetTotalMemory(false)</c>: the managed (C#) heap, MB.</summary>
    GcHeapMb,
    /// <summary>Managed allocation rate since the previous sample, MB/s.</summary>
    GcAllocMbPerS,
    /// <summary>Library <c>debug_stats()["memory_mb"]["total"]</c> (Rust tile memory), MB.</summary>
    LibMemoryMb,
    LibDataTilesMb,
    LibHeightTilesMb,
    // Library and tiles
    JobsQueued,
    JobsRunning,
    DataTilesLoaded,
    /// <summary>Tiles waiting in the library's queues (all layers, queued + fetching + meshing).</summary>
    TilesQueued,
    TilesVector,
    TilesTerrain,
    TilesLandmarks,
    /// <summary>Tile nodes created by the host pools so far (flat once the pools are warm).</summary>
    PoolNodes,
    // CPU (100 = one core busy)
    /// <summary>CPU time of the whole process over the last second, % of one core (user + system).</summary>
    CpuProcessPct,
    /// <summary>CPU time of the main thread (macOS only; NaN elsewhere), % of one core.</summary>
    CpuMainPct,
    /// <summary>Process minus main thread: the Rust workers (tokio, rayon) plus Godot's render, audio and GC threads, % of one core.</summary>
    CpuOtherPct,
    // Frame split, means over the last full one-second window (ms)
    FrameMs,
    ProcessWallMs,
    RenderCpuMs,
    /// <summary>Frame time minus process callbacks, physics and render CPU: presentation, GPU wait, vsync, idle.</summary>
    OtherWaitMs,
    // Visibility churn per second
    ShownVectorPerS,
    HiddenVectorPerS,
    ShownTerrainPerS,
    HiddenTerrainPerS,
    /// <summary>Toggles of a tile that had toggled less than a second before (flicker candidates).</summary>
    QuickTogglesVectorPerS,
    QuickTogglesTerrainPerS,
    FadesStartedPerS,
    FadesEndedPerS,
    /// <summary>Tiles in a dithered fade right now (both layers).</summary>
    FadesActive,
    /// <summary>Keys delivered again less than 10 s after they were removed (both layers).</summary>
    RedeliveriesPerS,
    // Uploads
    UploadedTilesPerS,
    UploadFramesOverBudgetPerS,
    // Per layer draw proxy: shown tiles inside the camera frustum and their triangles
    VectorInFrustum,
    VectorTrisInFrustum,
    TerrainInFrustum,
    TerrainTrisInFrustum,
}

/// <summary>One <see cref="Series"/> per <see cref="Metric"/>; a run keeps several (whole run, first minute, last minute, after the first minute).</summary>
public sealed class MetricSet
{
    static readonly string[] Names =
    {
        "process_mb", "godot_static_mb", "gc_heap_mb", "gc_alloc_mb_per_s", "lib_memory_total_mb", "lib_data_tiles_mb", "lib_height_tiles_mb",
        "jobs_queued", "jobs_running", "data_tiles_loaded", "tiles_queued", "tiles_vector", "tiles_terrain", "tiles_landmarks", "pool_nodes",
        "cpu_process_pct", "cpu_main_thread_pct", "cpu_other_threads_pct",
        "frame_ms", "process_wall_ms", "render_cpu_ms", "other_wait_ms",
        "shown_vector_per_s", "hidden_vector_per_s", "shown_terrain_per_s", "hidden_terrain_per_s", "quick_toggles_vector_per_s", "quick_toggles_terrain_per_s",
        "fades_started_per_s", "fades_ended_per_s", "fades_active", "redeliveries_per_s",
        "uploaded_tiles_per_s", "upload_frames_over_budget_per_s",
        "vector_tiles_in_frustum", "vector_tris_in_frustum", "terrain_tiles_in_frustum", "terrain_tris_in_frustum",
    };

    public static readonly int Count = Names.Length;

    readonly Series[] _series = new Series[Count];

    public MetricSet()
    {
        for (int i = 0; i < Count; i++) _series[i] = new Series();
    }

    public Series this[Metric metric] => _series[(int)metric];

    public void Add(double seconds, double[] values)
    {
        for (int i = 0; i < Count; i++) _series[i].Add(seconds, values[i]);
    }

    /// <summary>Every series as a field of the object <paramref name="j"/> is in.</summary>
    public void Write(JsonText j)
    {
        for (int i = 0; i < Count; i++) _series[i].Write(j, Names[i]);
    }

    public static string Name(Metric metric) => Names[(int)metric];

    /// <summary>The enum and the name table must agree.</summary>
    static MetricSet()
    {
        if (Enum.GetValues<Metric>().Length != Names.Length) throw new InvalidOperationException("Metric and MetricSet.Names are out of step");
    }
}
