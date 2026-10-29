#nullable enable
using Godot;

namespace SimWorld;

// The dither / fade side of the pool: rented material clones per fading tile.
public sealed partial class TilePool
{
    private static readonly StringName PFade = new("fade");
    private static readonly StringName PFadeParity = new("fade_parity");
    private static readonly StringName PFadeMul = new("fade_mul");

    /// <summary>
    /// Applies a poll_fades value (0..1, below 1) to a tile: gives each of its drawn nodes (building, ground and, unless its points are in a merged cell, point sprites and street pools) its own clone of
    /// the dithering variant (<see cref="FadeMaterials"/>) and writes fade / fade_parity (parity = z &amp; 1 of the key) into them as plain uniforms.
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
            t.MeshInstance.MaterialOverride = m[0];
            t.GroundInstance.MaterialOverride = m[1];
            // A tile whose points live in a merged cell has no sprite nodes of its own to scale: the cell's slot table carries its fade.
            if (t.CellMember == null)
            {
                m[2] = _sprites.PointFades.Rent();
                m[3] = _sprites.PoolFades.Rent();
                t.Points.MaterialOverride = m[2];
                t.Pools.MaterialOverride = m[3];
            }
            t.FadeMaterialActive = true;
        }
        m[0]!.SetShaderParameter(PFade, fade);
        m[1]!.SetShaderParameter(PFade, fade);
        m[2]?.SetShaderParameter(PFadeMul, fade);
        m[3]?.SetShaderParameter(PFadeMul, fade);
    }

    /// <summary>Nodes of a tile set that dither or scale with a fade: building, ground, point sprites, street pools.</summary>
    public const int FadeNodes = 4;

    /// <summary>Back to the opaque materials (no discard) once a tile left its transition, or when it is released; the clones go back to their pools.</summary>
    public void EndFade(TileNodes t)
    {
        if (!t.FadeMaterialActive) return;
        var m = t.FadeMats;
        t.MeshInstance.MaterialOverride = _buildingMaterial;
        t.GroundInstance.MaterialOverride = _groundMaterial;
        _buildingFades.Return(m[0]!);
        _groundFades.Return(m[1]!);
        if (m[2] != null)
        {
            t.Points.MaterialOverride = _sprites.Point;
            t.Pools.MaterialOverride = _sprites.Pool;
            _sprites.PointFades.Return(m[2]!);
            _sprites.PoolFades.Return(m[3]!);
        }
        for (int i = 0; i < FadeNodes; i++) m[i] = null;
        t.FadeMaterialActive = false;
    }

    /// <summary>
    /// Makes dither clones up front so the first swap bursts do not create them in the middle of a frame (a clone is a <c>Resource.Duplicate</c> of a material
    /// with its arrays and textures, tens of microseconds each, and a burst needs up to <c>fades.max_active</c> x 5 tiles x 4 nodes of them). Brings every kind of clone to
    /// <paramref name="tiles"/> in total, at most <paramref name="perCall"/> tiles' worth per call (the prewarm spreads it over its frames). True when done.
    /// </summary>
    public bool ReserveFadeClones(int tiles, int perCall)
    {
        int left = perCall;
        foreach (var f in FadeSets())
        {
            while (f.Clones < tiles && left > 0)
            {
                f.AddClone();
                left--;
            }
            if (f.Clones < tiles) return false;
        }
        return true;
    }

    private System.Collections.Generic.IEnumerable<FadeMaterials> FadeSets()
    {
        yield return _buildingFades;
        yield return _groundFades;
        yield return _sprites.PointFades;
        yield return _sprites.PoolFades;
    }
}
