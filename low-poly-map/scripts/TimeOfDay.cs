using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Simulated clock and sun for the spike (host side, see docs/spec-library-packaging.md "Test bench: daylight menu").
/// Owns date, local time and clock speed; computes sun elevation / azimuth at the streamer origin (NOAA approximation),
/// points <see cref="Sun"/> along the sun (or a fixed moon direction at night) and writes
/// <see cref="WorldView.SetTime"/> (night factor 0 day ... 1 night, smooth between sun elevation +6° and -6°).
/// Interim until Rust exports <c>set_clock</c> / <c>daylight()</c>: then <see cref="Update"/> delegates to them and keeps
/// the same public surface. <see cref="OverrideNight"/> pins the night factor for look tuning.
/// Launch args: <c>--time=HH:MM</c>, <c>--date=MM-DD</c>, <c>--time-speed=&lt;x&gt;</c>, <c>--night=&lt;0..1&gt;</c> (= override).
/// </summary>
public partial class TimeOfDay : Node
{
    /// <summary>Night factor is 0 above this sun elevation and 1 below its negative (civil twilight).</summary>
    public const double TwilightDeg = 6.0;
    /// <summary>The sun light never comes from below this elevation (no light through the ground at dusk).</summary>
    private const double MinLightElevationDeg = 2.0;
    /// <summary>Night light: a fixed moon opposite the sun's azimuth.</summary>
    private const double MoonElevationDeg = 35.0;
    private const int Year = 2026;

    [Export] public WorldView? Bridge { get; set; }
    [Export] public DirectionalLight3D? Sun { get; set; }

    /// <summary>Local time in hours, 0..24.</summary>
    [Export(PropertyHint.Range, "0,24,0.01")] public double LocalHour { get; set; } = 22.0;
    /// <summary>Day of year, 1..365 (non-leap year). Default 26 Oct.</summary>
    [Export(PropertyHint.Range, "1,365,1")] public int DayOfYear { get; set; } = 299;
    /// <summary>Simulated seconds per real second; 0 = paused.</summary>
    [Export] public double TimeSpeed { get; set; } = 0.0;
    /// <summary>UTC offset of local time in hours. NaN = derived from the origin longitude (round(lon / 15)).</summary>
    [Export] public double UtcOffsetHours { get; set; } = double.NaN;

    [Export] public bool OverrideNight { get; set; }
    [Export(PropertyHint.Range, "0,1,0.01")] public float NightOverride { get; set; } = 1f;

    // Outputs (the shape of the planned daylight() result).
    public double SunElevationDeg { get; private set; }
    public double SunAzimuthDeg { get; private set; }
    public float SunNightFactor { get; private set; }
    /// <summary>Night factor in effect (the override when on, else from the sun).</summary>
    public float NightFactor { get; private set; }
    public double Latitude => Bridge?.OriginLat ?? 54.6872;
    public double Longitude => Bridge?.OriginLon ?? 25.2797;
    public double UtcOffset => double.IsNaN(UtcOffsetHours) ? System.Math.Round(Longitude / 15.0) : UtcOffsetHours;

