using Godot;

namespace SimWorld;

public sealed partial class WorldStreamerFacade
{
    private static readonly StringName MLodState = new("lod_state");
    private bool? _hasLodState;

    /// <summary><c>lod_state()</c> (api 1.11): the live LOD keys without building the whole debug_stats dictionary.</summary>
    public bool HasLodState => _hasLodState ??= IsAvailable && ClassDB.ClassHasMethod(ClassNameSn, MLodState);

    /// <summary>
    /// Fills the LOD fields of <paramref name="into"/> (tau_eff, quality, edges, view) from <c>lod_state()</c>, or from a LOD-only
    /// <see cref="DebugStats"/> read on a library without it. Returns false if unavailable.
    /// </summary>
    public bool LodState(StreamerStats into)
    {
        if (!HasLodState) return DebugStats(into, lodOnly: true);
        using var timing = Timings.Time(StreamerCall.DebugStats);
        var v = _node!.Call(MLodState);
        if (v.VariantType != Variant.Type.Dictionary) return false;
        var d = v.AsGodotDictionary();
        into.HasLodStats = d.ContainsKey(KTauEff);
        into.TauEff = d.TryGetValue(KTauEff, out var tau) ? tau.AsDouble() : 0.0;
        into.LodQuality = d.TryGetValue(KLodQuality, out var lq) ? lq.AsDouble() : 1.0;
        System.Array.Clear(into.LodEdgesM);
        if (d.TryGetValue(KLodEdges, out var le) && le.VariantType == Variant.Type.PackedFloat64Array)
        {
            var arr = le.AsFloat64Array();
            for (int i = 0; i < arr.Length && i < StreamerStats.Rings; i++) into.LodEdgesM[i] = arr[i];
        }
        into.ViewFovYDeg = d.TryGetValue(KViewFov, out var vf) ? vf.AsDouble() : 0.0;
        into.ViewHeightPx = d.TryGetValue(KViewHeight, out var vh) ? (int)vh.AsInt64() : 0;
        return true;
    }
}
