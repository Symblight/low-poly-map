using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>A resolved camera spot: geodetic position, altitude above ground, compass heading (0 = north, 90 = east) and pitch (degrees, negative looks down).</summary>
public readonly record struct SpotPose(string Name, double Lat, double Lon, double AltAglM, double HeadingDeg, double PitchDeg, Vector3 Local);

/// <summary>
/// Camera spots of the two-cities seed world (docs/spec-world-seed.md 11a.2): <c>vilnius</c>, <c>kaunas</c>, <c>eyvi</c>, <c>eyka</c> (3 nm final aligned
/// with the main runway at 300 m), <c>a1</c>, <c>cruise</c>, <c>coast</c>, <c>lake</c>. Coordinates come from the world files (city centres, the A1 road, the
/// Baltic coast and Kauno marios polygons of <c>water.json</c>) or, for the airports, from <c>airports()</c>. Altitudes are metres above ground.
/// </summary>
public static class SeedSpots
{
    public static readonly string[] Names = { "vilnius", "kaunas", "eyvi", "eyka", "a1", "cruise", "coast", "lake" };

    /// <summary>
    /// The spots of <c>--seed-selftest</c>: <see cref="Names"/> plus <c>eyvi-field</c> / <c>eyka-field</c> (150 m above the middle of the main runway, along it:
    /// R0 / R1 tiles over the strip and the inner approaches; for the building and ground checks of the S2-S4 groups).
    /// </summary>
    public static readonly string[] SelfTestNames = { "vilnius", "kaunas", "eyvi", "eyka", "eyvi-field", "eyka-field", "a1", "cruise", "coast", "lake" };

    /// <summary>3 nautical miles in metres.</summary>
    public const double ThreeNm = 5556.0;
    public const double FinalAglM = 300.0;

    /// <summary>The value of <c>--seed-spot=</c>, or null.</summary>
    public static string? Requested()
    {
        foreach (var a in OS.GetCmdlineUserArgs())
            if (a.StartsWith("--seed-spot=", System.StringComparison.Ordinal)) return a.Substring(12).Trim().ToLowerInvariant();
        return null;
    }

    private static double Arg(string key, double fallback)
    {
        foreach (var a in OS.GetCmdlineUserArgs())
            if (a.StartsWith(key, System.StringComparison.Ordinal) && double.TryParse(a.Substring(key.Length), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var v)) return v;
        return fallback;
    }

    /// <summary>Resolves <paramref name="name"/> in the CURRENT local frame of <paramref name="view"/> (runway spots read <see cref="WorldView.Airports"/>).</summary>
    public static bool TryResolve(WorldView view, string name, out SpotPose pose)
    {
        bool ok = ResolveBase(view, name, out pose);
        if (!ok) return false;
        // Bench aids: --seed-spot-alt=<m AGL> --seed-spot-pitch=<deg> --seed-spot-heading=<deg> override the spot's own values (screenshots).
        pose = pose with { AltAglM = Arg("--seed-spot-alt=", pose.AltAglM), PitchDeg = Arg("--seed-spot-pitch=", pose.PitchDeg), HeadingDeg = Arg("--seed-spot-heading=", pose.HeadingDeg) };
        return true;
    }

    private static bool ResolveBase(WorldView view, string name, out SpotPose pose)
    {
        pose = default;
        switch (name)
        {
            case "vilnius": pose = Fixed(view, name, 54.6872, 25.2797, 400, 200, -22); return true;           // old town, looking south-west over the roofs
            case "kaunas": pose = Fixed(view, name, 54.8985, 23.9036, 400, 90, -22); return true;           // confluence of the Neris and the Nemunas
            case "a1": pose = Fixed(view, name, 54.8200, 24.5100, 1500, 90, -14); return true;              // on the A1 half way, looking east along the road
            case "cruise": pose = Fixed(view, name, 54.7930, 24.5900, 3000, 270, -10); return true;         // between the cities, Kaunas ahead
            case "coast": pose = Fixed(view, name, 55.8500, 21.0000, 2000, 90, -14); return true;           // 3.7 km off the Baltic coast north of Klaipeda, looking at the shore
            case "lake": pose = Fixed(view, name, 54.8550, 24.1700, 500, 0, -28); return true;              // over the middle of Kauno marios
            case "eyvi": return Final(view, "EYVI", name, out pose);
            case "eyka": return Final(view, "EYKA", name, out pose);
            case "eyvi-field": return Field(view, "EYVI", name, out pose);
            case "eyka-field": return Field(view, "EYKA", name, out pose);
            default: return false;
        }
    }

