using Godot;
using Godot.Collections;

namespace SimWorld;

// Engine surfaces (api 1.11, contract 3.1 / 4 / 4a): the meshes come as ready vertex bytes instead of Mesh.ARRAY_MAX Arrays.
public sealed partial class WorldStreamerFacade
{
    /// <summary>First contract minor with the <c>engine_surfaces</c> option and the <c>mesh_surface</c> / <c>ground_surface</c> / <c>terrain_surface</c> keys.</summary>
    public const int EngineSurfacesMinApiMinor = 11;

    private static readonly StringName KOptEngineSurfaces = new("engine_surfaces");
    private static readonly StringName KMeshSurface = new("mesh_surface");
    private static readonly StringName KGroundSurface = new("ground_surface");
    private static readonly StringName KTerrainSurface = new("terrain_surface");

    /// <summary>True when the library delivers engine surfaces on request (api 1.11+).</summary>
    public bool HasEngineSurfaces => IsAvailable && KnownCompatible && ApiMinor >= EngineSurfacesMinApiMinor;

    /// <summary>Sets (or removes) the <c>engine_surfaces</c> option: true = "I read contract section 4a".</summary>
    public static void AddEngineSurfaces(Dictionary options, bool on)
    {
        if (on) options[KOptEngineSurfaces] = true;
        else options.Remove(KOptEngineSurfaces);
    }

    /// <summary>The engine surface under <paramref name="key"/>, or null when the entry has none (no geometry, or the option is off).</summary>
    private static Dictionary? Surface(Dictionary d, StringName key, ref string? problem)
    {
        if (!d.TryGetValue(key, out var v) || v.VariantType == Variant.Type.Nil) return null;
        if (v.VariantType != Variant.Type.Dictionary)
        {
            problem ??= $"'{key}' is a {v.VariantType}, expected a Dictionary";
            return null;
        }
        return v.AsGodotDictionary();
    }
}
