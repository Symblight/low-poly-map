using Godot;

namespace SimWorld;

/// <summary>
/// The library's night look on an <see cref="Godot.Environment"/> the host passes in (docs/spec-night-rendering.md): bloom that
/// only catches emissive things, and a depth fog tuned so the 2.5..7 km blocks dissolve instead of reading as boxes.
/// - Glow threshold sits just below the emission of lit windows and light sprites at night
///   (<see cref="EmissionLevel"/> * <see cref="ThresholdBelowEmission"/>) and out of reach of sunlit surfaces by day
///   (<see cref="DayThreshold"/>); glow intensity rises with altitude so the light carpet blooms at cruise.
/// - Fog: depth fog plus a thin ground-haze height fog; with the terrain layer on, a day-time variant that reaches the far terrain bands.
/// What stays with the host: exposure and tone map, the sun or moon node, sky and ambient colors, the choice of environment.
/// Mobile-renderer features only. Call <see cref="ApplyStatic"/> once, then <see cref="Update"/> every frame (or whenever the clock or altitude changes).
/// </summary>
[GlobalClass]
public partial class NightLook : Resource
{
    // ---- bloom ----------------------------------------------------------------------------------
    /// <summary>
    /// HDR value (linear, before tone map) a lit window / sprite reaches. The glow threshold is derived from it; the building night
    /// shader (M6) must emit at least this much. Sprites emit 1.6 * color; roads do not glow.
    /// </summary>
    [ExportGroup("Bloom")]
    [Export] public float EmissionLevel { get; set; } = 1.0f;
    /// <summary>Glow threshold = EmissionLevel * this at night ("just below" the emission).</summary>
    [Export(PropertyHint.Range, "0.3,1,0.01")] public float ThresholdBelowEmission { get; set; } = 0.82f;
    /// <summary>Daytime threshold: above anything a sunlit surface or the sky reaches.</summary>
    [Export] public float DayThreshold { get; set; } = 3.0f;
    [Export] public float GlowIntensityGround { get; set; } = 0.4f;
    [Export] public float GlowIntensityCruise { get; set; } = 1.4f;
    [Export] public float GlowAltLowM { get; set; } = 300f;
    [Export] public float GlowAltHighM { get; set; } = 10000f;
    /// <summary>Glow threshold at cruise (night), reached over <see cref="GlowAltLowM"/>..<see cref="GlowAltHighM"/>: from altitude the distant,
    /// dimmed light carpet blooms too (the aerial-photo glow); low down the threshold stays just above the windows.</summary>
    [Export] public float ThresholdCruise { get; set; } = 0.35f;

    // ---- fog ------------------------------------------------------------------------------------
    /// <summary>Depth fog is pow(smoothstep(begin, end, slant distance), curve): ~5 % at the R1/R2 border (2.5 km), ~80 % at 7 km.</summary>
    [ExportGroup("Fog")]
    [Export] public float FogBeginM { get; set; } = 1500f;
    [Export] public float FogEndM { get; set; } = 9000f;
    [Export] public float FogCurve { get; set; } = 1.0f;
    /// <summary>Height fog (per fragment height, not per ray): fragments below this world y get 1 - exp(-(h - y) * density).</summary>
    [Export] public float HazeTopM { get; set; } = 150f;
    [Export] public float HazeDensity { get; set; } = 0.0006f;

    /// <summary>
    /// Day-time depth fog while the terrain layer runs (api 1.5; the fog above would hide everything past 9 km): fog begins at
    /// <see cref="TerrainDayFogBeginM"/> and is complete at <see cref="TerrainDayFogEndShare"/> x the terrain reach, so the far facets (bands T3 / T4,
    /// 25..130 km) read as haze. At night the fog above applies unchanged (night spec: far blocks dissolve, sprites fog by hand with the same curve);
    /// in between both are mixed by the night factor. Terrain off: <see cref="Update"/> leaves the fog exactly as <see cref="ApplyStatic"/> set it.
    /// </summary>
    [Export] public float TerrainDayFogBeginM { get; set; } = 2500f;
    [Export(PropertyHint.Range, "0.3,1,0.01")] public float TerrainDayFogEndShare { get; set; } = 0.95f;
    [Export] public float TerrainDayFogCurve { get; set; } = 1.7f;

