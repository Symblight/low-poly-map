using Godot;

namespace LowPolyMap;

/// <summary>
/// WASD + right-mouse look free-fly camera. Speed is set with the mouse wheel and has no built-in cap: the game decides
/// how fast it may go (<see cref="MaxSpeed"/> is unlimited unless the host or launch arg <c>--max-speed=&lt;m/s&gt;</c>
/// sets one). Shift multiplies the speed by 4, Ctrl ("turbo") by 10. The streaming budgets were tested up to
/// <see cref="TestedSpeed"/> m/s; faster flight can show holes and late tiles.
/// F toggles an autopilot: straight, level flight along the current horizontal heading at the
/// current Speed (mouse look still steers). Launch arg <c>--autopilot=&lt;speed_mps&gt;</c> starts it
/// flying north at 300 m AGL. PgUp / PgDn double / halve the altitude (instant jump, for LOD tests).
/// Launch arg <c>--autopilot-climb=&lt;m/s&gt;</c> climbs straight up at that vertical speed to 8000 m and then
/// descends back down to 100 m, repeating (combine with <c>--autopilot=</c> for a diagonal climb).
/// </summary>
public partial class FreeFlyCamera : Camera3D
{
    /// <summary>Streaming budgets (look-ahead, fades, tile queues) were tested up to this speed. Informational only.</summary>
    public const float TestedSpeed = 250f;
    public const float ShiftBoost = 4f;
    public const float TurboBoost = 10f;
    private const string MaxSpeedArg = "--max-speed=";
    /// <summary>Optional speed cap in m/s. Unlimited by default; the game (or <c>--max-speed=</c>) may set one.</summary>
    public static float MaxSpeed { get; set; } = float.PositiveInfinity;

    [Export(PropertyHint.Range, "0,1000,1,or_greater")] public float Speed { get; set; } = 80f;
    [Export] public float MouseSensitivity { get; set; } = 0.003f;
    /// <summary>Terrain height under the camera. StreamerBridge updates it every frame from WorldStreamer.ground_height_at (0 until a DEM exists).</summary>
    [Export] public float GroundY { get; set; } = 0f;

    public const float AutopilotAltitudeM = 300f;
    private const string AutopilotArg = "--autopilot=";
    private const string ClimbArg = "--autopilot-climb=";
    public const float ClimbCeilingM = 8000f;
    public const float ClimbFloorM = 100f;
    public const float MinAltitudeM = 20f;
    public const float MaxAltitudeM = 30000f;

    public Vector3 Velocity { get; private set; }
    public bool Autopilot { get; private set; }
    /// <summary>Vertical speed of the climb autopilot in m/s (0 = off).</summary>
    public float ClimbRate { get; private set; }
    private float _climbDir = 1f;
    public double AltitudeAgl => GlobalPosition.Y - GroundY;

    private float _yaw;
    private float _pitch;
    private bool _looking;

