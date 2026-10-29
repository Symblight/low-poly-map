#nullable enable
namespace SimWorld;

/// <summary>
/// Per-frame timing surface for host-side diagnostics (the addon itself reports nothing): the time spent in the library calls
/// (<see cref="Timings"/>) and the uploads of the last closed frame per layer. Allocation-free; a host reads it once per frame
/// through <see cref="EndTimingFrame"/>.
/// </summary>
public partial class WorldView
{
    /// <summary>What the host spent uploading ready tiles in one frame, from the cumulative per-layer counters.</summary>
    public struct UploadFrame
    {
        public double VectorMs, TerrainMs, LandmarkMs;
        public int Vector, Terrain, Landmarks;
        /// <summary>Tiles returned to the pool by poll_unload (all layers).</summary>
        public int Unloaded;
        /// <summary>Host work after the library calls, ms: applying the poll_unload batch (cover removal, hide, retire), applying poll_fades to the tiles, and the deferred node-set release (<see cref="TilePool.DrainRetired"/>).</summary>
        public double UnloadApplyMs, FadeApplyMs, ReleaseDrainMs;
        /// <summary>Origin re-centers applied.</summary>
        public int OriginShifts;
    }

    /// <summary>Host time to fill the sprites of a vector tile (<see cref="TilePool.SetSprites"/>: pack the points, build the points mesh, fill the pool MultiMesh), per ring R0..R3: tiles, total and worst microseconds.</summary>
    public readonly int[] SpriteFillCount = new int[4];
    public readonly long[] SpriteFillUsSum = new long[4], SpriteFillUsMax = new long[4];

    /// <summary>Time inside the library calls.</summary>
    public StreamerTimings Timings => Streamer.Timings;

    /// <summary>The last closed frame's uploads (see <see cref="EndTimingFrame"/>).</summary>
    public UploadFrame LastUploads => _lastUploads;

    private UploadFrame _lastUploads;
    private long _tmVectorUs, _tmTerrainUs, _tmLandmarkUs;
    private int _tmVector, _tmTerrain, _tmLandmarks, _tmUnloaded, _tmShifts;
    private long _tmUnloadApplyUs, _tmFadeApplyUs, _tmReleaseDrainUs;
    // Cumulative microseconds of the host-side phases (added by _Process).
    private long _unloadApplyUs, _fadeApplyUs, _releaseDrainUs;

    /// <summary>
    /// Closes a timing frame: <see cref="Timings"/> rolls over and <see cref="LastUploads"/> becomes the difference of the cumulative
    /// upload counters since the previous call. Call it once per frame, before the view's <c>_Process</c> (it then describes the frame before).
    /// </summary>
    public void EndTimingFrame()
    {
        Timings.EndFrame();
        long vectorUs = SumUs(TileUploadUsSum), terrainUs = SumUs(TerrainUploadUsSum), landmarkUs = LandmarkUploadUsSum;
        int unloaded = UnloadedTotal + TerrainUnloadedTotal + LandmarkUnloadedTotal;
        _lastUploads = new UploadFrame
        {
            VectorMs = (vectorUs - _tmVectorUs) / 1000.0,
            TerrainMs = (terrainUs - _tmTerrainUs) / 1000.0,
            LandmarkMs = (landmarkUs - _tmLandmarkUs) / 1000.0,
            Vector = ReadyTotal - _tmVector,
            Terrain = TerrainReadyTotal - _tmTerrain,
            Landmarks = LandmarkReadyTotal - _tmLandmarks,
            Unloaded = unloaded - _tmUnloaded,
            UnloadApplyMs = (_unloadApplyUs - _tmUnloadApplyUs) / 1000.0,
            FadeApplyMs = (_fadeApplyUs - _tmFadeApplyUs) / 1000.0,
            ReleaseDrainMs = (_releaseDrainUs - _tmReleaseDrainUs) / 1000.0,
            OriginShifts = OriginShiftCount - _tmShifts,
        };
        _tmVectorUs = vectorUs;
        _tmTerrainUs = terrainUs;
        _tmLandmarkUs = landmarkUs;
        _tmVector = ReadyTotal;
        _tmTerrain = TerrainReadyTotal;
        _tmLandmarks = LandmarkReadyTotal;
        _tmUnloaded = unloaded;
        _tmUnloadApplyUs = _unloadApplyUs;
        _tmFadeApplyUs = _fadeApplyUs;
        _tmReleaseDrainUs = _releaseDrainUs;
        _tmShifts = OriginShiftCount;
    }

    private static long SumUs(long[] perRing)
    {
        long sum = 0;
        for (int i = 0; i < perRing.Length; i++) sum += perRing[i];
        return sum;
    }
}
