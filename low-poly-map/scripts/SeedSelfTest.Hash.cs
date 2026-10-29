using Godot;
using SimWorld;

namespace LowPolyMap;

// The frame-independent hash of a tile's point sprites (SEED HASHES).
public partial class SeedSelfTest
{
    /// <summary>
    /// Hashes the lights of a tile: position (relative to the tile), colour, brightness scale, kind and index within the kind. A tile's points come from its own mesh (R0 / R1) or from its
    /// member of a merged cell (R2 / R3: bytes relative to the cell, the cell offset and the slot bits are taken out so the hash does not depend on arrival order).
    /// </summary>
    private static ulong HashSprites(ulong h, TileNodes n)
    {
        h = Mix(h, (ulong)n.SpriteCount);
        if (n.SpriteCount == 0) return h;
        if (n.CellMember is { } m)
        {
            var d = m.Data;
            var off = n.Root.Position - m.Cell.Origin;
            for (int i = 0; i < d.Count; i++)
            {
                h = Mix(h, Q(System.BitConverter.ToSingle(d.Pos, i * 12) - off.X));
                h = Mix(h, Q(System.BitConverter.ToSingle(d.Pos, i * 12 + 4) - off.Y));
                h = Mix(h, Q(System.BitConverter.ToSingle(d.Pos, i * 12 + 8) - off.Z));
                int o = i * 8;
                h = Mix(h, ((ulong)d.Attr[o] << 24) | ((ulong)d.Attr[o + 1] << 16) | ((ulong)d.Attr[o + 2] << 8) | d.Attr[o + 3]);
                h = Mix(h, d.Attr[o + 4]); h = Mix(h, (ulong)(d.Attr[o + 5] & 3)); h = Mix(h, d.Attr[o + 6]); h = Mix(h, d.Attr[o + 7]);
            }
            return h;
        }
        if (n.PointsMesh.GetSurfaceCount() == 0) return h;
        var arrays = n.PointsMesh.SurfaceGetArrays(0);
        var pos = arrays[(int)Mesh.ArrayType.Vertex].AsVector3Array();
        var col = arrays[(int)Mesh.ArrayType.Color].AsColorArray();
        var custom = arrays[(int)Mesh.ArrayType.Custom0].AsByteArray();
        for (int i = 0; i < n.SpriteCount && i < pos.Length; i++)
        {
            h = Mix(h, Q(pos[i].X)); h = Mix(h, Q(pos[i].Y)); h = Mix(h, Q(pos[i].Z));
            h = Mix(h, (ulong)col[i].ToRgba32());
            for (int k = 0; k < 4; k++) h = Mix(h, custom[i * 4 + k]);
        }
        return h;
    }
}
