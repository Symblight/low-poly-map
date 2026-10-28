using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// <c>--hole-flight[=&lt;seed&gt;]</c>: an unattended mixed flight for hunting streaming bugs with <see cref="HoleDetector"/>. A seeded random walk on the free-fly camera
/// (autopilot heading, speed 30..250 m/s, altitude 120..3500 m AGL with climbs and descents), with occasional zoom (vertical fov 60 / 30 / 15 / 90 through
/// <see cref="WorldView.SetViewOverride"/>), teleports of 2..12 km and a settings reconfigure (lod tau / max distance) every few minutes. Options:
/// <c>--hole-segment=&lt;s&gt;</c> (mean segment length, default 12 s), <c>--hole-teleports=&lt;s&gt;</c> (mean time between teleports, default 70 s, 0 = none),
/// <c>--hole-reconfigure=&lt;s&gt;</c> (default 0 = off), <c>--hole-zoom=&lt;s&gt;</c> (mean time between fov changes, default 25 s, 0 = none), <c>--hole-hover</c> (stop moving
/// now and then to let the world settle; <c>--hole-radius=&lt;m&gt;</c> turns the flight back toward the start when it is farther away than that (stay inside the archive) and the idle timers run), <c>--hole-look</c> (low altitude 250..600 m AGL, fast yaw sweeps up to 180 deg/s, pitch swings and
/// Z-style zoom through the camera's base fov, i.e. the real set_view path with direction and fov changes every few seconds).
/// </summary>
public partial class HoleFlight : Node
{
    private static double Arg(string name, double def)
    {
        foreach (var a in OS.GetCmdlineUserArgs())
            if (a.StartsWith(name + "=", System.StringComparison.Ordinal) && double.TryParse(a.Substring(name.Length + 1), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var v)) return v;
        return def;
    }

    public static bool Requested(out int seed)
    {
        seed = 1;
        foreach (var a in OS.GetCmdlineUserArgs())
        {
            if (a == "--hole-flight") return true;
            if (a.StartsWith("--hole-flight=", System.StringComparison.Ordinal)) { int.TryParse(a.Substring(14), out seed); return true; }
        }
        return false;
    }

    public static void Attach(WorldView view, Node? parent, int seed) =>
        (parent ?? view).CallDeferred(Node.MethodName.AddChild, new HoleFlight { Name = "HoleFlight", _view = view, _rng = new RandomNumberGenerator { Seed = (ulong)seed } });

    /// <summary>Game time (seconds) of the last teleport or reconfigure, set for HoleMetrics (a first fill is not a hole).</summary>
    public static double DisruptionClock;
    /// <summary>Game time of the last abrupt change of the view direction or fov (a new segment, a zoom); the pixel test skips the frames after it.</summary>
    public static double ViewChangeClock;
    private double _clock;
    private WorldView _view = null!;
    private RandomNumberGenerator _rng = null!;
    private FreeFlyCamera? _cam;
    private double _radius, _homeCheck;
    private double _segLeft, _tpLeft, _zoomLeft, _reconfLeft, _hoverLeft;
    private float _targetAgl = 300f, _yaw;
    private double _segMean, _tpMean, _zoomMean, _reconfMean;
    private bool _hover, _toggle, _look;
    private double _lookLeft, _zoomHold;
    private float _yawRate, _pitchTarget, _pitch, _baseFov;
    private static readonly float[] Fovs = { 60f, 30f, 15f, 90f, 60f };
    private static readonly float[] Alts = { 120f, 250f, 400f, 600f, 900f, 1500f, 3500f };
    private static readonly float[] Speeds = { 30f, 80f, 150f, 250f };

    public override void _Ready()
    {
        ProcessPriority = 50;
        _cam = _view.Camera as FreeFlyCamera;
        _segMean = Arg("--hole-segment", 12);
        _tpMean = Arg("--hole-teleports", 70);
        _zoomMean = Arg("--hole-zoom", 25);
        _reconfMean = Arg("--hole-reconfigure", 0);
        _radius = Arg("--hole-radius", 0);
        _hover = System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--hole-hover");
        _look = System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--hole-look");
        if (_cam == null) { GD.PushError("[HoleFlight] the view's camera is not a FreeFlyCamera"); SetProcess(false); return; }
        _cam.Autopilot = true;
        _baseFov = _cam.BaseFov > 0 ? _cam.BaseFov : 60f;
        _tpLeft = Next(_tpMean);
        _zoomLeft = Next(_zoomMean);
        _reconfLeft = Next(_reconfMean);
        NewSegment();
        GD.Print($"[HoleFlight] seed {_rng.Seed}: segment {_segMean} s, teleports every ~{_tpMean} s, zoom ~{_zoomMean} s, reconfigure ~{_reconfMean} s, hover {_hover}");
    }

    private double Next(double mean) => mean <= 0 ? double.PositiveInfinity : mean * _rng.RandfRange(0.5f, 1.5f);

