#nullable enable
using Godot;

namespace SimWorld;

/// <summary>
/// The materials of a tile's point sprites: one for the sprites themselves (<c>light_sprite.gdshader</c>: building lights, street lamps, airport lights and
/// beacons, told apart per vertex) and one for the street lamp pools (<c>street_light_pool.gdshader</c>, a MultiMesh of quads under the lamps), each with its
/// dither variant for the tiles in a LOD fade (<see cref="FadeMaterials"/>), plus the variant of the points shader for the merged cells of R2 / R3 tiles
/// (<see cref="CellFades"/>: one clone per <see cref="SpriteCell"/>, the tiles' fades come from its slot table). Every parameter change goes to the plain material, the template and all clones.
/// </summary>
public sealed class SpriteMaterials
{
    /// <summary>The unit quad every pool MultiMesh draws (the pool shader lays it flat under the lamp).</summary>
    public Mesh PoolQuad { get; }
    public ShaderMaterial Point { get; }
    public ShaderMaterial Pool { get; }
    public FadeMaterials PointFades { get; }
    public FadeMaterials PoolFades { get; }
    /// <summary>The points shader compiled with <c>CELL</c> (tile slot and fade table in the vertex stage); a clone per merged cell.</summary>
    public FadeMaterials CellFades { get; }

    private readonly Vector4[] _gate;

    public SpriteMaterials(WorldRenderSettings settings, string shaders, string textures)
    {
        var dot = GD.Load<Texture2D>(textures + "/light_dot_32.png");
        var plain = GD.Load<Shader>(shaders + "/light_sprite.gdshader");
        ShaderMaterial MakePoint(Shader sh)
        {
            var m = new ShaderMaterial { Shader = sh };
            m.SetShaderParameter("dot_tex", dot);
            settings.ApplyToPoints(m);
            return m;
        }
        ShaderMaterial MakePool(Shader sh)
        {
            var m = new ShaderMaterial { Shader = sh };
            settings.ApplyToStreetPools(m);
            return m;
        }
        Point = MakePoint(plain);
        PointFades = new FadeMaterials(MakePoint(TilePool.LoadVariant(shaders + "/light_sprite.gdshader", "LOD_DITHER")));
        CellFades = new FadeMaterials(MakePoint(TilePool.LoadVariant(shaders + "/light_sprite.gdshader", "CELL")));
        string poolPath = shaders + "/street_light_pool.gdshader";
        Pool = MakePool(GD.Load<Shader>(poolPath));
        PoolFades = new FadeMaterials(MakePool(TilePool.LoadDitherVariant(poolPath)));
        PoolQuad = new QuadMesh { Size = Vector2.One, Material = Pool };
        _gate = settings.SpriteKindGate();
    }

    private void SetPoint(StringName name, Variant value)
    {
        Point.SetShaderParameter(name, value);
        PointFades.Set(name, value);
        CellFades.Set(name, value);
    }

    private void SetPool(StringName name, Variant value)
    {
        Pool.SetShaderParameter(name, value);
        PoolFades.Set(name, value);
    }

    /// <summary>
    /// Moves the window / sprite cross-fade of the building lights (<paramref name="blendStartM"/>..<paramref name="blendEndM"/> of view depth, the window shader's
    /// averaged glow fades over the same range) and the fade of the street pools.
    /// </summary>
    public void SetLodRanges(float blendStartM, float blendEndM, float poolStartM, float poolEndM)
    {
        _gate[(int)SpriteKind.Light].X = blendStartM;
        _gate[(int)SpriteKind.Light].Y = blendEndM;
        SetPoint("kind_gate", _gate);
        SetPool("fade_start_m", poolStartM);
        SetPool("fade_end_m", poolEndM);
    }

    /// <summary>The far fade of every sprite kind: fully faded at <paramref name="endM"/> (bands.max_distance_m).</summary>
    public void SetFar(float fadeStartM, float endM)
    {
        SetPoint("fade_start_m", fadeStartM);
        SetPoint("far_end_m", endM);
    }

    /// <summary>The environment's depth fog curve; the sprites are additive and apply it themselves (fog_disabled), the pools too.</summary>
    public void SetFog(float beginM, float endM, float curve)
    {
        SetPoint("fog_begin_m", beginM);
        SetPoint("fog_end_m", endM);
        SetPoint("fog_curve", curve);
        SetPool("fog_begin_m", beginM);
        SetPool("fog_end_m", endM);
        SetPool("fog_curve", curve);
    }

    /// <summary>Pixel footprint per metre of view depth (see <c>px_per_depth</c> in light_sprite.gdshader).</summary>
    public void SetPxPerDepth(float k) => SetPoint("px_per_depth", k);

    /// <summary>Street lamp height above the ground (<c>lights.street.height_m</c>): the pools sit this far below each lamp.</summary>
    public void SetLampHeight(float heightM) => SetPool("lamp_height_m", heightM);

    /// <summary>
    /// The cross-fade ranges as the plain materials hold them (tests): building light blend start / end, pool fade start / end, sprite far fade start / end.
    /// </summary>
    public float[] ReadRanges()
    {
        var gate = Point.GetShaderParameter("kind_gate").AsVector4Array();
        return new[]
        {
            gate[(int)SpriteKind.Light].X, gate[(int)SpriteKind.Light].Y,
            Pool.GetShaderParameter("fade_start_m").AsSingle(), Pool.GetShaderParameter("fade_end_m").AsSingle(),
            Point.GetShaderParameter("fade_start_m").AsSingle(), Point.GetShaderParameter("far_end_m").AsSingle(),
        };
    }
}
