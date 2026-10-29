#nullable enable
using System.Runtime.InteropServices;
using Godot;

namespace SimWorld;

/// <summary>
/// Packs the point sprites of one tile into the bytes of a points surface (<see cref="PointBuffers"/>, <see cref="PointSurface"/>; <c>PRIMITIVE_POINTS</c>, one vertex per light,
/// <c>light_sprite.gdshader</c>): building lights, street lamps, airport lights and beacons in this order (<see cref="SpriteKind"/>). Per point: position (3 x f32) =
/// the light's position relative to the tile origin plus <c>offset</c> (a merged cell's points are relative to the cell, see <see cref="SpriteCell"/>), COLOR = the library's colour
/// (8 bits per channel, truncated like Godot's own packing), CUSTOM0 (RGBA8) = brightness scale x 32 (airport lights: Rust's intensity weight over the reference weight, else 1), kind
/// (in a cell's points also the tile's slot in the cell's fade table: <c>kind | slot &lt;&lt; 2</c>), and the light's index within its kind (low, high byte; it seeds the shader's
/// hashes, so the light pattern does not depend on how tiles are merged). Main thread only; one packer for the whole pool.
/// </summary>
internal sealed class PointPacker
{
    /// <summary>Alpha of a single light is clamp(sqrt(1)/8) = 0.125; anything clearly above is a cluster.</summary>
    private const float ClusterAlpha = 0.13f;
    private const byte NoScale = 32;
    private const float SingleLightAlpha = 0.125f;

    private readonly float _airportReferenceWeight;
    private PointBuffers _b = null!;
    private int _slotBits;
    private Vector3 _offset;

    /// <summary>Debug (user arg --light-alpha): force the weight of every building light and street lamp. Negative = off.</summary>
    public float DebugAlpha { get; set; } = -1f;

    public PointPacker(WorldRenderSettings settings)
    {
        _airportReferenceWeight = System.Math.Max(settings.AirportLightReferenceWeight, 0.001f);
    }

    /// <summary>
    /// Starts a tile of <paramref name="total"/> points (above 0) in <paramref name="into"/>. <paramref name="slot"/> below 0 = a tile's own mesh (the kind byte is the kind);
    /// 0..63 = a point of a merged cell (the kind byte also carries the slot). <paramref name="offset"/> is added to every position.
    /// </summary>
    public void Begin(PointBuffers into, int total, int slot, Vector3 offset)
    {
        _b = into;
        _b.Reset(total);
        _slotBits = slot < 0 ? 0 : slot << 2;
        _offset = offset;
    }

    /// <summary>
    /// Appends the lights of one kind. <paramref name="colors"/> carries one colour per point (rgb = colour, a = cluster weight; for a beacon the blink phase; for an airport light
    /// the intensity weight, which becomes the brightness scale because an alpha above 0.125 would read as a cluster); a short array is padded with warm white. Returns how many
    /// colours were missing.
    /// </summary>
    public int Add(SpriteKind kind, Vector3[] points, Color[] colors)
    {
        int n = points.Length, nc = colors.Length;
        bool beacon = kind == SpriteKind.Beacon, airport = kind == SpriteKind.Airport;
        var pos = MemoryMarshal.Cast<byte, float>(new System.Span<byte>(_b.Pos));
        var attr = new System.Span<byte>(_b.Attr);
        var min = _b.Min;
        var max = _b.Max;
        byte kindByte = (byte)((int)kind | _slotBits);
        for (int i = 0, k = _b.Count; i < n; i++, k++)
        {
            var p = points[i] + _offset;
            int po = k * 3;
            pos[po] = p.X; pos[po + 1] = p.Y; pos[po + 2] = p.Z;
            min = min.Min(p);
            max = max.Max(p);
            byte scale = NoScale;
            float r = 1f, g = 0.9f, b = 0.7f, a = SingleLightAlpha;
            if (i < nc)
            {
                var c = colors[i];
                r = c.R; g = c.G; b = c.B;
                if (airport)
                {
                    scale = (byte)Mathf.Clamp(Mathf.RoundToInt(c.A / _airportReferenceWeight * NoScale), 0, 255);
                }
                else
                {
                    a = DebugAlpha >= 0f && !beacon ? DebugAlpha : c.A;
                    if (kind == SpriteKind.Light && a > ClusterAlpha) _b.Clustered = true;
                }
            }
            int o = k * PointBuffers.AttrStride, idx = System.Math.Min(i, 65535);
            attr[o] = U8(r); attr[o + 1] = U8(g); attr[o + 2] = U8(b); attr[o + 3] = U8(a);
            attr[o + 4] = scale; attr[o + 5] = kindByte; attr[o + 6] = (byte)(idx & 255); attr[o + 7] = (byte)(idx >> 8);
        }
        _b.Count += n;
        _b.Min = min;
        _b.Max = max;
        return System.Math.Max(n - nc, 0);
    }

    /// <summary>Godot's own colour packing: trunc(clamp(c x 255, 0, 255)).</summary>
    private static byte U8(float c) => (byte)System.Math.Clamp((int)(c * 255.0), 0, 255);
}
