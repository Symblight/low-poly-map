using Godot;

namespace SimWorld;

// Counters and sizes for the overlay and the self-tests.
public partial class WorldView
{
    /// <summary>Tiles held in all layers.</summary>
    public int TotalLoaded => Cover.Loaded + TerrainCover.Loaded + LandmarkCover.Loaded;

    // Overlay counters.
    public long UploadUsLastFrame { get; private set; }
    public long UploadUsMaxLastSecond { get; private set; }
    public int ReadyLastSecond { get; private set; }
    public int UnloadedLastSecond { get; private set; }
    public int ReadyTotal { get; private set; }
    public int UnloadedTotal { get; private set; }
    /// <summary>Re-deliveries under an existing key (light clustering, coarsening) replaced in place.</summary>
    public int ReplacedTotal { get; private set; }
    public int ActiveTiles => Cover.Loaded;
    public int PoolFree => _pool?.FreeCount ?? 0;
    public int PoolCreated => _pool?.TotalCreated ?? 0;

    /// <summary>Idle node sets the pools destroyed (<see cref="PoolTrimmer"/>); <see cref="PoolCreated"/> counts the sets that exist now.</summary>
    public int PoolTrimmed => _pool?.Trimmed ?? 0;

    /// <summary>Frames that polled at least one tile / frames whose poll+upload exceeded the 2 ms budget (one big tile can).</summary>
    public int UploadFrames { get; private set; }
    public int UploadFramesOverBudget { get; private set; }
    public int ConsistencyErrors { get; private set; }
    public int ConsistencyChecks { get; private set; }
    /// <summary>Per tile upload cost in microseconds, indexed by ring 0..3.</summary>
    public readonly long[] TileUploadUsLast = new long[4];
    public readonly long[] TileUploadUsMax = new long[4];
    public readonly long[] TileUploadUsSum = new long[4];
    public readonly int[] TileUploadCount = new int[4];
    /// <summary>Part of the tile upload cost spent on the ground mesh (microseconds), all rings together.</summary>
    public long GroundUploadUsLast { get; private set; }
    public long GroundUploadUsMax { get; private set; }
    public long GroundUploadUsSum { get; private set; }
    public int GroundUploadCount { get; private set; }
    /// <summary>Terrain height under the camera in meters (Rust ground_height_at; 0 unless the terrain layer is active).</summary>
    public double CameraGroundHeight { get; private set; }

    /// <summary>Highest number of pooled tile node sets in use at once (vector tiles plus landmark tiles, since both come from the same pool; checked against the pool size by the settings self-test).</summary>
    public int PeakActiveTiles { get; private set; }
    /// <summary>Node sets (tile roots) under the pool container: created sets, in use or free (the pool's own helper nodes, the merged sprite cells' root, are not counted).</summary>
    public int TileRootCount => (_tileRoot?.GetChildCount() ?? 0) - (_pool?.HelperNodes ?? 0);
    /// <summary>Every node under the vector and terrain pool containers (roots and their children). Diagnostic walk (self-tests): scene node count minus this = nodes that do not belong to the tile pools.</summary>
    public int PooledNodeCount()
    {
        int n = 0;
        if (_tileRoot != null) n += CountDescendants(_tileRoot);
        if (_terrainRoot != null) n += CountDescendants(_terrainRoot);
        return n;
    }

    private static int CountDescendants(Node node)
    {
        int n = 0;
        foreach (var c in node.GetChildren()) n += 1 + CountDescendants(c);
        return n;
    }
}
