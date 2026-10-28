using Godot;

namespace SimWorld;

// The point sprite look (shaders/light_sprite.gdshader, street_light_pool.gdshader): which exported values go to which material parameter.
public partial class WorldRenderSettings
{
    /// <summary>
    /// Per sprite kind (<see cref="SpriteKind"/> order: light, street, airport, beacon) the <c>kind_look</c> uniform of the point sprite shader:
    /// x = size in pixels close, y = size in pixels far, z = intensity, w = colour variation (building lights only; street lamps vary per road in Rust,
    /// airport lights have fixed colours).
    /// </summary>
    public Vector4[] SpriteKindLook() => new[]
    {
        new Vector4(LightPxMin, LightPxMax, LightIntensity, LightColorVariation),
        new Vector4(LightPxMin, LightPxMax, StreetLightIntensity, 0f),
        new Vector4(AirportLightPxMin, AirportLightPxMax, AirportLightIntensity, 0f),
        new Vector4(BeaconPxNear, BeaconPxFar, BeaconIntensity, 0f),
    };

    /// <summary>
    /// The <c>kind_gate</c> uniform: x / y = window cross-fade start / end over view depth, z / w = grazing-angle thinning range. Only building lights
    /// cross-fade with the windows (a range below zero keeps the factor at 1: street lamps and airport lights show at every distance); airport lights
    /// are not thinned at grazing angles either (an approach at 300 m AGL looks along the lights at a low angle). The beacon entry is unused.
    /// </summary>
    public Vector4[] SpriteKindGate() => new[]
    {
        new Vector4(WindowSpriteBlendStartM, WindowSpriteBlendEndM, LightGrazeMin, LightGrazeMax),
        new Vector4(-2f, -1f, LightGrazeMin, LightGrazeMax),
        new Vector4(-2f, -1f, -2f, -1.5f),
        new Vector4(-2f, -1f, -2f, -1.5f),
    };

    /// <summary>The point sprite material (one for the building lights, street lamps, airport lights and beacons of a tile).</summary>
    public void ApplyToPoints(ShaderMaterial m)
    {
        m.SetShaderParameter("kind_look", SpriteKindLook());
        m.SetShaderParameter("kind_gate", SpriteKindGate());
        m.SetShaderParameter("px_end_m", LightPxEndM);
        m.SetShaderParameter("far_end_m", LightFarEndM);
        m.SetShaderParameter("fade_start_m", LightFadeStartM);
        m.SetShaderParameter("cluster_bright_gain", LightClusterBrightGain);
        m.SetShaderParameter("cluster_world_m", LightClusterWorldM);
        m.SetShaderParameter("falloff_ref_m", LightFalloffRefM);
        m.SetShaderParameter("fog_light_keep", LightFogKeep);
        m.SetShaderParameter("lift_m", LightLiftM);
        m.SetShaderParameter("blur_start_m", LightBlurStartM);
        m.SetShaderParameter("blur_end_m", LightBlurEndM);
        m.SetShaderParameter("blur_px", LightBlurPx);
        m.SetShaderParameter("alt_thin_start_m", LightAltThinStartM);
        m.SetShaderParameter("alt_thin_end_m", LightAltThinEndM);
        m.SetShaderParameter("alt_keep_min", LightAltKeepMin);
        m.SetShaderParameter("cool_tint", new Vector3(LightCoolTint.R, LightCoolTint.G, LightCoolTint.B));
        m.SetShaderParameter("amber_tint", new Vector3(LightAmberTint.R, LightAmberTint.G, LightAmberTint.B));
        m.SetShaderParameter("cool_share", LightCoolShare);
        m.SetShaderParameter("amber_share", LightAmberShare);
        m.SetShaderParameter("brightness_jitter", LightBrightnessJitter);
        m.SetShaderParameter("blink_period_s", BeaconBlinkPeriodS);
        m.SetShaderParameter("blink_on_share", BeaconOnShare);
        m.SetShaderParameter("off_level", BeaconOffLevel);
    }

    /// <summary>The street lamp light pool material (the pool discs of R0 / R1 tiles).</summary>
    public void ApplyToStreetPools(ShaderMaterial m)
    {
        m.SetShaderParameter("radius_m", StreetPoolRadiusM);
        m.SetShaderParameter("intensity", StreetPoolIntensity);
        m.SetShaderParameter("fade_start_m", StreetPoolFadeStartM);
        m.SetShaderParameter("fade_end_m", StreetPoolFadeEndM);
    }
}
