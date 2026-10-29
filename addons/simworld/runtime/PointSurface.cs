#nullable enable
using Godot;
using Godot.Collections;

namespace SimWorld;

/// <summary>
/// Builds the engine surface (contract section 4a: the shape <c>RenderingServer.mesh_add_surface</c> and ArrayMesh's <c>_surfaces</c> property take) of a points mesh from
/// <see cref="PointBuffers"/> bytes: PRIMITIVE_POINTS, VERTEX (3 x f32 in <c>vertex_data</c>) | COLOR (4 x u8) | CUSTOM0 (RGBA8 unorm, 4 x u8), the last two interleaved in
/// <c>attribute_data</c>. The bytes are the ones Godot's own array packing writes for the same arrays (checked against <c>RenderingServer.mesh_get_surface</c> of a mesh built
/// with <c>add_surface_from_arrays</c>), so the point shader and the culling are unchanged; the host skips the array conversion.
/// <para>An instance per caller (like <see cref="MeshSurfaces"/>): the surface dictionary and the one-element array are reused, and the two packed byte arrays are released as soon as
/// the mesh has copied them, so no finalizable wrapper and no native bytes wait for the next collection (with the collector paced, that could be minutes).</para>
/// </summary>
internal sealed class PointSurface
{
    private const long FormatVersion2 = 1L << 35, FormatVertex = 1, FormatColor = 8, FormatCustom0 = 64;
    private const long Format = FormatVersion2 | FormatVertex | FormatColor | FormatCustom0; // CUSTOM0 type 0 = RGBA8_UNORM
    private static readonly StringName SurfacesProperty = new("_surfaces");
    private static readonly StringName KPrimitive = new("primitive"), KFormat = new("format"), KVertexCount = new("vertex_count"),
        KVertexData = new("vertex_data"), KAttributeData = new("attribute_data"), KAabb = new("aabb");

    private readonly Dictionary _surface = new()
    {
        [KPrimitive] = (long)Mesh.PrimitiveType.Points,
        [KFormat] = Format,
    };
    private readonly Array _holder = new();

    /// <summary>Replaces the mesh's surfaces with the one built from <paramref name="b"/> (one call: clears the mesh, adds the surface, keeps the AABB right).</summary>
    public void Set(ArrayMesh mesh, PointBuffers b) => Set(mesh, b.PosBytes, b.AttrBytes, b.Count, b.Min, b.Max);

    /// <summary>The surface of <paramref name="count"/> points (above 0) with these bytes and this bounding box (made at least 1 mm thick per axis), set on <paramref name="mesh"/>.</summary>
    public void Set(ArrayMesh mesh, System.Span<byte> pos, System.Span<byte> attr, int count, Vector3 min, Vector3 max)
    {
        var size = (max - min).Max(new Vector3(1e-3f, 1e-3f, 1e-3f));
        _surface[KVertexCount] = (long)count;
        using (var v = Variant.CreateFrom(pos)) _surface[KVertexData] = v;
        using (var v = Variant.CreateFrom(attr)) _surface[KAttributeData] = v;
        _surface[KAabb] = new Aabb(min, size);
        using (var item = (Variant)_surface) _holder.Add(item);
        using (var holder = (Variant)_holder) mesh.Set(SurfacesProperty, holder);
        _holder.Clear();
        // The rendering server has its copy: drop ours now, not at the next collection.
        _surface[KVertexData] = default;
        _surface[KAttributeData] = default;
    }
}
