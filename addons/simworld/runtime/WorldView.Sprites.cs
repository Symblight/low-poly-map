#nullable enable
using Godot;

namespace SimWorld;

// The point sprites' side of the view: what the world profile says about them, and the street pool's fade range.
public partial class WorldView
{
    /// <summary>Reads the profile values the sprites and the fades need (after every configure): the street lamp height (api 1.1: the light pools sit this far below each lamp) and <c>fades.max_active</c> (how many dither clones the prewarm reserves).</summary>
    private void ApplySpriteProfile(Godot.Collections.Dictionary profile)
    {
        if (_pool == null) return;
        if (profile.TryGetValue("lights", out var lv) && lv.VariantType == Variant.Type.Dictionary
            && lv.AsGodotDictionary().TryGetValue("street", out var sv) && sv.VariantType == Variant.Type.Dictionary
            && sv.AsGodotDictionary().TryGetValue("height_m", out var hv))
            _pool.SetStreetLampHeight((float)hv.AsDouble());
        if (_prewarm != null && profile.TryGetValue("fades", out var fv) && fv.VariantType == Variant.Type.Dictionary
            && fv.AsGodotDictionary().TryGetValue("max_active", out var mv))
            _prewarm.ReserveTiles = (int)mv.AsDouble() * _settings.FadeReserveTilesPerSwap;
    }

    /// <summary>The band hysteresis of the world profile (<c>bands.hysteresis</c>, 1.15 by default): a tile stays in its ring until its distance is this much beyond the ring's edge.</summary>
    public float Hysteresis => _hysteresis;

    /// <summary>
    /// End of the street pool fade for the live R2 to R1 edge <paramref name="d"/>: the usual share of it, but never beyond d / hysteresis, where an R1 tile ends
    /// and a coarser one (which draws no pools) takes over; farther out a pool would only cost vertex work.
    /// </summary>
    private float PoolFadeEndM(double d) => (float)System.Math.Min(d * WorldRenderSettings.PoolEndShare, d / System.Math.Max(_hysteresis, 1f));
}
