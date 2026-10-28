using Godot;

namespace SimWorld;

/// <summary>
/// The pooled node set that represents one streamed tile: a building MeshInstance3D, a ground MeshInstance3D and the light sprite sets (building
/// lights, street lamps with their ground pools, beacons, airport lights) under one <see cref="Sprites"/> container, all children of one root (so one
/// Visible flag and one origin shift cover them; the container is the one switch that hides every sprite set by day).
/// </summary>
public class TileNodes
{
    public Node3D Root = null!;
    public MeshInstance3D MeshInstance = null!;
    /// <summary>Reused (ClearSurfaces + AddSurfaceFromArrays) for every tile.</summary>
    public ArrayMesh Mesh = null!;
    /// <summary>Flat landuse / water / road mesh (R0..R2), reused like <see cref="Mesh"/>.</summary>
    public MeshInstance3D GroundInstance = null!;
    public ArrayMesh GroundMesh = null!;
    /// <summary>Parent of the five sprite instances below; hidden by <see cref="TilePool.SetSpritesVisible"/> while the night factor is 0 (the sprites add nothing by day).</summary>
    public Node3D Sprites = null!;
    /// <summary>Whether <see cref="Sprites"/> is currently shown (mirrors the node so the pool does not read it back).</summary>
    public bool SpritesShown = true;
    public MultiMeshInstance3D Lights = null!;
    public MultiMesh LightsMm = null!;
    /// <summary>Street lamps (api 1.1): same sprite shader, its own material without the window cross-fade.</summary>
    public MultiMeshInstance3D StreetLights = null!;
    public MultiMesh StreetLightsMm = null!;
    /// <summary>Light pools on the ground under the lamps: a second instance of <see cref="StreetLightsMm"/> (no extra upload).</summary>
    public MultiMeshInstance3D StreetPools = null!;
    /// <summary>Aircraft warning beacons (api 1.2), own blinking material.</summary>
    public MultiMeshInstance3D Beacons = null!;
    public MultiMesh BeaconsMm = null!;
    public int BeaconCount;
    /// <summary>Airport lights of a seed world (api 1.7): the sprite shader with fixed colours, shown at every distance, always on at night.</summary>
    public MultiMeshInstance3D AirportLights = null!;
    public MultiMesh AirportLightsMm = null!;
    public int AirportLightCount;

    /// <summary>
    /// The extra frustum-cull margin (m) last written to this set's instances: the earth curvature sinks the drawn vertices below the flat AABB by up
    /// to d^2 / 2R at the tile's far edge (<see cref="CullMargins"/>). NaN until the first write. <see cref="HalfSpanM"/> = half the tile's diagonal.
    /// </summary>
    public float CullMargin = float.NaN;
    public float HalfSpanM;

    public string Key = "";
    /// <summary>Time of the first upload under this key (ms since start); churn diagnostics.</summary>
    public ulong LoadedMs;
    public long Packed;
    public int Ring;
    public bool HasMesh;
    public int Tris;
    public bool HasGround;
    public int GroundTris;
    public int LightCount;
    public int StreetLightCount;
    /// <summary>True when any light carries a cluster weight (alpha above a single light's 0.125).</summary>
    public bool Clustered;

    // Managed by TileCover.
    public TileCover.Vis Vis;
    public bool Applied;
    /// <summary>Managed by TileCover: whether the root is currently shown, and when it last changed (Stopwatch ticks; 0 = never), for the churn counters.</summary>
    public bool Shown;
    public long LastToggleTicks;
    /// <summary>Uploaded but poll_fades has not run since: the root stays hidden (see <see cref="TileCover"/>).</summary>
    public bool FadeAwait;
    /// <summary>Has a poll_fades entry below 1: visible whenever uploaded, drawn with the dithering material variants.</summary>
    public bool Fading;
    /// <summary>Last fade value from poll_fades (1 = fully visible).</summary>
    public float Fade = 1f;
    /// <summary>Bridge bookkeeping: frame counter of the last poll_fades that listed this tile.</summary>
    public int FadeStamp;
    /// <summary>True while the dithering material variants are assigned (instance uniforms are valid).</summary>
    public bool FadeMaterialActive;
    /// <summary>The dither material clones this node set rents while <see cref="FadeMaterialActive"/> (see <see cref="FadeMaterials"/>): building, ground, five sprite sets; a terrain tile uses [0].</summary>
    public readonly ShaderMaterial?[] FadeMats = new ShaderMaterial?[7];
}
