using System;

namespace SimWorld;

/// <summary>
/// Allocation-free helpers for tile keys. A key "z/x/y" is packed into a long so that parents, siblings and
/// children can be computed by arithmetic without building strings (z in bits 48+, x in 24..47, y in 0..23).
/// </summary>
public static class TileKey
{
    /// <summary>Coarsest zoom of the quadtree cover (R3).</summary>
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

    public static bool TryParse(string key, out long packed)
    {
        packed = 0;
        int a = key.IndexOf('/');
        if (a < 0) return false;
        int b = key.IndexOf('/', a + 1);
        if (b < 0) return false;
        if (!int.TryParse(key.AsSpan(0, a), out int z)
            || !int.TryParse(key.AsSpan(a + 1, b - a - 1), out int x)
            || !int.TryParse(key.AsSpan(b + 1), out int y)) return false;
        packed = Pack(z, x, y);
        return true;
    }

    public static string ToText(long k) => $"{Z(k)}/{X(k)}/{Y(k)}";
}
