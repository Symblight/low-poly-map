#nullable enable
using Godot;

namespace SimWorld;

/// <summary>
/// What the building shader needs from the world profile (docs/spec-night-windows.md, W4): the hourly lit-window share of each light
/// profile, the window colours and the landmark floodlight. Read from <c>world_profile()["lights"]["profiles"]</c>; the tables are data
/// in the profile, so retuning them needs no code change. Profile ids follow the vertex COLOR <c>r</c> channel:
/// 0 residential, 1 office, 2 retail, 3 industrial, 4 civic, 5 unknown, 6 landmark, 7 dark.
/// </summary>
public sealed class WindowProfile
{
    public const int ProfileCount = 8;
    /// <summary>Profile names by id (null = no name in the profile: id 5 "unknown").</summary>
    private static readonly string?[] Names = { "residential", "office", "retail", "industrial", "civic", null, "landmark", "dark" };

    /// <summary>24 hourly shares (0..1) per profile id; null = the profile has no windows (share always 0).</summary>
    public readonly float[]?[] LitShare = new float[]?[ProfileCount];
    /// <summary>Window / light colour per profile id (the profile's <c>color</c>, linear, as the shader multiplies it).</summary>
    public readonly Vector3[] Colors = new Vector3[ProfileCount];
    public Vector3 FloodlightColor = new(1f, 0.75f, 0.45f);
    public float FloodlightStrength = 1.2f;
    public float FloodlightFadeHeightM = 25f;

    /// <summary>Colour of id 5 (unknown), which has no profile entry.</summary>
    private static readonly Vector3 UnknownColor = new(1.0f, 0.8f, 0.55f);

    /// <summary>
    /// Parses the profile; null when it carries no <c>lights.profiles</c> (an old library) or no lit_share table at all. Missing single
    /// entries fall back to "no windows" / the shader defaults, a malformed table (not 24 numbers) is ignored with a warning.
    /// </summary>
    public static WindowProfile? FromWorldProfile(Godot.Collections.Dictionary profile)
    {
        if (!TryDict(profile, "lights", out var lights) || !TryDict(lights, "profiles", out var profiles)) return null;
        var wp = new WindowProfile();
        wp.Colors[5] = UnknownColor;
        int tables = 0;
        for (int id = 0; id < ProfileCount; id++)
        {
            var name = Names[id];
            if (name == null || !TryDict(profiles, name, out var p)) continue;
            if (p.TryGetValue("color", out var cv) && TryVec3(cv, out var c)) wp.Colors[id] = c;
            if (TryDict(p, "windows", out var w) && w.TryGetValue("lit_share", out var lv))
            {
                if (TryTable(lv, out var table)) { wp.LitShare[id] = table; tables++; }
                else GD.PushWarning($"[WorldView] world profile lights.profiles.{name}.windows.lit_share is not 24 numbers; {name} gets no windows");
            }
            if (id == 6 && TryDict(p, "floodlight", out var f))
            {
                wp.FloodlightColor = wp.Colors[id];
                if (f.TryGetValue("strength", out var s)) wp.FloodlightStrength = (float)s.AsDouble();
                if (f.TryGetValue("fade_height_m", out var h)) wp.FloodlightFadeHeightM = (float)h.AsDouble();
            }
        }
        return tables > 0 ? wp : null;
    }

    /// <summary>
    /// Share per profile id at <paramref name="hour"/>: each table's entry i is the value at hour i, linear between hours and
    /// wrapping 23 -> 0. Ids without a table stay 0 (industrial, unknown, landmark, dark). <paramref name="dst"/> needs 8 entries.
    /// </summary>
    public void SharesAt(float hour, float[] dst)
    {
        float h = hour % 24f;
        if (h < 0f) h += 24f;
        int i0 = Mathf.Clamp((int)h, 0, 23);
        int i1 = (i0 + 1) % 24;
        float t = h - i0;
        for (int id = 0; id < ProfileCount; id++)
        {
            var table = LitShare[id];
            dst[id] = table == null ? 0f : Mathf.Lerp(table[i0], table[i1], t);
        }
    }

    private static bool TryDict(Godot.Collections.Dictionary d, string key, out Godot.Collections.Dictionary value)
    {
        if (d.TryGetValue(key, out var v) && v.VariantType == Variant.Type.Dictionary) { value = v.AsGodotDictionary(); return true; }
        value = null!;
        return false;
    }

    private static bool TryVec3(Variant v, out Vector3 c)
    {
        c = default;
        if (v.VariantType != Variant.Type.Array) return false;
        var a = v.AsGodotArray();
        if (a.Count < 3) return false;
        c = new Vector3((float)a[0].AsDouble(), (float)a[1].AsDouble(), (float)a[2].AsDouble());
        return true;
    }

    private static bool TryTable(Variant v, out float[] table)
    {
        table = System.Array.Empty<float>();
        if (v.VariantType != Variant.Type.Array) return false;
        var a = v.AsGodotArray();
        if (a.Count != 24) return false;
        table = new float[24];
        for (int i = 0; i < 24; i++) table[i] = Mathf.Clamp((float)a[i].AsDouble(), 0f, 1f);
        return true;
    }
}
