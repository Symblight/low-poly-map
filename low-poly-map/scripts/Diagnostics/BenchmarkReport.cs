using System;
using System.Globalization;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>Writes the JSON result of a <see cref="Benchmark"/> (and the short baseline summary of it).</summary>
public static class BenchmarkReport
{
    public static string Build(Benchmark b, string outputPath)
    {
        var view = b.View;
        var stats = new StreamerStats();
        view.Streamer.DebugStats(stats);
        var session = PerfMonitor.Session;

        var j = new JsonText();
        j.Begin();
        j.Field("benchmark", b.Def.Name);
        j.Field("status", "ok");
        j.Field("finished_utc", DateTime.UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", CultureInfo.InvariantCulture));
        j.Field("args", string.Join(" ", OS.GetCmdlineUserArgs()));
        WriteConfig(j, b, stats);
        j.BeginObject("end_state");
        j.Field("sim_s", Math.Round(b.MeasuredSeconds + Benchmark.WarmupSeconds, 2));
        j.Field("east_m", Math.Round(b.Pose.East, 1));
        j.Field("north_m", Math.Round(b.Pose.North, 1));
        j.Field("agl_m", Math.Round(b.Pose.AglM, 1));
        j.Field("origin_shifts", view.OriginShiftCount);
        j.End();
        j.Field("build", new RawJson(MachineInfo.BuildJson(view, b.LibraryVersion)));
        j.Field("machine", new RawJson(MachineInfo.MachineJson()));
        j.BeginObject("perf");
        session.WriteFields(j);
        if (session.GpuFrames == 0)
            j.Field("gpu_note", $"no GPU time: the {RenderingServer.GetCurrentRenderingDriverName()} driver returned 0 for every viewport timestamp");
        j.End();
        j.BeginObject("markers");
        PerfMarker.WriteFields(j);
        j.End();
        WriteFrameSplit(j, b);
        WriteChurn(j, b);
        WriteUploads(j, b);
        WriteGc(j, b);
        b.Diagnostics.Stalls.WriteFields(j);
        WriteOverhead(j, b.Diagnostics);
        j.BeginObject("series");
        b.AllSeries.Write(j);
        j.End();
        if (b.Args.SoakSeconds > 0) WriteSoak(j, b);
        j.BeginObject("noise");
        j.Field("before", new RawJson(b.NoiseStart));
        j.Field("after", new RawJson(b.NoiseEnd));
        j.Field("note", "numbers measured while other processes were busy are noisy; write baselines only on an idle machine");
        j.End();
        j.Field("baseline", BenchmarkFiles.Compare(BenchmarkFiles.BaselineKey(b.Def.Name, b.LibraryFlavour, b.GcQuietMb), session));
        j.Field("output", outputPath);
        j.End();
        return j.Pretty();
    }

    /// <summary>The entry <c>--update-baseline</c> stores: the numbers a later run is compared with.</summary>
    public static string BaselineEntry(Benchmark b)
    {
        var s = PerfMonitor.Session;
        var j = new JsonText();
        j.Begin();
        j.Field("frame_p95_ms", Math.Round(s.P95Ms, 3));
        j.Field("frame_mean_ms", Math.Round(s.MeanFrameMs, 3));
        j.Field("avg_fps", Math.Round(s.AvgFps, 1));
        j.Field("gpu_p95_ms", Math.Round(s.P95GpuMs, 3));
        j.Field("draw_calls_mean", Math.Round(s.MeanDrawCalls));
        j.Field("process_mb_last", Math.Round(b.AllSeries[Metric.ProcessMb].Last, 1));
        j.Field("seconds", Math.Round(s.DurationS, 1));
        j.Field("library_flavour", b.LibraryFlavour);
        j.Field("recorded_utc", DateTime.UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", CultureInfo.InvariantCulture));
        j.End();
        return j.ToString();
    }

    static void WriteConfig(JsonText j, Benchmark b, StreamerStats stats)
    {
        var view = b.View;
        j.BeginObject("config");
        j.Field("path", b.Path.Describe);
        j.Field("hour", b.Def.Hour);
        j.Field("seconds", b.Args.Seconds);
        j.Field("frames", b.Args.Frames);
        j.Field("soak_s", b.Args.SoakSeconds);
        j.Field("warmup_s", Benchmark.WarmupSeconds);
        j.Field("settle_s", Math.Round(b.SettleSeconds, 1));
        j.Field("settled", b.Settled);
        j.Field("fill_s", Math.Round(b.FillSeconds, 2));
        j.Field("fill_tiles", b.FillTiles);
        j.Field("measured_s", Math.Round(b.MeasuredSeconds, 2));
        j.Field("tile_source", view.TileSource);
        j.Field("terrain_active", view.TerrainActive);
        j.Field("landmarks_active", view.LandmarksActive);
        j.Field("view_fov_deg", Math.Round(view.ViewFovYDeg, 2));
        j.Field("view_height_px", view.ViewHeightPx);
        j.Field("tau_eff", Math.Round(stats.TauEff, 3));
        j.Field("lod_quality", stats.LodQuality);
        j.Field("lod_edges_m", string.Join(" ", Array.ConvertAll(stats.LodEdgesM, e => e.ToString("0", CultureInfo.InvariantCulture))));
        j.Field("reconfigures", b.Reconfigures);
        j.Field("reconfigure_failures", b.ReconfigureFailures);
        j.End();
    }

    /// <summary>
    /// Where the frame time goes: script work (the process callbacks, measured wall time), physics, the renderer's CPU work, GPU, and the rest
    /// (presentation, GPU wait, vsync, idle). With the main thread's CPU use this says CPU-bound or waiting. Means over the measured part.
    /// </summary>
    static void WriteFrameSplit(JsonText j, Benchmark b)
    {
        var s = PerfMonitor.Session;
        double frame = Math.Max(s.MeanFrameMs, 1e-6);
        double mainPct = b.AllSeries[Metric.CpuMainPct].Mean, processPct = b.AllSeries[Metric.CpuProcessPct].Mean;
        string verdict = double.IsNaN(mainPct) || b.AllSeries[Metric.CpuMainPct].Count == 0 ? "unknown (main-thread CPU is read on macOS only)"
            : mainPct >= 90 ? "main thread bound: it is busy " + Math.Round(mainPct) + " % of the time"
            : "main thread idle " + Math.Round(100 - mainPct) + " % of the time: waiting on the GPU, the presentation or a worker";
        j.BeginObject("frame_split");
        j.Field("frame_mean_ms", Math.Round(s.MeanFrameMs, 3));
        j.Field("process_wall_ms", Math.Round(s.MeanProcessWallMs, 3));
        j.Field("physics_ms", Math.Round(s.MeanPhysicsMs, 3));
        j.Field("render_cpu_ms", Math.Round(s.MeanRenderCpuMs, 3));
        j.Field("gpu_ms", s.GpuFrames == 0 ? "not measured" : Math.Round(s.MeanGpuMs, 3).ToString(System.Globalization.CultureInfo.InvariantCulture));
        j.Field("other_wait_ms", Math.Round(s.MeanOtherMs, 3));
        j.Field("process_wall_share_pct", Math.Round(s.MeanProcessWallMs / frame * 100, 1));
        j.Field("render_cpu_share_pct", Math.Round(s.MeanRenderCpuMs / frame * 100, 1));
        j.Field("other_wait_share_pct", Math.Round(s.MeanOtherMs / frame * 100, 1));
        j.Field("process_cpu_pct_mean", Math.Round(processPct, 1));
        j.Field("main_thread_cpu_pct_mean", Math.Round(mainPct, 1));
        j.Field("other_threads_cpu_pct_mean", Math.Round(b.AllSeries[Metric.CpuOtherPct].Mean, 1));
        j.Field("verdict", verdict);
        j.End();
    }

    /// <summary>Visibility churn over the measured part: tiles shown / hidden per layer, quick toggles of the same tile (flicker candidates), fades, re-deliveries.</summary>
    static void WriteChurn(JsonText j, Benchmark b)
    {
        var d = ViewTotals.Read(b.View) - b.StartTotals;
        double perS = 1.0 / Math.Max(b.MeasuredSeconds, 1e-3);
        j.BeginObject("visibility_churn");
        j.Field("vector_shown", d.VectorShown);
        j.Field("vector_hidden", d.VectorHidden);
        j.Field("vector_quick_toggles", d.VectorQuick);
        j.Field("terrain_shown", d.TerrainShown);
        j.Field("terrain_hidden", d.TerrainHidden);
        j.Field("terrain_quick_toggles", d.TerrainQuick);
        j.Field("vector_toggles_per_s", Math.Round((d.VectorShown + d.VectorHidden) * perS, 2));
        j.Field("terrain_toggles_per_s", Math.Round((d.TerrainShown + d.TerrainHidden) * perS, 2));
        j.Field("quick_toggles_per_s", Math.Round((d.VectorQuick + d.TerrainQuick) * perS, 2));
        j.Field("fades_started", d.FadesStarted);
        j.Field("fades_ended", d.FadesEnded);
        j.Field("fades_active_max", b.AllSeries[Metric.FadesActive].Max);
        j.Field("redeliveries", d.Redeliveries);
        j.Field("note", "quick toggle = the same tile changed shown/hidden twice within 1 s; redelivery = a key delivered again less than 10 s after it was removed");
        j.End();
    }

    /// <summary>Uploads over the measured part: tiles per layer, frames that uploaded, frames over the upload budget; the time per layer is in <c>markers</c>.</summary>
    static void WriteUploads(JsonText j, Benchmark b)
    {
        var d = ViewTotals.Read(b.View) - b.StartTotals;
        var s = PerfMonitor.Session;
        j.BeginObject("uploads");
        j.Field("vector_tiles", d.UploadedVector);
        j.Field("terrain_tiles", d.UploadedTerrain);
        j.Field("landmark_tiles", d.UploadedLandmarks);
        j.Field("frames_with_uploads", d.UploadFrames);
        j.Field("frames_over_budget", d.UploadFramesOverBudget);
        j.Field("budget_us", b.View.Settings?.UploadBudgetUs ?? 0);
        j.Field("frames_with_uploads_pct", Math.Round(d.UploadFrames * 100.0 / Math.Max(s.Frames, 1), 2));
        j.Field("tiles_per_frame_with_uploads", Math.Round((d.UploadedVector + d.UploadedTerrain + d.UploadedLandmarks) / (double)Math.Max(d.UploadFrames, 1), 2));
        j.Field("max_ms_in_a_frame", Math.Round(Math.Max(PerfMarker.MaxMs(PerfCat.UploadVector), Math.Max(PerfMarker.MaxMs(PerfCat.UploadTerrain), PerfMarker.MaxMs(PerfCat.UploadLandmarks))), 3));
        j.Field("burst_frames", b.View.UploadBurstFrames);
        for (int r = 0; r < 4; r++)
        {
            int n = b.View.SpriteFillCount[r];
            j.Field($"sprite_fill_r{r}_tiles", n);
            j.Field($"sprite_fill_r{r}_mean_ms", n == 0 ? 0 : Math.Round(b.View.SpriteFillUsSum[r] / 1000.0 / n, 3));
            j.Field($"sprite_fill_r{r}_max_ms", Math.Round(b.View.SpriteFillUsMax[r] / 1000.0, 3));
        }
        var cells = b.View.SpriteCells; // merged point sprite cells (R2 / R3), counters since the start of the run
        j.Field("sprite_cells", cells.Cells);
        j.Field("sprite_cell_tiles", cells.Tiles);
        j.Field("sprite_cell_points", cells.Points);
        j.Field("sprite_cell_dead_points", cells.DeadPoints);
        j.Field("sprite_cell_rebuilds", cells.Rebuilds);
        j.Field("sprite_cell_rebuilt_points", cells.RebuiltPoints);
        j.Field("sprite_cell_rebuild_total_ms", Math.Round(cells.RebuildUs / 1000.0, 2));
        j.Field("sprite_cell_rebuild_max_ms", Math.Round(cells.RebuildUsMax / 1000.0, 3));
        j.Field("sprite_cell_table_writes", cells.TableWrites);
        j.End();
    }

    /// <summary>C# garbage collector over the measured part: collections per generation, allocated MB and the mean rate.</summary>
    static void WriteGc(JsonText j, Benchmark b)
    {
        double allocatedMb = (GC.GetTotalAllocatedBytes() - b.AllocatedBytes) / 1048576.0;
        j.BeginObject("gc");
        j.Field("server_gc", System.Runtime.GCSettings.IsServerGC);
        j.Field("latency_mode", System.Runtime.GCSettings.LatencyMode.ToString());
        j.Field("concurrent_switch", AppContext.GetData("System.GC.Concurrent")?.ToString() ?? "unset");
        j.Field("gc_count_total", GC.CollectionCount(0));
        if (b.View.GcPacing is { } pacer)
        {
            j.Field("pacer_budget_mb", pacer.BudgetBytes >> 20);
            j.Field("pacer_starts", pacer.Starts);
            j.Field("pacer_rotations", pacer.Rotations);
            j.Field("pacer_early_exits", pacer.EarlyExits);
            j.Field("pacer_refused", pacer.Refused);
            j.Field("pacer_rotation_ms_max", Math.Round(pacer.RotationMsMax, 2));
            j.Field("pacer_memory_warnings", pacer.MemoryWarnings);
            j.Field("pacer_users", GcPacer.Users);
        }
        j.Field("collections_gen0", GC.CollectionCount(0) - b.GcGen0);
        j.Field("collections_gen1", GC.CollectionCount(1) - b.GcGen1);
        j.Field("collections_gen2", GC.CollectionCount(2) - b.GcGen2);
        j.Field("allocated_mb", Math.Round(allocatedMb, 2));
        j.Field("allocated_mb_per_s", Math.Round(allocatedMb / Math.Max(PerfMonitor.Session.DurationS, 1e-3), 4));
        b.Allocs.WriteFields(j, PerfMonitor.Session.DurationS, PerfMonitor.Session.Frames);
        b.Diagnostics.Collections.WriteFields(j);
        j.End();
    }

    /// <summary>What the measuring itself cost: the per-frame part, the once-a-second part, and the two together per frame.</summary>
    static void WriteOverhead(JsonText j, DiagnosticsNode d)
    {
        long frames = Math.Max(d.FrameCost.Count, 1);
        j.BeginObject("diagnostics_overhead");
        j.Field("frame_mean_ms", Math.Round(d.FrameCost.Mean, 5));
        j.Field("frame_p99_ms", Math.Round(d.FrameCost.Percentile(0.99), 5));
        j.Field("frame_max_ms", Math.Round(d.FrameCost.Max, 5));
        j.Field("second_mean_ms", Math.Round(d.SecondCost.Mean, 4));
        j.Field("second_max_ms", Math.Round(d.SecondCost.Max, 4));
        j.Field("per_frame_total_ms", Math.Round((d.FrameCost.Sum + d.SecondCost.Sum) / frames, 5));
        j.Field("frame_allocated_bytes", d.FrameAllocatedBytes);
        j.End();
    }

    /// <summary>First minute against last minute (thermal throttling, leaks) and the memory slopes after the first minute.</summary>
    static void WriteSoak(JsonText j, Benchmark b)
    {
        j.BeginObject("soak");
        j.BeginObject("first_minute");
        b.FirstMinute.WriteFields(j);
        j.BeginObject("series"); b.FirstSeries.Write(j); j.End();
        j.End();
        j.BeginObject("last_minute");
        b.LastMinute.WriteFields(j);
        j.BeginObject("series"); b.LastSeries.Write(j); j.End();
        j.End();
        j.BeginObject("after_first_minute");
        j.BeginObject("series"); b.SettledSeries.Write(j); j.End();
        j.End();
        double first = b.FirstMinute.P95Ms, last = b.LastMinute.P95Ms;
        j.Field("p95_change_pct", first > 0 ? Math.Round((last - first) / first * 100.0, 1) : 0);
        j.BeginObject("memory_slope_mb_per_min");
        foreach (var metric in new[] { Metric.ProcessMb, Metric.GcHeapMb, Metric.GodotStaticMb, Metric.LibMemoryMb })
            j.Field(MetricSet.Name(metric), Math.Round(b.SettledSeries[metric].SlopePerMinute, 3));
        j.End();
        j.End();
    }
}
