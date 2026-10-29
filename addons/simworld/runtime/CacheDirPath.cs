using Godot;

namespace SimWorld;

/// <summary>Resolves the <see cref="WorldView.CacheDir"/> property to the OS path the library gets.</summary>
internal static class CacheDirPath
{
    private const string Prefix = "cache://";

    /// <summary><c>cache://a/b</c> -> <c>OS.GetCacheDir()/&lt;project name&gt;/a/b</c> (the desktop cache folder is shared by all apps);
    /// other <c>://</c> paths through <c>GlobalizePath</c>; OS paths and "" unchanged.</summary>
    public static string Resolve(string dir)
    {
        if (!dir.StartsWith(Prefix, System.StringComparison.Ordinal))
            return dir.Contains("://") ? ProjectSettings.GlobalizePath(dir) : dir;
        string app = ProjectSettings.GetSetting("application/config/name").AsString().Trim();
        foreach (char c in ":/\\?*\"|%<>") app = app.Replace(c, '_');
        return OS.GetCacheDir().PathJoin(app.Length > 0 ? app : "simworld").PathJoin(dir.Substring(Prefix.Length));
    }
}
