using Godot;

namespace SimWorld;

/// <summary>Where the addon sits in the project, derived from the runtime scripts' resource path (set by <see cref="WorldView"/>).</summary>
public static class AddonPaths
{
    /// <summary>The addon folder, e.g. <c>res://addons/simworld</c> (no trailing slash).</summary>
    public static string Root { get; set; } = "res://addons/simworld";

    public static string Shaders => Root + "/shaders";
    public static string Textures => Root + "/textures";

    /// <summary>Sets <see cref="Root"/> from the resource path of a script in <c>runtime/</c>; returns false when the path is not usable.</summary>
    public static bool SetFromScriptPath(string scriptPath)
    {
        int i = scriptPath.LastIndexOf("/runtime/", System.StringComparison.Ordinal);
        if (i <= "res://".Length) return false;
        Root = scriptPath.Substring(0, i);
        return true;
    }
}

/// <summary>
/// The addon's global shader uniforms. Every world shader reads them as <c>global uniform</c>; the host's clock writes them
/// through <see cref="WorldView.SetTime"/>. They are registered in two places: the editor plugin adds them to the project settings
/// (<see cref="RegisterInProject"/>), and the runtime adds any that is missing (<see cref="EnsureRegistered"/>), which covers hosts
/// that never enabled the plugin.
/// </summary>
public static class ShaderGlobals
{
    /// <summary>0 = day, 1 = full night (light sprites).</summary>
    public const string NightFactor = "simworld_night_factor";
    /// <summary>Local solar hour 0..24 (window light profiles, M6; not read by a shader yet).</summary>
    public const string Hour = "simworld_hour";
    /// <summary>Camera position in world space (earth curvature origin).</summary>
    public const string CurveCenter = "simworld_curve_center";
    /// <summary>Curvature / (2 R) in 1/m; 0 = flat.</summary>
    public const string CurveInv2R = "simworld_curve_inv_2r";

    public readonly record struct Def(string Name, RenderingServer.GlobalShaderParameterType Type, string SettingsType, Variant Default);

    public static readonly Def[] All =
    {
        new(NightFactor, RenderingServer.GlobalShaderParameterType.Float, "float", 1.0f),
        new(Hour, RenderingServer.GlobalShaderParameterType.Float, "float", 12.0f),
        new(CurveCenter, RenderingServer.GlobalShaderParameterType.Vec3, "vec3", Vector3.Zero),
        new(CurveInv2R, RenderingServer.GlobalShaderParameterType.Float, "float", 0.0f),
    };

    // GlobalShaderParameterGetList() only works in the editor (it errors and returns nothing in a running game), so "registered" is
    // tracked here: the rendering server loads shader_globals/* from the project settings at startup, and this set covers the rest.
    private static readonly System.Collections.Generic.HashSet<string> Registered = new();

    /// <summary>
    /// Adds every global that the project settings did not already register at startup. Call before a world shader is loaded.
    /// Idempotent and cheap (called every time the clock is set).
    /// </summary>
    public static void EnsureRegistered()
    {
        foreach (var d in All)
        {
            if (Registered.Contains(d.Name)) continue;
            Registered.Add(d.Name);
            if (ProjectSettings.HasSetting("shader_globals/" + d.Name) && !Engine.IsEditorHint()) continue;
            if (Engine.IsEditorHint() && RenderingServer.GlobalShaderParameterGetList().Contains(d.Name)) continue;
            RenderingServer.GlobalShaderParameterAdd(d.Name, d.Type, d.Default);
        }
    }

    /// <summary>
    /// Editor plugin side: writes <c>shader_globals/&lt;name&gt;</c> into the project settings (and saves them) for every missing
    /// global, and adds it to the running editor so shaders compile now. Returns the number of settings added.
    /// </summary>
    public static int RegisterInProject()
    {
        int added = 0;
        foreach (var d in All)
        {
            string key = "shader_globals/" + d.Name;
            if (ProjectSettings.HasSetting(key)) continue;
            ProjectSettings.SetSetting(key, new Godot.Collections.Dictionary { ["type"] = d.SettingsType, ["value"] = d.Default });
            added++;
        }
        if (added > 0) ProjectSettings.Save();
        EnsureRegistered();
        return added;
    }
}
