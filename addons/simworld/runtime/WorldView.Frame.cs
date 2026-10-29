#nullable enable
using Godot;

namespace SimWorld;

/// <summary>The per-frame driver: the call order of docs/contract.md section 3 and the upload budget.</summary>
public partial class WorldView
{
    private ulong _lastShiftFrame = ulong.MaxValue - 1;
    private int _shiftRun;
    private bool _shiftRunReported;

    public override void _Process(double delta)
    {
        using var probe = AllocProbe.Measure(AllocSite.ViewFrame);
        if (_settings.GcQuietBudgetMb > 0) (GcPacing ??= GcPacer.Acquire(_settings.GcQuietBudgetMb)).Tick();
        if (_prewarmPending) StartPrewarmIfReady();
        if (_prewarm != null && _prewarm.Tick()) _prewarm = null;
        if (_source != null && Streamer.IsAvailable)
        {
            (_source as IWorldCameraTick)?.Tick(delta);
            // A queued reconfigure runs first, so this frame's update_camera already carries the new origin and camera position.
            if (_pendingOverrides != null) RunReconfigure();
            // Height above ground = camera y - terrain height under it (0 unless the terrain layer is active).
            var pos = _source.GlobalPosition;
            using (AllocProbe.Measure(AllocSite.GroundHeight)) CameraGroundHeight = Streamer.GroundHeightAt(pos.X, pos.Z);
            _source.SetGroundHeight(CameraGroundHeight);
            using (AllocProbe.Measure(AllocSite.UpdateView)) UpdateView(); // before the camera update (also on the external path), so the cover of this frame already uses the new view
            if (ExternalCameraUpdate != null) ExternalCameraUpdate(delta);
            else using (AllocProbe.Measure(AllocSite.UpdateCamera)) Streamer.UpdateCamera(_source.GlobalPosition, _source.Velocity, _source.AltitudeAgl);

            // Shift first so tiles polled below (already in the new frame) are not shifted twice.
            Vector3 shift;
            using (AllocProbe.Measure(AllocSite.TakeOriginShift)) shift = Streamer.TakeOriginShift();
            LastShift = shift;
            if (shift != Vector3.Zero)
            {
                OriginShiftCount++;
                TotalShift += shift;
                // A host that does not move its vehicle and cameras by the shift (contract section 2, origin_shifted) is farther than the re-centre distance again next frame: the origin
                // then moves every frame and the whole world jumps back and forth (SimPlane, 2026-10-29: 5 km per frame, "everything is blinking"). Said once, as an error, after three frames in a row.
                ulong frame = Engine.GetProcessFrames();
                _shiftRun = frame == _lastShiftFrame + 1 ? _shiftRun + 1 : 1;
                _lastShiftFrame = frame;
                if (_shiftRun == 3 && !_shiftRunReported)
                {
                    _shiftRunReported = true;
                    GD.PushError("[WorldView] origin shifted on 3 consecutive frames: the host does not apply the shift to its camera or vehicle (origin_shifted / IWorldCameraSource.ApplyOriginShift), so the camera is beyond origin.recenter_distance_m again every frame and the world jumps each frame. Apply the shift, or raise origin.recenter_distance_m until the host can.");
                }
                _source.ApplyOriginShift(shift);
                Cover.Shift(shift);
                _pool!.ShiftSprites(shift);
                TerrainCover.Shift(shift);
                LandmarkCover.Shift(shift);
                _collision?.Shift(shift);
                if (SeedWorldActive) RefreshSeed(false); // airport positions follow the floating origin
                OriginShifted?.Invoke(shift);
            }
            using (AllocProbe.Measure(AllocSite.ShaderGlobals))
            {
                RenderingServer.GlobalShaderParameterSet(ShaderGlobals.CurveCenterName, _source.GlobalPosition);
                RenderingServer.GlobalShaderParameterSet(ShaderGlobals.CurveInv2RName, (float)(Curvature / (2.0 * EarthRadiusM)));
            }

            long t0 = (long)Time.GetTicksUsec();
            int n0 = ReadyTotal + LandmarkReadyTotal;
            UploadReady(t0);
            PollFades();
            string[] unload;
            using (AllocProbe.Measure(AllocSite.PollUnload)) unload = Streamer.PollUnload();
            long tu = (long)Time.GetTicksUsec();
            using (AllocProbe.Measure(AllocSite.UnloadApply)) for (int i = 0; i < unload.Length; i++) UnloadTile(unload[i]);
            _unloadApplyUs += (long)Time.GetTicksUsec() - tu;
            _pool!.SyncSprites(_source.GlobalPosition, InvTwoR); // after every show / hide / fade / unload of the frame, before it is drawn
            UploadUsLastFrame = (long)Time.GetTicksUsec() - t0;
            if (n0 != ReadyTotal + LandmarkReadyTotal) { UploadFrames++; if (UploadUsLastFrame > _budget!.BudgetUs) UploadFramesOverBudget++; }
            if (UploadUsLastFrame > _secUploadMax) _secUploadMax = UploadUsLastFrame;
            using (AllocProbe.Measure(AllocSite.PoolMaintenance))
            {
                DrainRetired();
                UpdateCollision();
                _pool!.Trim(delta);
                _terrainPool!.Trim(delta);
                _markings?.Update(_source.GlobalPosition, delta);
            }
            _lodReadT += delta;
            if (_lodReadT >= LodReadIntervalS)
            {
                _lodReadT = 0;
                using (AllocProbe.Measure(AllocSite.LodRead)) ReadLod();
                using (AllocProbe.Measure(AllocSite.CullRefresh)) RefreshCullMargins();
            }
        }

        using (AllocProbe.Measure(AllocSite.SpriteScale)) UpdateSpriteScale();
        if (TerrainDebugLog && TerrainActive) LogTerrainState(delta);

        _sinceCheck += delta;
        if (_settings.DebugChecks && _settings.VerifyIntervalS > 0 && _sinceCheck >= _settings.VerifyIntervalS)
        {
            _sinceCheck = 0;
            VerifyCover();
        }

        _secTimer += delta;
        if (_secTimer >= 1.0)
        {
            _secTimer -= 1.0;
            ReadyLastSecond = _secReady;
            UnloadedLastSecond = _secUnloaded;
            UploadUsMaxLastSecond = _secUploadMax;
            _secReady = 0;
            _secUnloaded = 0;
            _secUploadMax = 0;
        }
    }

