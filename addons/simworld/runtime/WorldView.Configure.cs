#nullable enable
using System.Text;
using Godot;

namespace SimWorld;

// Configure: configure_world with the profile, the effective profile overrides and what happens after a configure.
public partial class WorldView
{
    /// <summary>Latest data-source status (contract 3.5): from status() after configure and from the status_changed signal.</summary>
    public StreamerStatus Status { get; private set; } = new();
    /// <summary>Non-empty when the world could not be configured (e.g. "cache_dir in use"); the spike then runs without a world.</summary>
    public string ConfigureError { get; private set; } = "";
    /// <summary>True when a configure call succeeded and the world is streaming.</summary>
    public bool WorldRunning { get; private set; }
    /// <summary>Attribution strings to show while map data is visible (empty for the synthetic city).</summary>
    public string[] Attributions { get; private set; } = System.Array.Empty<string>();

    /// <summary>The effective profile overrides passed to configure_world (base + panel; empty = none), for logs and tests.</summary>
    public Godot.Collections.Dictionary ProfileOverrides { get; private set; } = new();

    /// <summary>
    /// configure_world (api 0.6+, with the profile) or the legacy configure. On a profile error the world is retried with the
    /// default profile; any other failure (e.g. cache_dir in use) leaves the view running without a world.
    /// </summary>
    private void ConfigureWorld(string url, string cacheDir)
    {
        var overrides = ComposeOverrides(null);
        var profilePath = ResolveProfilePath();
        ProfileOverrides = overrides;
        bool haveProfileArgs = profilePath.Length > 0 || overrides.Count > 0;
        if (profilePath.Length > 0) GD.Print($"[WorldView] profile file: {profilePath}");
        if (overrides.Count > 0) GD.Print($"[WorldView] profile overrides: {Godot.Json.Stringify(overrides)}");

        if (!Streamer.HasConfigureWorld)
        {
            if (haveProfileArgs) GD.PushWarning($"[WorldView] profile ignored: library api {Streamer.ApiVersionText} has no configure_world (needs 0.6)");
            Streamer.Configure(OriginLat, OriginLon, url, cacheDir);
            WorldRunning = true; // legacy configure reports errors only in the Godot log
            AfterConfigure();
            return;
        }

        var options = WorldStreamerFacade.MakeWorldOptions(OriginLat, OriginLon, url, cacheDir);
        AddHostOptions(options);
        if (profilePath.Length > 0) options["profile_path"] = profilePath;
        if (overrides.Count > 0) options["profile_overrides"] = overrides;
        var result = Streamer.ConfigureWorld(options);
        if (!result.Ok && haveProfileArgs && result.Error.StartsWith("profile", System.StringComparison.Ordinal))
        {
            GD.PushError($"[WorldView] profile rejected by the library: {result.Error}");
            GD.Print("[WorldView] retrying with the default world profile");
            ProfileOverrides = new Godot.Collections.Dictionary();
            ProfilePath = "";
            BaseProfileOverrides = new Godot.Collections.Dictionary();
            options.Remove("profile_path");
            options.Remove("profile_overrides");
            var retry = ComposeOverrides(null);
            if (retry.Count > 0) options["profile_overrides"] = retry;
            result = Streamer.ConfigureWorld(options);
        }
        if (!result.Ok)
        {
            ConfigureError = result.Error.Length > 0 ? result.Error : "configure_world failed";
            GD.PushError($"[WorldView] configure_world failed: {ConfigureError}. Running without a world (no tiles will load).");
            return;
        }
        WorldRunning = true;
        Attributions = result.Attributions;
        AfterConfigure();
    }

    /// <summary>The profile file as an OS path for the library (<c>res://</c> / <c>user://</c> globalized, a relative path made absolute); "" = none.</summary>
    private string ResolveProfilePath() =>
        ProfilePath.Length == 0 ? "" : ProfilePath.Contains("://") ? ProjectSettings.GlobalizePath(ProfilePath) : System.IO.Path.GetFullPath(ProfilePath);

    /// <summary>The base profile overrides plus <c>terrain.enabled</c> from <see cref="TerrainEnabled"/> (unless the overrides set the key), then <paramref name="panel"/> merged over (panel wins).</summary>
    private Godot.Collections.Dictionary ComposeOverrides(Godot.Collections.Dictionary? panel)
    {
        var merged = (Godot.Collections.Dictionary)BaseProfileOverrides.Duplicate(true);
        if (TerrainEnabled)
        {
            var t = ProfileMerge.Section(merged, "terrain");
            if (!t.ContainsKey("enabled")) t["enabled"] = true;
        }
        if (SeedWorldActive || SeedWorld.Trim().Length > 0)
        {
            // Contract 3.1: a seed world needs terrain.enabled and no flat zone (its runway strips and water rule replace it). Explicit host values win
            // (and fail in the library with its error text, which the panel shows).
            var t = ProfileMerge.Section(merged, "terrain");
            if (!t.ContainsKey("enabled")) t["enabled"] = true;
            if (!t.ContainsKey("flat_radius_m")) t["flat_radius_m"] = 0.0;
            if (!t.ContainsKey("flat_blend_m")) t["flat_blend_m"] = 0.0;
        }
        if (MemoryBudget.Apply(merged, _settings.MemoryBudgetMb, ProfilePath, Streamer.HasSetView) && !_loggedMemoryBudget)
        {
            _loggedMemoryBudget = true;
            GD.Print($"[WorldView] memory.budget_mb {_settings.MemoryBudgetMb:0.#} (WorldRenderSettings.MemoryBudgetMb; the library default is 512)");
        }
        if (panel != null) ProfileMerge.MergeInto(merged, panel);
        return merged;
    }

