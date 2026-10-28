using System.Collections.Generic;
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
    private readonly Mesh _streetLightMesh;
    private readonly ShaderMaterial _streetLightMaterial;
    private readonly ShaderMaterial _streetPoolMaterial;
    private readonly Mesh _airportLightMesh;
    private readonly ShaderMaterial _airportLightMaterial;
    private readonly float _airportReferenceWeight;
    private readonly Mesh _beaconMesh;
    private readonly ShaderMaterial _beaconMaterial;
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
    public ShaderMaterial StreetLightMaterial => _streetLightMaterial;
    public ShaderMaterial StreetPoolMaterial => _streetPoolMaterial;
    public ShaderMaterial BeaconMaterial => _beaconMaterial;
    public ShaderMaterial AirportLightMaterial => _airportLightMaterial;
    public ShaderMaterial GroundMaterial => _groundMaterial;

    /// <summary>
    /// Copy of <paramref name="path"/>'s shader with <c>#define LOD_DITHER</c> inserted after the shader_type line. The copy has no
    /// path of its own, so relative <c>#include</c>s are made absolute against the original's folder.
    /// </summary>
    internal static Shader LoadDitherVariant(string path)
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
        _streetLightMaterial = new ShaderMaterial { Shader = _lightMaterial.Shader };
        _streetLightMaterial.SetShaderParameter("dot_tex", _lightMaterial.GetShaderParameter("dot_tex"));
        settings.ApplyToStreetLights(_streetLightMaterial);
        _streetLightMesh = new QuadMesh { Size = Vector2.One, Material = _streetLightMaterial };
        _airportReferenceWeight = System.Math.Max(settings.AirportLightReferenceWeight, 0.001f);
        _airportLightMaterial = new ShaderMaterial { Shader = _lightMaterial.Shader };
        _airportLightMaterial.SetShaderParameter("dot_tex", _lightMaterial.GetShaderParameter("dot_tex"));
        settings.ApplyToAirportLights(_airportLightMaterial);
        _airportLightMesh = new QuadMesh { Size = Vector2.One, Material = _airportLightMaterial };
        _streetPoolMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/street_light_pool.gdshader") };
        settings.ApplyToStreetPools(_streetPoolMaterial);
        _beaconMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/beacon_light.gdshader") };
        _beaconMaterial.SetShaderParameter("dot_tex", _lightMaterial.GetShaderParameter("dot_tex"));
        settings.ApplyToBeacons(_beaconMaterial);
        _beaconMesh = new QuadMesh { Size = Vector2.One, Material = _beaconMaterial };

        // One shared material for every building mesh of every tile (opaque, no discard).
        _buildingMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/building_windows.gdshader") };
        _buildingFadeMaterial = new ShaderMaterial { Shader = LoadDitherVariant(shaders + "/building_windows.gdshader") };
        settings.ApplyToBuilding(_buildingMaterial);
        settings.ApplyToBuilding(_buildingFadeMaterial);
        SetWindowShares(FallbackWindowShares);
        SetProfileColors(DefaultProfileColors);

        // One shared material for every ground mesh (palette decoded from vertex COLOR).
        _groundMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/ground.gdshader") };
        _groundFadeMaterial = new ShaderMaterial { Shader = LoadDitherVariant(shaders + "/ground.gdshader") };
        settings.ApplyToGround(_groundMaterial);
        settings.ApplyToGround(_groundFadeMaterial);
    }

    /// <summary>
    /// Sets <c>emission_level</c> (the HDR level a lit window / sprite reaches, <see cref="NightLook.EmissionLevel"/>) on both building
    /// materials, which must never drift apart. Windows emit <c>window_emission</c> (0.65) times this, under the glow threshold.
    /// </summary>
    public void SetEmissionLevel(float level)
    {
        _buildingMaterial.SetShaderParameter("emission_level", level);
        _buildingFadeMaterial.SetShaderParameter("emission_level", level);
    }

    /// <summary>
    /// Moves the window / sprite cross-fade (building materials: averaged window glow fades out; building light sprites: fade in) and the street
    /// light pool fade. Called by <see cref="WorldView"/> when the live LOD edge between R2 and R1 changes (<see cref="WorldRenderSettings.BlendFollowsLod"/>).
    /// </summary>
    public void SetLodRanges(float blendStartM, float blendEndM, float poolStartM, float poolEndM)
    {
        foreach (var m in new[] { _buildingMaterial, _buildingFadeMaterial })
        {
            m.SetShaderParameter("avg_fade_start_m", blendStartM);
            m.SetShaderParameter("avg_fade_end_m", blendEndM);
        }
        _lightMaterial.SetShaderParameter("blend_start_m", blendStartM);
        _lightMaterial.SetShaderParameter("blend_end_m", blendEndM);
        _streetPoolMaterial.SetShaderParameter("fade_start_m", poolStartM);
        _streetPoolMaterial.SetShaderParameter("fade_end_m", poolEndM);
    }

    /// <summary>Moves the far fade of every sprite set (building, street, airport lights and beacons): fully faded at <paramref name="endM"/> (bands.max_distance_m).</summary>
    public void SetLightFar(float fadeStartM, float endM)
    {
        foreach (var m in new[] { _lightMaterial, _streetLightMaterial, _airportLightMaterial, _beaconMaterial })
        {
            m.SetShaderParameter("fade_start_m", fadeStartM);
            m.SetShaderParameter("far_end_m", endM);
        }
    }

    /// <summary>The environment's depth fog curve on the light sprites, which are additive and fog themselves (fog_disabled).</summary>
    public void SetLightFog(float beginM, float endM, float curve)
    {
        foreach (var m in new[] { _lightMaterial, _streetLightMaterial, _streetPoolMaterial, _beaconMaterial, _airportLightMaterial })
        {
            m.SetShaderParameter("fog_begin_m", beginM);
            m.SetShaderParameter("fog_end_m", endM);
            m.SetShaderParameter("fog_curve", curve);
        }
    }

    /// <summary>Street lamp height above the ground (<c>lights.street.height_m</c>): the light pools sit this far below each lamp.</summary>
    public void SetStreetLampHeight(float heightM) => _streetPoolMaterial.SetShaderParameter("lamp_height_m", heightM);

    /// <summary>Pixel footprint per metre of view depth on both sprite materials (see <c>px_per_depth</c> in light_sprite.gdshader).</summary>
    public void SetSpritePxPerDepth(float k)
    {
        _lightMaterial.SetShaderParameter("px_per_depth", k);
        _streetLightMaterial.SetShaderParameter("px_per_depth", k);
        _beaconMaterial.SetShaderParameter("px_per_depth", k);
        _airportLightMaterial.SetShaderParameter("px_per_depth", k);
    }

    /// <summary>
    /// Fallback shares of lit windows per profile id, used until a world profile with <c>lit_share</c> tables has been read (W4:
    /// <see cref="WorldView"/> replaces them with the interpolated hourly tables), e.g. with a library that has no profile.
    /// </summary>
    public static readonly float[] FallbackWindowShares = { 0.40f, 0.30f, 0.60f, 0f, 0.30f, 0f, 0f, 0f };
    /// <summary>Fallback window colours per profile id (linear): residential 2700 K, office cool, retail white, industrial sodium, civic warm, unknown, landmark, dark.</summary>
    private static readonly Vector3[] DefaultProfileColors =
    {
        new(1.0f, 0.655f, 0.341f), new(0.86f, 0.9f, 1.0f), new(1.0f, 0.98f, 0.95f), new(1.0f, 0.55f, 0.15f),
        new(1.0f, 0.706f, 0.42f), new(1.0f, 0.8f, 0.55f), new(1.0f, 0.75f, 0.45f), new(0f, 0f, 0f),
    };

    /// <summary>
    /// Lit-window share (0..1) per light profile id 0..7 at the current hour, on both building materials (W4 calls this when the hour
    /// moves; <paramref name="shares"/> must have 8 entries). Ids without windows stay 0.
    /// </summary>
    public void SetWindowShares(float[] shares)
    {
        Variant a = shares; // PackedFloat32Array
        _buildingMaterial.SetShaderParameter("window_share", a);
        _buildingFadeMaterial.SetShaderParameter("window_share", a);
    }

    /// <summary>The <c>window_share</c> values currently on the plain (or the dithering) building material (tests: both must be equal).</summary>
    public float[] ReadWindowShares(bool fadeMaterial)
    {
        var v = (fadeMaterial ? _buildingFadeMaterial : _buildingMaterial).GetShaderParameter("window_share");
        return v.VariantType == Variant.Type.PackedFloat32Array ? v.AsFloat32Array() : System.Array.Empty<float>();
    }

    /// <summary>
    /// Landmark floodlight (profile 6): colour, strength (x <c>emission_level</c>, so the base blooms) and the height in metres over which
    /// it fades out up the wall, on both building materials. Values come from <c>lights.profiles.landmark</c> (W4).
    /// </summary>
    public void SetFloodlight(Vector3 color, float strength, float fadeHeightM)
    {
        foreach (var m in new[] { _buildingMaterial, _buildingFadeMaterial })
        {
            m.SetShaderParameter("floodlight_color", color);
            m.SetShaderParameter("floodlight_strength", strength);
            m.SetShaderParameter("floodlight_fade_height_m", fadeHeightM);
        }
    }

    /// <summary>Window colour (linear) per light profile id 0..7, on both building materials (8 entries).</summary>
    public void SetProfileColors(Vector3[] colors)
    {
        Variant a = colors; // PackedVector3Array
        _buildingMaterial.SetShaderParameter("profile_color", a);
        _buildingFadeMaterial.SetShaderParameter("profile_color", a);
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

    private static (MultiMeshInstance3D, MultiMesh) CreateSprites(string name, Mesh quad, ShaderMaterial material)
    {
        var mm = new MultiMesh
        {
            TransformFormat = MultiMesh.TransformFormatEnum.Transform3D,
            UseColors = true,
            Mesh = quad,
        };
        mm.InstanceCount = 0; // after format/colors are set
        var mmi = new MultiMeshInstance3D
        {
            Name = name,
            Multimesh = mm,
            // Also as override so the per-instance fade_mul uniform is found on the MultiMesh instance.
            MaterialOverride = material,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            Visible = false,
            // The billboard is expanded in the vertex shader; keep distant sprites out of any frustum-edge pop.
            ExtraCullMargin = 200f + CurveCullMargin,
        };
        return (mmi, mm);
    }

    private TileNodes Create()
    {
        var (mmi, mm) = CreateSprites("Lights", _lightMesh, _lightMaterial);
        var (smi, smm) = CreateSprites("StreetLights", _streetLightMesh, _streetLightMaterial);
        var (bmi, bmm) = CreateSprites("Beacons", _beaconMesh, _beaconMaterial);
        var (ami, amm) = CreateSprites("AirportLights", _airportLightMesh, _airportLightMaterial);
        var pools = new MultiMeshInstance3D
        {
            Name = "StreetPools",
            Multimesh = smm,
            MaterialOverride = _streetPoolMaterial,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            Visible = false,
            ExtraCullMargin = 50f + CurveCullMargin, // the disc reaches below and around the lamp's AABB
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
        root.AddChild(smi);
        root.AddChild(pools);
        root.AddChild(bmi);
        root.AddChild(ami);
        _container.AddChild(root);
        return new TileNodes
        {
            Root = root, MeshInstance = mi, Mesh = am, GroundInstance = gi, GroundMesh = gm, Lights = mmi, LightsMm = mm,
            StreetLights = smi, StreetLightsMm = smm, StreetPools = pools, Beacons = bmi, BeaconsMm = bmm,
            AirportLights = ami, AirportLightsMm = amm,
        };
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
        t.StreetLights.SetInstanceShaderParameter(PFadeMul, fade);
        t.StreetPools.SetInstanceShaderParameter(PFadeMul, fade);
        t.Beacons.SetInstanceShaderParameter(PFadeMul, fade);
        t.AirportLights.SetInstanceShaderParameter(PFadeMul, fade);
    }

    /// <summary>Back to the opaque materials (no discard) once a tile left its transition, or when it is released.</summary>
    public void EndFade(TileNodes t)
    {
        if (!t.FadeMaterialActive) return;
        t.MeshInstance.MaterialOverride = _buildingMaterial;
        t.GroundInstance.MaterialOverride = _groundMaterial;
        t.Lights.SetInstanceShaderParameter(PFadeMul, 1f);
        t.StreetLights.SetInstanceShaderParameter(PFadeMul, 1f);
        t.StreetPools.SetInstanceShaderParameter(PFadeMul, 1f);
        t.Beacons.SetInstanceShaderParameter(PFadeMul, 1f);
        t.AirportLights.SetInstanceShaderParameter(PFadeMul, 1f);
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
        t.StreetLightsMm.VisibleInstanceCount = 0;
        t.StreetLights.Visible = false;
        t.StreetPools.Visible = false;
        t.StreetLightCount = 0;
        t.BeaconsMm.VisibleInstanceCount = 0;
        t.Beacons.Visible = false;
        t.BeaconCount = 0;
        t.AirportLightsMm.VisibleInstanceCount = 0;
        t.AirportLights.Visible = false;
        t.AirportLightCount = 0;
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
    /// ArrayMesh. Null = no ground (empty tile; R3 before api 1.3): the instance is hidden. Visibility of the whole tile is the
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
        t.LightCount = lights.Length;
        t.Clustered = Fill(t.Lights, t.LightsMm, ref t.Buffer, lights, colors);
    }

    /// <summary>Fills the tile's street lamp MultiMesh (api 1.1), same layout as <see cref="SetLights"/>.</summary>
    public void SetStreetLights(TileNodes t, Vector3[] lights, Color[] colors)
    {
        t.StreetLightCount = lights.Length;
        Fill(t.StreetLights, t.StreetLightsMm, ref t.StreetBuffer, lights, colors);
        t.StreetPools.Visible = t.StreetLights.Visible;
    }

    /// <summary>Fills the tile's beacon MultiMesh (api 1.2); colour alpha is the blink phase, passed through untouched.</summary>
    public void SetBeacons(TileNodes t, Vector3[] lights, Color[] colors)
    {
        t.BeaconCount = lights.Length;
        Fill(t.Beacons, t.BeaconsMm, ref t.BeaconBuffer, lights, colors, rawAlpha: true);
    }

    /// <summary>
    /// Fills the tile's airport light MultiMesh (api 1.7). Rust sends the fixed colour (blue / white / green / red) and the intensity weight in the
    /// alpha (<c>seed.airport.lights.intensity</c>, 0.35); the sprite shader would read an alpha above 0.125 as a cluster, so the buffer carries
    /// alpha 0.125 (a single light) and the weight scales the colour instead (weight / reference weight, 1 at the default).
    /// </summary>
    public void SetAirportLights(TileNodes t, Vector3[] lights, Color[] colors)
    {
        t.AirportLightCount = lights.Length;
        Fill(t.AirportLights, t.AirportLightsMm, ref t.AirportBuffer, lights, colors, airportWeight: _airportReferenceWeight);
    }

    /// <summary>Uploads one sprite set; returns true when any light carries a cluster weight.</summary>
    private static bool Fill(MultiMeshInstance3D mmi, MultiMesh mm, ref float[] buffer, Vector3[] lights, Color[] colors, bool rawAlpha = false, float airportWeight = 0f)
    {
        int n = lights.Length;
        if (n == 0)
        {
            mm.VisibleInstanceCount = 0;
            mmi.Visible = false;
            return false;
        }

        int cap = (n + CapacityStep - 1) / CapacityStep * CapacityStep;
        if (mm.InstanceCount != cap) mm.InstanceCount = cap;
        int len = cap * FloatsPerInstance;
        if (buffer.Length != len) buffer = new float[len];

        var buf = buffer;
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
                if (airportWeight > 0f)
                {
                    float k = c.A / airportWeight;
                    buf[o + 12] = c.R * k; buf[o + 13] = c.G * k; buf[o + 14] = c.B * k; buf[o + 15] = 0.125f;
                    continue;
                }
                float a = DebugLightAlpha >= 0f && !rawAlpha ? DebugLightAlpha : c.A;
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
        mmi.Visible = true;
        return clustered;
    }
}