    /// <summary>The process's collector pacer while <see cref="WorldRenderSettings.GcQuietBudgetMb"/> is above 0 (counters for diagnostics; released in <c>_ExitTree</c>); null otherwise.</summary>
    public GcPacer? GcPacing { get; private set; }
    private readonly System.Collections.Generic.List<ReadyTile> _readyBuf = new(4);
    private double _tileCostEmaUs = 500;
    private UploadBudget? _budget;

    /// <summary>Frames that ran with the burst upload budget (a deep backlog and a slow camera, see <see cref="UploadBudget"/>).</summary>
    public int UploadBurstFrames => _budget?.BurstFrames ?? 0;

    /// <summary>
    /// Polls ready tiles one at a time and uploads them until the budget is spent (<see cref="WorldRenderSettings.UploadBudgetUs"/>, at most
    /// <see cref="WorldRenderSettings.MaxTilesPerFrame"/> tiles; a deep backlog with a slow camera gets the burst budget, see <see cref="UploadBudget"/>). The next tile is not even polled when the typical cost of a tile (a moving average of the
    /// recent ones) would carry the frame over the budget, so only the first tile of a frame can exceed it; Rust keeps the rest queued.
    /// </summary>
    private void UploadReady(long frameStartUs)
    {
        var budget = _budget ??= new UploadBudget(_settings);
        budget.BeginFrame(_source!.Velocity.Length());
        bool limited = true;
        for (int n = 0; n < budget.MaxTiles; n++)
        {
            int count = Streamer.PollReady(1, _readyBuf);
            if (count == 0) { limited = false; break; }
            long c0 = (long)Time.GetTicksUsec();
            for (int i = 0; i < count; i++)
            {
                var tile = _readyBuf[i];
                if (!tile.Usable)
                {
                    UploadProblems.Error("entry#" + tile.Problem, $"[WorldView] poll_ready entry without a usable key {tile.Problem}; the library holds a tile the host cannot place");
                    continue;
                }
                if (tile.IsTerrain) UploadTerrainTile(in tile);
                else if (tile.IsLandmark) UploadLandmarkTile(in tile);
                else UploadTile(in tile);
                tile.ReleaseHandles();
            }
            _readyBuf.Clear();
            long now = (long)Time.GetTicksUsec();
            _tileCostEmaUs += ((now - c0) - _tileCostEmaUs) * 0.2;
            if (now - frameStartUs >= budget.BudgetUs || now - frameStartUs + _tileCostEmaUs > budget.BudgetUs) break;
        }
        budget.EndFrame(limited);
    }

    /// <summary>Frees the meshes and sprite buffers of released tiles within <see cref="WorldRenderSettings.ReleaseBudgetUs"/> (each pool makes progress at least one set per frame).</summary>
    private void DrainRetired()
    {
        if (_pool!.RetiringCount == 0 && _terrainPool!.RetiringCount == 0) return;
        long t0 = (long)Time.GetTicksUsec();
        long budget = _settings.ReleaseBudgetUs;
        _pool!.DrainRetired(budget);
        _terrainPool!.DrainRetired(System.Math.Max(budget - ((long)Time.GetTicksUsec() - t0), 0));
        _releaseDrainUs += (long)Time.GetTicksUsec() - t0;
    }

    /// <summary>After the unloads: creates / frees the collision patch with the flag, re-centres it on the camera.</summary>
    private void UpdateCollision()
    {
        if (!TerrainCollisionEnabled)
        {
            if (_collision != null) { _collision.Free(); _collision = null; }
            return;
        }
        _collision ??= new TerrainCollision(this, (x, z) => Streamer.GroundHeightAt(x, z));
        _collision.Samples = TerrainCollisionSamples;
        _collision.SpacingM = TerrainCollisionSpacingM;
        _collision.Update(_source!.GlobalPosition);
    }
}
