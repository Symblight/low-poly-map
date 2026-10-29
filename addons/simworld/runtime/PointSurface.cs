using Godot;
using Godot.Collections;

namespace SimWorld;

/// <summary>
/// Builds the engine surface (contract section 4a: the shape <c>RenderingServer.mesh_add_surface</c> and ArrayMesh's <c>_surfaces</c> property take) of a points mesh from
/// <see cref="PointBuffers"/> bytes: PRIMITIVE_POINTS, VERTEX (3 x f32 in <c>vertex_data</c>) | COLOR (4 x u8) | CUSTOM0 (RGBA8 unorm, 4 x u8), the last two interleaved in
/// <c>attribute_data</c>. The bytes are the ones Godot's own array packing writes for the same arrays (checked against <c>RenderingServer.mesh_get_surface</c> of a mesh built
/// with <c>add_surface_from_arrays</c>), so the point shader and the culling are unchanged; the host skips the array conversion.
/// </summary>
internal static class PointSurface
{
    private const long FormatVersion2 = 1L << 35, FormatVertex = 1, FormatColor = 8, FormatCustom0 = 64;
    private const long Format = FormatVersion2 | FormatVertex | FormatColor | FormatCustom0; // CUSTOM0 type 0 = RGBA8_UNORM
    private static readonly StringName SurfacesProperty = new("_surfaces");
    private static readonly StringName KPrimitive = new("primitive"), KFormat = new("format"), KVertexCount = new("vertex_count"),
        KVertexData = new("vertex_data"), KAttributeData = new("attribute_data"), KAabb = new("aabb");

    /// <summary>The surface of <paramref name="count"/> points (above 0) with these bytes and this bounding box (made at least 1 mm thick per axis).</summary>
    public static Dictionary Make(System.Span<byte> pos, System.Span<byte> attr, int count, Vector3 min, Vector3 max)
    {
        var size = (max - min).Max(new Vector3(1e-3f, 1e-3f, 1e-3f));
        return new Dictionary
        {
            [KPrimitive] = (long)Mesh.PrimitiveType.Points,
            [KFormat] = Format,
            [KVertexCount] = (long)count,
            [KVertexData] = Variant.CreateFrom(pos),
            [KAttributeData] = Variant.CreateFrom(attr),
            [KAabb] = new Aabb(min, size),
        };
    }

    /// <summary>Replaces the mesh's surfaces with the one built from <paramref name="b"/> (one call: clears the mesh, adds the surface, keeps the AABB right).</summary>
    public static void Set(ArrayMesh mesh, PointBuffers b) => Set(mesh, b.PosBytes, b.AttrBytes, b.Count, b.Min, b.Max);

    public static void Set(ArrayMesh mesh, System.Span<byte> pos, System.Span<byte> attr, int count, Vector3 min, Vector3 max) =>
        mesh.Set(SurfacesProperty, new Array { Make(pos, attr, count, min, max) });
}
