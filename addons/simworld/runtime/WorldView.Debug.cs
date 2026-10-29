using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>Debug aids: cover verification, terrain state log, frustum counts.</summary>
public partial class WorldView
{
    /// <summary>Debug aid: false hides the whole vector layer (buildings, ground, lights) so the terrain can be inspected on its own; the streaming is unchanged.</summary>
    public bool VectorVisible
    {
        get => _tileRoot?.Visible ?? true;
        set { if (_tileRoot != null) _tileRoot.Visible = value; }
    }

    /// <summary>Debug: log the terrain layer's state once a second (held, visible, holes, camera against the terrain).</summary>
    public bool TerrainDebugLog { get; set; }
    private double _terrainLogT;
    private readonly HashSet<long> _logPending = new();

    private void LogTerrainState(double delta)
    {
        _terrainLogT += delta;
        if (_terrainLogT < 1.0) return;
        _terrainLogT = 0;
        _logPending.Clear();
        if (Streamer.HasDebugTiles)
        {
            Streamer.DebugTiles(_verifyTiles);
            foreach (var t in _verifyTiles)
                if (t.Terrain && t.Packed != 0 && t.State is TileState.Queued or TileState.Fetching or TileState.Meshing) _logPending.Add(TileKey.Ancestor(t.Packed, TerrainCover.RootZ));
        }
        int holes = TerrainCover.CountUncoveredRoots(null, out float worst);
        int holesSettled = TerrainCover.CountUncoveredRoots(_logPending, out _);
        var c = TerrainCover;
        int fadeZero = 0;
        foreach (var n in c.All) if (n.Fading && n.Fade <= 0.01f && n.Vis == TileCover.Vis.Visible) fadeZero++;
        var cam = _source!.GlobalPosition;
        GD.Print($"[Terrain] held {c.Loaded} vis {string.Join("/", c.VisibleByRing)} hiddenByChildren {c.HiddenByChildren} waitingSiblings {c.WaitingSiblings} awaitingFade {c.AwaitingFadeTiles} fading {c.FadingTiles}(at0 {fadeZero}) "
            + $"| uncovered roots {holes} (worst {worst:0.00}, excluding pending {holesSettled}) | cam y {cam.Y:0} ground {CameraGroundHeight:0} agl {cam.Y - CameraGroundHeight:0} far {Camera?.Far ?? 0:0} | uploads {TerrainReadyTotal} unloads {TerrainUnloadedTotal}");
    }

    private readonly HashSet<ulong> _frustumIds = new();

    /// <summary>Diagnostic (<c>--hole-check</c>): false, with the reason, when a tile the cover shows has point sprites that this frame would not draw (see <see cref="SpriteCells.LightsDrawn"/>).</summary>
    public bool LightsDrawn(TileNodes tile, out string why)
    {
        why = "";
        return _pool == null || _pool.Cells.LightsDrawn(tile, out why);
    }

    /// <summary>The merged point sprite cells of the R2 / R3 tiles (counters since start).</summary>
    public SpriteCellStats SpriteCells
    {
        get
        {
            var c = _pool?.Cells;
            return c == null ? default : new SpriteCellStats(c.CellCount, c.MemberCount, c.MeshPoints, c.DeadPoints, c.Rebuilds, c.RebuiltPoints, c.RebuildUs, c.RebuildUsMax, c.TableWrites);
        }
    }

    /// <summary>
    /// Counts what the camera can see (draw call estimate): terrain tiles that are visible by the cover rule and whose mesh AABB (plus the cull margin)
    /// intersects the camera frustum, as the renderer culls them. <paramref name="visibleByRule"/> = tiles shown by the visibility rule,
    /// <paramref name="triangles"/> = their triangles. Debug aid, allocates a little (call a few times per second at most).
    /// </summary>
    public int CountTerrainInFrustum(Camera3D cam, out int visibleByRule, out long triangles) => CountInFrustum(cam, TerrainCover, out visibleByRule, out triangles);

