#nullable enable
using System.Collections.Generic;
using Godot;

namespace SimWorld;

// Time of day: the shader globals, the lit-window shares, the window profile and the day / night switch of the light sprites.
public partial class WorldView
{
    /// <summary>0 = day, 1 = night, as last given to <see cref="SetTime"/>.</summary>
    public float NightFactor { get; private set; } = 1f;
    /// <summary>Local solar hour as last given to <see cref="SetTime"/>.</summary>
    public float Hour { get; private set; } = 12f;

    /// <summary>
    /// Time of day for the shaders: writes the global uniforms <c>simworld_hour</c> and <c>simworld_night_factor</c> (light sprites
    /// fade in with the night factor). The host owns the clock and calls this whenever it changes.
    /// </summary>
    public void SetTime(float localHour, float nightFactor)
    {
        Hour = localHour;
        NightFactor = nightFactor;
        ShaderGlobals.EnsureRegistered();
        RenderingServer.GlobalShaderParameterSet(ShaderGlobals.HourName, localHour);
        RenderingServer.GlobalShaderParameterSet(ShaderGlobals.NightFactorName, nightFactor);
        UpdateWindowShares(false);
        UpdateSpriteSwitch(nightFactor);
    }

    /// <summary>Night factor below which the light sprites are hidden, and above which they are shown again (hysteresis, so a clock near dusk does not toggle every tile each frame).</summary>
    public const float SpritesOffBelow = 0.02f, SpritesOnAbove = 0.05f;

    /// <summary>True while the point sprites (building lights, street lamps and their pools, beacons, airport lights) are shown. They scale with the night factor, so by day they are hidden: no draw calls, no additive black pixels.</summary>
    public bool SpritesVisible => _pool?.SpritesOn ?? true;
    /// <summary>Times the sprite switch changed state since start, and tiles touched by the last change (tests, overlay).</summary>
    public int SpriteSwitches { get; private set; }
    public int SpriteSwitchTiles { get; private set; }

    private void UpdateSpriteSwitch(float nightFactor)
    {
        if (_pool == null) return;
        bool on = _pool.SpritesOn ? nightFactor > SpritesOffBelow : nightFactor >= SpritesOnAbove;
        if (on == _pool.SpritesOn) return;
        SpriteSwitchTiles = _pool.SetSpritesVisible(on, SpriteHolders());
        SpriteSwitches++;
        GD.Print($"[WorldView] light sprites {(on ? "shown" : "hidden")} at night factor {nightFactor:0.000} ({SpriteSwitchTiles} tiles)");
    }

    // Every tile that holds sprites: the vector tiles and the landmark tiles (same pool).
    private IEnumerable<TileNodes> SpriteHolders()
    {
        foreach (var n in Cover.All) yield return n;
        foreach (var n in LandmarkCover.All) yield return n;
    }

    /// <summary>Hour change (hours, wrapping at 24) that triggers a new <c>window_share</c> upload.</summary>
    private const float WindowShareStepH = 0.01f;
    private readonly float[] _shares = new float[WindowProfile.ProfileCount];
    private float _sharesHour = float.NaN;
    private WindowProfile? _windowProfile;
    /// <summary>Lit-window share per light profile id last written to the building materials (8 entries, ids 3 5 6 7 are 0).</summary>
    public System.ReadOnlySpan<float> WindowShares => _shares;
    /// <summary>The <c>window_share</c> array as read back from the plain / dithering building material (tests).</summary>
    /// <summary>The cross-fade ranges now on the materials (tests): window average fade start / end (building material), then the sprite materials' (<see cref="SpriteMaterials.ReadRanges"/>): building light blend start / end, street pool fade start / end, sprite far fade start / end.</summary>
    public float[] ReadMaterialBlendRanges()
    {
        if (_pool == null) return System.Array.Empty<float>();
        var s = _pool.Sprites.ReadRanges();
        float F(string p) => _pool.BuildingMaterial.GetShaderParameter(p).AsSingle();
        return new[] { F("avg_fade_start_m"), F("avg_fade_end_m"), s[0], s[1], s[2], s[3], s[4], s[5] };
    }

    public float[] ReadMaterialWindowShares(bool fadeMaterial) => _pool?.ReadWindowShares(fadeMaterial) ?? System.Array.Empty<float>();
    /// <summary>True when the shares come from the world profile's lit_share tables (false = the TilePool fallback values).</summary>
    public bool WindowSharesFromProfile => _windowProfile != null;
    /// <summary>Number of <c>window_share</c> uploads so far (tests: it must not grow every frame).</summary>
    public int WindowShareUploads { get; private set; }

    /// <summary>
    /// Writes the lit-window share of every profile at the current <see cref="Hour"/> to both building materials: the profile's hourly
    /// table interpolated, or the pool's fallback values when no profile is readable. Skipped while the hour moved by less than
    /// 0.01 h (<paramref name="force"/> overrides, e.g. after the profile changed).
    /// </summary>
    private void UpdateWindowShares(bool force)
    {
        if (_pool == null) return;
        float d = Mathf.Abs(Hour - _sharesHour);
        d = Mathf.Min(d, 24f - d); // the clock wraps at 24 h
        if (!force && !float.IsNaN(_sharesHour) && d <= WindowShareStepH) return;
        _sharesHour = Hour;
        if (_windowProfile != null) _windowProfile.SharesAt(Hour, _shares);
        else System.Array.Copy(TilePool.FallbackWindowShares, _shares, _shares.Length);
        _pool.SetWindowShares(_shares);
        WindowShareUploads++;
    }

    /// <summary>Reads colours, floodlight and lit_share tables from the world profile and pushes them to the pool (after every configure).</summary>
    private void ApplyWindowProfile(Godot.Collections.Dictionary profile)
    {
        _windowProfile = WindowProfile.FromWorldProfile(profile);
        if (_pool == null) return;
        if (_windowProfile != null)
        {
            _pool.SetProfileColors(_windowProfile.Colors);
            _pool.SetFloodlight(_windowProfile.FloodlightColor, _windowProfile.FloodlightStrength, _windowProfile.FloodlightFadeHeightM);
            GD.Print($"[WorldView] window profile: lit_share tables for {CountTables(_windowProfile)} light profiles, floodlight strength {_windowProfile.FloodlightStrength:0.##} over {_windowProfile.FloodlightFadeHeightM:0.#} m");
        }
        else GD.PushWarning("[WorldView] world profile has no lights.profiles lit_share tables (library older than api 1.0?); using the built-in window shares and colours");
        UpdateWindowShares(true);
    }

    private static int CountTables(WindowProfile wp)
    {
        int n = 0;
        foreach (var t in wp.LitShare) if (t != null) n++;
        return n;
    }

    /// <summary>
    /// HDR level a lit window reaches (windows emit 0.65 of it, light sprites 1.6 x color): the host passes
    /// <see cref="NightLook.EmissionLevel"/> so the glow threshold (0.82 x EmissionLevel) stays above windows. Remembered if the pool does not exist yet.
    /// </summary>
    public void SetEmissionLevel(float level)
    {
        _emissionLevel = level;
        _pool?.SetEmissionLevel(level);
    }

    /// <summary>
    /// The environment's depth fog (<see cref="NightLook.FogBeginM"/> / <see cref="NightLook.FogEndM"/> / <see cref="NightLook.FogCurve"/>)
    /// for the light sprites, which are additive and apply it by hand. Remembered if the pool does not exist yet.
    /// </summary>
    public void SetLightFog(float beginM, float endM, float curve)
    {
        _lightFog = new Vector3(beginM, endM, curve);
        _pool?.SetLightFog(beginM, endM, curve);
    }
}