    /// <summary>Values written by the last <see cref="Update"/>, for debug overlays.</summary>
    public float GlowThresholdNow { get; private set; }
    public float GlowIntensityNow { get; private set; }

    /// <summary>Everything that does not change per frame: bloom levels and the fog setup.</summary>
    public void ApplyStatic(Godot.Environment env)
    {
        // Bloom: half-resolution first level, three levels (1/2, 1/4, 1/8); no bloom term, so only the threshold decides.
        env.GlowEnabled = true;
        env.GlowNormalized = false;
        env.GlowBloom = 0f;
        env.GlowStrength = 1.0f;
        env.GlowBlendMode = Godot.Environment.GlowBlendModeEnum.Additive; // Softlight showed no visible bloom on Mobile
        env.GlowHdrScale = 0.45f;
        env.GlowHdrLuminanceCap = 12f;
        // SetGlowLevel is 0-based (glow_levels/1 == index 0); 7 levels exist.
        for (int i = 0; i < 7; i++) env.SetGlowLevel(i, 0f);
        env.SetGlowLevel(0, 0.6f); // 1/2 resolution
        env.SetGlowLevel(1, 1.0f); // 1/4
        env.SetGlowLevel(2, 0.8f); // 1/8
        env.SetGlowLevel(3, 0.5f); // 1/16: the wide haze around the light carpet

        env.FogEnabled = true;
        env.FogMode = Godot.Environment.FogModeEnum.Depth;
        env.FogDensity = 1.0f;
        env.FogDepthBegin = FogBeginM;
        env.FogDepthEnd = FogEndM;
        env.FogDepthCurve = FogCurve;
        env.FogHeight = HazeTopM;
        env.FogHeightDensity = HazeDensity;
        env.FogSkyAffect = 0f; // the host matches the sky horizon to the fog colour instead
        env.FogSunScatter = 0f;
        env.FogLightEnergy = 1f;
        // No depth of field: too heavy for the phone, pilots do not see it.
    }

    /// <summary>Per frame: glow threshold from the night factor and glow intensity from the altitude above ground.</summary>
    public void Update(Godot.Environment env, float nightFactor, float altitudeAgl, float terrainReachM = 0f)
    {
        if (terrainReachM > 0f)
        {
            float dayEnd = Mathf.Max(terrainReachM * TerrainDayFogEndShare, TerrainDayFogBeginM + 1000f);
            env.FogDepthBegin = Mathf.Lerp(TerrainDayFogBeginM, FogBeginM, nightFactor);
            env.FogDepthEnd = Mathf.Lerp(dayEnd, FogEndM, nightFactor);
            env.FogDepthCurve = Mathf.Lerp(TerrainDayFogCurve, FogCurve, nightFactor);
        }
        else
        {
            env.FogDepthBegin = FogBeginM;
            env.FogDepthEnd = FogEndM;
            env.FogDepthCurve = FogCurve;
        }
        float altK = Mathf.SmoothStep(GlowAltLowM, GlowAltHighM, altitudeAgl);
        float nightThreshold = Mathf.Lerp(EmissionLevel * ThresholdBelowEmission, EmissionLevel * ThresholdCruise, altK);
        GlowThresholdNow = Mathf.Lerp(DayThreshold, nightThreshold, nightFactor);
        GlowIntensityNow = Mathf.Lerp(GlowIntensityGround, GlowIntensityCruise, altK);
        env.GlowHdrThreshold = GlowThresholdNow;
        env.GlowIntensity = GlowIntensityNow;
    }
}