    private void AfterConfigure()
    {
        _viewForce = true; // a new world starts with the library's default view: send ours with the next frame
        if (LandmarkCover.Loaded != 0)
        {
            GD.PushError($"[WorldView] landmark tile map still holds {LandmarkCover.Loaded} tiles at configure; releasing them");
            _dropScratch.Clear();
            foreach (var n in LandmarkCover.All) _dropScratch.Add(n.Packed);
            for (int i = 0; i < _dropScratch.Count; i++) ReleaseLandmarkTile(_dropScratch[i]);
            _dropScratch.Clear();
        }
        Status = Streamer.Status();
        if (Attributions.Length == 0) Attributions = Streamer.Attributions();
        if (Attributions.Length > 0) GD.Print($"[WorldView] attribution: {string.Join(" | ", Attributions)}");
        else if (Streamer.HasHostApi) GD.Print("[WorldView] attribution: none required (synthetic city)");

        var profile = Streamer.WorldProfile();
        ApplyWindowProfile(profile); ApplySpriteProfile(profile);
        ConfigureTerrainLayer(profile);
        ReadBandsProfile(profile);
        _lodBlendD = 0; _lodMaxDist = 0; _lodRingEdge0 = 0; // a new world: write the cross-fade ranges again
        ReadLod();
        if (BandsText.Length > 0) GD.Print($"[WorldView] effective {BandsText}{(HasLodStats ? $", tau_eff {TauEff:0.##} px (quality x{LodQuality:0.##}), library view {LibraryViewFovYDeg:0.#} deg / {LibraryViewHeightPx} px" : "")}");
        RefreshSeed(true);
    }

    /// <summary>The values the library is running with (world_profile), e.g. "tau 4 px (quality x1, fov 60 deg, H 1080 px), edges 0.6/2.5/7/25 km, max distance 25 km, min building height R0/R1/R2 0/0/0 m".</summary>
    public string EffectiveProfileText()
    {
        var p = Streamer.WorldProfile();
        var sb = new StringBuilder();
        ReadLod();
        if (HasLodStats)
        {
            sb.Append("tau ").Append(TauEff.ToString("0.##", System.Globalization.CultureInfo.InvariantCulture)).Append(" px (quality x").Append(LodQuality.ToString("0.##", System.Globalization.CultureInfo.InvariantCulture))
              .Append(", fov ").Append(LibraryViewFovYDeg.ToString("0.#", System.Globalization.CultureInfo.InvariantCulture)).Append(" deg, H ").Append(LibraryViewHeightPx).Append(" px), ").Append(EdgesText());
        }
        else sb.Append(EdgesText());
        if (p.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary && bv.AsGodotDictionary().TryGetValue("max_distance_m", out var mdv))
            sb.Append(sb.Length > 0 ? ", " : "").Append("max distance ").Append((mdv.AsDouble() / 1000.0).ToString("0.##", System.Globalization.CultureInfo.InvariantCulture)).Append(" km");
        if (p.TryGetValue("buildings", out var bd) && bd.VariantType == Variant.Type.Dictionary)
        {
            var d = bd.AsGodotDictionary();
            sb.Append(sb.Length > 0 ? ", " : "").Append("min building height R0/R1/R2 ");
            for (int i = 0; i < 3; i++)
            {
                double h = d.TryGetValue("r" + i, out var rv) && rv.VariantType == Variant.Type.Dictionary && rv.AsGodotDictionary().TryGetValue("min_height_m", out var mh) ? mh.AsDouble() : double.NaN;
                sb.Append(i > 0 ? "/" : "").Append(double.IsNaN(h) ? "?" : h.ToString("0.##", System.Globalization.CultureInfo.InvariantCulture));
            }
            sb.Append(" m");
        }
        if (TerrainActive) sb.Append(sb.Length > 0 ? ", " : "").Append("terrain z").Append(TerrainCover.RootZ).Append("..z").Append(TerrainCover.LeafZ).Append(" to ").Append((TerrainReachM / 1000.0).ToString("0.#", System.Globalization.CultureInfo.InvariantCulture)).Append(" km");
        if (p.TryGetValue("lights", out var lt) && lt.VariantType == Variant.Type.Dictionary && lt.AsGodotDictionary().TryGetValue("min_ring", out var mr))
            sb.Append(sb.Length > 0 ? ", " : "").Append("lights from R").Append(mr.AsInt64());
        return sb.ToString();
    }
}
