#nullable enable
using Godot;

namespace SimWorld;

// Reconfigure (settings panel): a queued configure_world at the camera position, run at the start of the next frame.
public partial class WorldView
{
    /// <summary>Raised from <c>_Process</c> after every <see cref="ApplyProfileOverrides"/> request has run: (ok, library error text).</summary>
    public event System.Action<bool, string>? Reconfigured;
    /// <summary>Successful reconfigures since start (not counting the startup configure).</summary>
    public int ReconfigureCount { get; private set; }
    public bool LastReconfigureOk { get; private set; } = true;
    public string LastReconfigureError { get; private set; } = "";

    /// <summary>True from <see cref="ApplyProfileOverrides"/> until the next frame start runs it.</summary>
    public bool ReconfigurePending => _pendingOverrides != null;

    private Godot.Collections.Dictionary? _pendingOverrides;

    // ---- reconfigure (settings panel) ---------------------------------------

    /// <summary>
    /// Queues a world reconfigure with <paramref name="panelOverrides"/> (nested like the profile JSON, e.g. bands.max_distance_m) merged
    /// over the launch-arg profile (panel wins). It runs at the start of the next <c>_Process</c>, before update_camera, and ends with
    /// <see cref="Reconfigured"/>. A later call before that replaces the queued one. The world restarts at the camera's current
    /// position: that is the new origin, the camera goes back to local x/z = 0 (altitude and orientation kept), and every tile is
    /// released through the unload batch the library returns for the teleport.
    /// </summary>
    public void ApplyProfileOverrides(Godot.Collections.Dictionary panelOverrides, bool? terrain = null)
    {
        _pendingOverrides = panelOverrides;
        _pendingTerrain = terrain;
    }

    private bool? _pendingTerrain;

    private void RunReconfigure()
    {
        var panel = _pendingOverrides!;
        var terrain = _pendingTerrain;
        _pendingOverrides = null;
        _pendingTerrain = null;
        // The terrain opt-in flags change with the new world; a rejected profile keeps the previous world running with its old flags.
        bool oldEnable = EnableTerrain, oldProfile = TerrainEnabled;
        if (terrain.HasValue) EnableTerrain = TerrainEnabled = terrain.Value || SeedWorldActive; // a seed world cannot run without the terrain layer
        bool ok = TryReconfigure(panel, out var error);
        if (!ok) { EnableTerrain = oldEnable; TerrainEnabled = oldProfile; }
        LastReconfigureOk = ok;
        LastReconfigureError = error;
        if (ok) ReconfigureCount++;
        else GD.PushError($"[WorldView] reconfigure rejected, previous world keeps running: {error}");
        Reconfigured?.Invoke(ok, error);
    }

    private bool TryReconfigure(Godot.Collections.Dictionary panel, out string error)
    {
        error = "";
        if (!Streamer.HasConfigureWorld) { error = $"configure_world needs api 0.6 (library {Streamer.ApiVersionText})"; return false; }
        if (_source == null) { error = "no camera assigned"; return false; }
        long t0 = (long)Time.GetTicksUsec();

        // New origin = where the camera is now (f32 geodetic: 0.1-0.4 m). Without a running world there is nothing to convert from.
        var cam = _source.GlobalPosition;
        bool recenter = WorldRunning;
        double lat = OriginLat, lon = OriginLon;
        if (recenter)
        {
            var geo = Streamer.LocalToGeodetic(cam);
            if (geo == Vector3.Zero) { error = "local_to_geodetic failed (camera position not convertible)"; return false; }
            lat = geo.X;
            lon = geo.Y;
        }

        var merged = ComposeOverrides(panel);
        var options = WorldStreamerFacade.MakeWorldOptions(lat, lon, _tileUrl, _cacheDir);
        AddHostOptions(options);
        var profilePath = ResolveProfilePath();
        if (profilePath.Length > 0) options["profile_path"] = profilePath;
        if (merged.Count > 0) options["profile_overrides"] = merged;
        // Same cache_dir, no Shutdown() in between: the lock is kept across the teleport (contract 11a).
        var result = Streamer.ConfigureWorld(options);
        if (!result.Ok)
        {
            error = result.Error.Length > 0 ? result.Error : "configure_world failed";
            return false;
        }

        // Fresh start. Take the unload batch right now (before any new delivery can reuse a key), release stragglers too.
        int unloadedBefore = UnloadedTotal;
        var unload = Streamer.PollUnload();
        for (int i = 0; i < unload.Length; i++) UnloadTile(unload[i]);
        int strays = ReleaseHeldTiles();
        if (strays > 0)
            GD.PushWarning($"[WorldView] reconfigure: {strays} tiles were not in the library's unload batch of {unload.Length}; released anyway");
        _fading.Clear();
        _suspectRoots.Clear();
        _pendingRoots.Clear();
        _suspectRootsT.Clear();
        _pendingRootsT.Clear();

        if (recenter) _source.ApplyOriginShift(new Vector3(cam.X, 0f, cam.Z));
        LastShift = Vector3.Zero;
        TotalShift = Vector3.Zero;
        ProfileOverrides = merged;
        WorldRunning = true;
        ConfigureError = "";
        Attributions = result.Attributions;
        AfterConfigure();
        GD.Print($"[WorldView] reconfigured: origin=({lat:0.000000}, {lon:0.000000}) camera -> (0, {cam.Y:0.0}, 0) from ({cam.X:0.0}, {cam.Z:0.0}), "
            + $"{UnloadedTotal - unloadedBefore} tiles released, {((long)Time.GetTicksUsec() - t0) / 1000.0:0.0} ms; effective {EffectiveProfileText()}");
        return true;
    }

    /// <summary>Releases every tile the three tile maps still hold (stragglers the unload batch did not name); returns how many there were.</summary>
    private int ReleaseHeldTiles()
    {
        _dropScratch.Clear();
        foreach (var n in Cover.All) _dropScratch.Add(n.Packed);
        for (int i = 0; i < _dropScratch.Count; i++) ReleaseTile(_dropScratch[i]);
        int strays = _dropScratch.Count;
        _dropScratch.Clear();
        foreach (var n in TerrainCover.All) _dropScratch.Add(n.Packed);
        for (int i = 0; i < _dropScratch.Count; i++) ReleaseTerrainTile(_dropScratch[i]);
        strays += _dropScratch.Count;
        _dropScratch.Clear();
        foreach (var n in LandmarkCover.All) _dropScratch.Add(n.Packed);
        for (int i = 0; i < _dropScratch.Count; i++) ReleaseLandmarkTile(_dropScratch[i]);
        strays += _dropScratch.Count;
        return strays;
    }
}
