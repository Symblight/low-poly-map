using Godot;

namespace SimWorld;

/// <summary>
/// Tuning of the addon's rendering side: upload budget, pooling and the material parameters of the building, ground and light
/// sprite shaders. A <c>.tres</c> of this resource goes into <see cref="WorldView.Settings"/>; without one the defaults below apply.
/// The material values are written when the view builds its pool, so change them before the view starts. The defaults equal the
/// shaders' own defaults. Streaming and meshing tuning is not here: that is the world profile (docs/contract.md 3.2).
/// </summary>
[GlobalClass]
public partial class WorldRenderSettings : Resource
{
    // ---- upload -------------------------------------------------------------------------------
    /// <summary>Microseconds per frame after which no further ready tile is polled (one big tile can still exceed it).</summary>
    [ExportGroup("Upload")]
    [Export] public long UploadBudgetUs { get; set; } = 2000;
    [Export] public int MaxTilesPerFrame { get; set; } = 2;
    /// <summary>Tile node sets created at start to avoid first-use hitches.</summary>
    [Export] public int PrewarmTiles { get; set; } = 24;
    /// <summary>Seconds between cover consistency checks (0 = never).</summary>
    [Export] public double VerifyIntervalS { get; set; } = 5.0;

    // ---- debug --------------------------------------------------------------------------------
    [ExportGroup("Debug")]
    /// <summary>Force every light sprite's weight (ignores Rust's alpha). Negative = off.</summary>
    [Export] public float DebugLightAlpha { get; set; } = -1f;
    /// <summary>Pretend poll_fades lists every R1 tile with this fade (checks the dither path without Rust). Negative = off.</summary>
    [Export] public float DebugFakeFade { get; set; } = -1f;

    // ---- building material (shaders/building_day.gdshader) -------------------------------------
    [ExportGroup("Buildings")]
    [Export(PropertyHint.Range, "0,1,0.01")] public float RoofDarken { get; set; } = 0.62f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float FloorLineStrength { get; set; } = 0.22f;
    [Export] public float FloorLineWidthM { get; set; } = 0.35f;
    [Export] public float FloorLineFadeStart { get; set; } = 150f;
    [Export] public float FloorLineFadeEnd { get; set; } = 450f;
    [Export(PropertyHint.Range, "0,0.5,0.01")] public float SeedVariation { get; set; } = 0.12f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float Roughness { get; set; } = 0.85f;

    // ---- ground material (shaders/ground.gdshader) --------------------------------------------
    [ExportGroup("Ground")]
    [Export(PropertyHint.Range, "0,1,0.01")] public float GroundDarken { get; set; } = 0.88f;
    [Export] public float GroundDepthBiasPerM { get; set; } = 0.00002f;
    [Export] public float GroundDepthBiasMaxM { get; set; } = 3f;

    // ---- light sprites (shaders/light_sprite.gdshader) ----------------------------------------
    [ExportGroup("Light sprites")]
    [Export] public float LightIntensity { get; set; } = 1.6f;
    [Export] public float LightNearPxMin { get; set; } = 2f;
    [Export] public float LightNearPxMax { get; set; } = 4f;
    [Export] public float LightFarPxMin { get; set; } = 6f;
    [Export] public float LightFarPxMax { get; set; } = 12f;
    [Export] public float LightSoftStartM { get; set; } = 6000f;
    [Export] public float LightNearEndM { get; set; } = 7000f;
    [Export] public float LightSoftEndM { get; set; } = 8000f;
    [Export] public float LightFarEndM { get; set; } = 25000f;
    [Export] public float LightFadeStartM { get; set; } = 21000f;
    [Export] public float LightClusterSizeGain { get; set; } = 1.2f;
    [Export] public float LightClusterBrightGain { get; set; } = 0.8f;
    [Export] public float LightClusterWorldM { get; set; } = 40f;

    public void ApplyToBuilding(ShaderMaterial m)
    {
        m.SetShaderParameter("roof_darken", RoofDarken);
        m.SetShaderParameter("floor_line_strength", FloorLineStrength);
        m.SetShaderParameter("floor_line_width_m", FloorLineWidthM);
        m.SetShaderParameter("floor_line_fade_start", FloorLineFadeStart);
        m.SetShaderParameter("floor_line_fade_end", FloorLineFadeEnd);
        m.SetShaderParameter("seed_variation", SeedVariation);
        m.SetShaderParameter("roughness_value", Roughness);
    }

    public void ApplyToGround(ShaderMaterial m)
    {
        m.SetShaderParameter("darken", GroundDarken);
        m.SetShaderParameter("depth_bias_per_m", GroundDepthBiasPerM);
        m.SetShaderParameter("depth_bias_max_m", GroundDepthBiasMaxM);
    }

    public void ApplyToLights(ShaderMaterial m)
    {
        m.SetShaderParameter("intensity", LightIntensity);
        m.SetShaderParameter("near_px_min", LightNearPxMin);
        m.SetShaderParameter("near_px_max", LightNearPxMax);
        m.SetShaderParameter("far_px_min", LightFarPxMin);
        m.SetShaderParameter("far_px_max", LightFarPxMax);
        m.SetShaderParameter("soft_start_m", LightSoftStartM);
        m.SetShaderParameter("near_end_m", LightNearEndM);
        m.SetShaderParameter("soft_end_m", LightSoftEndM);
        m.SetShaderParameter("far_end_m", LightFarEndM);
        m.SetShaderParameter("fade_start_m", LightFadeStartM);
        m.SetShaderParameter("cluster_size_gain", LightClusterSizeGain);
        m.SetShaderParameter("cluster_bright_gain", LightClusterBrightGain);
        m.SetShaderParameter("cluster_world_m", LightClusterWorldM);
    }
}
