#nullable enable
using Godot;

namespace SimWorld;

/// <summary>
/// Frustum-cull margin of one tile. The visual earth curvature (<c>earth_curve.gdshaderinc</c>) lowers every vertex by d^2 / 2R below the flat AABB the
/// engine culls with, so a tile must be culled with a box grown by the drop at its far edge. A margin sized for the farthest tile of the layer (the camera
/// far plane squared over 2R: 80 m for the vector layer, 1.8 km for the terrain) keeps near tiles in the frustum that are not visible; this sizes it per
/// tile from its distance to the camera instead: near tiles get the floor, the farthest ones what they need. The distance changes as the camera flies, so
/// the view refreshes every tile a few times a second (<see cref="WorldView"/>) and the margin carries <see cref="SlackM"/> for what moves in between.
/// </summary>
public static class CullMargins
{
    /// <summary>Distance the tile can get farther in the time between two refreshes (a quarter of a second at 250 m/s is 63 m; a margin of a few seconds of flight is generous).</summary>
    public const float SlackM = 600f;
    /// <summary>The margin never goes below this (m); it also keeps the writes rare for near tiles.</summary>
    public const float MinMarginM = 0.5f;
    private const double EquatorM = 40075016.686;

    /// <summary>Half the diagonal of a Web Mercator tile of zoom <paramref name="z"/> at latitude <paramref name="latDeg"/>, in ground metres, padded by 25 % (the frame is a tangent plane at the origin).</summary>
    public static float HalfSpanM(int z, double latDeg)
    {
        double width = EquatorM * System.Math.Cos(latDeg * System.Math.PI / 180.0) / (1L << z);
        return (float)(width * 0.7071 * 1.25);
    }

    /// <summary>
    /// Decides whether a tile's margin must be written. The margin <paramref name="written"/> (NaN = never) is kept while it still covers the drop at the
    /// tile's far edge and is not more than twice what a fresh one would be; otherwise <paramref name="margin"/> is the new value (the drop at the far edge
    /// plus <see cref="SlackM"/> of distance) and the result is true. <paramref name="invTwoR"/> = curvature / 2R.
    /// </summary>
    public static bool TryUpdate(float written, Vector3 camera, Vector3 center, float halfSpanM, float invTwoR, out float margin)
    {
        float dx = center.X - camera.X, dz = center.Z - camera.Z;
        float d = Mathf.Sqrt(dx * dx + dz * dz) + halfSpanM;
        float need = d * d * invTwoR;
        float ds = d + SlackM;
        margin = Mathf.Max(ds * ds * invTwoR, MinMarginM);
        return float.IsNaN(written) || need > written || written > 2f * margin;
    }
}
