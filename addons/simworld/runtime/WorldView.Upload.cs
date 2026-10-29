#nullable enable
using Godot;

namespace SimWorld;

/// <summary>Ready tiles into pooled node sets, one method per layer (vector, terrain, landmarks).</summary>
public partial class WorldView
{
    /// <summary>Delivered entries the host could not draw (no usable key, a malformed or rejected mesh, an unparsable tile key): logged once per key, counted here (overlay), together with the pool's own (a short light colour array). The library holds such a tile, so each one is a hole.</summary>
    public OnceLog UploadProblems => _pool?.Problems ?? NoProblems;
    private static readonly OnceLog NoProblems = new();

    /// <summary>Logs the problem the parser recorded for the entry, if any (once per key).</summary>
    private void ReportEntryProblem(in ReadyTile tile)
    {
        if (tile.Problem != null) UploadProblems.Error(tile.Key + "#parse", $"[WorldView] ready tile {tile.Key}: {tile.Problem}; that mesh is not drawn");
    }

    /// <summary>Marks a mesh Godot refused (its own error names no tile) so the log says which one.</summary>
    private void ReportRejectedMesh(in ReadyTile tile, string what, Godot.Collections.Array? arrays, Godot.Collections.Dictionary? surface) =>
        UploadProblems.Error(tile.Key + "#" + what, $"[WorldView] tile {tile.Key}: Godot rejected the {what} mesh ({MeshSurfaces.Describe(arrays, surface)}); the tile is drawn without it");

