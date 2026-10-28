using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Pool of tile node sets. Nodes stay in the tree (hidden) when released and are re-used, so
/// no QueueFree / new happens per tile once the pool has warmed up. The one exception is <see cref="Trim"/>: sets that stayed
/// idle in the free list for a whole window above a margin are destroyed a few per frame (<see cref="PoolTrimmer"/>).
/// Releasing is two steps (<see cref="Release"/> hides the set at once, <see cref="DrainRetired"/> frees its meshes and sprite buffers within a per-frame
/// budget), so a big poll_unload batch costs a few hundred microseconds a frame instead of one hitch; a retired set is never handed out before it is cleared.
/// </summary>
public sealed partial class TilePool
{
    private readonly Node3D _container;
    private readonly Stack<TileNodes> _free = new();
    // Released but not yet cleared (still holding meshes and sprite buffers); oldest first.
    private readonly RetireQueue<TileNodes> _retiring = new();
    private readonly PoolTrimmer _trimmer;
    private readonly System.Action<TileNodes> _destroy, _finishRetired;
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
    private readonly FadeMaterials _buildingFades, _groundFades;
    // The same for the five sprite sets (shader compiled with LOD_DITHER: plain uniform fade_mul instead of the constant 1).
    private readonly FadeMaterials _lightFades, _streetFades, _poolFades, _beaconFades, _airportFades;

    /// <summary>Debug (user arg --light-alpha=&lt;v&gt;): force every light's weight, ignoring Rust's alpha. Negative = off.</summary>
    public static float DebugLightAlpha { get; set; } = -1f;
    /// <summary>The earth-curvature drop (m) at the camera far plane, for helpers that are not per-tile (the runway markings). Tile sets size their own margin per tile (<see cref="CullMargins"/>, <see cref="SetCullMargin"/>).</summary>
    public static float CurveCullMargin { get; set; }

    /// <summary>Node sets that exist now (in use plus free); a trimmed set no longer counts.</summary>
    public int TotalCreated { get; private set; }
    /// <summary>Node sets destroyed by <see cref="Trim"/> since start; <c>TotalCreated + Trimmed</c> is every set ever allocated.</summary>
    public int Trimmed => _trimmer.Trimmed;
    /// <summary>Sets that can be handed out: cleared ones plus the retired ones waiting for <see cref="DrainRetired"/> (cleared on demand).</summary>
    public int FreeCount => _free.Count + _retiring.Count;
    /// <summary>Released sets whose meshes and sprite buffers are not freed yet.</summary>
    public int RetiringCount => _retiring.Count;
    public ShaderMaterial BuildingMaterial => _buildingMaterial;
    /// <summary>The configured dither variants (templates; fading tiles use clones, see <see cref="FadeMaterials"/>).</summary>
    public ShaderMaterial BuildingFadeMaterial => _buildingFades.Template;
    public ShaderMaterial GroundFadeMaterial => _groundFades.Template;
    public FadeMaterials BuildingFades => _buildingFades;
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
        _trimmer = new PoolTrimmer("vector tiles", settings);
        _destroy = Destroy;
        _finishRetired = FinishRetired;
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
        var buildingFade = new ShaderMaterial { Shader = LoadDitherVariant(shaders + "/building_windows.gdshader") };
        settings.ApplyToBuilding(_buildingMaterial);
        settings.ApplyToBuilding(buildingFade);
        _buildingFades = new FadeMaterials(buildingFade);
        SetWindowShares(FallbackWindowShares);
        SetProfileColors(DefaultProfileColors);

        // One shared material for every ground mesh (palette decoded from vertex COLOR).
        _groundMaterial = new ShaderMaterial { Shader = GD.Load<Shader>(shaders + "/ground.gdshader") };
        var groundFade = new ShaderMaterial { Shader = LoadDitherVariant(shaders + "/ground.gdshader") };
        settings.ApplyToGround(_groundMaterial);
        settings.ApplyToGround(groundFade);
        _groundFades = new FadeMaterials(groundFade);

