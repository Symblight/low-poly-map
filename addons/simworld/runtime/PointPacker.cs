using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Packs the point sprites of one tile into the arrays of its points mesh (<c>PRIMITIVE_POINTS</c>, one vertex per light, <c>light_sprite.gdshader</c>): building lights, street
/// lamps, airport lights and beacons in this order (<see cref="SpriteKind"/>). VERTEX = the light's position relative to the tile origin, COLOR = the library's colour (8 bits per
/// channel once in the mesh), CUSTOM0 (RGBA8) = brightness scale x 32 (airport lights: Rust's intensity weight over the reference weight, else 1), kind, and the light's index within
/// its kind (low, high byte; it seeds the shader's hashes). The arrays are padded to a multiple of 256 vertices (1024 above 4096) so one staging set serves every tile of that size; a padding vertex
/// carries a kind the shader does not draw and the first light's position (so it does not widen the mesh's bounds). Main thread only; one packer for the whole pool.
/// </summary>
internal sealed class PointPacker
{
    public const int PadTo = 256, CoarsePadTo = 1024, CoarseAbove = 4096;
    /// <summary>Largest padded size whose staging arrays are kept (about 520 KB for 16384 lights; sizes above 4096 round to 1024, so there are at most 28 of them); bigger tiles get temporary ones.</summary>
    private const int CachedMaxPoints = 16384;
    /// <summary>Alpha of a single light is clamp(sqrt(1)/8) = 0.125; anything clearly above is a cluster.</summary>
    private const float ClusterAlpha = 0.13f;
    private const byte NoScale = 32, DeadKind = 255;

    private sealed class Staging
    {
        public readonly Vector3[] Pos;
        public readonly Color[] Col;
        public readonly byte[] Custom;
        public Staging(int n) { Pos = new Vector3[n]; Col = new Color[n]; Custom = new byte[n * 4]; }
    }

    private readonly Dictionary<int, Staging> _cache = new();
    private Godot.Collections.Array? _arrays;
    private readonly float _airportReferenceWeight;
    private Staging _s = null!;
    private int _n, _padded;

    /// <summary>True when any building light of the last tile carries a cluster weight.</summary>
    public bool Clustered { get; private set; }
    /// <summary>Debug (user arg --light-alpha): force the weight of every building light and street lamp. Negative = off.</summary>
    public float DebugAlpha { get; set; } = -1f;

    public PointPacker(WorldRenderSettings settings)
    {
        _airportReferenceWeight = System.Math.Max(settings.AirportLightReferenceWeight, 0.001f);
    }

    /// <summary>Starts a tile of <paramref name="total"/> lights (above 0).</summary>
    public void Begin(int total)
    {
        int step = total > CoarseAbove ? CoarsePadTo : PadTo;
        _padded = (total + step - 1) / step * step;
        if (_padded > CachedMaxPoints) _s = new Staging(_padded);
        else if (!_cache.TryGetValue(_padded, out _s!)) _cache[_padded] = _s = new Staging(_padded);
        _n = 0;
        Clustered = false;
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
        for (int i = 0; i < n; i++, _n++)
        {
            _s.Pos[_n] = points[i];
            byte scale = NoScale;
            if (i < nc)
            {
                var c = colors[i];
                if (airport)
                {
                    scale = (byte)Mathf.Clamp(Mathf.RoundToInt(c.A / _airportReferenceWeight * NoScale), 0, 255);
                    _s.Col[_n] = new Color(c.R, c.G, c.B, 0.125f);
                }
                else
                {
                    float a = DebugAlpha >= 0f && !beacon ? DebugAlpha : c.A;
                    _s.Col[_n] = new Color(c.R, c.G, c.B, a);
                    if (kind == SpriteKind.Light && a > ClusterAlpha) Clustered = true;
                }
            }
            else _s.Col[_n] = new Color(1f, 0.9f, 0.7f, 0.125f);
            int o = _n * 4, idx = System.Math.Min(i, 65535);
            _s.Custom[o] = scale; _s.Custom[o + 1] = (byte)kind; _s.Custom[o + 2] = (byte)(idx & 255); _s.Custom[o + 3] = (byte)(idx >> 8);
        }
        return System.Math.Max(n - nc, 0);
    }

    /// <summary>Pads the arrays and returns them in the layout of <c>ArrayMesh.AddSurfaceFromArrays</c> (valid until the next <see cref="Begin"/>).</summary>
    public Godot.Collections.Array Finish()
    {
        var first = _s.Pos[0];
        for (int i = _n; i < _padded; i++)
        {
            _s.Pos[i] = first;
            _s.Col[i] = default;
            int o = i * 4;
            _s.Custom[o] = 0; _s.Custom[o + 1] = DeadKind; _s.Custom[o + 2] = 0; _s.Custom[o + 3] = 0;
        }
        if (_arrays == null)
        {
            _arrays = new Godot.Collections.Array();
            _arrays.Resize((int)Mesh.ArrayType.Max);
        }
        _arrays[(int)Mesh.ArrayType.Vertex] = _s.Pos;
        _arrays[(int)Mesh.ArrayType.Color] = _s.Col;
        _arrays[(int)Mesh.ArrayType.Custom0] = _s.Custom;
        return _arrays;
    }
}
