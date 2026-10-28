using System;

namespace SimWorld;

/// <summary>The layer a tile key belongs to: the vector world ("z/x/y"), the terrain ("t/z/x/y", api 1.5) or the landmarks ("l/z/x/y", api 1.11).</summary>
public enum TileLayer { World = 0, Terrain = 1, Landmarks = 2 }

/// <summary>
/// Allocation-free helpers for tile keys (all layers). A key "z/x/y" is packed into a long so that parents, siblings and
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
    /// Parses "z/x/y" (vector layer), "t/z/x/y" (terrain layer, api 1.5) or "l/z/x/y" (landmark layer, api 1.11) without logging.
    /// <paramref name="layer"/> tells the layer; the packed value is the same z/x/y packing for all three (each layer has its own tile map,
    /// so the maps never mix).
    /// </summary>
    public static bool TryParse(string key, out long packed, out TileLayer layer)
    {
        packed = 0;
        layer = TileLayer.World;
        var span = key.AsSpan();
        if (span.Length > 2 && span[1] == '/' && (span[0] == 't' || span[0] == 'l'))
        {
            layer = span[0] == 't' ? TileLayer.Terrain : TileLayer.Landmarks;
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

    /// <summary>Vector-layer keys only: false (silently) for a terrain key "t/z/x/y" or a landmark key "l/z/x/y", which belong to their own tile maps.</summary>
    public static bool TryParse(string key, out long packed)
    {
        bool ok = TryParse(key, out packed, out TileLayer layer);
        if (layer != TileLayer.World) { packed = 0; return false; }
        return ok;
    }

    public static string ToText(long k) => $"{Z(k)}/{X(k)}/{Y(k)}";

    /// <summary>Key text of the terrain layer: "t/z/x/y".</summary>
    public static string ToTerrainText(long k) => "t/" + ToText(k);

    /// <summary>Key text of the landmark layer: "l/z/x/y".</summary>
    public static string ToLandmarkText(long k) => "l/" + ToText(k);
}
