#nullable enable
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>poll_unload: tiles leave the cover at once and their node sets go back to the pool.</summary>
public partial class WorldView
{
    private bool ReleaseLandmarkTile(long packed)
    {
        var nodes = LandmarkCover.Remove(packed);
        if (nodes == null) return false;
        _pool!.Release(nodes);
        LandmarkUnloadedTotal++;
        return true;
    }

    private void UnloadTile(string key)
    {
        if (!TileKey.TryParse(key, out long packed, out TileLayer layer))
        {
            UploadProblems.Error("unload#" + key, $"[WorldView] poll_unload key '{key}' cannot be parsed; the tile it names is never released");
            return;
        }
        bool held;
        if (layer == TileLayer.Landmarks) { LandmarkKeysSeen++; held = ReleaseLandmarkTile(packed); }
        else if (layer == TileLayer.Terrain) { TerrainKeysSeen++; ChurnUnload("terrain", key, TerrainCover.TryGet(packed, out var tn) ? tn : null, "poll_unload"); held = ReleaseTerrainTile(packed); }
        else { ChurnUnload("world", key, Cover.TryGet(packed, out var vn) ? vn : null, "poll_unload"); VectorTileUnloading?.Invoke(key, Cover.TryGet(packed, out var un) ? un : null); held = ReleaseTile(packed); }
        // The library only unloads what it delivered; a key the host does not hold is a delivery that never became a tile (see UploadProblems).
        if (!held) UploadProblems.Warn("unload#" + key, $"[WorldView] poll_unload names {key}, which the host does not hold (its delivery was dropped or never uploaded)");
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
}
