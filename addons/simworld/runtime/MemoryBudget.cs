using Godot;

namespace SimWorld;

/// <summary>
/// Per-platform default of the library's memory budget (profile <c>memory.budget_mb</c>, api 1.10; the library default is 512 MiB everywhere): puts
/// <see cref="WorldRenderSettings.MemoryBudgetMb"/> into the profile overrides of a configure unless the host already chose a value.
/// </summary>
public static class MemoryBudget
{
    /// <summary>
    /// Adds <c>memory.budget_mb</c> to <paramref name="overrides"/> when <paramref name="budgetMb"/> is above 0, the library supports the key and neither
    /// the overrides nor the profile file at <paramref name="profilePath"/> (empty = none) carry it. Returns true when it added the key.
    /// </summary>
    public static bool Apply(Godot.Collections.Dictionary overrides, float budgetMb, string profilePath, bool librarySupports)
    {
        if (budgetMb <= 0f || !librarySupports) return false;
        if (HasKey(overrides) || FileHasKey(profilePath)) return false;
        Godot.Collections.Dictionary section;
        if (overrides.TryGetValue("memory", out var mv) && mv.VariantType == Variant.Type.Dictionary) section = mv.AsGodotDictionary();
        else overrides["memory"] = section = new Godot.Collections.Dictionary();
        section["budget_mb"] = (double)budgetMb;
        return true;
    }

    private static bool HasKey(Godot.Collections.Dictionary d) =>
        d.TryGetValue("memory", out var mv) && mv.VariantType == Variant.Type.Dictionary && mv.AsGodotDictionary().ContainsKey("budget_mb");

    private static bool FileHasKey(string path)
    {
        if (path.Length == 0 || !FileAccess.FileExists(path)) return false;
        using var f = FileAccess.Open(path, FileAccess.ModeFlags.Read);
        if (f == null) return false;
        var parsed = Json.ParseString(f.GetAsText());
        return parsed.VariantType == Variant.Type.Dictionary && HasKey(parsed.AsGodotDictionary());
    }
}
