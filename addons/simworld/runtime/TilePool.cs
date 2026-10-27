using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// The pooled node set that represents one streamed tile: a building MeshInstance3D, a ground MeshInstance3D
/// and a light MultiMeshInstance3D, all children of one root (so one Visible flag and one origin shift cover them).
/// </summary>
public sealed class TileNodes
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

    public string Key = "";
    public long Packed;
    public int Ring;
    public bool HasMesh;
    public int Tris;
    public bool HasGround;
    public int GroundTris;
    public int LightCount;
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

/// <summary>
/// Pool of tile node sets. Nodes stay in the tree (hidden) when released and are re-used, so
/// no QueueFree / new happens per tile once the pool has warmed up.
/// </summary>
public sealed class TilePool
{
    private const int FloatsPerInstance = 16;
    private const int CapacityStep = 256;
    /// <summary>Alpha of a single light is clamp(sqrt(1)/8) = 0.125; anything clearly above is a cluster.</summary>
    private const float ClusterAlpha = 0.13f;

    private readonly Node3D _container;
    private readonly Stack<TileNodes> _free = new();
    private readonly Mesh _lightMesh;
    private readonly ShaderMaterial _lightMaterial;
    private readonly ShaderMaterial _buildingMaterial;
    private readonly ShaderMaterial _groundMaterial;
    // Dithering variants of the two opaque materials: the same shader source compiled with LOD_DITHER, so the discard
    // (and its loss of early depth rejection) exists only while a tile is in a fade. Per-tile values are instance uniforms.
    private readonly ShaderMaterial _buildingFadeMaterial;
    private readonly ShaderMaterial _groundFadeMaterial;

    private static readonly StringName PFade = new("fade");
    private static readonly StringName PFadeParity = new("fade_parity");
    private static readonly StringName PFadeMul = new("fade_mul");

    /// <summary>Debug (user arg --light-alpha=&lt;v&gt;): force every light's weight, ignoring Rust's alpha. Negative = off.</summary>
    public static float DebugLightAlpha { get; set; } = -1f;
    /// <summary>Extra cull margin (m) for the earth-curvature drop of building and ground meshes; set before the pool is created.</summary>
    public static float CurveCullMargin { get; set; }

    public int TotalCreated { get; private set; }
    public int FreeCount => _free.Count;
    public ShaderMaterial BuildingMaterial => _buildingMaterial;
    public ShaderMaterial BuildingFadeMaterial => _buildingFadeMaterial;
    public ShaderMaterial GroundFadeMaterial => _groundFadeMaterial;
    public ShaderMaterial LightMaterial => _lightMaterial;
    public ShaderMaterial GroundMaterial => _groundMaterial;

    /// <summary>
    /// Copy of <paramref name="path"/>'s shader with <c>#define LOD_DITHER</c> inserted after the shader_type line. The copy has no
    /// path of its own, so relative <c>#include</c>s are made absolute against the original's folder.
    /// </summary>
    private static Shader LoadDitherVariant(string path)
    {
        var src = GD.Load<Shader>(path);
        var dir = path.Substring(0, path.LastIndexOf('/'));
        var code = System.Text.RegularExpressions.Regex.Replace(src.Code, "#include \"(?!res://)([^\"]+)\"", m => $"#include \"{dir}/{m.Groups[1].Value}\"");
        int nl = code.IndexOf('\n', code.IndexOf("shader_type", System.StringComparison.Ordinal));
        var variant = new Shader { Code = code.Insert(nl + 1, "#define LOD_DITHER\n") };
        variant.ResourceName = path + " (LOD_DITHER)";
        return variant;
    }

    /// <param name="container">Parent of the pooled tile roots.</param>
    /// <param name="settings">Material parameters; null = <see cref="WorldRenderSettings"/> defaults.</param>
    /// <param name="addonRoot">Addon folder; null = <see cref="AddonPaths.Root"/>.</param>
    public TilePool(Node3D container, WorldRenderSettings? settings = null, string? addonRoot = null)
    {
        _container = container;
        settings ??= new WorldRenderSettings();
        string shaders = (addonRoot ?? AddonPaths.Root) + "/shaders";
        string textures = (addonRoot ?? AddonPaths.Root) + "/textures";

        // Unit quad; the sprite shader sizes it in pixels and billboards it.
        _lightMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/light_sprite.gdshader") };
        _lightMaterial.SetShaderParameter("dot_tex", GD.Load<Texture2D>(textures + "/light_dot_32.png"));
        settings.ApplyToLights(_lightMaterial);
        _lightMesh = new QuadMesh { Size = Vector2.One, Material = _lightMaterial };

        // One shared material for every building mesh of every tile (opaque, no discard).
        _buildingMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/building_day.gdshader") };
        _buildingFadeMaterial = new ShaderMaterial { Shader = LoadDitherVariant(shaders + "/building_day.gdshader") };
        settings.ApplyToBuilding(_buildingMaterial);
        settings.ApplyToBuilding(_buildingFadeMaterial);

        // One shared material for every ground mesh (palette decoded from vertex COLOR).
        _groundMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/ground.gdshader") };
        _groundFadeMaterial = new ShaderMaterial { Shader = LoadDitherVariant(shaders + "/ground.gdshader") };
        settings.ApplyToGround(_groundMaterial);
        settings.ApplyToGround(_groundFadeMaterial);
    }

