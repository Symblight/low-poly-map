#nullable enable
using Godot;
using Godot.Collections;

namespace SimWorld;

/// <summary>
/// Puts a delivered mesh into a pooled <see cref="ArrayMesh"/>: either an engine surface (api 1.11, contract 4a: the vertex bytes were packed on the library's
/// worker threads, so this is a hand-over) or, for an older library or with <see cref="WorldRenderSettings.EngineSurfaces"/> off, a <c>Mesh.ARRAY_MAX</c> Array
/// that Godot packs here. Main thread only.
/// </summary>
public sealed class MeshSurfaces
{
    // ArrayMesh's own storage property: one call clears the mesh, adds the surface and keeps the surface list and the AABB right
    // (RenderingServer.MeshAddSurface on an ArrayMesh would leave both stale).
    private static readonly StringName SurfacesProperty = new("_surfaces");
    private static readonly StringName KIndexCount = new("index_count");
    private static readonly StringName KVertexCount = new("vertex_count");

    // The one-element Array handed to ArrayMesh's _surfaces property, reused for every tile of the owning pool (the mesh copies what it needs while the property is set).
    // An instance, not a static: a static wrapper would be finalized after the engine is gone.
    private readonly Array _holder = new();

    /// <summary>Replaces the mesh's single surface with <paramref name="surface"/> (an engine surface) or, when that is null, with <paramref name="arrays"/>. False when Godot rejected it (it prints its own error, without the tile).</summary>
    public bool Fill(ArrayMesh mesh, Array? arrays, Dictionary? surface)
    {
        if (surface != null)
        {
            using (var item = (Variant)surface) _holder.Add(item);
            using (var holder = (Variant)_holder) mesh.Set(SurfacesProperty, holder);
            _holder.Clear(); // drops the reference to the delivered vertex bytes
        }
        else if (arrays != null) mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays);
        return mesh.GetSurfaceCount() > 0;
    }

    /// <summary>Triangles of the mesh's only surface.</summary>
    public static int Triangles(ArrayMesh mesh)
    {
        int idx = mesh.SurfaceGetArrayIndexLen(0);
        return (idx > 0 ? idx : mesh.SurfaceGetArrayLen(0)) / 3;
    }

    /// <summary>One-line description of what was delivered, for the log of a rejected mesh.</summary>
    public static string Describe(Array? arrays, Dictionary? surface)
    {
        if (surface == null) return arrays == null ? "no mesh" : DescribeArrays(arrays);
        long v = surface.TryGetValue(KVertexCount, out var vc) ? vc.AsInt64() : -1;
        long i = surface.TryGetValue(KIndexCount, out var ic) ? ic.AsInt64() : -1;
        return $"engine surface vertices={v} indices={i}";
    }

    private static string DescribeArrays(Array a)
    {
        var sb = new System.Text.StringBuilder();
        for (int i = 0; i < a.Count; i++)
        {
            var v = a[i];
            if (v.VariantType == Variant.Type.Nil) continue;
            sb.Append(i).Append(':').Append(v.VariantType);
            if (v.VariantType == Variant.Type.PackedVector3Array) sb.Append('[').Append(v.AsVector3Array().Length).Append(']');
            else if (v.VariantType == Variant.Type.PackedInt32Array) sb.Append('[').Append(v.AsInt32Array().Length).Append(']');
            sb.Append(' ');
        }
        return sb.ToString();
    }
}
