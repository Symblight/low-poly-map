#nullable enable
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
        using var v = _node!.Call(MLodState);
        if (v.VariantType != Variant.Type.Dictionary) return false;
        using var d = v.AsGodotDictionary();
        ReadLodFields(d, into);
        return true;
    }
}
