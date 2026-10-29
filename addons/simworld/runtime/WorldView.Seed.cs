using Godot;

namespace SimWorld;

// World seed (api 1.7): seed_world_info(), airports() and the lift of the drawn runway.
public partial class WorldView
{
    /// <summary>True when this view runs a seed world (api 1.7): <see cref="SeedWorld"/> was set and the library has the phase S1 API.</summary>
    public bool SeedWorldActive { get; private set; }
    /// <summary>seed_world_info() as read after the last configure; null without a seed world.</summary>
    public SeedWorldInfo? SeedInfo { get; private set; }
    /// <summary>airports() in the current local frame (re-read on configure and on every origin shift); empty without a seed world.</summary>
    public AirportInfo[] Airports { get; private set; } = System.Array.Empty<AirportInfo>();
    /// <summary>The runway marking helper; null until the view is initialized.</summary>
    public AirportMarkings? Markings => _markings;

    /// <summary>The drawn runway height above ground_height_at per ring R0..R3 as the host computes it (set by <see cref="ComputeRunwayLifts"/>; tests, overlay).</summary>
    public Vector4 RunwayLift { get; private set; } = new(1.2f, 3.6f, 9.5f, 25f);
    /// <summary>Which rule <see cref="RunwayLift"/> follows: "level" (generator 2: runway level x lift_step_m, no drape lift) or "drape replay" (generator 1 or unknown).</summary>
    public string RunwayLiftRule { get; private set; } = "drape replay";

    /// <summary>
    /// How far the drawn runway is above ground_height_at, per ring (the markings are laid on it). Generator 2 and later (<c>seed_world_info()["generator_version"]</c>,
    /// contract section 9 "Airports"): inside an airport fence the ground has no drape lift, the runway is at stacking level
    /// <see cref="WorldRenderSettings.MarkingRunwayLevel"/> (3) x <c>ground.rN.lift_step_m</c> and nothing else (0.06 / 0.18 / 0.45 / 1.5 m by default). It
    /// does NOT depend on the general drape lift. Generator 1 or a library without the key (older libraries): replays the old rule, lift(band N) = the largest
    /// <c>max_error_m</c> of the terrain bands whose range starts below <c>rN_edge_m x hysteresis</c> (a generator-1 library has the old fixed edges), times <c>terrain.detail_scale_max</c>, plus
    /// <c>MarkingRunwayLevel</c> x step; there the level must be set back to 10 (the old runway level), which this method does by itself.
    /// </summary>
    private void ComputeRunwayLifts(Godot.Collections.Dictionary profile)
    {
        if (_markings == null) return;
        int generator = SeedInfo?.GeneratorVersion ?? 0;
        bool levelRule = generator >= 2;
        // The old library drew the runway at level 10; the setting is the generator-2 level (3) and must not be applied to it.
        int level = levelRule ? _settings.MarkingRunwayLevel : LegacyRunwayLevel;
        var lift = new float[4] { 1.2f, 3.6f, 9.5f, 25f };
        if (levelRule) lift = new float[4] { 0.06f, 0.18f, 0.45f, 1.5f };
        try
        {
            var bands = profile["bands"].AsGodotDictionary();
            double hyst = bands["hysteresis"].AsDouble();
            var ground = profile["ground"].AsGodotDictionary();
            Godot.Collections.Array? tb = null;
            double detail = 1;
            if (!levelRule)
            {
                var terrain = profile["terrain"].AsGodotDictionary();
                detail = terrain["detail_scale_max"].AsDouble();
                tb = terrain["bands"].AsGodotArray();
            }
            for (int n = 0; n < 4; n++)
            {
                // Generator 1 only: the band reach of the old drape replay. The edges are the old fixed ones (a generator-1 library has no screen-space LOD).
                double rEdge = !levelRule && bands.TryGetValue(BandEdgeKeys[n], out var rev) ? rev.AsDouble() : LegacyEdgesM[n];
                double step = ground["r" + n].AsGodotDictionary()["lift_step_m"].AsDouble();
                double worst = 0;
                if (tb != null)
                {
                    double reach = rEdge * hyst, prev = 0;
                    for (int k = 0; k < tb.Count; k++)
                    {
                        var b = tb[k].AsGodotDictionary();
                        if (prev < reach) worst = System.Math.Max(worst, b["max_error_m"].AsDouble());
                        prev = b["edge_m"].AsDouble();
                    }
                }
                lift[n] = (float)(worst * detail + level * step);
            }
        }
        catch (System.Exception e)
        {
            GD.PushWarning($"[WorldView] runway marking lifts: cannot read the profile ({e.Message}); using the default lifts");
        }
        RunwayLift = new Vector4(lift[0], lift[1], lift[2], lift[3]);
        RunwayLiftRule = levelRule ? "level" : "drape replay";
        // Ring edges (view distance beyond which ring 1, 2, 3 applies): the live edges x (2 - hysteresis); before the first read the old fixed 600 / 2500 / 7000 m.
        _lodRingEdge0 = 0;
        float ek = 2f - _hysteresis;
        _markings.SetRingLifts(RunwayLift, new Vector3((float)LegacyEdgesM[0] * ek, (float)LegacyEdgesM[1] * ek, (float)LegacyEdgesM[2] * ek));
        FollowMarkingEdges();
        GD.Print($"[WorldView] runway lift per ring (generator {generator}, {(levelRule ? $"level {level} x lift_step_m, no drape lift" : "old drape replay")}): "
            + $"{lift[0]:0.###} / {lift[1]:0.###} / {lift[2]:0.###} / {lift[3]:0.###} m above ground_height_at");
    }