    public override void _Ready()
    {
        var e = Rotation;
        _yaw = e.Y;
        _pitch = e.X;

        // The cap first: --autopilot= below clamps against it, whatever the argument order.
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (!arg.StartsWith(MaxSpeedArg, System.StringComparison.Ordinal)) continue;
            if (!float.TryParse(arg.Substring(MaxSpeedArg.Length), System.Globalization.NumberStyles.Float,
                    System.Globalization.CultureInfo.InvariantCulture, out var ms) || ms <= 0f) continue;
            MaxSpeed = ms;
            GD.Print($"[FreeFlyCamera] max speed capped at {MaxSpeed} m/s");
        }

        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith(ClimbArg, System.StringComparison.Ordinal))
            {
                if (float.TryParse(arg.Substring(ClimbArg.Length), System.Globalization.NumberStyles.Float,
                        System.Globalization.CultureInfo.InvariantCulture, out var cr) && cr > 0f)
                {
                    ClimbRate = cr;
                    GD.Print($"[FreeFlyCamera] climb autopilot: {cr} m/s up to {ClimbCeilingM} m, then down to {ClimbFloorM} m");
                }
                continue;
            }
            if (!arg.StartsWith(AutopilotArg, System.StringComparison.Ordinal)) continue;
            if (!float.TryParse(arg.Substring(AutopilotArg.Length), System.Globalization.NumberStyles.Float,
                    System.Globalization.CultureInfo.InvariantCulture, out var v)) v = Speed;
            Speed = Mathf.Clamp(v, 0f, MaxSpeed);
            if (v > MaxSpeed) GD.Print($"[FreeFlyCamera] --autopilot speed {v} clamped to --max-speed {MaxSpeed} m/s");
            // Fly north (-Z), level, at 300 m AGL.
            _yaw = 0f;
            _pitch = -0.12f;
            Rotation = new Vector3(_pitch, _yaw, 0f);
            var p = GlobalPosition;
            GlobalPosition = new Vector3(p.X, GroundY + AutopilotAltitudeM, p.Z);
            Autopilot = true;
            GD.Print($"[FreeFlyCamera] autopilot on: north at {Speed} m/s, {AutopilotAltitudeM} m AGL");
        }
    }

    public override void _UnhandledInput(InputEvent @event)
    {
        switch (@event)
        {
            case InputEventKey { Pressed: true, Echo: false, PhysicalKeycode: Key.F }:
                Autopilot = !Autopilot;
                GD.Print($"[FreeFlyCamera] autopilot {(Autopilot ? "on" : "off")} at {Speed} m/s");
                break;
            case InputEventKey { Pressed: true, Echo: false, PhysicalKeycode: Key.Pageup }:
                SetAltitude(Mathf.Max(AltitudeAgl, MinAltitudeM) * 2f);
                break;
            case InputEventKey { Pressed: true, Echo: false, PhysicalKeycode: Key.Pagedown }:
                SetAltitude(AltitudeAgl * 0.5f);
                break;
            case InputEventMouseButton mb:
                if (mb.ButtonIndex == MouseButton.Right)
                {
                    _looking = mb.Pressed;
                    Input.MouseMode = _looking ? Input.MouseModeEnum.Captured : Input.MouseModeEnum.Visible;
                }
                else if (mb.Pressed && mb.ButtonIndex == MouseButton.WheelUp)
                    Speed = Mathf.Clamp(Speed * 1.25f + 2f, 0f, MaxSpeed);
                else if (mb.Pressed && mb.ButtonIndex == MouseButton.WheelDown)
                    Speed = Mathf.Clamp(Speed / 1.25f - 2f, 0f, MaxSpeed);
                break;
            case InputEventMouseMotion mm when _looking:
                _yaw -= mm.Relative.X * MouseSensitivity;
                _pitch = Mathf.Clamp(_pitch - mm.Relative.Y * MouseSensitivity, -1.55f, 1.55f);
                Rotation = new Vector3(_pitch, _yaw, 0f);
                break;
        }
    }

    /// <summary>Jumps to the given altitude above ground (clamped), keeping x/z and orientation.</summary>
    public void SetAltitude(double altAgl)
    {
        altAgl = System.Math.Clamp(altAgl, MinAltitudeM, MaxAltitudeM);
        var p = GlobalPosition;
        GlobalPosition = new Vector3(p.X, GroundY + (float)altAgl, p.Z);
        GD.Print($"[FreeFlyCamera] altitude -> {altAgl:0} m AGL");
    }

    public override void _Process(double delta)
    {
        if (ClimbRate > 0f)
        {
            var fwd = -GlobalTransform.Basis.Z;
            fwd.Y = 0f;
            fwd = fwd.LengthSquared() > 1e-6f ? fwd.Normalized() : Vector3.Forward;
            var horiz = Autopilot ? fwd * Mathf.Min(Speed, MaxSpeed) : Vector3.Zero;
            float alt = (float)AltitudeAgl;
            if (_climbDir > 0f && alt >= ClimbCeilingM) { _climbDir = -1f; GD.Print($"[FreeFlyCamera] ceiling reached ({alt:0} m), descending"); }
            else if (_climbDir < 0f && alt <= ClimbFloorM) { _climbDir = 1f; GD.Print($"[FreeFlyCamera] floor reached ({alt:0} m), climbing"); }
            Velocity = horiz + Vector3.Up * (ClimbRate * _climbDir);
            GlobalPosition += Velocity * (float)delta;
            return;
        }
        if (Autopilot)
        {
            // Horizontal projection of the view direction, constant altitude.
            var fwd = -GlobalTransform.Basis.Z;
            fwd.Y = 0f;
            fwd = fwd.LengthSquared() > 1e-6f ? fwd.Normalized() : Vector3.Forward;
            Velocity = fwd * Mathf.Min(Speed, MaxSpeed);
            GlobalPosition += Velocity * (float)delta;
            return;
        }

        var dir = Vector3.Zero;
        var basis = GlobalTransform.Basis;
        if (Input.IsPhysicalKeyPressed(Key.W)) dir -= basis.Z;
        if (Input.IsPhysicalKeyPressed(Key.S)) dir += basis.Z;
        if (Input.IsPhysicalKeyPressed(Key.D)) dir += basis.X;
        if (Input.IsPhysicalKeyPressed(Key.A)) dir -= basis.X;
        if (Input.IsPhysicalKeyPressed(Key.E)) dir += Vector3.Up;
        if (Input.IsPhysicalKeyPressed(Key.Q)) dir -= Vector3.Up;

        var boost = Input.IsPhysicalKeyPressed(Key.Ctrl) ? TurboBoost : Input.IsPhysicalKeyPressed(Key.Shift) ? ShiftBoost : 1f;
        Velocity = dir.LengthSquared() > 0f ? dir.Normalized() * Mathf.Min(Speed * boost, MaxSpeed) : Vector3.Zero;
        GlobalPosition += Velocity * (float)delta;
    }
}