    private void NewSegment()
    {
        ViewChangeClock = _clock;
        _segLeft = Next(_segMean);
        _yaw += _rng.RandfRange(-1.4f, 1.4f);
        _targetAgl = _look ? _rng.RandfRange(250f, 600f) : Alts[_rng.RandiRange(0, Alts.Length - 1)];
        if (_hover && _rng.Randf() < 0.25f) { _hoverLeft = _rng.RandfRange(8f, 30f); _cam!.Speed = 0f; }
        else { _hoverLeft = 0; _cam!.Speed = Speeds[_rng.RandiRange(0, Speeds.Length - 1)]; }
        _pitch = Mathf.DegToRad(_rng.RandfRange(-35f, 0f));
        _cam.SetLook(_yaw, _pitch);
    }

    public override void _Process(double delta)
    {
        var cam = _cam!;
        _clock += delta;
        _segLeft -= delta;
        if (_segLeft <= 0) NewSegment();
        // Height: climb or descend toward the target above the ground the view measured under the camera.
        var p = cam.GlobalPosition;
        float want = (float)_view.CameraGroundHeight + _targetAgl;
        p.Y += Mathf.Clamp(want - p.Y, -120f * (float)delta, 120f * (float)delta);
        cam.GlobalPosition = p;

        if (_radius > 0 && (_homeCheck -= delta) <= 0)
        {
            _homeCheck = 1.0;
            var w = cam.GlobalPosition + _view.TotalShift; // world position relative to the start origin
            if (new Vector2(w.X, w.Z).Length() > _radius)
            {
                _yaw = Mathf.Atan2(w.X, w.Z); // forward = (-sin yaw, 0, -cos yaw) points at the origin
                _segLeft = System.Math.Max(_segLeft, 8.0);
                if (_cam!.Speed < 30f) _cam.Speed = 80f;
            }
        }
        if (_look) Look(delta, cam);
        _tpLeft -= delta;
        if (_tpLeft <= 0)
        {
            _tpLeft = Next(_tpMean);
            float ang = _rng.RandfRange(0f, Mathf.Tau), dist = _rng.RandfRange(2000f, 12000f);
            cam.GlobalPosition = cam.GlobalPosition + new Vector3(Mathf.Cos(ang) * dist, 0f, Mathf.Sin(ang) * dist);
            DisruptionClock = _clock;
            GD.Print($"[HoleFlight] teleport {dist:0} m");
        }
        _zoomLeft -= delta;
        if (_zoomLeft <= 0 && _look)
        {
            _zoomLeft = Next(_zoomMean);
            _zoomHold = _rng.RandfRange(3f, 8f);
            ViewChangeClock = _clock;
            cam.BaseFov = _baseFov / FreeFlyCamera.ZoomFactor; // what holding Z does
            GD.Print($"[HoleFlight] zoom x{FreeFlyCamera.ZoomFactor} for {_zoomHold:0.0} s");
        }
        else if (_zoomLeft <= 0)
        {
            _zoomLeft = Next(_zoomMean); // an override changes the LOD view only, not the picture: no ViewChangeClock
            float fov = Fovs[_rng.RandiRange(0, Fovs.Length - 1)];
            _view.SetViewOverride(fov, 1080);
            GD.Print($"[HoleFlight] view fov {fov}");
        }
        _reconfLeft -= delta;
        if (_reconfLeft <= 0)
        {
            _reconfLeft = Next(_reconfMean);
            _toggle = !_toggle;
            var o = new Godot.Collections.Dictionary { { "lod", new Godot.Collections.Dictionary { { "tau_px", _toggle ? 3.0 : 1.5 } } } };
            DisruptionClock = _clock;
            _view.ApplyProfileOverrides(o);
            GD.Print($"[HoleFlight] reconfigure tau_px {(_toggle ? 3.0 : 1.5)}");
        }
    }

    /// <summary>Mouse-look stand-in: yaw sweeps at 15..180 deg/s, pitch swinging between -45 and +10 degrees, fov back from the zoom.</summary>
    private void Look(double delta, FreeFlyCamera cam)
    {
        _lookLeft -= delta;
        if (_lookLeft <= 0)
        {
            _lookLeft = _rng.RandfRange(1.5f, 5f);
            float[] rates = { 0f, 15f, 60f, 120f, 180f };
            _yawRate = Mathf.DegToRad(rates[_rng.RandiRange(0, rates.Length - 1)]) * (_rng.Randf() < 0.5f ? -1f : 1f);
            _pitchTarget = Mathf.DegToRad(_rng.RandfRange(-45f, 10f));
        }
        _yaw += _yawRate * (float)delta;
        _pitch = Mathf.MoveToward(_pitch, _pitchTarget, Mathf.DegToRad(90f) * (float)delta);
        cam.SetLook(_yaw, _pitch);
        if (_zoomHold > 0 && (_zoomHold -= delta) <= 0) cam.BaseFov = _baseFov;
    }
}
