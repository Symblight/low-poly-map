#nullable enable
#if TOOLS
using Godot;

namespace SimWorld;

/// <summary>
/// Editor plugin: registers the addon's global shader uniforms (<see cref="ShaderGlobals"/>) in the project settings when the plugin
/// is enabled. Without them a shader that reads <c>global uniform simworld_*</c> does not compile in the editor. Hosts that never enable the
/// plugin are covered at runtime by <see cref="WorldView"/>, but their scenes only open cleanly in the editor with the plugin on.
/// </summary>
[Tool]
public partial class SimWorldPlugin : EditorPlugin
{
    public override void _EnterTree()
    {
        int added = ShaderGlobals.RegisterInProject();
        if (added > 0) GD.Print($"[SimWorld] registered {added} global shader uniform(s) in the project settings");
    }
}
#endif
