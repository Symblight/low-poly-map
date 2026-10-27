using System.Collections.Generic;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Test-bench launch arguments for the <see cref="WorldView"/> (host side: the addon itself reads no command line). Applies
/// <c>--tile-url=</c>, <c>--curvature=</c>, <c>--light-alpha=</c>, <c>--fake-fade=</c>, <c>--profile=</c>, <c>--area=</c>,
/// <c>--set=</c> and <c>--cover-selftest</c>, then starts the view (the scene turns <c>AutoStart</c> off). See low-poly-map/CLAUDE.md.
/// </summary>
public partial class BenchArgs : Node
{
    private const string TileUrlArg = "--tile-url=";
    private const string ProfileArg = "--profile=";
    private const string SetArg = "--set=";
    private const string AreaArg = "--area=";
    private static readonly string[] BandEdgeKeys = { "r0_edge_m", "r1_edge_m", "r2_edge_m", "r3_edge_m" };
    private static readonly double[] BandEdgeDefaults = { 600.0, 2500.0, 7000.0, 25000.0 };

    [Export] public WorldView? View { get; set; }

    private static bool TryNumber(string text, out double v) =>
        double.TryParse(text, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out v);

    public override void _Ready()
    {
        var view = View;
        if (view == null) { GD.PushError("[BenchArgs] no WorldView assigned"); return; }
        var settings = view.Settings ??= new WorldRenderSettings();

        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(TileUrlArg, System.StringComparison.Ordinal)) view.TileUrl = arg.Substring(TileUrlArg.Length); // may be empty -> synthetic
            else if (arg.StartsWith("--curvature=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(12), out var cv)) view.Curvature = Mathf.Max((float)cv, 0f);
            else if (arg.StartsWith("--light-alpha=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(14), out var la)) settings.DebugLightAlpha = (float)la;
            else if (arg.StartsWith("--fake-fade=", System.StringComparison.Ordinal) && TryNumber(arg.Substring(12), out var ff)) settings.DebugFakeFade = (float)ff;
            else if (arg.StartsWith(ProfileArg, System.StringComparison.Ordinal)) view.ProfilePath = arg.Substring(ProfileArg.Length).Trim();
        }
        // Relative profile paths are relative to the cwd; the view globalizes res:// and user:// itself.
        view.BaseProfileOverrides = ParseOverrides(view);

        if (CoverSelfTest.Requested()) CoverSelfTest.Run(view);
        view.Start();
    }

    /// <summary>
    /// --area=&lt;0.05..1&gt; (scales the four band edges, applied first) and repeated --set=&lt;dotted.key&gt;=&lt;number&gt; (applied last, wins).
    /// Returns the nested overrides (may be empty).
    /// </summary>
    private static Godot.Collections.Dictionary ParseOverrides(WorldView view)
    {
        double area = 0;
        var sets = new List<(string Key, Variant Value)>();
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(AreaArg, System.StringComparison.Ordinal))
            {
                if (!TryNumber(arg.Substring(AreaArg.Length), out var a)) { GD.PushWarning($"[BenchArgs] bad {arg}; expected --area=<0.05..1>"); continue; }
                area = System.Math.Clamp(a, 0.05, 1.0);
                if (area != a) GD.Print($"[BenchArgs] --area={a} clamped to {area}");
            }
            else if (arg.StartsWith(SetArg, System.StringComparison.Ordinal))
            {
                var kv = arg.Substring(SetArg.Length);
                int eq = kv.IndexOf('=');
                if (eq <= 0 || eq == kv.Length - 1) { GD.PushWarning($"[BenchArgs] bad {arg}; expected --set=<dotted.key>=<number>"); continue; }
                var key = kv.Substring(0, eq).Trim();
                var text = kv.Substring(eq + 1).Trim();
                // Numbers always go as float (like Godot's JSON parser): Rust accepts integral floats for integer keys.
                if (TryNumber(text, out var num)) sets.Add((key, num));
                else if (text == "true" || text == "false") sets.Add((key, text == "true"));
                else sets.Add((key, text));
            }
        }

        var overrides = new Godot.Collections.Dictionary();
        if (area > 0)
        {
            var bands = new Godot.Collections.Dictionary();
            var profile = view.DefaultProfile();
            Godot.Collections.Dictionary? pb = profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary ? bv.AsGodotDictionary() : null;
            for (int i = 0; i < BandEdgeKeys.Length; i++)
            {
                double def = pb != null && pb.TryGetValue(BandEdgeKeys[i], out var e) ? e.AsDouble() : BandEdgeDefaults[i];
                bands[BandEdgeKeys[i]] = def * area;
            }
            overrides["bands"] = bands;
        }
        foreach (var (key, value) in sets) SetDotted(overrides, key, value);
        return overrides;
    }

    private static void SetDotted(Godot.Collections.Dictionary root, string dotted, Variant value)
    {
        var parts = dotted.Split('.');
        var d = root;
        for (int i = 0; i < parts.Length - 1; i++)
        {
            if (!(d.TryGetValue(parts[i], out var sub) && sub.VariantType == Variant.Type.Dictionary))
            {
                var nd = new Godot.Collections.Dictionary();
                d[parts[i]] = nd;
                d = nd;
            }
            else d = sub.AsGodotDictionary();
        }
        d[parts[^1]] = value;
    }
}
