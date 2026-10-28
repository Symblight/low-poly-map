using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Terrain tile node set (api 1.5, layer "terrain"): a root and ONE MeshInstance3D with a reused ArrayMesh. It is a
/// <see cref="TileNodes"/> so the <see cref="TileCover"/> bookkeeping (visibility rule, fades, counters) is shared with the vector
/// layer; the building / ground / light fields stay unused (null).
/// </summary>
public sealed class TerrainNodes : TileNodes
{
    /// <summary>Lowest / highest surface vertex in metres above MSL, as delivered (skirts excluded).</summary>
    public float HMin, HMax;
}

/// <summary>
/// Pool of terrain tile nodes. Same rules as <see cref="TilePool"/>: nodes stay in the tree (hidden) when released and are re-used,
/// one shared opaque material plus its dithering variant (assigned only while a tile has a poll_fades entry below 1).
/// The mesh is uploaded uncompressed (AddSurfaceFromArrays without ARRAY_FLAG_COMPRESS_ATTRIBUTES: positions stay 32-bit, 16-bit
/// packing would give 0.3 m steps on a 22 km tile). Terrain receives shadows and casts none.
/// </summary>
public sealed class TerrainPool
{
    private readonly Node3D _container;
    private readonly Stack<TerrainNodes> _free = new();
    private readonly PoolTrimmer _trimmer;
    private readonly System.Action<TerrainNodes> _destroy;
    private readonly ShaderMaterial _material;
    private readonly ShaderMaterial _fadeMaterial;

    private static readonly StringName PFade = new("fade");
    private static readonly StringName PFadeParity = new("fade_parity");

    /// <summary>
    /// Extra frustum-cull margin (m) of every terrain mesh: the visual earth curvature lowers vertices by d^2 / 2R below the flat tile AABB, which
    /// is up to 1.8 km at a 150 km far plane (<see cref="WorldView"/> sets it after each configure from the camera far plane and the terrain reach).
    /// </summary>
    public float CullMargin { get; set; } = TilePool.CurveCullMargin;

    /// <summary>Node sets that exist now (in use plus free); a trimmed set no longer counts.</summary>
    public int TotalCreated { get; private set; }
    /// <summary>Node sets destroyed by <see cref="Trim"/> since start.</summary>
    public int Trimmed => _trimmer.Trimmed;
    public int FreeCount => _free.Count;
    public ShaderMaterial Material => _material;
    public ShaderMaterial FadeMaterial => _fadeMaterial;

    public TerrainPool(Node3D container, WorldRenderSettings? settings = null, string? addonRoot = null)
    {
        _container = container;
        settings ??= new WorldRenderSettings();
        _trimmer = new PoolTrimmer("terrain tiles", settings);
        _destroy = Destroy;
        string shaders = (addonRoot ?? AddonPaths.Root) + "/shaders";
        _material = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/terrain.gdshader") };
        _fadeMaterial = new ShaderMaterial { Shader = TilePool.LoadDitherVariant(shaders + "/terrain.gdshader") };
        settings.ApplyToTerrain(_material);
        settings.ApplyToTerrain(_fadeMaterial);
    }

    /// <summary>Flat shading from screen derivatives (profile terrain.shading = "flat", default) or the mesh NORMAL ("smooth").</summary>
    public void SetSmoothShading(bool smooth)
    {
        _material.SetShaderParameter("use_vertex_normal", smooth);
        _fadeMaterial.SetShaderParameter("use_vertex_normal", smooth);
    }

    public TerrainNodes Acquire(string key, long packed, int ring)
    {
        if (!_free.TryPop(out var t))
        {
            t = Create();
            TotalCreated++;
        }
        t.Key = key;
        t.Packed = packed;
        t.Ring = ring;
        t.MeshInstance.ExtraCullMargin = CullMargin;
        return t;
    }

    /// <summary>Once per frame: destroys a few idle node sets when the free list has stayed above its margin (see <see cref="PoolTrimmer"/>). Returns how many.</summary>
    public int Trim(double delta)
    {
        int n = _trimmer.Step(delta, TotalCreated - _free.Count, _free, _destroy);
        TotalCreated -= n;
        return n;
    }

    private void Destroy(TerrainNodes t)
    {
        _container.RemoveChild(t.Root);
        t.Root.QueueFree();
    }

    public void Release(TerrainNodes t)
    {
        t.Root.Visible = false;
        EndFade(t);
        t.Fade = 1f;
        t.Fading = false;
        t.FadeAwait = false;
        ClearMesh(t);
        t.Key = "";
        t.Packed = 0;
        t.Applied = false;
        _free.Push(t);
    }

    private TerrainNodes Create()
    {
        var am = new ArrayMesh();
        var mi = new MeshInstance3D
        {
            Name = "Terrain",
            Mesh = am,
            MaterialOverride = _material,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            // Curved vertices sink below the flat tile AABB: widen the cull box by the drop at the camera's far plane.
            ExtraCullMargin = TilePool.CurveCullMargin,
            Visible = false,
        };
        var root = new Node3D { Name = "TerrainTile", Visible = false };
        root.AddChild(mi);
        _container.AddChild(root);
        return new TerrainNodes { Root = root, MeshInstance = mi, Mesh = am };
    }

    /// <summary>Switches to the dithering material and sets the per-instance fade / fade_parity (parity = z &amp; 1 of the key).</summary>
    public void SetFade(TerrainNodes t, float fade)
    {
        if (!t.FadeMaterialActive)
        {
            t.MeshInstance.MaterialOverride = _fadeMaterial;
            t.FadeMaterialActive = true;
            t.MeshInstance.SetInstanceShaderParameter(PFadeParity, (float)(TileKey.Z(t.Packed) & 1));
        }
        t.MeshInstance.SetInstanceShaderParameter(PFade, fade);
    }

    public void EndFade(TerrainNodes t)
    {
        if (!t.FadeMaterialActive) return;
        t.MeshInstance.MaterialOverride = _material;
        t.FadeMaterialActive = false;
    }

    private static void ClearMesh(TerrainNodes t)
    {
        if (t.HasMesh) t.Mesh.ClearSurfaces();
        t.HasMesh = false;
        t.Tris = 0;
        t.MeshInstance.Visible = false;
    }

    /// <summary>
    /// Uploads the terrain mesh (VERTEX + INDEX, NORMAL with smooth shading) into the tile's reused ArrayMesh, replacing the previous
    /// surface (a re-delivery under the same key is not expected since api 1.10, handled anyway). Null = nothing usable: the instance is hidden.
    /// </summary>
    public void SetMesh(TerrainNodes t, Godot.Collections.Array? arrays, float hMin, float hMax)
    {
        ClearMesh(t);
        t.HMin = hMin;
        t.HMax = hMax;
        if (arrays == null) return;
        t.Mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays); // no compress flags: uncompressed positions
        if (t.Mesh.GetSurfaceCount() == 0) return;
        int idx = t.Mesh.SurfaceGetArrayIndexLen(0);
        t.Tris = (idx > 0 ? idx : t.Mesh.SurfaceGetArrayLen(0)) / 3;
        t.HasMesh = true;
        t.MeshInstance.Visible = true;
    }
}