    /// <summary>Creates <paramref name="count"/> hidden node sets up front to avoid first-use hitches.</summary>
    public void Prewarm(int count)
    {
        for (int i = 0; i < count; i++)
        {
            _free.Push(Create());
            TotalCreated++;
        }
    }

    /// <summary>Takes a node set from the pool. The root stays hidden; <see cref="TileCover"/> decides visibility.</summary>
    public TileNodes Acquire(string key, long packed, int ring)
    {
        if (!_free.TryPop(out var t))
        {
            t = Create();
            TotalCreated++;
        }
        t.Key = key;
        t.Packed = packed;
        t.Ring = ring;
        // Only the finest band is close enough to profit from shadows (also keeps the draw-call count down).
        t.MeshInstance.CastShadow = ring == 0
            ? GeometryInstance3D.ShadowCastingSetting.On
            : GeometryInstance3D.ShadowCastingSetting.Off;
        return t;
    }

    public void Release(TileNodes t)
    {
        t.Root.Visible = false;
        EndFade(t);
        t.Fade = 1f;
        t.Fading = false;
        t.FadeAwait = false;
        ClearMesh(t);
        ClearGround(t);
        ClearLights(t);
        t.Key = "";
        t.Packed = 0;
        t.Applied = false;
        _free.Push(t);
    }

    private TileNodes Create()
    {
        var mm = new MultiMesh
        {
            TransformFormat = MultiMesh.TransformFormatEnum.Transform3D,
            UseColors = true,
            Mesh = _lightMesh,
        };
        mm.InstanceCount = 0; // after format/colors are set
        var mmi = new MultiMeshInstance3D
        {
            Name = "Lights",
            Multimesh = mm,
            // Also as override so the per-instance fade_mul uniform is found on the MultiMesh instance.
            MaterialOverride = _lightMaterial,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            Visible = false,
            // The billboard is expanded in the vertex shader; keep distant sprites out of any frustum-edge pop.
            ExtraCullMargin = 200f + CurveCullMargin,
        };
        var am = new ArrayMesh();
        var mi = new MeshInstance3D
        {
            Name = "Mesh",
            Mesh = am,
            MaterialOverride = _buildingMaterial,
            ExtraCullMargin = CurveCullMargin,
            Visible = false,
        };
        var gm = new ArrayMesh();
        var gi = new MeshInstance3D
        {
            Name = "Ground",
            Mesh = gm,
            MaterialOverride = _groundMaterial,
            // Flat ground never needs to cast shadows (and every shadow caster costs an extra draw call).
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            ExtraCullMargin = CurveCullMargin,
            Visible = false,
        };
        var root = new Node3D { Name = "Tile", Visible = false };
        root.AddChild(mi);
        root.AddChild(gi);
        root.AddChild(mmi);
        _container.AddChild(root);
        return new TileNodes { Root = root, MeshInstance = mi, Mesh = am, GroundInstance = gi, GroundMesh = gm, Lights = mmi, LightsMm = mm };
    }

    /// <summary>
    /// Applies a poll_fades value (0..1, below 1) to a tile: switches the building and ground meshes to the dithering
    /// material variants and sets their per-instance fade / fade_parity (parity = z &amp; 1 of the key). The light
    /// sprites never dither; their brightness is scaled by the same value instead, so the two additive sides of a swap
    /// sum to constant brightness (no double-bright flash while R3/R2 lights overlap).
    /// </summary>
    public void SetFade(TileNodes t, float fade)
    {
        if (!t.FadeMaterialActive)
        {
            t.MeshInstance.MaterialOverride = _buildingFadeMaterial;
            t.GroundInstance.MaterialOverride = _groundFadeMaterial;
            t.FadeMaterialActive = true;
            float parity = TileKey.Z(t.Packed) & 1;
            t.MeshInstance.SetInstanceShaderParameter(PFadeParity, parity);
            t.GroundInstance.SetInstanceShaderParameter(PFadeParity, parity);
        }
        t.MeshInstance.SetInstanceShaderParameter(PFade, fade);
        t.GroundInstance.SetInstanceShaderParameter(PFade, fade);
        t.Lights.SetInstanceShaderParameter(PFadeMul, fade);
    }

    /// <summary>Back to the opaque materials (no discard) once a tile left its transition, or when it is released.</summary>
    public void EndFade(TileNodes t)
    {
        if (!t.FadeMaterialActive) return;
        t.MeshInstance.MaterialOverride = _buildingMaterial;
        t.GroundInstance.MaterialOverride = _groundMaterial;
        t.Lights.SetInstanceShaderParameter(PFadeMul, 1f);
        t.FadeMaterialActive = false;
    }

