using Godot;

namespace SimWorld;

/// <summary>
/// The pooled node set that represents one streamed tile: a building MeshInstance3D, a ground MeshInstance3D
/// and two light MultiMeshInstance3Ds (building lights, street lamps), all children of one root (so one Visible flag and one origin shift cover them).
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
    public MultiMeshInstance3D Lights = null!;
    public MultiMesh LightsMm = null!;
    /// <summary>Reused MultiMesh buffer (transform 12 floats + color 4 floats per instance).</summary>
    public float[] Buffer = System.Array.Empty<float>();
    /// <summary>Street lamps (api 1.1): same sprite shader, its own material without the window cross-fade.</summary>
    public MultiMeshInstance3D StreetLights = null!;
    public MultiMesh StreetLightsMm = null!;
    /// <summary>Light pools on the ground under the lamps: a second instance of <see cref="StreetLightsMm"/> (no extra upload).</summary>
    public MultiMeshInstance3D StreetPools = null!;
    /// <summary>Aircraft warning beacons (api 1.2), own blinking material.</summary>
    public MultiMeshInstance3D Beacons = null!;
    public MultiMesh BeaconsMm = null!;
    public float[] BeaconBuffer = System.Array.Empty<float>();
    public int BeaconCount;
    public float[] StreetBuffer = System.Array.Empty<float>();
    /// <summary>Airport lights of a seed world (api 1.7): the sprite shader with fixed colours, shown at every distance, always on at night.</summary>
    public MultiMeshInstance3D AirportLights = null!;
    public MultiMesh AirportLightsMm = null!;
    public float[] AirportBuffer = System.Array.Empty<float>();
    public int AirportLightCount;

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
}
