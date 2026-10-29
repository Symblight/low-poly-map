using Godot;

namespace SimWorld;

/// <summary>Which mesh layout a material draws (the vertex format is part of the pipeline the engine compiles): building (VERTEX NORMAL COLOR UV), ground (VERTEX NORMAL COLOR), points (VERTEX COLOR CUSTOM0 as PRIMITIVE_POINTS) or a quad from a MultiMesh with colours (the pools).</summary>
public enum WarmupKind { Building, Ground, Points, Pools }

/// <summary>A material the pool draws with, and the layout it is drawn with.</summary>
public readonly record struct WarmupMaterial(Material Material, WarmupKind Kind);

// Every material a tile can be drawn with, for MaterialPrewarm.
public sealed partial class TilePool
{
    /// <summary>
    /// The plain and the dithering variant (<see cref="FadeMaterials.Template"/>) of the building, ground, point sprite and street pool materials: the 9 shader
    /// permutations a tile meets, the dither ones only on its first fade.
    /// </summary>
    public WarmupMaterial[] WarmupMaterials() => new WarmupMaterial[]
    {
        new(_buildingMaterial, WarmupKind.Building), new(_buildingFades.Template, WarmupKind.Building),
        new(_groundMaterial, WarmupKind.Ground), new(_groundFades.Template, WarmupKind.Ground),
        new(_sprites.Point, WarmupKind.Points), new(_sprites.PointFades.Template, WarmupKind.Points), new(_sprites.CellFades.Template, WarmupKind.Points),
        new(_sprites.Pool, WarmupKind.Pools), new(_sprites.PoolFades.Template, WarmupKind.Pools),
    };
}
