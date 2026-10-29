using System.Collections.Generic;
using Godot;
using Godot.Collections;

namespace SimWorld;

// debug_stats() and debug_tiles() read into reused host records. Every Variant and collection wrapper taken from the library's dictionaries is released at once (see
// WorldStreamerFacade.Frame.cs): these run 1 to 4 times a second and used to leave tens of kilobytes and hundreds of finalizable objects behind per read.
public sealed partial class WorldStreamerFacade
{
    private static readonly StringName KP50 = new("p50"), KP95 = new("p95"), KMaxMs = new("max"), KCount = new("n");
    private static readonly StringName KDetail = new("detail"), KOverview = new("overview"), KTwoLevel = new("two_level"), KLandmarksFlag = new("landmarks");
    private bool? _hasDebugTiles;

    /// <summary>Raw debug_stats() dictionary (prefer the typed overload). The caller disposes it.</summary>
    public Dictionary DebugStats()
    {
        if (!IsAvailable) return new Dictionary();
        using var timing = Timings.Time(StreamerCall.DebugStats);
        using var v = _node!.Call(MDebugStats);
        return v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : new Dictionary();
    }

    /// <summary>
    /// Fills <paramref name="into"/> from debug_stats(). Returns false if unavailable. <paramref name="lodOnly"/> reads just the ring counters, cache / origin / version and the
    /// screen-space LOD keys (tau_eff, quality, edges, view) and leaves everything else of <paramref name="into"/> untouched: no latency table, memory, jobs, data tile or terrain parsing
    /// (for the host's 4x per second LOD read, which does not need them). <paramref name="latency"/> false leaves <see cref="StreamerStats.Latency"/> alone (the table is the most expensive part to read).
    /// </summary>
    public bool DebugStats(StreamerStats into, bool lodOnly = false, bool latency = true)
    {
        if (!IsAvailable) return false;
        using var timing = Timings.Time(StreamerCall.DebugStats);
        using var v = _node!.Call(MDebugStats);
        if (v.VariantType != Variant.Type.Dictionary) return false;
        using var d = v.AsGodotDictionary();
        ReadRing(d, KQueued, into.Queued);
        ReadRing(d, KFetching, into.Fetching);
        ReadRing(d, KMeshing, into.Meshing);
        ReadRing(d, KLoaded, into.Loaded);
        into.CacheMb = Dbl(d, KCacheMb);
        into.OriginLat = Dbl(d, KOriginLat);
        into.OriginLon = Dbl(d, KOriginLon);
        if (d.ContainsKey(KVersion)) into.Version = Txt(d, KVersion);
        if (!lodOnly && latency) ReadLatency(d, into);
        ReadLodFields(d, into);
        if (lodOnly) return true;
        into.DataTilesLoaded = Lng(d, KDataTilesLoaded);
        into.DataTilesDecoded = Lng(d, KDataTilesDecoded);
        into.HasRequestsSkipped = d.ContainsKey(KRequestsSkippedEmpty) || d.ContainsKey(KRequestsSkippedMoving);
        into.RequestsSkippedEmpty = Lng(d, KRequestsSkippedEmpty);
        into.RequestsSkippedMoving = Lng(d, KRequestsSkippedMoving);
        into.HasJobs = d.ContainsKey(KJobsQueued);
        into.JobsQueued = Lng(d, KJobsQueued);
        into.JobsRunning = Lng(d, KJobsRunning);
        into.JobsCancelled = Lng(d, KJobsCancelled);
        ReadMemoryAndDataTiles(d, into);
        into.HasDataLevels = false;
        if (d.TryGetValue(KDataLevels, out var dlv))
        {
            if (dlv.VariantType == Variant.Type.Dictionary)
            {
                using var levels = dlv.AsGodotDictionary();
                into.HasDataLevels = true;
                into.DetailZoom = (int)Lng(levels, KDetail);
                into.OverviewZoom = (int)Lng(levels, KOverview);
                into.TwoLevel = Flag(levels, KTwoLevel);
                into.LandmarksInArchive = Flag(levels, KLandmarksFlag);
            }
            dlv.Dispose();
        }
        into.HasLandmarkStats = d.ContainsKey(KLandmarksLoaded);
        into.LandmarksLoaded = (int)Lng(d, KLandmarksLoaded);
        into.LandmarksPending = (int)Lng(d, KLandmarksPending);
        into.LandmarksEmpty = (int)Lng(d, KLandmarksEmpty);
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
            into.TerrainRings = ReadRing(d, KTerrainLoaded, into.TerrainLoaded);
            if (into.TerrainRings > StreamerStats.TerrainRingsMax) into.TerrainRings = StreamerStats.TerrainRingsMax;
            into.TerrainTris = Lng(d, KTerrainTris);
            if (d.ContainsKey(KTerrainDetail)) into.TerrainDetail = Dbl(d, KTerrainDetail);
        }
        return true;
    }

    /// <summary>The screen-space LOD keys (tau_eff, quality, edges, view) of a debug_stats() or lod_state() dictionary.</summary>
    private static void ReadLodFields(Dictionary d, StreamerStats into)
    {
        into.HasLodStats = d.ContainsKey(KTauEff);
        into.TauEff = Dbl(d, KTauEff);
        into.LodQuality = Dbl(d, KLodQuality, 1.0);
        System.Array.Clear(into.LodEdgesM);
        if (d.TryGetValue(KLodEdges, out var le))
        {
            if (le.VariantType == Variant.Type.PackedFloat64Array)
            {
                var arr = le.AsFloat64Array();
                for (int i = 0; i < arr.Length && i < StreamerStats.Rings; i++) into.LodEdgesM[i] = arr[i];
            }
            le.Dispose();
        }
        into.ViewFovYDeg = Dbl(d, KViewFov);
        into.ViewHeightPx = (int)Lng(d, KViewHeight);
    }

    // The key names of the library's dictionaries that are not known in advance (the stages of the latency table, the pools of memory_mb, the extra data_tiles_* counters). Enumerating a
    // dictionary turns every key into a Variant and a string, which was most of the cost of a read; the names are collected once per size of the dictionary (and again every 64th read in case a
    // minor release renames one) and the values are then looked up by StringName.
    private sealed class KeyNames
    {
        public int Count = -1, Reads;
        public readonly List<(StringName Key, string Name)> Items = new();

        public bool Refresh(Dictionary d, System.Func<string, bool> accept)
        {
            if (Count == d.Count && ++Reads % 64 != 0) return false;
            Count = d.Count;
            Items.Clear();
            foreach (var kv in d)
            {
                if (kv.Key.VariantType is Variant.Type.String or Variant.Type.StringName)
                {
                    string name = kv.Key.AsString();
                    if (accept(name)) Items.Add((new StringName(name), name));
                }
                kv.Key.Dispose();
                kv.Value.Dispose();
            }
            return true;
        }
    }

    private readonly KeyNames _memoryNames = new(), _extraNames = new(), _latencyNames = new();
    private static bool AnyKey(string name) => true;
    private static bool ExtraDataTilesKey(string name) =>
        name.StartsWith("data_tiles_", System.StringComparison.Ordinal) && name != "data_tiles_loaded" && name != "data_tiles_decoded";

    /// <summary>"memory_mb" (every number it holds) and the extra "data_tiles_*" counters.</summary>
    private void ReadMemoryAndDataTiles(Dictionary d, StreamerStats into)
    {
        into.MemoryMb.Clear();
        if (d.TryGetValue(KMemoryMb, out var mv))
        {
            if (mv.VariantType == Variant.Type.Dictionary)
            {
                using var mem = mv.AsGodotDictionary();
                _memoryNames.Refresh(mem, AnyKey);
                foreach (var (key, name) in _memoryNames.Items)
                {
                    if (!mem.TryGetValue(key, out var v)) continue;
                    if (v.VariantType is Variant.Type.Float or Variant.Type.Int) into.MemoryMb.Add(new(name, v.AsDouble()));
                    v.Dispose();
                }
            }
            mv.Dispose();
        }
        into.DataTilesExtra.Clear();
        _extraNames.Refresh(d, ExtraDataTilesKey);
        foreach (var (key, name) in _extraNames.Items)
        {
            if (!d.TryGetValue(key, out var v)) continue;
            if (v.VariantType is Variant.Type.Int or Variant.Type.Float) into.DataTilesExtra.Add(new(name.Substring(11), v.AsInt64()));
            v.Dispose();
        }
    }

    private void ReadLatency(Dictionary d, StreamerStats into)
    {
        into.RecycleLatency();
        if (!d.TryGetValue(KLatency, out var lv)) return;
        if (lv.VariantType == Variant.Type.Dictionary)
        {
            using var table = lv.AsGodotDictionary();
            _latencyNames.Refresh(table, AnyKey);
            foreach (var (key, name) in _latencyNames.Items)
            {
                if (!table.TryGetValue(key, out var sv)) continue;
                if (sv.VariantType == Variant.Type.Dictionary)
                {
                    using var sd = sv.AsGodotDictionary();
                    var st = into.NextLatencyStage(name);
                    Floats(sd, KP50, st.P50);
                    Floats(sd, KP95, st.P95);
                    Floats(sd, KMaxMs, st.Max);
                    System.Array.Clear(st.N);
                    if (sd.TryGetValue(KCount, out var n))
                    {
                        if (n.VariantType == Variant.Type.PackedInt32Array)
                        {
                            var src = n.AsInt32Array();
                            System.Array.Copy(src, st.N, System.Math.Min(src.Length, st.N.Length));
                        }
                        n.Dispose();
                    }
                    into.KeepLatencyStageIfSampled();
                }
                sv.Dispose();
            }
        }
        lv.Dispose();
    }

    private static void Floats(Dictionary d, StringName key, float[] dst)
    {
        System.Array.Clear(dst);
        if (!d.TryGetValue(key, out var v)) return;
        if (v.VariantType == Variant.Type.PackedFloat32Array)
        {
            var src = v.AsFloat32Array();
            System.Array.Copy(src, dst, System.Math.Min(src.Length, dst.Length));
        }
        v.Dispose();
    }

    /// <summary>Copies the per-ring counts under <paramref name="key"/> into <paramref name="dst"/>; returns how many rings the library sent.</summary>
    private static int ReadRing(Dictionary d, StringName key, int[] dst)
    {
        System.Array.Clear(dst);
        if (!d.TryGetValue(key, out var v)) return 0;
        int rings = 0;
        if (v.VariantType == Variant.Type.PackedInt32Array)
        {
            var src = v.AsInt32Array();
            rings = src.Length;
            System.Array.Copy(src, dst, System.Math.Min(src.Length, dst.Length));
        }
        else if (v.VariantType == Variant.Type.Int)
        {
            dst[0] = (int)v.AsInt64(); // legacy scalar form: put everything on ring 0
        }
        v.Dispose();
        return rings;
    }

    private static double Dbl(Dictionary d, StringName key, double fallback = 0.0)
    {
        if (!d.TryGetValue(key, out var v)) return fallback;
        double r = v.AsDouble();
        v.Dispose();
        return r;
    }

    private static long Lng(Dictionary d, StringName key)
    {
        if (!d.TryGetValue(key, out var v)) return 0;
        long r = v.AsInt64();
        v.Dispose();
        return r;
    }

    private static bool Flag(Dictionary d, StringName key)
    {
        if (!d.TryGetValue(key, out var v)) return false;
        bool r = v.AsBool();
        v.Dispose();
        return r;
    }

    private static string Txt(Dictionary d, StringName key)
    {
        if (!d.TryGetValue(key, out var v)) return "";
        string r = v.AsString();
        v.Dispose();
        return r;
    }

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
        using var v = _node!.Call(MDebugTiles);
        if (v.VariantType != Variant.Type.Array) return 0;
        using var arr = v.AsGodotArray();
        for (int i = 0; i < arr.Count; i++)
        {
            using var ev = arr[i];
            if (ev.VariantType != Variant.Type.Dictionary) continue;
            using var d = ev.AsGodotDictionary();
            var t = new DebugTile { State = TileState.Unknown, Key = "", Source = "" };
            if (d.ContainsKey(KKey))
            {
                t.Key = Txt(d, KKey);
                if (!TileKey.TryParse(t.Key, out t.Packed, out t.Layer)) t.Packed = 0;
                t.Terrain = t.Layer == TileLayer.Terrain;
            }
            if (d.TryGetValue(KRing, out var r)) t.Ring = (int)r.AsInt64();
            if (d.ContainsKey(KState)) t.State = ParseState(Txt(d, KState));
            if (d.TryGetValue(KMin, out var mn)) t.Min = mn.AsVector2();
            if (d.TryGetValue(KMax, out var mx)) t.Max = mx.AsVector2();
            if (d.ContainsKey(KSource)) t.Source = Txt(d, KSource);
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