    private void ClearMesh(TileNodes t)
    {
        if (t.HasMesh) t.Mesh.ClearSurfaces();
        t.HasMesh = false;
        t.Tris = 0;
        t.MeshInstance.Visible = false;
    }

    private void ClearGround(TileNodes t)
    {
        if (t.HasGround) t.GroundMesh.ClearSurfaces();
        t.HasGround = false;
        t.GroundTris = 0;
        t.GroundInstance.Visible = false;
    }

    private void ClearLights(TileNodes t)
    {
        t.LightsMm.VisibleInstanceCount = 0;
        t.Lights.Visible = false;
        t.LightCount = 0;
        t.Clustered = false;
    }

    /// <summary>
    /// Uploads <paramref name="arrays"/> (Mesh.ARRAY_MAX layout) into the tile's reused ArrayMesh on the
    /// calling (main) thread. Null = empty tile (no surface, instance hidden).
    /// </summary>
    public void SetMesh(TileNodes t, Godot.Collections.Array? arrays)
    {
        ClearMesh(t);
        if (arrays == null) return;
        t.Mesh.AddSurfaceFromArrays(Godot.Mesh.PrimitiveType.Triangles, arrays);
        if (t.Mesh.GetSurfaceCount() == 0) return; // Godot rejected the arrays (error already printed)
        int idx = t.Mesh.SurfaceGetArrayIndexLen(0);
        t.Tris = (idx > 0 ? idx : t.Mesh.SurfaceGetArrayLen(0)) / 3;
        t.HasMesh = true;
        t.MeshInstance.Visible = true;
    }

    /// <summary>
    /// Uploads the ground mesh (Mesh.ARRAY_MAX layout: VERTEX, NORMAL, COLOR, INDEX) into the tile's reused ground
    /// ArrayMesh. Null = no ground (R3 / empty tile): the instance is hidden. Visibility of the whole tile is the
    /// root's, so ground swaps exactly with the buildings.
    /// </summary>
    public void SetGround(TileNodes t, Godot.Collections.Array? arrays)
    {
        ClearGround(t);
        if (arrays == null) return;
        t.GroundMesh.AddSurfaceFromArrays(Godot.Mesh.PrimitiveType.Triangles, arrays);
        if (t.GroundMesh.GetSurfaceCount() == 0) return;
        int idx = t.GroundMesh.SurfaceGetArrayIndexLen(0);
        t.GroundTris = (idx > 0 ? idx : t.GroundMesh.SurfaceGetArrayLen(0)) / 3;
        t.HasGround = true;
        t.GroundInstance.Visible = true;
    }

    /// <summary>
    /// Fills the tile's light MultiMesh from positions relative to the tile origin. Color rgb = light color,
    /// alpha = intensity weight (0.125 single light .. 1 big cluster); the sprite shader reads it from COLOR.
    /// </summary>
    public void SetLights(TileNodes t, Vector3[] lights, Color[] colors)
    {
        var mm = t.LightsMm;
        int n = lights.Length;
        t.LightCount = n;
        t.Clustered = false;
        if (n == 0)
        {
            mm.VisibleInstanceCount = 0;
            t.Lights.Visible = false;
            return;
        }

        int cap = (n + CapacityStep - 1) / CapacityStep * CapacityStep;
        if (mm.InstanceCount != cap) mm.InstanceCount = cap;
        int len = cap * FloatsPerInstance;
        if (t.Buffer.Length != len) t.Buffer = new float[len];

        var buf = t.Buffer;
        int nc = colors.Length;
        bool clustered = false;
        for (int i = 0, o = 0; i < n; i++, o += FloatsPerInstance)
        {
            var p = lights[i];
            buf[o + 0] = 1f; buf[o + 1] = 0; buf[o + 2] = 0; buf[o + 3] = p.X;
            buf[o + 4] = 0; buf[o + 5] = 1f; buf[o + 6] = 0; buf[o + 7] = p.Y;
            buf[o + 8] = 0; buf[o + 9] = 0; buf[o + 10] = 1f; buf[o + 11] = p.Z;
            if (i < nc)
            {
                var c = colors[i];
                float a = DebugLightAlpha >= 0f ? DebugLightAlpha : c.A;
                buf[o + 12] = c.R; buf[o + 13] = c.G; buf[o + 14] = c.B; buf[o + 15] = a;
                if (a > ClusterAlpha) clustered = true;
            }
            else
            {
                buf[o + 12] = 1; buf[o + 13] = 0.9f; buf[o + 14] = 0.7f; buf[o + 15] = 0.125f;
            }
        }
        mm.Buffer = buf;
        mm.VisibleInstanceCount = n;
        t.Lights.Visible = true;
        t.Clustered = clustered;
    }
}
