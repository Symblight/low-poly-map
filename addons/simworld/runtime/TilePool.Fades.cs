using Godot;

namespace SimWorld;

// The dither / fade side of the pool: rented material clones per fading tile.
public sealed partial class TilePool
{
    private static readonly StringName PFade = new("fade");
    private static readonly StringName PFadeParity = new("fade_parity");
    private static readonly StringName PFadeMul = new("fade_mul");

    /// <summary>
    /// Applies a poll_fades value (0..1, below 1) to a tile: gives each of its seven drawn nodes (building, ground, five sprite sets) its own clone of the
    /// dithering variant (<see cref="FadeMaterials"/>) and writes fade / fade_parity (parity = z &amp; 1 of the key) into them as plain uniforms.
    /// The light sprites never dither; their brightness is scaled by the same value instead (<c>fade_mul</c>), so the two additive sides of a swap
    /// sum to constant brightness (no double-bright flash while R3/R2 lights overlap).
    /// </summary>
    public void SetFade(TileNodes t, float fade)
    {
        var m = t.FadeMats;
        if (!t.FadeMaterialActive)
        {
            float parity = TileKey.Z(t.Packed) & 1;
            m[0] = _buildingFades.Rent();
            m[1] = _groundFades.Rent();
            m[0]!.SetShaderParameter(PFadeParity, parity);
            m[1]!.SetShaderParameter(PFadeParity, parity);
            m[2] = _lightFades.Rent();
            m[3] = _streetFades.Rent();
            m[4] = _poolFades.Rent();
            m[5] = _beaconFades.Rent();
            m[6] = _airportFades.Rent();
            t.MeshInstance.MaterialOverride = m[0];
            t.GroundInstance.MaterialOverride = m[1];
            t.Lights.MaterialOverride = m[2];
            t.StreetLights.MaterialOverride = m[3];
            t.StreetPools.MaterialOverride = m[4];
            t.Beacons.MaterialOverride = m[5];
            t.AirportLights.MaterialOverride = m[6];
            t.FadeMaterialActive = true;
        }
        m[0]!.SetShaderParameter(PFade, fade);
        m[1]!.SetShaderParameter(PFade, fade);
        for (int i = 2; i < FadeNodes; i++) m[i]!.SetShaderParameter(PFadeMul, fade);
    }

    /// <summary>Nodes of a tile set that dither or scale with a fade: building, ground, lights, street lamps, street pools, beacons, airport lights.</summary>
    public const int FadeNodes = 7;

    /// <summary>Back to the opaque materials (no discard) once a tile left its transition, or when it is released; the clones go back to their pools.</summary>
    public void EndFade(TileNodes t)
    {
        if (!t.FadeMaterialActive) return;
        var m = t.FadeMats;
        t.MeshInstance.MaterialOverride = _buildingMaterial;
        t.GroundInstance.MaterialOverride = _groundMaterial;
        t.Lights.MaterialOverride = _lightMaterial;
        t.StreetLights.MaterialOverride = _streetLightMaterial;
        t.StreetPools.MaterialOverride = _streetPoolMaterial;
        t.Beacons.MaterialOverride = _beaconMaterial;
        t.AirportLights.MaterialOverride = _airportLightMaterial;
        _buildingFades.Return(m[0]!);
        _groundFades.Return(m[1]!);
        _lightFades.Return(m[2]!);
        _streetFades.Return(m[3]!);
        _poolFades.Return(m[4]!);
        _beaconFades.Return(m[5]!);
        _airportFades.Return(m[6]!);
        for (int i = 0; i < FadeNodes; i++) m[i] = null;
        t.FadeMaterialActive = false;
    }
}
