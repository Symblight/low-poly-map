#nullable enable
using Godot;

namespace SimWorld;

/// <summary>
/// The pooled node set that represents one streamed tile: a building MeshInstance3D, a ground MeshInstance3D and the point sprites (building lights, street
/// lamps, airport lights and beacons in ONE mesh of points, <see cref="Points"/>, plus on the tiles that can show them the street lamp pools, a MultiMesh of ground
/// quads, <see cref="Pools"/>) under one <see cref="Sprites"/> container, all children of one root (so one Visible flag and one origin shift cover them; the
/// container is the one switch that hides every sprite by day). The R2 / R3 tiles' points are not drawn from <see cref="Points"/> but from a merged
/// <see cref="SpriteCell"/> (<see cref="CellMember"/>).
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
    /// <summary>Parent of <see cref="Points"/> and <see cref="Pools"/>; hidden by <see cref="TilePool.SetSpritesVisible"/> while the night factor is 0 (the sprites add nothing by day).</summary>
    public Node3D Sprites = null!;
    /// <summary>Whether <see cref="Sprites"/> is currently shown (mirrors the node so the pool does not read it back).</summary>
    public bool SpritesShown = true;
    /// <summary>The tile's point sprites, one vertex each: <see cref="LightCount"/> building lights, then <see cref="StreetLightCount"/> street lamps, <see cref="AirportLightCount"/> airport lights and <see cref="BeaconCount"/> beacons (see <see cref="SpriteKind"/>), in <see cref="PointsMesh"/> (reused: one engine surface set per tile, see <see cref="PointSurface"/>) for the tiles of R0 / R1; the R2 / R3 tiles' points are in a merged cell.</summary>
    public MeshInstance3D Points = null!;
    public ArrayMesh PointsMesh = null!;
    /// <summary>Whether <see cref="PointsMesh"/> holds a surface (the tile draws its own points).</summary>
    public bool PointsFilled;
    /// <summary>The tile's points live in a merged <see cref="SpriteCell"/> (R2 / R3) instead of <see cref="PointsMesh"/>; its fade and visibility reach them through the cell's slot table.</summary>
    internal CellMember? CellMember;
    /// <summary>Light pools on the ground under the street lamps: a MultiMesh with one instance per lamp, filled only on rings up to <see cref="WorldRenderSettings.StreetPoolMaxRing"/> and drawn only while <see cref="PoolsInRange"/>.</summary>
    public MultiMeshInstance3D Pools = null!;
    public MultiMesh PoolsMm = null!;
    /// <summary>The pool MultiMesh holds this tile's lamps (its ring can show pools and it has lamps), and the tile is within the pool fade range of the camera (refreshed a few times a second by the view; true at upload).</summary>
    public bool PoolsFilled, PoolsInRange = true;
    public int LightCount;
    public int StreetLightCount;
    public int AirportLightCount;
    public int BeaconCount;
    /// <summary>All sprites of the tile (the sum of the four counts).</summary>
    public int SpriteCount => LightCount + StreetLightCount + AirportLightCount + BeaconCount;

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
    /// <summary>The dither material clones this node set rents while <see cref="FadeMaterialActive"/> (see <see cref="FadeMaterials"/>): building, ground, point sprites, pools; a terrain tile uses [0].</summary>
    public readonly ShaderMaterial?[] FadeMats = new ShaderMaterial?[TilePool.FadeNodes];
}
