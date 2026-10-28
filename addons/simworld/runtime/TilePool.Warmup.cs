using Godot;

namespace SimWorld;

/// <summary>Which mesh layout a material draws (the vertex format is part of the pipeline the engine compiles): building (VERTEX NORMAL COLOR UV), ground (VERTEX NORMAL COLOR) or a billboard sprite quad from a MultiMesh with colours.</summary>
public enum WarmupKind { Building, Ground, Sprite }

/// <summary>A material the pool draws with, and the layout it is drawn with.</summary>
public readonly record struct WarmupMaterial(Material Material, WarmupKind Kind);

// Every material a tile can be drawn with, for MaterialPrewarm.
public sealed partial class TilePool
{
    /// <summary>
    /// The plain and the dithering variant (<see cref="FadeMaterials.Template"/>) of the building, ground and five sprite materials: the 14 shader
    /// permutations a tile meets, the dither ones only on its first fade.
    /// </summary>
    public WarmupMaterial[] WarmupMaterials() => new WarmupMaterial[]
    {
        new(_buildingMaterial, WarmupKind.Building), new(_buildingFades.Template, WarmupKind.Building),
        new(_groundMaterial, WarmupKind.Ground), new(_groundFades.Template, WarmupKind.Ground),
        new(_lightMaterial, WarmupKind.Sprite), new(_lightFades.Template, WarmupKind.Sprite),
        new(_streetLightMaterial, WarmupKind.Sprite), new(_streetFades.Template, WarmupKind.Sprite),
        new(_streetPoolMaterial, WarmupKind.Sprite), new(_poolFades.Template, WarmupKind.Sprite),
        new(_beaconMaterial, WarmupKind.Sprite), new(_beaconFades.Template, WarmupKind.Sprite),
        new(_airportLightMaterial, WarmupKind.Sprite), new(_airportFades.Template, WarmupKind.Sprite),
    };
}
