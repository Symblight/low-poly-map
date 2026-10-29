using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Fills the street pool MultiMesh of a tile: one instance per street lamp (identity basis at the lamp, COLOR = the lamp colour; 16 floats), only for tiles that can
/// show pools (a few dozen R0 / R1 tiles). The MultiMesh keeps its capacity while smaller tiles reuse it (<c>InstanceCount</c> only grows, in steps of 256 sprites, since
/// each change reallocates the GPU buffer and zero-fills it), <c>VisibleInstanceCount</c> says how many lamps the tile has, and <see cref="MultiMesh.CustomAabb"/> is the box
/// of the lamps so the engine never scans the instances for it. Main thread only; one writer for the whole pool.
/// </summary>
internal sealed class PoolWriter
{
    private const int Stride = 16, Step = 256;
    /// <summary>Capacities up to this many instances keep a cached staging array (it must match the MultiMesh's whole buffer).</summary>
    private const int CachedMaxInstances = 4096;
    private const float BoundsPadM = 1f;

    private readonly Dictionary<int, float[]> _staging = new();

    /// <summary>Writes <paramref name="points"/> (above 0) into <paramref name="mm"/>; a short colour array is padded with warm white.</summary>
    public void Fill(MultiMesh mm, Vector3[] points, Color[] colors)
    {
        int n = points.Length, nc = colors.Length;
        int need = (n + Step - 1) / Step * Step, cap = mm.InstanceCount;
        if (cap < need)
        {
            cap = (System.Math.Max(need, cap + cap / 4) + Step - 1) / Step * Step;
            mm.InstanceCount = cap;
        }
        var buf = Staging(cap);
        var min = new Vector3(float.MaxValue, float.MaxValue, float.MaxValue);
        var max = new Vector3(float.MinValue, float.MinValue, float.MinValue);
        for (int i = 0, o = 0; i < n; i++, o += Stride)
        {
            var p = points[i];
            buf[o + 0] = 1f; buf[o + 1] = 0; buf[o + 2] = 0; buf[o + 3] = p.X;
            buf[o + 4] = 0; buf[o + 5] = 1f; buf[o + 6] = 0; buf[o + 7] = p.Y;
            buf[o + 8] = 0; buf[o + 9] = 0; buf[o + 10] = 1f; buf[o + 11] = p.Z;
            if (i < nc) { var c = colors[i]; buf[o + 12] = c.R; buf[o + 13] = c.G; buf[o + 14] = c.B; buf[o + 15] = c.A; }
            else { buf[o + 12] = 1; buf[o + 13] = 0.9f; buf[o + 14] = 0.7f; buf[o + 15] = 0.125f; }
            min = min.Min(p); max = max.Max(p);
        }
        mm.Buffer = buf;
        mm.VisibleInstanceCount = n;
        var pad = new Vector3(BoundsPadM, BoundsPadM, BoundsPadM);
        mm.CustomAabb = new Aabb(min - pad, max - min + 2f * pad);
    }

    private float[] Staging(int capacity)
    {
        if (capacity > CachedMaxInstances) return new float[capacity * Stride];
        if (!_staging.TryGetValue(capacity, out var a)) _staging[capacity] = a = new float[capacity * Stride];
        return a;
    }
}
