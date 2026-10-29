using Godot;

namespace SimWorld;

// The point sprite look (shaders/light_sprite.gdshader, street_light_pool.gdshader): which exported values go to which material parameter.
public partial class WorldRenderSettings
{
    /// <summary>
    /// Rings (R0..R3) from this one up have their point sprites merged into one mesh per spatial cell (<see cref="SpriteCell"/>), so the far, many tiles cost a few draw calls instead of
    /// one each; finer rings keep a points mesh per tile (few tiles, their fades are per tile). 2 = R2 and R3, 4 = off (every tile its own mesh).
    /// </summary>
    [Export(PropertyHint.Range, "0,4,1")] public int SpriteCellMinRing { get; set; } = 2;
    /// <summary>Zoom of the cell that merges R2 tiles (z14) and R3 tiles (z13): a tile of that zoom, 10..14 (at most 3 below the tile's zoom: the cell's fade table has 64 slots). Bigger cells = fewer draws but more points rebuilt when one tile joins and drawn while the cell is partly in view.</summary>
    [Export(PropertyHint.Range, "10,14,1")] public int SpriteCellZoomR2 { get; set; } = 12;
    [Export(PropertyHint.Range, "10,13,1")] public int SpriteCellZoomR3 { get; set; } = 11;
    /// <summary>A cell whose mesh still holds the points of tiles that left is rebuilt once those dead points are this share of it (they are drawn collapsed until then).</summary>
    [Export(PropertyHint.Range, "0.05,1,0.05")] public float SpriteCellCompactShare { get; set; } = 0.3f;

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
