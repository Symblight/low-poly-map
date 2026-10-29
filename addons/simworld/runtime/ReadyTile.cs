#nullable enable
using Godot;

namespace SimWorld;

/// <summary>
/// Typed view of one poll_ready() dictionary. <see cref="Mesh"/> (buildings) and <see cref="Ground"/> are Mesh.ARRAY_MAX
/// arrays (null when missing/empty, e.g. empty tiles and all of R3).
/// </summary>
public struct ReadyTile
{
    /// <summary>Why a mesh array of the entry was dropped (not an Array, or its vertex slot is not a PackedVector3Array); null when the entry is well formed. The view logs it once per key.</summary>
    public string? Problem;
    public string Key;
    /// <summary>False for an entry without a usable key: only <see cref="Problem"/> (the keys and types it carried) is set.</summary>
    public readonly bool Usable => Key != null;
    /// <summary>True for a terrain-layer entry (api 1.5: layer "terrain", key "t/z/x/y"); false for the vector layer.</summary>
    public bool IsTerrain;
    /// <summary>True for a landmark-layer entry (api 1.11: layer "landmarks", key "l/z/x/y", ring 0): it carries the vector keys (mesh with windows, lights, beacons; no ground), drawn like a vector tile but kept in the landmark tile map.</summary>
    public bool IsLandmark;
    /// <summary>Terrain mesh Array (VERTEX, INDEX, optional NORMAL), relative to <see cref="Origin"/>; null when absent. Terrain entries only.</summary>
    public Godot.Collections.Array? Terrain;
    /// <summary>Lowest / highest surface vertex (m above MSL) of a terrain tile.</summary>
    public float HMin, HMax;
    public int Ring;
    public Vector3 Origin;
    public Vector3[] Lights;
    public Color[] LightColors;
    /// <summary>Street lamps (api 1.1; empty from an older library), relative to <see cref="Origin"/>; drawn at every distance.</summary>
    public Vector3[] StreetLights;
    public Color[] StreetLightColors;
    /// <summary>Aircraft warning beacons (api 1.2; empty from an older library): rgb = colour, a = blink phase of the building.</summary>
    public Vector3[] BeaconLights;
    public Color[] BeaconLightColors;
    /// <summary>Airport lights of a seed world (api 1.7; empty otherwise), relative to <see cref="Origin"/>: rgb = fixed colour (blue / white / green / red), a = intensity weight.</summary>
    public Vector3[] AirportLights;
    public Color[] AirportLightColors;
    public Godot.Collections.Array? Mesh;
    /// <summary>Engine surfaces (api 1.11, contract 4a; only with <see cref="WorldRenderSettings.EngineSurfaces"/>): when set, the matching Array above is null and this is the mesh. Null = no such mesh.</summary>
    public Godot.Collections.Dictionary? MeshSurface, GroundSurface, TerrainSurface;
    /// <summary>
    /// True when the dictionary carried a "ground" key at all (an empty Array still counts). Lets the uploader tell
    /// "no ground for this tile" (R3, or an empty tile) from "this delivery does not speak about ground" (older Rust build).
    /// </summary>
    public bool HasGroundKey;
    /// <summary>Ground mesh Array (VERTEX, NORMAL, COLOR, INDEX), relative to <see cref="Origin"/>. Null when absent or empty (R3).</summary>
    public Godot.Collections.Array? Ground;

    /// <summary>
    /// Releases the mesh handles (Arrays and surface Dictionaries) the entry carried, right after the meshes went into their <see cref="ArrayMesh"/>: they own finalizable wrappers and the
    /// delivered vertex data, which would otherwise wait for a garbage collection and its finalizer thread. Call once per tile; the light arrays are plain managed arrays.
    /// </summary>
    public readonly void ReleaseHandles()
    {
        Terrain?.Dispose();
        Mesh?.Dispose();
        Ground?.Dispose();
        MeshSurface?.Dispose();
        GroundSurface?.Dispose();
        TerrainSurface?.Dispose();
    }
}