    /// <summary>Stacking level of the runway before generator 2 (contract section 9, the old drape replay).</summary>
    private const int LegacyRunwayLevel = 10;

    /// <summary>
    /// Seed world only: reads seed_world_info() (when <paramref name="full"/>, after a configure; logs the summary and the load-time warnings) and
    /// airports() in the current frame (also after every origin shift) and hands the airports to the marking helper.
    /// </summary>
    private void RefreshSeed(bool full)
    {
        if (!SeedWorldActive)
        {
            SeedInfo = null;
            Airports = System.Array.Empty<AirportInfo>();
            _markings?.SetAirports(Airports);
            return;
        }
        if (full)
        {
            SeedInfo = Streamer.SeedInfo();
            if (SeedInfo == null) GD.PushWarning("[WorldView] seed_world_info() is empty although a seed world was configured");
            else
            {
                GD.Print($"[WorldView] seed world: {SeedInfo.Summary}");
                foreach (var c in SeedInfo.Cities) GD.Print($"[WorldView]   city {c.Id} \"{c.Name}\" r={c.RadiusKm:0.#} km v{c.Version} source={c.Source} airports=[{string.Join(", ", c.Airports)}]");
                // Load-time validation warnings (e.g. approximately placed runways of the OurAirports cut) are data notes, not host problems: print, do not push.
                for (int i = 0; i < SeedInfo.Warnings.Length && i < 5; i++) GD.Print($"[WorldView]   seed warning: {SeedInfo.Warnings[i]}");
                if (SeedInfo.Warnings.Length > 5) GD.Print($"[WorldView]   ... and {SeedInfo.Warnings.Length - 5} more seed warnings (overlay F1 shows the first 3)");
            }
        }
        Airports = Streamer.Airports();
        _markings?.SetAirports(Airports);
        if (full) ComputeRunwayLifts(Streamer.WorldProfile());
        if (full) GD.Print($"[WorldView] airports(): {Airports.Length} with an open runway ({string.Join(" ", System.Linq.Enumerable.Select(Airports, a => $"{a.Ident}[{a.Runways.Length}]"))})");
    }
}
