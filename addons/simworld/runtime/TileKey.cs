using System;

namespace SimWorld;

/// <summary>
/// Allocation-free helpers for tile keys (both layers). A key "z/x/y" is packed into a long so that parents, siblings and
/// children can be computed by arithmetic without building strings (z in bits 48+, x in 24..47, y in 0..23).
/// </summary>
public static class TileKey
{
    /// <summary>Coarsest zoom of the vector layer's quadtree cover (R3). The terrain layer has its own root and leaf zoom
    /// (<see cref="TileCover.RootZ"/> / <see cref="TileCover.LeafZ"/>, from the profile's terrain.far_zoom / near_zoom).</summary>
    public const int RootZ = 13;
    /// <summary>Finest zoom of the quadtree cover (R0).</summary>
    public const int LeafZ = 16;

    public static long Pack(int z, int x, int y) => ((long)z << 48) | ((long)x << 24) | (long)y;
    public static int Z(long k) => (int)(k >> 48);
    public static int X(long k) => (int)((k >> 24) & 0xFFFFFF);
    public static int Y(long k) => (int)(k & 0xFFFFFF);
    public static int RingOf(int z) => LeafZ - z;

    public static long Parent(long k) => Pack(Z(k) - 1, X(k) >> 1, Y(k) >> 1);

    /// <summary>The ancestor at zoom <paramref name="z"/> (z must be &lt;= the key's zoom).</summary>
    public static long Ancestor(long k, int z)
    {
        int d = Z(k) - z;
        return Pack(z, X(k) >> d, Y(k) >> d);
    }

    /// <summary>Child q in NW, NE, SW, SE order.</summary>
    public static long Child(long k, int q) => Pack(Z(k) + 1, X(k) * 2 + (q & 1), Y(k) * 2 + (q >> 1));

    /// <summary>Sibling q (0..3) of the group that contains this key (may be the key itself).</summary>
    public static long Sibling(long k, int q) => Pack(Z(k), (X(k) & ~1) + (q & 1), (Y(k) & ~1) + (q >> 1));

    /// <summary>
    /// Parses "z/x/y" (vector layer) or "t/z/x/y" (terrain layer, api 1.5) without logging. <paramref name="terrain"/> tells the layer;
    /// the packed value is the same z/x/y packing for both (each layer has its own tile map, so the two never mix).
    /// </summary>
    public static bool TryParse(string key, out long packed, out bool terrain)
    {
        packed = 0;
        terrain = false;
        var span = key.AsSpan();
        if (span.Length > 2 && span[0] == 't' && span[1] == '/')
        {
            terrain = true;
            span = span.Slice(2);
        }
        int a = span.IndexOf('/');
        if (a < 0) return false;
        int b = span.Slice(a + 1).IndexOf('/');
        if (b < 0) return false;
        b += a + 1;
        if (!int.TryParse(span.Slice(0, a), out int z)
            || !int.TryParse(span.Slice(a + 1, b - a - 1), out int x)
            || !int.TryParse(span.Slice(b + 1), out int y)) return false;
        packed = Pack(z, x, y);
        return true;
    }

    /// <summary>Vector-layer keys only: false (silently) for a terrain key "t/z/x/y", which belongs to the terrain tile map.</summary>
    public static bool TryParse(string key, out long packed)
    {
        bool ok = TryParse(key, out packed, out bool terrain);
        if (terrain) { packed = 0; return false; }
        return ok;
    }

    public static string ToText(long k) => $"{Z(k)}/{X(k)}/{Y(k)}";

    /// <summary>Key text of the terrain layer: "t/z/x/y".</summary>
    public static string ToTerrainText(long k) => "t/" + ToText(k);
}
