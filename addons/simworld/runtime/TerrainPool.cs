#nullable enable
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
    private readonly MeshSurfaces _surfaces = new();
    private readonly Stack<TerrainNodes> _free = new();
    private readonly RetireQueue<TerrainNodes> _retiring = new(); // released, mesh not freed yet (see TilePool.DrainRetired)
    private readonly PoolTrimmer _trimmer;
    private readonly System.Action<TerrainNodes> _destroy, _finishRetired;
    private readonly ShaderMaterial _material;
    private readonly FadeMaterials _fades;

    private static readonly StringName PFade = new("fade");
    private static readonly StringName PFadeParity = new("fade_parity");

    /// <summary>Node sets that exist now (in use plus free); a trimmed set no longer counts.</summary>
    public int TotalCreated { get; private set; }
    /// <summary>Node sets destroyed by <see cref="Trim"/> since start.</summary>
    public int Trimmed => _trimmer.Trimmed;
    /// <summary>Sets that can be handed out: cleared ones plus retired ones waiting for <see cref="DrainRetired"/>.</summary>
    public int FreeCount => _free.Count + _retiring.Count;
    public int RetiringCount => _retiring.Count;
    public ShaderMaterial Material => _material;
    /// <summary>The configured dither variant (template; fading tiles use clones, see <see cref="FadeMaterials"/>).</summary>
    public ShaderMaterial FadeMaterial => _fades.Template;

    public TerrainPool(Node3D container, WorldRenderSettings? settings = null, string? addonRoot = null)
    {
        _container = container;
        settings ??= new WorldRenderSettings();
        _trimmer = new PoolTrimmer("terrain tiles", settings);
        _destroy = Destroy;
        _finishRetired = FinishRetired;
        string shaders = (addonRoot ?? AddonPaths.Root) + "/shaders";
        _material = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/terrain.gdshader") };
        var fadeMaterial = new ShaderMaterial { Shader = TilePool.LoadDitherVariant(shaders + "/terrain.gdshader") };
        settings.ApplyToTerrain(_material);
        settings.ApplyToTerrain(fadeMaterial);
        _fades = new FadeMaterials(fadeMaterial);
    }

    /// <summary>Makes dither clones ahead of need, up to <paramref name="tiles"/> in total, at most <paramref name="perCall"/> per call (see <see cref="TilePool.ReserveFadeClones"/>). True when done.</summary>
    public bool ReserveFadeClones(int tiles, int perCall)
    {
        for (int i = 0; i < perCall && _fades.Clones < tiles; i++) _fades.AddClone();
        return _fades.Clones >= tiles;
    }

    /// <summary>Flat shading from screen derivatives (profile terrain.shading = "flat", default) or the mesh NORMAL ("smooth").</summary>
    public void SetSmoothShading(bool smooth)
    {
        _material.SetShaderParameter("use_vertex_normal", smooth);
        _fades.Set("use_vertex_normal", smooth);
    }

    public TerrainNodes Acquire(string key, long packed, int ring)
    {
        if (!_free.TryPop(out var t))
        {
            if (_retiring.TryDequeue(out t)) ClearMesh(t); // never reuse a set before its release ran
            else
            {
                t = Create();
                TotalCreated++;
            }
        }
        t.Key = key;
        t.Packed = packed;
        t.Ring = ring;
        return t;
    }

    /// <summary>Once per frame: destroys a few idle node sets when the free list has stayed above its margin (see <see cref="PoolTrimmer"/>). Returns how many.</summary>
    public int Trim(double delta)
    {
        int n = _trimmer.Step(delta, TotalCreated - FreeCount, _free, _destroy);
        TotalCreated -= n;
        return n;
    }

    private void Destroy(TerrainNodes t)
    {
        _container.RemoveChild(t.Root);
        t.Root.QueueFree();
    }

    /// <summary>Hides the tile at once and queues it for <see cref="DrainRetired"/>, which frees its mesh later (see <see cref="TilePool.Release"/>).</summary>
    public void Release(TerrainNodes t)
    {
        t.Root.Visible = false;
        EndFade(t);
        t.Fade = 1f;
        t.Fading = false;
        t.FadeAwait = false;
        t.Key = "";
        t.Packed = 0;
        t.Applied = false;
        _retiring.Enqueue(t);
    }

    /// <summary>Frees the meshes of retired sets within <paramref name="budgetUs"/> microseconds (at least one) and returns them to the free list.</summary>
    public int DrainRetired(long budgetUs) => _retiring.Drain(budgetUs, _finishRetired);

    private void FinishRetired(TerrainNodes t)
    {
        ClearMesh(t);
        _free.Push(t);
    }

    /// <summary>Earth-curvature cull margin of the tile's mesh (<see cref="CullMargins"/>).</summary>
    public static void SetCullMargin(TerrainNodes t, float margin)
    {
        t.CullMargin = margin;
        t.MeshInstance.ExtraCullMargin = margin;
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
            // The curvature margin is written per tile (SetCullMargin).
            Visible = false,
        };
        var root = new Node3D { Name = "TerrainTile", Visible = false };
        root.AddChild(mi);
        _container.AddChild(root);
        return new TerrainNodes { Root = root, MeshInstance = mi, Mesh = am };
    }

    /// <summary>Switches to a clone of the dithering material (rented per fading tile) and sets its fade / fade_parity (parity = z &amp; 1 of the key).</summary>
    public void SetFade(TerrainNodes t, float fade)
    {
        if (!t.FadeMaterialActive)
        {
            var m = _fades.Rent();
            m.SetShaderParameter(PFadeParity, (float)(TileKey.Z(t.Packed) & 1));
            t.FadeMats[0] = m;
            t.MeshInstance.MaterialOverride = m;
            t.FadeMaterialActive = true;
        }
        t.FadeMats[0]!.SetShaderParameter(PFade, fade);
    }

    public void EndFade(TerrainNodes t)
    {
        if (!t.FadeMaterialActive) return;
        t.MeshInstance.MaterialOverride = _material;
        _fades.Return(t.FadeMats[0]!);
        t.FadeMats[0] = null;
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
    /// Puts the terrain mesh (VERTEX + INDEX, NORMAL with smooth shading; an engine surface when given, api 1.11, else an Array) into the tile's reused
    /// ArrayMesh, replacing the previous surface (a re-delivery under the same key is not expected since api 1.10, handled anyway). Both null = nothing
    /// usable: the instance is hidden. Uncompressed either way (positions stay 32-bit).
    /// </summary>
    public bool SetMesh(TerrainNodes t, Godot.Collections.Array? arrays, Godot.Collections.Dictionary? surface, float hMin, float hMax)
    {
        ClearMesh(t);
        t.HMin = hMin;
        t.HMax = hMax;
        if (arrays == null && surface == null) return true;
        if (!_surfaces.Fill(t.Mesh, arrays, surface)) return false; // rejected by Godot
        t.Tris = MeshSurfaces.Triangles(t.Mesh);
        t.HasMesh = true;
        t.MeshInstance.Visible = true;
        return true;
    }
}
