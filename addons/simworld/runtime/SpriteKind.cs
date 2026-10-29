#nullable enable
namespace SimWorld;

/// <summary>
/// What a point sprite is. The value is <c>INSTANCE_CUSTOM.x</c> in <c>light_sprite.gdshader</c> (its <c>KIND_*</c> constants) and the index of the per-kind
/// uniform arrays <c>kind_look</c> / <c>kind_gate</c>. The kinds sit in a tile's MultiMesh in this order.
/// </summary>
public enum SpriteKind
{
    /// <summary>Building lights (api 0.x): windows seen from afar, cross-fading with the window shader.</summary>
    Light = 0,
    /// <summary>Street lamps (api 1.1); the only kind with a ground pool.</summary>
    Street = 1,
    /// <summary>Airport lights of a seed world (api 1.7): fixed colours, no cross-fade, no thinning.</summary>
    Airport = 2,
    /// <summary>Aircraft warning beacons (api 1.2): red / green, blinking.</summary>
    Beacon = 3,
}