        // Sprite fade variants, configured like their plain materials.
        var dot = _lightMaterial.GetShaderParameter("dot_tex");
        var lightVariant = LoadDitherVariant(shaders + "/light_sprite.gdshader");
        ShaderMaterial Sprite(Shader sh, System.Action<ShaderMaterial> apply, bool withDot = true)
        {
            var m = new ShaderMaterial { Shader = sh };
            if (withDot) m.SetShaderParameter("dot_tex", dot);
            apply(m);
            return m;
        }
        _lightFades = new FadeMaterials(Sprite(lightVariant, settings.ApplyToLights));
        _streetFades = new FadeMaterials(Sprite(lightVariant, settings.ApplyToStreetLights));
        _airportFades = new FadeMaterials(Sprite(lightVariant, settings.ApplyToAirportLights));
        _poolFades = new FadeMaterials(Sprite(LoadDitherVariant(shaders + "/street_light_pool.gdshader"), settings.ApplyToStreetPools, false));
        _beaconFades = new FadeMaterials(Sprite(LoadDitherVariant(shaders + "/beacon_light.gdshader"), settings.ApplyToBeacons));
    }

    /// <summary>
    /// Sets <c>emission_level</c> (the HDR level a lit window / sprite reaches, <see cref="NightLook.EmissionLevel"/>) on both building
    /// materials, which must never drift apart. Windows emit <c>window_emission</c> (0.65) times this, under the glow threshold.
    /// </summary>
    public void SetEmissionLevel(float level)
    {
        _buildingMaterial.SetShaderParameter("emission_level", level);
        _buildingFades.Set("emission_level", level);
    }

    /// <summary>
    /// Moves the window / sprite cross-fade (building materials: averaged window glow fades out; building light sprites: fade in) and the street
    /// light pool fade. Called by <see cref="WorldView"/> when the live LOD edge between R2 and R1 changes (<see cref="WorldRenderSettings.BlendFollowsLod"/>).
    /// </summary>
    public void SetLodRanges(float blendStartM, float blendEndM, float poolStartM, float poolEndM)
    {
        SetBuilding("avg_fade_start_m", blendStartM);
        SetBuilding("avg_fade_end_m", blendEndM);
        _lightMaterial.SetShaderParameter("blend_start_m", blendStartM);
        _lightMaterial.SetShaderParameter("blend_end_m", blendEndM);
        _lightFades.Set("blend_start_m", blendStartM);
        _lightFades.Set("blend_end_m", blendEndM);
        _streetPoolMaterial.SetShaderParameter("fade_start_m", poolStartM);
        _streetPoolMaterial.SetShaderParameter("fade_end_m", poolEndM);
        _poolFades.Set("fade_start_m", poolStartM);
        _poolFades.Set("fade_end_m", poolEndM);
    }

    /// <summary>Moves the far fade of every sprite set (building, street, airport lights and beacons): fully faded at <paramref name="endM"/> (bands.max_distance_m).</summary>
    public void SetLightFar(float fadeStartM, float endM)
    {
        foreach (var (m, f) in SpriteSets(false))
        {
            m.SetShaderParameter("fade_start_m", fadeStartM);
            m.SetShaderParameter("far_end_m", endM);
            f.Set("fade_start_m", fadeStartM);
            f.Set("far_end_m", endM);
        }
    }

    /// <summary>The environment's depth fog curve on the light sprites, which are additive and fog themselves (fog_disabled).</summary>
    public void SetLightFog(float beginM, float endM, float curve)
    {
        foreach (var (m, f) in SpriteSets(true))
        {
            m.SetShaderParameter("fog_begin_m", beginM);
            m.SetShaderParameter("fog_end_m", endM);
            m.SetShaderParameter("fog_curve", curve);
            f.Set("fog_begin_m", beginM);
            f.Set("fog_end_m", endM);
            f.Set("fog_curve", curve);
        }
    }

    /// <summary>Street lamp height above the ground (<c>lights.street.height_m</c>): the light pools sit this far below each lamp.</summary>
    public void SetStreetLampHeight(float heightM)
    {
        _streetPoolMaterial.SetShaderParameter("lamp_height_m", heightM);
        _poolFades.Set("lamp_height_m", heightM);
    }

    /// <summary>Each sprite material with its dither variants (the street pool disc only when <paramref name="withPool"/>: it has no light_sprite parameters).</summary>
    private (ShaderMaterial, FadeMaterials)[] SpriteSets(bool withPool) => withPool
        ? new[] { (_lightMaterial, _lightFades), (_streetLightMaterial, _streetFades), (_streetPoolMaterial, _poolFades), (_beaconMaterial, _beaconFades), (_airportLightMaterial, _airportFades) }
        : new[] { (_lightMaterial, _lightFades), (_streetLightMaterial, _streetFades), (_airportLightMaterial, _airportFades), (_beaconMaterial, _beaconFades) };

    /// <summary>A building shader parameter on the plain material and every dither clone.</summary>
    private void SetBuilding(StringName name, Variant value)
    {
        _buildingMaterial.SetShaderParameter(name, value);
        _buildingFades.Set(name, value);
    }

    /// <summary>Pixel footprint per metre of view depth on both sprite materials (see <c>px_per_depth</c> in light_sprite.gdshader).</summary>
    public void SetSpritePxPerDepth(float k)
    {
        foreach (var (m, f) in SpriteSets(false))
        {
            m.SetShaderParameter("px_per_depth", k);
            f.Set("px_per_depth", k);
        }
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
        SetBuilding("window_share", a);
    }

    /// <summary>The <c>window_share</c> values currently on the plain (or the dithering) building material (tests: both must be equal).</summary>
    public float[] ReadWindowShares(bool fadeMaterial)
    {
        var v = (fadeMaterial ? _buildingFades.Template : _buildingMaterial).GetShaderParameter("window_share");
        return v.VariantType == Variant.Type.PackedFloat32Array ? v.AsFloat32Array() : System.Array.Empty<float>();
    }

    /// <summary>
    /// Landmark floodlight (profile 6): colour, strength (x <c>emission_level</c>, so the base blooms) and the height in metres over which
    /// it fades out up the wall, on both building materials. Values come from <c>lights.profiles.landmark</c> (W4).
    /// </summary>
    public void SetFloodlight(Vector3 color, float strength, float fadeHeightM)
    {
        SetBuilding("floodlight_color", color);
        SetBuilding("floodlight_strength", strength);
        SetBuilding("floodlight_fade_height_m", fadeHeightM);
    }

    /// <summary>Window colour (linear) per light profile id 0..7, on both building materials (8 entries).</summary>
    public void SetProfileColors(Vector3[] colors)
    {
        Variant a = colors; // PackedVector3Array
        SetBuilding("profile_color", a);
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
            if (_retiring.TryDequeue(out t)) Clear(t); // never reuse a set before its release ran
            else
            {
                t = Create();
                TotalCreated++;
            }
        }
        ApplySprites(t);
        t.Key = key;
        t.Packed = packed;
        t.Ring = ring;
        // Only the finest band is close enough to profit from shadows (also keeps the draw-call count down).
        t.MeshInstance.CastShadow = ring == 0
            ? GeometryInstance3D.ShadowCastingSetting.On
            : GeometryInstance3D.ShadowCastingSetting.Off;
        return t;
    }

    /// <summary>Once per frame: destroys a few idle node sets when the free list has stayed above its margin (see <see cref="PoolTrimmer"/>). Returns how many.</summary>
    public int Trim(double delta)
    {
        int n = _trimmer.Step(delta, TotalCreated - FreeCount, _free, _destroy);
        TotalCreated -= n;
        return n;
    }

    private void Destroy(TileNodes t)
    {
        _container.RemoveChild(t.Root);
        t.Root.QueueFree();
    }

    /// <summary>
    /// Takes a tile out of play: hidden at once, fade materials returned, state reset, then queued for <see cref="DrainRetired"/>, which clears its meshes and
    /// sprite buffers later. The set is not handed out again before that (<see cref="Acquire"/> clears it on demand if the free list is empty).
    /// </summary>
    public void Release(TileNodes t)
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

    /// <summary>
    /// Clears retired sets (frees their meshes, empties their sprite sets) and returns them to the free list, until <paramref name="budgetUs"/> microseconds
    /// are spent; at least one per call when any waits, so the queue cannot stall. Returns how many were cleared.
    /// </summary>
    public int DrainRetired(long budgetUs) => _retiring.Drain(budgetUs, _finishRetired);

    private void FinishRetired(TileNodes t)
    {
        Clear(t);
        _free.Push(t);
    }

    private void Clear(TileNodes t)
    {
        ClearMesh(t);
        ClearGround(t);
        ClearLights(t);
    }

    /// <summary>Writes the earth-curvature cull margin (<see cref="CullMargins"/>) to the set's instances; the sprite sets keep their own padding on top.</summary>
    public void SetCullMargin(TileNodes t, float margin)
    {
        t.CullMargin = margin;
        t.MeshInstance.ExtraCullMargin = margin;
        t.GroundInstance.ExtraCullMargin = margin;
        t.Lights.ExtraCullMargin = margin + SpriteCullPad;
        t.StreetLights.ExtraCullMargin = margin + SpriteCullPad;
        t.StreetPools.ExtraCullMargin = margin + PoolCullPad;
        t.Beacons.ExtraCullMargin = margin + SpriteCullPad;
        t.AirportLights.ExtraCullMargin = margin + SpriteCullPad;
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
            ExtraCullMargin = PoolCullPad, // the disc reaches below and around the lamp's AABB; the curvature margin is added by SetCullMargin
        };
        var am = new ArrayMesh();
        var mi = new MeshInstance3D
        {
            Name = "Mesh",
            Mesh = am,
            MaterialOverride = _buildingMaterial,
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
            Visible = false,
        };
        var sprites = new Node3D { Name = "Sprites", Visible = _spritesOn };
        var root = new Node3D { Name = "Tile", Visible = false };
        root.AddChild(mi);
        root.AddChild(gi);
        sprites.AddChild(mmi);
        sprites.AddChild(smi);
        sprites.AddChild(pools);
        sprites.AddChild(bmi);
        sprites.AddChild(ami);
        root.AddChild(sprites);
        _container.AddChild(root);
        return new TileNodes
        {
            Root = root, Sprites = sprites, SpritesShown = _spritesOn, MeshInstance = mi, Mesh = am, GroundInstance = gi, GroundMesh = gm, Lights = mmi, LightsMm = mm,
            StreetLights = smi, StreetLightsMm = smm, StreetPools = pools, Beacons = bmi, BeaconsMm = bmm,
            AirportLights = ami, AirportLightsMm = amm,
        };
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

    /// <summary>
    /// Uploads <paramref name="arrays"/> (Mesh.ARRAY_MAX layout) into the tile's reused ArrayMesh on the
    /// calling (main) thread. Null = empty tile (no surface, instance hidden). Returns false when Godot rejected the arrays.
    /// </summary>
    public bool SetMesh(TileNodes t, Godot.Collections.Array? arrays)
    {
        ClearMesh(t);
        if (arrays == null) return true;
        t.Mesh.AddSurfaceFromArrays(Godot.Mesh.PrimitiveType.Triangles, arrays);
        if (t.Mesh.GetSurfaceCount() == 0) return false; // Godot rejected the arrays (it printed its own error, without the tile)
        int idx = t.Mesh.SurfaceGetArrayIndexLen(0);
        t.Tris = (idx > 0 ? idx : t.Mesh.SurfaceGetArrayLen(0)) / 3;
        t.HasMesh = true;
        t.MeshInstance.Visible = true;
        return true;
    }

    /// <summary>
    /// Uploads the ground mesh (Mesh.ARRAY_MAX layout: VERTEX, NORMAL, COLOR, INDEX) into the tile's reused ground
    /// ArrayMesh. Null = no ground (empty tile; R3 before api 1.3): the instance is hidden. Visibility of the whole tile is the
    /// root's, so ground swaps exactly with the buildings.
    /// </summary>
    public bool SetGround(TileNodes t, Godot.Collections.Array? arrays)
    {
        ClearGround(t);
        if (arrays == null) return true;
        t.GroundMesh.AddSurfaceFromArrays(Godot.Mesh.PrimitiveType.Triangles, arrays);
        if (t.GroundMesh.GetSurfaceCount() == 0) return false;
        int idx = t.GroundMesh.SurfaceGetArrayIndexLen(0);
        t.GroundTris = (idx > 0 ? idx : t.GroundMesh.SurfaceGetArrayLen(0)) / 3;
        t.HasGround = true;
        t.GroundInstance.Visible = true;
        return true;
    }
}
