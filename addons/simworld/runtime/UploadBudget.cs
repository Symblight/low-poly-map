namespace SimWorld;

/// <summary>
/// The per-frame upload budget of <see cref="WorldView"/>: the normal one (<see cref="WorldRenderSettings.UploadBudgetUs"/> and <c>MaxTilesPerFrame</c>) keeps a steady
/// flight smooth, but a cold start or a teleport leaves hundreds of tiles waiting and the library delivers them no faster than that (the R3 delivery latency was
/// 1.4 s at 650 queued tiles). While the budget was the limit in each of the last <see cref="WorldRenderSettings.UploadBacklogDeep"/> / 4 frames, i.e. the library
/// had a tile ready every time the loop stopped, and the camera is slow (below <see cref="WorldRenderSettings.UploadBurstMaxSpeedMps"/>), the burst budget
/// (<see cref="WorldRenderSettings.UploadBudgetBurstUs"/>, <see cref="WorldRenderSettings.UploadBurstMaxTiles"/>) applies. The library delivers coarse tiles first
/// (contract 4, delivery order), so the burst fills the world from the horizon in. No allocation.
/// </summary>
internal sealed class UploadBudget
{
    private readonly WorldRenderSettings _s;
    private int _limitedFrames;

    public UploadBudget(WorldRenderSettings settings) => _s = settings;

    /// <summary>The budget (microseconds) and the tile limit in force this frame; <see cref="Bursting"/> says which one.</summary>
    public long BudgetUs { get; private set; }
    public int MaxTiles { get; private set; }
    public bool Bursting { get; private set; }
    /// <summary>Frames that ran with the burst budget since start.</summary>
    public int BurstFrames { get; private set; }

    /// <summary>Chooses this frame's budget for a camera moving at <paramref name="speedMps"/>.</summary>
    public void BeginFrame(float speedMps)
    {
        int deepFrames = System.Math.Max(_s.UploadBacklogDeep / 4, 1);
        Bursting = _s.UploadBudgetBurstUs > _s.UploadBudgetUs && _limitedFrames >= deepFrames && speedMps < _s.UploadBurstMaxSpeedMps;
        BudgetUs = Bursting ? _s.UploadBudgetBurstUs : _s.UploadBudgetUs;
        MaxTiles = Bursting ? System.Math.Max(_s.UploadBurstMaxTiles, _s.MaxTilesPerFrame) : _s.MaxTilesPerFrame;
        if (Bursting) BurstFrames++;
    }

    /// <summary>Closes the frame: <paramref name="limited"/> = the upload loop stopped because of the budget or the tile limit with the library still delivering, not because nothing was ready.</summary>
    public void EndFrame(bool limited) => _limitedFrames = limited ? _limitedFrames + 1 : 0;
}