    private static SpotPose Fixed(WorldView view, string name, double lat, double lon, double aglM, double heading, double pitch)
    {
        var local = view.Streamer.GeodeticToLocal(lat, lon, 0.0);
        return new SpotPose(name, lat, lon, aglM, heading, pitch, local);
    }

    /// <summary>Over the middle of the main runway, 150 m above ground, heading along it (runway-end threshold midpoint).</summary>
    private static bool Field(WorldView view, string ident, string name, out SpotPose pose)
    {
        pose = default;
        foreach (var a in view.Airports)
        {
            if (a.Ident != ident || a.MainRunway is not { } rw) continue;
            var local = (rw.Le.ThresholdPosition + rw.He.ThresholdPosition) * 0.5f;
            var geo = view.Streamer.LocalToGeodetic(local);
            if (geo == Vector3.Zero) return false;
            pose = new SpotPose(name, geo.X, geo.Y, 150.0, rw.Le.HeadingDeg, -8.0, local);
            return true;
        }
        return false;
    }

    /// <summary>3 nm out on the extended centreline of the main runway's first end, 300 m above ground, heading along the runway.</summary>
    private static bool Final(WorldView view, string ident, string name, out SpotPose pose)
    {
        pose = default;
        foreach (var a in view.Airports)
        {
            if (a.Ident != ident || a.MainRunway is not { } rw) continue;
            var end = rw.Le;
            double h = Mathf.DegToRad(end.HeadingDeg);
            var dir = new Vector3((float)System.Math.Sin(h), 0f, (float)-System.Math.Cos(h));
            var local = end.ThresholdPosition - dir * (float)Arg("--seed-spot-out=", ThreeNm);
            var geo = view.Streamer.LocalToGeodetic(local);
            if (geo == Vector3.Zero) return false;
            pose = new SpotPose(name, geo.X, geo.Y, FinalAglM, end.HeadingDeg, -4.0, local);
            return true;
        }
        return false;
    }

    /// <summary>
    /// Places the free-fly camera at a spot through update_camera_geodetic, once, in the next frame: the hook replaces that frame's camera update (the
    /// library re-centres the origin onto the spot, tens or hundreds of kilometres away, and the view shifts the camera and every node), then normal flight resumes.
    /// </summary>
    public static void Teleport(WorldView view, FreeFlyCamera cam, SpotPose pose, System.Action<double>? then = null)
    {
        view.ExternalCameraUpdate = _ =>
        {
            view.ExternalCameraUpdate = null;
            // The spot was resolved in the frame of that moment; place the camera in the current frame first, then let the library shift.
            var local = view.Streamer.GeodeticToLocal(pose.Lat, pose.Lon, 0.0);
            double ground = view.Streamer.GroundHeightAt(local.X, local.Z);
            double alt = ground + pose.AltAglM;
            cam.GlobalPosition = new Vector3(local.X, (float)alt, local.Z);
            cam.SetLook(Mathf.DegToRad((float)-pose.HeadingDeg), Mathf.DegToRad((float)pose.PitchDeg));
            view.Streamer.UpdateCameraGeodetic(pose.Lat, pose.Lon, alt, Vector3.Zero);
            then?.Invoke(alt);
        };
    }
}

/// <summary>Launch arg <c>--seed-spot=&lt;name&gt;</c>: puts the camera at a named spot of the seed world at the start (see <see cref="SeedSpots"/>).</summary>
public partial class SeedSpotDriver : Node
{
    private WorldView _view = null!;
    private FreeFlyCamera? _cam;
    private string _name = "";
    private bool _done;

    public static void Attach(WorldView view, Node? parent, string name) =>
        (parent ?? view).CallDeferred(Node.MethodName.AddChild, new SeedSpotDriver { Name = "SeedSpotDriver", _view = view, _name = name });

    public override void _Ready() => _cam = _view.Camera as FreeFlyCamera;

    public override void _Process(double delta)
    {
        if (_done || _cam == null || !_view.WorldRunning) return;
        _done = true;
        if (!SeedSpots.TryResolve(_view, _name, out var pose))
        {
            GD.PushError($"[SeedSpot] unknown spot '{_name}' (known: {string.Join(", ", SeedSpots.Names)}; the airport spots need a seed world with that airport)");
            return;
        }
        GD.Print($"[SeedSpot] {pose.Name}: lat {pose.Lat:0.00000} lon {pose.Lon:0.00000}, {pose.AltAglM:0} m AGL, heading {pose.HeadingDeg:0}, pitch {pose.PitchDeg:0}");
        SeedSpots.Teleport(_view, _cam, pose);
    }
}
