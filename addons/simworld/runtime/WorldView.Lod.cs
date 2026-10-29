#nullable enable
using System.Text;
using Godot;

namespace SimWorld;

// Screen-space LOD (api 1.10): the live band edges and the cross-fade ranges that follow them.
public partial class WorldView
{
    /// <summary>The band edge keys of a library before api 1.10 (gone since: <c>bands.r3_edge_m</c> is now <c>bands.max_distance_m</c>); read only to describe an older library.</summary>
    private static readonly string[] BandEdgeKeys = { "r0_edge_m", "r1_edge_m", "r2_edge_m", "r3_edge_m" };
    private static readonly double[] LegacyEdgesM = { 600.0, 2500.0, 7000.0, 25000.0 };

    /// <summary>Effective band edges for the overlay, e.g. "edges 0.6/2.5/7/25 km": the live screen-space refine distances (api 1.10), or on an older library the profile's fixed edges (empty until readable).</summary>
    public string BandsText => EdgesText();
    private string _legacyBandsText = "";

    /// <summary>After a configure: reads the band hysteresis and, for a library before api 1.10, the fixed band edges that <see cref="BandsText"/> describes.</summary>
    private void ReadBandsProfile(Godot.Collections.Dictionary profile)
    {
        _legacyBandsText = "";
        if (profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary)
        {
            var b = bv.AsGodotDictionary();
            if (b.TryGetValue("hysteresis", out var hv)) _hysteresis = (float)hv.AsDouble();
            var sb = new StringBuilder("bands ");
            for (int i = 0; i < BandEdgeKeys.Length; i++)
            {
                if (!b.TryGetValue(BandEdgeKeys[i], out var e)) { sb.Clear(); break; }
                if (i > 0) sb.Append('/');
                sb.Append((e.AsDouble() / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            }
            if (sb.Length > 0) _legacyBandsText = sb.Append(" km").ToString();
        }
    }

    // ---- screen-space LOD (api 1.10) ----------------------------------------------------------

    /// <summary>The band edges the cover uses now in metres, R0..R3 (library <c>lod_edges_m</c>; R0..R2 are the refine distances of the screen-space error, R3 is <c>bands.max_distance_m</c>). All 0 until read or on a library before api 1.10. Refreshed 4 times a second and by <see cref="ReadLod"/>.</summary>
    public readonly double[] LodEdgesM = new double[4];
    /// <summary>True once the library has reported the screen-space LOD fields (<c>lod_edges_m</c>, <c>tau_eff</c>).</summary>
    public bool HasLodStats { get; private set; }
    /// <summary>tau_eff in pixels (profile <c>lod.tau_px</c> x <see cref="LodQuality"/>), the set_lod_quality scale in effect, and the view the library works with (echo of set_view).</summary>
    public double TauEff { get; private set; }
    public double LodQuality { get; private set; } = 1.0;
    public double LibraryViewFovYDeg { get; private set; }
    public int LibraryViewHeightPx { get; private set; }
    /// <summary>The R2 to R1 edge D the cross-fade ranges were last computed from (0 = not written yet), and how often they were written.</summary>
    public double BlendEdgeM => _lodBlendD;
    public int BlendUpdates { get; private set; }

    private readonly StreamerStats _lodStats = new();
    private double _lodBlendD, _lodMaxDist, _lodRingEdge0, _lodReadT;
    private readonly double[] _lodMarkEdges = new double[3];
    private float _hysteresis = 1.15f;
    private const double LodReadIntervalS = 0.25, LodBlendRelChange = 0.05;

    /// <summary>
    /// set_lod_quality (api 1.10): live multiplier of the pixel error threshold, clamped by the library to [0.25, 4]; 2 halves every refine distance, 0.5
    /// doubles them. No reconfigure; the library keeps it across <see cref="ApplyProfileOverrides"/>. Returns the scale in effect (NaN on an older library).
    /// The edges change with the next camera update; <see cref="ReadLod"/> then shows them.
    /// </summary>
    public double SetLodQuality(double scale)
    {
        double r = Streamer.SetLodQuality(scale);
        if (!double.IsNaN(r)) LodQuality = r;
        return r;
    }

    /// <summary>Reads <c>lod_edges_m</c>, <c>tau_eff</c>, <c>lod_quality</c> and the view from lod_state() (debug_stats() before api 1.11) into the properties above and, when the live R2 to R1 edge moved by more than 5 %, rewrites the cross-fade ranges.</summary>
    public void ReadLod()
    {
        if (!Streamer.IsAvailable || !Streamer.HasSetView || !Streamer.LodState(_lodStats) || !_lodStats.HasLodStats) return;
        HasLodStats = true;
        TauEff = _lodStats.TauEff;
        LodQuality = _lodStats.LodQuality;
        LibraryViewFovYDeg = _lodStats.ViewFovYDeg;
        LibraryViewHeightPx = _lodStats.ViewHeightPx;
        System.Array.Copy(_lodStats.LodEdgesM, LodEdgesM, 4);
        if (_settings.BlendFollowsLod && _pool != null) FollowLodEdges();
        FollowMarkingEdges();
    }

    /// <summary>Contract-time part of S3: the window / sprite blend and the street pool fade follow D = the R2 to R1 edge, the sprites' far fade follows the maximum distance.</summary>
    private void FollowLodEdges()
    {
        double d = LodEdgesM[1];
        if (d > 1 && (_lodBlendD <= 0 || System.Math.Abs(d - _lodBlendD) > _lodBlendD * LodBlendRelChange))
        {
            _lodBlendD = d;
            BlendUpdates++;
            _pool!.SetLodRanges((float)(d * WorldRenderSettings.BlendStartShare), (float)(d * WorldRenderSettings.BlendEndShare),
                (float)(d * WorldRenderSettings.PoolStartShare), PoolFadeEndM(d));
        }
        double far = LodEdgesM[3];
        if (far > 1 && far != _lodMaxDist)
        {
            _lodMaxDist = far;
            _pool!.SetLightFar((float)(far * WorldRenderSettings.LightFadeShare), (float)far);
        }
    }

    /// <summary>The runway markings choose the lift of the ground ring by view distance: their ring edges follow the live edges x (2 - hysteresis).</summary>
    private void FollowMarkingEdges()
    {
        if (_markings == null || LodEdgesM[2] <= 1) return;
        bool moved = _lodRingEdge0 <= 0;
        for (int i = 0; i < 3 && !moved; i++) moved = System.Math.Abs(LodEdgesM[i] - _lodMarkEdges[i]) > _lodMarkEdges[i] * 0.01;
        if (!moved) return;
        for (int i = 0; i < 3; i++) _lodMarkEdges[i] = LodEdgesM[i];
        _lodRingEdge0 = LodEdgesM[0];
        float k = 2f - _hysteresis;
        _markings.SetRingLifts(RunwayLift, new Vector3((float)LodEdgesM[0] * k, (float)LodEdgesM[1] * k, (float)LodEdgesM[2] * k));
    }

    private string EdgesText()
    {
        if (HasLodStats && LodEdgesM[3] > 0)
        {
            var sb = new StringBuilder("edges ");
            for (int i = 0; i < 4; i++) sb.Append(i > 0 ? "/" : "").Append((LodEdgesM[i] / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            return sb.Append(" km").ToString();
        }
        return _legacyBandsText;
    }
}