    public override void _Ready()
    {
        ProcessPriority = -1; // before WorldView / NightRenderController read the night factor this frame
        var inv = System.Globalization.CultureInfo.InvariantCulture;
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith("--time=", System.StringComparison.Ordinal))
            {
                var p = arg.Substring(7).Split(':');
                if (int.TryParse(p[0], out var h))
                    LocalHour = Mathf.Clamp(h + (p.Length > 1 && int.TryParse(p[1], out var m) ? m / 60.0 : 0.0), 0.0, 24.0);
            }
            else if (arg.StartsWith("--date=", System.StringComparison.Ordinal))
            {
                var p = arg.Substring(7).Split('-');
                if (p.Length == 2 && int.TryParse(p[0], out var mo) && int.TryParse(p[1], out var d) && mo is >= 1 and <= 12)
                    DayOfYear = new System.DateTime(Year, mo, System.Math.Clamp(d, 1, System.DateTime.DaysInMonth(Year, mo))).DayOfYear;
            }
            else if (arg.StartsWith("--time-speed=", System.StringComparison.Ordinal) && double.TryParse(arg.Substring(13), System.Globalization.NumberStyles.Float, inv, out var s))
                TimeSpeed = s;
            else if (arg.StartsWith("--night=", System.StringComparison.Ordinal) && float.TryParse(arg.Substring(8), System.Globalization.NumberStyles.Float, inv, out var nf))
            {
                OverrideNight = true;
                NightOverride = Mathf.Clamp(nf, 0f, 1f);
            }
        }
        Update();
    }

    public override void _Process(double delta)
    {
        if (TimeSpeed != 0.0)
        {
            LocalHour += delta * TimeSpeed / 3600.0;
            while (LocalHour >= 24.0) { LocalHour -= 24.0; DayOfYear = DayOfYear % 365 + 1; }
            while (LocalHour < 0.0) { LocalHour += 24.0; DayOfYear = (DayOfYear + 363) % 365 + 1; }
        }
        Update();
    }

    /// <summary>Local hour of the first 1-minute step where the sun crosses <paramref name="elevDeg"/> (rising or setting) on
    /// the current date; -1 if it never does (polar day / night).</summary>
    public double HourOfElevation(double elevDeg, bool rising)
    {
        SolarPosition(DayOfYear, 0.0, UtcOffset, Latitude, Longitude, out var prev, out _);
        for (int m = 1; m <= 24 * 60; m++)
        {
            double h = m / 60.0;
            SolarPosition(DayOfYear, h, UtcOffset, Latitude, Longitude, out var e, out _);
            if (rising ? prev < elevDeg && e >= elevDeg : prev > elevDeg && e <= elevDeg) return h;
            prev = e;
        }
        return -1.0;
    }

    /// <summary>Local hour of the highest (<paramref name="highest"/>) or lowest sun on the current date, 1-minute resolution.</summary>
    public double HourOfExtremeSun(bool highest)
    {
        double best = highest ? double.MinValue : double.MaxValue, bestH = 0.0;
        for (int m = 0; m < 24 * 60; m++)
        {
            SolarPosition(DayOfYear, m / 60.0, UtcOffset, Latitude, Longitude, out var e, out _);
            if (highest ? e > best : e < best) { best = e; bestH = m / 60.0; }
        }
        return bestH;
    }

    /// <summary>Formatted date, e.g. "26 Oct".</summary>
    public string DateText => new System.DateTime(Year, 1, 1).AddDays(DayOfYear - 1).ToString("d MMM", System.Globalization.CultureInfo.InvariantCulture);

    /// <summary>Formatted local time, e.g. "22:05".</summary>
    public string TimeText
    {
        get
        {
            int min = (int)System.Math.Round(LocalHour * 60.0) % (24 * 60);
            return $"{min / 60:00}:{min % 60:00}";
        }
    }

    private void Update()
    {
        SolarPosition(DayOfYear, LocalHour, UtcOffset, Latitude, Longitude, out var elev, out var az);
        SunElevationDeg = elev;
        SunAzimuthDeg = az;
        SunNightFactor = 1f - Mathf.SmoothStep((float)-TwilightDeg, (float)TwilightDeg, (float)elev);
        NightFactor = OverrideNight ? NightOverride : SunNightFactor;
        Bridge?.SetTime((float)LocalHour, NightFactor);
        if (Sun != null) AimLight(elev, az, NightFactor);
    }

    /// <summary>Points the light from the sun by day and from the moon at night, blended by the night factor.</summary>
    private void AimLight(double elevDeg, double azDeg, float night)
    {
        var toSun = Direction(System.Math.Max(elevDeg, MinLightElevationDeg), azDeg);
        var toMoon = Direction(MoonElevationDeg, azDeg + 180.0);
        var to = toSun.Slerp(toMoon, night).Normalized();
        // A DirectionalLight3D shines along its -Z.
        var up = Mathf.Abs(to.Y) > 0.99f ? Vector3.Forward : Vector3.Up;
        var t = Sun!.GlobalTransform;
        t.Basis = Basis.LookingAt(-to, up);
        Sun.GlobalTransform = t;
    }

    /// <summary>Unit vector toward elevation / azimuth (azimuth from north, clockwise) in Godot axes (x east, z = -north).</summary>
    private static Vector3 Direction(double elevDeg, double azDeg)
    {
        double e = Mathf.DegToRad(elevDeg), a = Mathf.DegToRad(azDeg);
        return new Vector3((float)(System.Math.Sin(a) * System.Math.Cos(e)), (float)System.Math.Sin(e), (float)(-System.Math.Cos(a) * System.Math.Cos(e)));
    }

    /// <summary>NOAA general solar position (accurate to ~0.5°, plenty for lighting).</summary>
    public static void SolarPosition(int dayOfYear, double localHour, double utcOffset, double latDeg, double lonDeg,
        out double elevationDeg, out double azimuthDeg)
    {
        double g = 2.0 * System.Math.PI / 365.0 * (dayOfYear - 1 + (localHour - utcOffset - 12.0) / 24.0);
        double eqTimeMin = 229.18 * (0.000075 + 0.001868 * System.Math.Cos(g) - 0.032077 * System.Math.Sin(g)
            - 0.014615 * System.Math.Cos(2 * g) - 0.040849 * System.Math.Sin(2 * g));
        double decl = 0.006918 - 0.399912 * System.Math.Cos(g) + 0.070257 * System.Math.Sin(g) - 0.006758 * System.Math.Cos(2 * g)
            + 0.000907 * System.Math.Sin(2 * g) - 0.002697 * System.Math.Cos(3 * g) + 0.00148 * System.Math.Sin(3 * g);
        double trueSolarMin = localHour * 60.0 + eqTimeMin + 4.0 * lonDeg - 60.0 * utcOffset;
        double ha = Mathf.DegToRad(trueSolarMin / 4.0 - 180.0);
        double lat = Mathf.DegToRad(latDeg);
        double sinElev = System.Math.Sin(lat) * System.Math.Sin(decl) + System.Math.Cos(lat) * System.Math.Cos(decl) * System.Math.Cos(ha);
        elevationDeg = Mathf.RadToDeg(System.Math.Asin(System.Math.Clamp(sinElev, -1.0, 1.0)));
        // Azimuth from south (westward positive), then shifted to "from north, clockwise".
        double azSouth = System.Math.Atan2(System.Math.Sin(ha), System.Math.Cos(ha) * System.Math.Sin(lat) - System.Math.Tan(decl) * System.Math.Cos(lat));
        azimuthDeg = (Mathf.RadToDeg(azSouth) + 180.0 + 360.0) % 360.0;
    }
}
