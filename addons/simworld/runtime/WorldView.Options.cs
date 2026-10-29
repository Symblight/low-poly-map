#nullable enable
using Godot;

namespace SimWorld;

// The configure_world options the view decides: seed world, layers, terrain centre, engine surfaces.
public partial class WorldView
{
    /// <summary>True when the last configure asked for engine surfaces (<see cref="WorldRenderSettings.EngineSurfaces"/> and a library with api 1.11): the meshes arrive as ready vertex bytes (contract 4a).</summary>
    public bool EngineSurfacesActive { get; private set; }

    /// <summary>Every option the view adds to a <c>configure_world</c> call besides origin, tile source and profile (first configure and reconfigure alike).</summary>
    private void AddHostOptions(Godot.Collections.Dictionary options)
    {
        AddTerrainLayerOption(options);
        AddSeedWorldOption(options);
        AddEngineSurfacesOption(options);
    }

    /// <summary>Adds the <c>seed_world</c> option (api 1.7) for a seed world host. OS paths are made absolute (relative to the cwd); <c>res://</c> and <c>user://</c> go as they are.</summary>
    private void AddSeedWorldOption(Godot.Collections.Dictionary options)
    {
        if (!SeedWorldActive) { WorldStreamerFacade.AddSeedWorld(options, ""); return; }
        var path = SeedWorld.Trim();
        if (!path.Contains("://")) path = System.IO.Path.GetFullPath(path);
        WorldStreamerFacade.AddSeedWorld(options, path);
    }

    /// <summary>Adds the <c>layers</c> opt-in: <c>terrain</c> when the host wants it and the library has it (api 1.5; warns once when it has not) and <c>landmarks</c> when <see cref="LandmarksEnabled"/> and the library has the layer (api 1.11).</summary>
    private void AddTerrainLayerOption(Godot.Collections.Dictionary options)
    {
        bool want = EnableTerrain;
        if (want && !Streamer.HasTerrainLayer)
        {
            if (!_warnedNoTerrainLayer) GD.PushWarning($"[WorldView] EnableTerrain ignored: library api {Streamer.ApiVersionText} has no terrain layer (needs 1.5); the world stays flat");
            _warnedNoTerrainLayer = true;
            want = false;
        }
        bool landmarks = LandmarksEnabled && Streamer.HasLandmarkLayer;
        if (LandmarksEnabled && !landmarks && !_loggedNoLandmarks)
        {
            _loggedNoLandmarks = true;
            GD.Print($"[WorldView] landmark layer off: library api {Streamer.ApiVersionText} has none (needs 1.{WorldStreamerFacade.LandmarksMinApiMinor}); landmarks stay inside the vector tiles");
        }
        LandmarksActive = landmarks;
        WorldStreamerFacade.AddLayers(options, want, landmarks);
        // Keep the flat zone (and so the relief) where the first configure put it: a reconfigure may move the origin (api 1.6).
        WorldStreamerFacade.AddTerrainCenter(options, want && Streamer.HasTerrainActiveFlag, OriginLat, OriginLon);
    }

    /// <summary>Adds the <c>engine_surfaces</c> option (api 1.11, contract 4a) when <see cref="WorldRenderSettings.EngineSurfaces"/> is on and the library has it; sets <see cref="EngineSurfacesActive"/>.</summary>
    private void AddEngineSurfacesOption(Godot.Collections.Dictionary options)
    {
        EngineSurfacesActive = _settings.EngineSurfaces && Streamer.HasEngineSurfaces;
        WorldStreamerFacade.AddEngineSurfaces(options, EngineSurfacesActive);
        GD.Print($"[WorldView] engine surfaces {(EngineSurfacesActive ? "on" : _settings.EngineSurfaces ? $"off: library api {Streamer.ApiVersionText} has none (needs 1.{WorldStreamerFacade.EngineSurfacesMinApiMinor})" : "off (WorldRenderSettings.EngineSurfaces)")}");
    }
}
