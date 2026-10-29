using System.Buffers;
using Godot;

namespace SimWorld;

/// <summary>
/// The point sprites of one tile as the bytes of a mesh surface (<see cref="PointSurface"/>): <see cref="Pos"/> = 3 floats per point (12 bytes), <see cref="Attr"/> = per point
/// COLOR as 4 x u8 then CUSTOM0 as 4 x u8 (8 bytes), see <see cref="PointPacker"/>. The arrays only grow (a buffer that held a big tile serves the next one without allocating);
/// <see cref="Count"/> says how many points are valid. <see cref="Min"/> / <see cref="Max"/> = the box of the written positions. A buffer made with <c>pooled</c> (a merged tile's
/// cached points, one per tile for as long as the tile lives) rents its arrays from <see cref="ArrayPool{T}"/> and gives them back in <see cref="Release"/>, so the memory follows the
/// tiles instead of the largest tile each buffer ever held; the two scratch buffers (a tile's own mesh, a cell rebuild) keep their arrays.
/// </summary>
internal sealed class PointBuffers
{
    public const int PosStride = 12, AttrStride = 8;
    private readonly bool _pooled;

    public byte[] Pos = System.Array.Empty<byte>();
    public byte[] Attr = System.Array.Empty<byte>();
    public int Count;
    public Vector3 Min, Max;
    /// <summary>True when a building light carries a cluster weight (alpha above a single light's 0.125).</summary>
    public bool Clustered;

    public PointBuffers(bool pooled = false) { _pooled = pooled; }

    /// <summary>Makes room for <paramref name="points"/> points (contents are not kept) and empties the buffer.</summary>
    public void Reset(int points)
    {
        if (Pos.Length < points * PosStride)
        {
            Release();
            if (_pooled)
            {
                Pos = ArrayPool<byte>.Shared.Rent(points * PosStride);
                Attr = ArrayPool<byte>.Shared.Rent(points * AttrStride);
            }
            else
            {
                int cap = System.Math.Max(points + points / 4, 256);
                Pos = new byte[cap * PosStride];
                Attr = new byte[cap * AttrStride];
            }
        }
        Count = 0;
        Clustered = false;
        Min = new Vector3(float.MaxValue, float.MaxValue, float.MaxValue);
        Max = new Vector3(float.MinValue, float.MinValue, float.MinValue);
    }

    /// <summary>Gives a pooled buffer's arrays back (a scratch buffer keeps them).</summary>
    public void Release()
    {
        if (_pooled && Pos.Length > 0)
        {
            ArrayPool<byte>.Shared.Return(Pos);
            ArrayPool<byte>.Shared.Return(Attr);
            Pos = Attr = System.Array.Empty<byte>();
        }
        Count = 0;
    }

    /// <summary>The bytes this buffer holds, trimmed to <see cref="Count"/> points.</summary>
    public System.Span<byte> PosBytes => new(Pos, 0, Count * PosStride);
    public System.Span<byte> AttrBytes => new(Attr, 0, Count * AttrStride);
}
