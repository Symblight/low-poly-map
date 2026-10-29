#nullable enable
using Godot;

namespace SimWorld;

/// <summary>Helpers for the nested profile override dictionaries (same shape as the profile JSON).</summary>
internal static class ProfileMerge
{
    /// <summary>Deep merge: nested dictionaries merge, everything else in <paramref name="over"/> replaces.</summary>
    public static void MergeInto(Godot.Collections.Dictionary into, Godot.Collections.Dictionary over)
    {
        foreach (var kv in over)
        {
            if (kv.Value.VariantType == Variant.Type.Dictionary && into.TryGetValue(kv.Key, out var cur) && cur.VariantType == Variant.Type.Dictionary)
                MergeInto(cur.AsGodotDictionary(), kv.Value.AsGodotDictionary());
            else
                into[kv.Key] = kv.Value.VariantType == Variant.Type.Dictionary ? kv.Value.AsGodotDictionary().Duplicate(true) : kv.Value;
        }
    }

    /// <summary>The nested dictionary <paramref name="key"/> of <paramref name="root"/>, created when it is missing or not a dictionary.</summary>
    public static Godot.Collections.Dictionary Section(Godot.Collections.Dictionary root, string key)
    {
        if (root.TryGetValue(key, out var v) && v.VariantType == Variant.Type.Dictionary) return v.AsGodotDictionary();
        var section = new Godot.Collections.Dictionary();
        root[key] = section;
        return section;
    }
}