    /// <summary>Same for the vector layer's building meshes (one draw call per tile; ground and sprites not counted).</summary>
    public int CountVectorInFrustum(Camera3D cam, out int visibleByRule, out long triangles) => CountInFrustum(cam, Cover, out visibleByRule, out triangles);

    /// <summary>
    /// The sprite draws the engine would make for the held vector tiles that are shown by the visibility rule and the day / night switch: the point meshes and the street pools
    /// whose instance is in the camera's frustum (draw calls, per ring in <paramref name="pointsByRing"/>). Counting aid, not for every frame.
    /// </summary>
    public int CountSpriteDrawsInFrustum(Camera3D cam, int[] pointsByRing, out int pools)
    {
        _frustumIds.Clear();
        foreach (var id in RenderingServer.InstancesCullConvex(cam.GetFrustum(), GetWorld3D().Scenario)) _frustumIds.Add((ulong)id);
        System.Array.Clear(pointsByRing);
        pools = 0;
        int points = 0;
        if (!SpritesVisible) return 0;
        foreach (var n in Cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible) continue;
            if (n.Points.Visible && _frustumIds.Contains(n.Points.GetInstanceId())) { points++; pointsByRing[System.Math.Clamp(n.Ring, 0, pointsByRing.Length - 1)]++; }
            if (n.Pools.Visible && _frustumIds.Contains(n.Pools.GetInstanceId())) pools++;
        }
        foreach (var c in _pool!.Cells.Cells)
            if (c.Shown && _frustumIds.Contains(c.Instance.GetInstanceId())) { points++; pointsByRing[System.Math.Clamp(c.Ring, 0, pointsByRing.Length - 1)]++; }
        return points;
    }

    private int CountInFrustum(Camera3D cam, TileCover cover, out int visibleByRule, out long triangles)
    {
        visibleByRule = 0;
        triangles = 0;
        _frustumIds.Clear();
        foreach (var id in RenderingServer.InstancesCullConvex(cam.GetFrustum(), GetWorld3D().Scenario)) _frustumIds.Add((ulong)id);
        int inFrustum = 0;
        foreach (var n in cover.All)
        {
            if (n.Vis != TileCover.Vis.Visible) continue;
            visibleByRule++;
            if (n.Tris > 0 && _frustumIds.Contains(n.MeshInstance.GetInstanceId()))
            {
                inFrustum++;
                triangles += n.Tris;
            }
        }
        return inFrustum;
    }

    /// <summary>One-line cover summary (also callable from GDScript probes).</summary>
    public string DescribeCover()
    {
        var c = Cover;
        return $"loaded={c.Loaded} visible R0..R3={c.VisibleByRing[0]}/{c.VisibleByRing[1]}/{c.VisibleByRing[2]}/{c.VisibleByRing[3]} "
            + $"hiddenByChildren={c.HiddenByChildren} waitingSiblings={c.WaitingSiblings} tris={c.TrianglesVisible}/{c.TrianglesLoaded} "
            + $"ground={c.GroundTilesVisible}/{c.GroundTilesLoaded} tiles {c.GroundTrianglesVisible}/{c.GroundTrianglesLoaded} tris "
            + $"lights={c.LightsVisible}/{c.LightsLoaded} clusteredTiles={c.ClusteredTilesVisible}/{c.LightTilesVisible} "
            + $"uploadMaxMs={UploadUsMaxLastSecond / 1000.0:0.00} replaced={ReplacedTotal} cover errors={ConsistencyErrors}"
            + (LandmarksActive ? $" landmarks loaded={LandmarkCover.Loaded} tris={LandmarkCover.TrianglesLoaded} lights={LandmarkCover.LightsLoaded}" : "")
            + (TerrainActive ? $" terrain loaded={TerrainCover.Loaded} visible={string.Join("/", TerrainCover.VisibleByRing)} tris={TerrainCover.TrianglesVisible}/{TerrainCover.TrianglesLoaded}" : "");
    }

    /// <summary>
    /// The merged point sprite cells against the cover: the cells' own bookkeeping (<see cref="SpriteCells.Verify"/>: fade table = what each tile shows, point accounting) and that
    /// every held tile of a merging ring that has points has them in its cell, all of them (a lit tile whose lights are missing would be a hole in the light carpet).
    /// </summary>
    private int VerifySpriteCells(System.Text.StringBuilder report)
    {
        int errors = 0;
        var problem = _pool!.Cells.Verify();
        if (problem != null) { errors++; report.Append("sprite cells: ").Append(problem).Append('\n'); }
        foreach (var n in Cover.All)
        {
            if (!_pool.Cells.Merges(n.Ring) || n.SpriteCount == 0) continue;
            if (n.CellMember == null || n.CellMember.Data.Count != n.SpriteCount)
            {
                errors++;
                report.Append("sprite cells: tile ").Append(n.Key).Append(" ring ").Append(n.Ring).Append(" has ").Append(n.SpriteCount).Append(" lights, its cell holds ")
                      .Append(n.CellMember?.Data.Count ?? 0).Append('\n');
            }
            else if (n.Points.Visible) { errors++; report.Append("sprite cells: tile ").Append(n.Key).Append(" draws its own points besides its cell's\n"); }
        }
        return errors;
    }

    /// <summary>
    /// Periodic consistency check of the quadtree cover (every 5 s): stored visibility vs the rule, double cover,
    /// holes in settled roots, counters. Roots that still have pending tiles in Rust are skipped, and a hole must
    /// persist over two consecutive checks before it is reported. Pushes an error on any finding.
    /// </summary>
    public void VerifyCover()
    {
        _pendingRoots.Clear();
        _pendingRootsT.Clear();
        if (Streamer.IsAvailable)
        {
            if (Streamer.HasDebugTiles)
            {
                Streamer.DebugTiles(_verifyTiles);
                for (int i = 0; i < _verifyTiles.Count; i++)
                {
                    var t = _verifyTiles[i];
                    if (t.Packed == 0 || t.Layer == TileLayer.Landmarks) continue; // landmark tiles have no quadtree roots (flat set)
                    if (t.State is TileState.Queued or TileState.Fetching or TileState.Meshing)
                    {
                        if (t.Terrain) _pendingRootsT.Add(TileKey.Ancestor(t.Packed, TerrainCover.RootZ));
                        else _pendingRoots.Add(TileKey.Ancestor(t.Packed, TileKey.RootZ));
                    }
                }
            }
        }
        _report.Clear();
        int errors = Cover.Verify(_pendingRoots, _suspectRoots, _report);
        if (TerrainActive) errors += TerrainCover.Verify(_pendingRootsT, _suspectRootsT, _report);
        errors += LandmarkCover.Verify(_report);
        errors += VerifySpriteCells(_report);
        ConsistencyChecks++;
        if (errors > 0)
        {
            ConsistencyErrors += errors;
            GD.PushError($"[WorldView] cover consistency: {errors} problem(s)\n{_report}");
        }
        else if (ConsistencyChecks == 1 || Cover.Loaded > 0 && ConsistencyChecks % 12 == 0)
        {
            GD.Print($"[WorldView] cover consistent: {Cover.Loaded} tiles loaded, visible R0..R3 = "
                + $"{Cover.VisibleByRing[0]}/{Cover.VisibleByRing[1]}/{Cover.VisibleByRing[2]}/{Cover.VisibleByRing[3]}, "
                + $"hidden by children {Cover.HiddenByChildren}, waiting siblings {Cover.WaitingSiblings}, sprite cells {_pool!.Cells.CellCount} ({_pool.Cells.MemberCount} tiles, {_pool.Cells.MeshPoints} points, {_pool.Cells.DeadPoints} dead)");
        }
    }
}