    private void UploadTile(in ReadyTile tile)
    {
        using var probe = AllocProbe.Measure(AllocSite.UploadVector);
        long t0 = (long)Time.GetTicksUsec();
        if (!TileKey.TryParse(tile.Key, out long packed))
        {
            UploadProblems.Error(tile.Key + "#key", $"[WorldView] ready tile with unparsable key '{tile.Key}'; not drawn");
            return;
        }
        ReportEntryProblem(in tile);
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
        if (!pool.SetMesh(nodes, tile.Mesh, tile.MeshSurface)) ReportRejectedMesh(in tile, "building", tile.Mesh, tile.MeshSurface);
        long tm1 = (long)Time.GetTicksUsec();
        // Ground (every band; R3 since api 1.3): same root, so it is shown / hidden with the buildings. A delivery without
        // a "ground" key (older Rust build) leaves an existing ground mesh alone; an empty Array clears it.
        if (tile.HasGroundKey)
        {
            if (!pool.SetGround(nodes, tile.Ground, tile.GroundSurface)) ReportRejectedMesh(in tile, "ground", tile.Ground, tile.GroundSurface);
            long gus = (long)Time.GetTicksUsec() - tm1;
            GroundUploadUsLast = gus;
            if (gus > GroundUploadUsMax) GroundUploadUsMax = gus;
            GroundUploadUsSum += gus;
            GroundUploadCount++;
        }
        long tm1b = (long)Time.GetTicksUsec();
        // Building lights exist on the bands lights.min_ring..R3 (R1..R3 by default); street lamps on every band. All four kinds go into one MultiMesh.
        using (AllocProbe.Measure(AllocSite.SetSprites)) pool.SetSprites(nodes, in tile);

        long tm2 = (long)Time.GetTicksUsec();
        int fr = System.Math.Clamp(tile.Ring, 0, 3);
        long fillUs = tm2 - tm1b;
        SpriteFillCount[fr]++;
        SpriteFillUsSum[fr] += fillUs;
        if (fillUs > SpriteFillUsMax[fr]) SpriteFillUsMax[fr] = fillUs;
        InitCullMargin(nodes);
        if (replace)
        {
            Cover.EndReplace(nodes);
            ReplacedTotal++;
        }
        else
        {
            Cover.Add(nodes); // sibling-swap re-evaluation of the affected keys
        }

        VectorTileUploaded?.Invoke(nodes, replace);
        _secReady++;
        ReadyTotal++;
        if (Cover.Loaded + LandmarkCover.Loaded > PeakActiveTiles) PeakActiveTiles = Cover.Loaded + LandmarkCover.Loaded;

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
            // One-time stats block so the headless smoke test shows the debug_stats contract is live (a debug check: it costs 7 to 21 ms in the first frame).
            if (_settings.DebugChecks) GD.Print($"[WorldView] debug_stats {Godot.Json.Stringify(Streamer.DebugStats())}");
        }
        if (us > 2000 && _slowLogs < 40)
        {
            _slowLogs++;
            GD.Print($"[WorldView] slow upload {tile.Key} ring={tile.Ring} tris={nodes.Tris} groundTris={nodes.GroundTris} lights={tile.Lights.Length} {us / 1000.0:0.00} ms (mesh {(tm1 - tm0) / 1000.0:0.00}, ground {(tm1b - tm1) / 1000.0:0.00}, lights {(tm2 - tm1b) / 1000.0:0.00}) {MeshSurfaces.Describe(tile.Mesh, tile.MeshSurface)}");
        }
    }

    private void UploadTerrainTile(in ReadyTile tile)
    {
        using var probe = AllocProbe.Measure(AllocSite.UploadTerrain);
        long t0 = (long)Time.GetTicksUsec();
        TerrainKeysSeen++;
        if (!TileKey.TryParse(tile.Key, out long packed, out TileLayer tileLayer) || tileLayer != TileLayer.Terrain)
        {
            UploadProblems.Error(tile.Key + "#key", $"[WorldView] terrain ready tile with unparsable key '{tile.Key}'; not drawn");
            return;
        }
        ReportEntryProblem(in tile);
        var pool = _terrainPool!;
        TerrainNodes nodes;
        bool replace = TerrainCover.TryGet(packed, out var existing);
        if (replace)
        {
            // Same key delivered again (not expected since api 1.10; before it set_terrain_detail did this): replace the mesh in place, keep the node and its visibility.
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
        if (!pool.SetMesh(nodes, tile.Terrain, tile.TerrainSurface, tile.HMin, tile.HMax)) ReportRejectedMesh(in tile, "terrain", tile.Terrain, tile.TerrainSurface);
        InitCullMargin(nodes);
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
        if (nodes.Tris == 0) UploadProblems.Error(tile.Key + "#empty", $"[WorldView] terrain tile {tile.Key} produced no surface (unusable arrays)");
        if (us > 2000 && _slowLogs < 40)
        {
            _slowLogs++;
            GD.Print($"[WorldView] slow terrain upload {tile.Key} ring={tile.Ring} tris={nodes.Tris} {us / 1000.0:0.00} ms");
        }
    }

    private void UploadLandmarkTile(in ReadyTile tile)
    {
        using var probe = AllocProbe.Measure(AllocSite.UploadLandmark);
        long t0 = (long)Time.GetTicksUsec();
        LandmarkKeysSeen++;
        if (!TileKey.TryParse(tile.Key, out long packed, out TileLayer layer) || layer != TileLayer.Landmarks)
        {
            UploadProblems.Error(tile.Key + "#key", $"[WorldView] landmark ready tile with unparsable key '{tile.Key}'; not drawn");
            return;
        }
        ReportEntryProblem(in tile);
        var pool = _pool!;
        // A held key delivered again (the contract says it never is) replaces in place and never adds a second copy.
        bool replace = LandmarkCover.TryGet(packed, out var nodes);
        if (replace)
        {
            LandmarkCover.BeginReplace(nodes);
            nodes.Ring = tile.Ring;
        }
        else
        {
            nodes = pool.Acquire(tile.Key, packed, tile.Ring);
            nodes.LoadedMs = Time.GetTicksMsec();
            ChurnDeliver("landmarks", tile.Key);
        }
        nodes.Root.Position = tile.Origin;
        if (!pool.SetMesh(nodes, tile.Mesh, tile.MeshSurface)) ReportRejectedMesh(in tile, "landmark", tile.Mesh, tile.MeshSurface);
        // Lights like R1 (the shared sprite material cross-fades them with the building windows) and the beacons; a landmark tile has no ground
        // and no street or airport lights (empty in the entry), so its sprite MultiMesh holds lights and beacons only.
        using (AllocProbe.Measure(AllocSite.SetSprites)) pool.SetSprites(nodes, in tile);
        InitCullMargin(nodes);
        if (replace)
        {
            LandmarkCover.EndReplace(nodes);
            LandmarkReplacedTotal++;
        }
        else LandmarkCover.Add(nodes);
        LandmarkReadyTotal++;
        if (Cover.Loaded + LandmarkCover.Loaded > PeakActiveTiles) PeakActiveTiles = Cover.Loaded + LandmarkCover.Loaded; // landmark tiles take nodes from the same pool

        long us = (long)Time.GetTicksUsec() - t0;
        LandmarkUploadUsLast = us;
        if (us > LandmarkUploadUsMax) LandmarkUploadUsMax = us;
        LandmarkUploadUsSum += us;
        LandmarkUploadCount++;
        if (!_loggedFirstLandmark)
        {
            _loggedFirstLandmark = true;
            GD.Print($"[WorldView] first landmark tile {tile.Key} origin={tile.Origin} tris={nodes.Tris} lights={nodes.LightCount} beacons={nodes.BeaconCount}");
        }
        if (us > 2000 && _slowLogs < 40)
        {
            _slowLogs++;
            GD.Print($"[WorldView] slow landmark upload {tile.Key} tris={nodes.Tris} lights={nodes.LightCount} beacons={nodes.BeaconCount} {us / 1000.0:0.00} ms");
        }
    }

    private bool _loggedFirstLandmark;
}
