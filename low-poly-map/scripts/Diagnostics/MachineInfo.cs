using System;
using System.Text.RegularExpressions;
using Godot;
using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Which machine and which build a result comes from: OS, CPU, RAM, GPU, screen and render settings, the git commit of the checkout,
/// the engine, and the Rust library's version, api and build flavour (debug / release). Read at report time (it may start
/// <c>git</c>), never per frame.
/// </summary>
public static class MachineInfo
{
    /// <summary>File-name key of this machine: the model name with anything but letters, digits, '-' and '_' as '-'.</summary>
    public static string Key()
    {
        string model = OS.GetModelName();
        if (string.IsNullOrEmpty(model) || model == "GenericDevice") model = OS.GetName();
        return Regex.Replace(model, "[^A-Za-z0-9_-]", "-");
    }

    /// <summary>"debug" or "release" from the library's version string (<c>simworld-godot 0.5.0 (build release @169...)</c>); "unknown" without one.</summary>
    public static string LibraryFlavour(string versionText)
    {
        var m = Regex.Match(versionText, @"\(build (\w+)");
        return m.Success ? m.Groups[1].Value : "unknown";
    }

    /// <summary>
    /// How busy the machine is right now: the load averages and the other processes using the CPU (from <c>ps</c>, a decaying average, so only
    /// a hint). A benchmark records it before and after the run; baselines should only be written when it is quiet. Starts two short child
    /// processes, so never call it while measuring.
    /// </summary>
    public static string NoiseJson()
    {
        var j = new JsonText();
        j.Begin();
        var load = new Godot.Collections.Array();
        j.Field("load_avg", OS.Execute("sysctl", new[] { "-n", "vm.loadavg" }, load) == 0 && load.Count > 0 ? load[0].AsString().Trim('{', '}', ' ', '\n') : "unknown");
        var ps = new Godot.Collections.Array();
        double otherPct = 0;
        var busy = new System.Text.StringBuilder();
        if (OS.Execute("ps", new[] { "-axo", "pid=,pcpu=,comm=" }, ps) == 0 && ps.Count > 0)
        {
            int self = OS.GetProcessId();
            foreach (var line in ps[0].AsString().Split('\n', StringSplitOptions.RemoveEmptyEntries))
            {
                var parts = line.Trim().Split(' ', 3, StringSplitOptions.RemoveEmptyEntries);
                if (parts.Length < 3 || !int.TryParse(parts[0], out int pid) || pid == self
                    || !double.TryParse(parts[1], System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out double pcpu) || pcpu < 5) continue;
                otherPct += pcpu;
                if (busy.Length < 200) busy.Append(System.IO.Path.GetFileName(parts[2])).Append(' ').Append(pcpu.ToString("0")).Append("%; ");
            }
        }
        j.Field("other_processes_cpu_pct", Math.Round(otherPct, 1));
        j.Field("other_busy_processes", busy.ToString());
        j.End();
        return j.ToString();
    }

    public static string MachineJson()
    {
        var j = new JsonText();
        j.Begin();
        j.Field("key", Key());
        j.Field("os", OS.GetName() + " " + OS.GetVersion());
        j.Field("model", OS.GetModelName());
        j.Field("cpu", OS.GetProcessorName());
        j.Field("cpu_threads", OS.GetProcessorCount());
        var memory = OS.GetMemoryInfo();
        j.Field("ram_mb", memory.TryGetValue("physical", out var ram) ? Math.Round(ram.AsInt64() / 1048576.0) : -1);
        j.Field("gpu", RenderingServer.GetVideoAdapterName());
        j.Field("gpu_api", RenderingServer.GetVideoAdapterApiVersion());
        j.Field("renderer", RenderingServer.GetCurrentRenderingMethod());
        j.Field("driver", RenderingServer.GetCurrentRenderingDriverName());
        var window = DisplayServer.WindowGetSize();
        j.Field("window", $"{window.X}x{window.Y}");
        j.Field("screen_scale", DisplayServer.ScreenGetScale());
        j.Field("screen_hz", Math.Round(DisplayServer.ScreenGetRefreshRate(), 1));
        j.Field("vsync", DisplayServer.WindowGetVsyncMode().ToString());
        j.End();
        return j.ToString();
    }

    public static string BuildJson(WorldView view, string libraryVersionText)
    {
        var j = new JsonText();
        j.Begin();
        var (hash, dirty) = Git();
        j.Field("git_commit", hash);
        j.Field("git_dirty", dirty);
        j.Field("godot", Engine.GetVersionInfo()["string"].AsString());
        j.Field("godot_build", OS.IsDebugBuild() ? "debug (editor / debug template)" : "release template");
        j.Field("library_version", libraryVersionText);
        j.Field("library_api", view.Streamer.ApiVersionText);
        j.Field("library_flavour", LibraryFlavour(libraryVersionText));
        j.Field("process_memory_kind", ProcessMemory.Kind);
        j.End();
        return j.ToString();
    }

    /// <summary>Abbreviated HEAD of the repository that holds the project, and whether the tree has uncommitted changes.</summary>
    static (string Hash, bool Dirty) Git()
    {
        string dir = ProjectSettings.GlobalizePath("res://");
        var output = new Godot.Collections.Array();
        if (OS.Execute("git", new[] { "-C", dir, "rev-parse", "--short=10", "HEAD" }, output) != 0 || output.Count == 0) return ("unknown", false);
        string hash = output[0].AsString().Trim();
        output.Clear();
        // Only tracked files count as a change: the result files and generated copies are ignored by git.
        bool dirty = OS.Execute("git", new[] { "-C", dir, "status", "--porcelain", "--untracked-files=no" }, output) == 0
            && output.Count > 0 && output[0].AsString().Trim().Length > 0;
        return (hash, dirty);
    }
}
