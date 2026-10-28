using System;

namespace LowPolyMap.Diagnostics;

/// <summary>Where the benchmark camera is at one instant: metres east / north of the world origin (Vilnius centre), height above
/// ground, compass heading and pitch in radians (negative looks down), vertical field of view in degrees.</summary>
public readonly record struct FlightPose(double East, double North, double AglM, double HeadingRad, double PitchRad, double FovDeg);

/// <summary>A scripted camera path: a pure function of the simulated time since the start, so every run flies the same line.</summary>
public interface IFlightPath
{
    /// <summary>One line for the result file.</summary>
    string Describe { get; }

    /// <summary>True when the path changes the field of view (the view sent to the library must then follow the camera's fov).</summary>
    bool VariesFov { get; }

    FlightPose At(double seconds);
}

/// <summary>
/// A closed racetrack over Vilnius: a straight leg north through the centre, a half circle to the right, a leg south, a half circle
/// back, repeated forever (the first leg alone is "250 m/s north through the centre"; a long run loops over the same ground instead of
/// leaving the data). Optionally the height above ground cycles between two values at a constant climb rate.
/// </summary>
public sealed class RacetrackPath : IFlightPath
{
    const double LegM = 15000.0, TurnRadiusM = 2000.0;
    const double LevelPitchRad = -0.12, ClimbPitchRad = -0.24;

    readonly double _speed, _agl, _aglHigh, _climb;
    readonly double _lap;

    /// <param name="speed">Ground speed, m/s.</param>
    /// <param name="aglLow">Height above ground, m (the floor of the climb cycle).</param>
    /// <param name="aglHigh">Ceiling of the climb cycle, m; ignored without a climb.</param>
    /// <param name="climbRate">Vertical speed of the climb cycle, m/s; 0 = level flight at <paramref name="aglLow"/>.</param>
    public RacetrackPath(double speed, double aglLow, double aglHigh = 0, double climbRate = 0)
    {
        _speed = speed;
        _agl = aglLow;
        _aglHigh = aglHigh;
        _climb = climbRate;
        _lap = 2 * LegM + 2 * Math.PI * TurnRadiusM;
    }

    public bool VariesFov => false;

    public string Describe => _climb > 0
        ? $"racetrack {LegM / 1000:0.#} km legs, {_speed:0} m/s, height above ground {_agl:0}..{_aglHigh:0} m at {_climb:0} m/s"
        : $"racetrack {LegM / 1000:0.#} km legs, {_speed:0} m/s level at {_agl:0} m above ground";

    public FlightPose At(double seconds)
    {
        double s = _speed * seconds % _lap;
        double half = LegM / 2, arc = Math.PI * TurnRadiusM;
        double east, north, heading;
        if (s < LegM)                       // north through the centre, x = 0
        {
            east = 0; north = -half + s; heading = 0;
        }
        else if (s < LegM + arc)            // right-hand half circle around (R, +half)
        {
            double a = (s - LegM) / TurnRadiusM;
            east = TurnRadiusM - TurnRadiusM * Math.Cos(a); north = half + TurnRadiusM * Math.Sin(a); heading = a;
        }
        else if (s < 2 * LegM + arc)        // south along x = 2R
        {
            east = 2 * TurnRadiusM; north = half - (s - LegM - arc); heading = Math.PI;
        }
        else                                // half circle around (R, -half) back to the start
        {
            double a = (s - 2 * LegM - arc) / TurnRadiusM;
            east = 2 * TurnRadiusM - TurnRadiusM + TurnRadiusM * Math.Cos(a); north = -half - TurnRadiusM * Math.Sin(a); heading = Math.PI + a;
        }
        return new FlightPose(east, north, Agl(seconds), heading, _climb > 0 ? ClimbPitchRad : LevelPitchRad, 60);
    }

    /// <summary>Height above ground: constant, or a triangle wave between the low and the high value.</summary>
    double Agl(double seconds)
    {
        if (_climb <= 0) return _agl;
        double span = _aglHigh - _agl;
        double x = _climb * seconds % (2 * span);
        return _agl + (x < span ? x : 2 * span - x);
    }
}

/// <summary>Hovers over the centre of Vilnius looking down at 20 degrees while turning once round per minute; the field of view
/// goes 60 -> 15 -> 60 degrees every 40 s, so the screen-space LOD (set_view) is exercised by the camera's own fov.</summary>
public sealed class ZoomPath : IFlightPath
{
    const double HeightM = 1000.0, PeriodS = 40.0, TurnPeriodS = 60.0, FovWide = 60.0, FovNarrow = 15.0;

    public bool VariesFov => true;

    public string Describe => $"hover at {HeightM:0} m above the centre, one turn per {TurnPeriodS:0} s, fov {FovWide:0} -> {FovNarrow:0} -> {FovWide:0} deg every {PeriodS:0} s";

    public FlightPose At(double seconds)
    {
        double fov = (FovWide + FovNarrow) / 2 + (FovWide - FovNarrow) / 2 * Math.Cos(2 * Math.PI * seconds / PeriodS);
        return new FlightPose(0, 0, HeightM, 2 * Math.PI * seconds / TurnPeriodS, -Math.PI / 9, fov);
    }
}
