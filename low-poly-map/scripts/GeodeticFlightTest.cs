using System.Collections.Generic;
using System.Globalization;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Host-only test driver (user arg <c>--geodetic-test</c>): replaces the FreeFlyCamera pose source and flies a scripted path
/// that is fed to the library only as geodetic input (P4 acceptance: no coordinate code in C#).
/// <para>
/// Path (distances scale with nothing, durations with the speed): north <c>PathNorthM</c> at <c>--geodetic-speed</c> (default 120 m/s),
/// then east <c>PathEastM</c>, then a climb of <c>PathClimbM</c> at a quarter of the speed. Velocity goes in as NED.
/// Each frame: the path position is a plain NED displacement (flight-model state, meters travelled), turned into geodetic
/// terms by the library (<c>local_to_geodetic</c> of <c>geodetic_to_local(start)</c> plus the displacement; this one axis
/// mapping line is the only host-side "coordinate" knowledge, standing in for a flight model that integrates geodetically),
/// sent with <c>update_camera_geodetic</c>; the Camera3D is placed ONLY from <c>geodetic_to_local</c>.
/// </para>
/// <para>
/// Checks: the camera stays continuous across every origin shift (within 1 m after applying the shift), the camera equals
/// <c>geodetic_to_local</c> of the flown position after the bridge applied the shift, every held tile root still sits on its
/// <c>debug_tiles</c> rect, the origin_shifted signal agrees with take_origin_shift, tiles stay loaded, status is not
/// error/offline. Headless (or any run) quits when the path ends: exit code 0 on PASS, 1 on FAIL, with one summary line.
/// Other args: <c>--geodetic-dt=&lt;s&gt;</c> (fixed simulated step, default 0.1), <c>--geodetic-fps=&lt;n&gt;</c> (frame cap, default 60),
/// <c>--geodetic-timeout=&lt;s&gt;</c> (real-time failsafe, default 300).
/// </para>
/// </summary>
public partial class GeodeticFlightTest : Node
{
    private const string TestArg = "--geodetic-test";
    private const string SpeedArg = "--geodetic-speed=";
    private const string DtArg = "--geodetic-dt=";
    private const string FpsArg = "--geodetic-fps=";
    private const string TimeoutArg = "--geodetic-timeout=";

    private const double PathNorthM = 7200.0;
    private const double PathEastM = 2400.0;
    private const double PathClimbM = 600.0;
    private const double StartAltMslM = 300.0;
    private const double JumpLimitM = 1.0;
    private const double TileLimitM = 1.0;
    private const int TileCheckEveryFrames = 30;

    [Export] public WorldView? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }

    private bool _enabled;
    private bool _started;
    private bool _finished;
    private double _speed = 120.0;
    private double _dt = 0.1;
    private double _timeout = 300.0;
    private double _startedAt;

    // Path state: meters travelled, not coordinates.
    private int _phase;
    private double _phaseDist;
    private double _north, _east, _up;
    private double _simTime;
    private double _stepLen;
    private bool _pathDone;
    private Vector3 _velNed;

    // Per-frame check state.
    private Vector3 _geo;
    private bool _stepped;
    private bool _havePrev;
    private Vector3 _prevCam;
    private int _frames;
    private double _maxJump;
    private double _maxConsistency;
    private double _maxTileErr;
    private int _tilesChecked;
    private int _tileMismatches;
    private int _shiftFrames;
    private int _shiftChecksFailed;
    private int _loadedBeforeShift;
    private readonly List<string> _failures = new();
    private readonly List<DebugTile> _tiles = new(256);

    public override void _Ready()
    {
        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg == TestArg) _enabled = true;
            else if (Num(arg, SpeedArg, out var v)) _speed = System.Math.Max(v, 1.0);
            else if (Num(arg, DtArg, out v)) _dt = System.Math.Clamp(v, 0.005, 1.0);
            else if (Num(arg, FpsArg, out v)) Engine.MaxFps = (int)v;
            else if (Num(arg, TimeoutArg, out v)) _timeout = v;
        }
        ProcessPriority = 1; // after WorldView (0): its shift handling is done when the checks run
        if (!_enabled) { SetProcess(false); return; }
        if (Engine.MaxFps == 0) Engine.MaxFps = 60; // leave the workers real time to deliver tiles
        GD.Print($"[GeodeticFlightTest] enabled: speed {_speed} m/s, dt {_dt} s, north {PathNorthM} m, east {PathEastM} m, climb {PathClimbM} m");
    }

    private static bool Num(string arg, string prefix, out double v)
    {
        v = 0;
        return arg.StartsWith(prefix, System.StringComparison.Ordinal)
            && double.TryParse(arg.Substring(prefix.Length), NumberStyles.Float, CultureInfo.InvariantCulture, out v);
    }

    private void Start()
    {
        _started = true;
        _startedAt = Time.GetTicksMsec() / 1000.0;
        var s = Bridge?.Streamer;
        if (Bridge == null || Camera == null || s == null || !s.IsAvailable) { Fail("bridge/camera/WorldStreamer unavailable"); return; }
        if (!s.HasHostApi) { Fail($"library api {s.ApiVersionText} has no geodetic API (needs 1.0)"); return; }
        if (!Bridge.WorldRunning) { Fail("world not configured: " + Bridge.ConfigureError); return; }
        Camera.SetProcess(false);
        Camera.SetProcessUnhandledInput(false);
        Bridge.ExternalCameraUpdate = Step;
        Camera.Rotation = new Vector3(-0.1f, 0f, 0f);
    }

    /// <summary>Called by the bridge instead of update_camera, before take_origin_shift. Camera is placed in the pre-shift frame.</summary>
    private void Step(double delta)
    {
        var s = Bridge!.Streamer;
        if (_pathDone) { s.UpdateCameraGeodetic(_geo.X, _geo.Y, _geo.Z, Vector3.Zero); return; }
        double dt = _dt;
        double rate = _phase == 2 ? _speed * 0.25 : _speed;
        double total = _phase == 0 ? PathNorthM : _phase == 1 ? PathEastM : PathClimbM;
        double adv = System.Math.Min(rate * dt, total - _phaseDist);
        _phaseDist += adv;
        _stepLen = adv;
        _simTime += dt;
        switch (_phase)
        {
            case 0: _north += adv; _velNed = new Vector3((float)rate, 0f, 0f); break;
            case 1: _east += adv; _velNed = new Vector3(0f, (float)rate, 0f); break;
            default: _up += adv; _velNed = new Vector3(0f, 0f, -(float)rate); break;
        }
        if (_phaseDist >= total - 1e-9)
        {
            _phase++;
            _phaseDist = 0;
            if (_phase >= 3) _pathDone = true;
            else if (_phase == 1) Camera!.Rotation = new Vector3(-0.1f, -Mathf.Pi / 2f, 0f);
            else Camera!.Rotation = new Vector3(0.3f, -Mathf.Pi / 2f, 0f);
        }

        // Flown position in geodetic terms (library conversions only), then the camera from geodetic_to_local only.
        var anchor = s.GeodeticToLocal(Bridge.OriginLat, Bridge.OriginLon, StartAltMslM);
        _geo = s.LocalToGeodetic(anchor + new Vector3((float)_east, (float)_up, -(float)_north));
        Camera!.GlobalPosition = s.GeodeticToLocal(_geo.X, _geo.Y, _geo.Z);
        s.UpdateCameraGeodetic(_geo.X, _geo.Y, _geo.Z, _velNed);
        _stepped = true;
    }

    public override void _Process(double delta)
    {
        if (_finished) return;
        if (!_started) { Start(); if (_finished) return; }
        if (Time.GetTicksMsec() / 1000.0 - _startedAt > _timeout) { Fail($"real-time timeout {_timeout} s"); Finish(); return; }
        if (!_stepped) return;
        _stepped = false;

        var bridge = Bridge!;
        var s = bridge.Streamer;
        var cam = Camera!.GlobalPosition;
        _frames++;

        // The camera must be exactly the flown position in the post-shift frame.
        double consistency = (cam - s.GeodeticToLocal(_geo.X, _geo.Y, _geo.Z)).Length();
        if (consistency > _maxConsistency) _maxConsistency = consistency;
        double jump = consistency;
        if (_havePrev)
        {
            double step = (cam - (_prevCam - bridge.LastShift)).Length();
            jump = System.Math.Max(jump, System.Math.Abs(step - _stepLen));
        }
        if (jump > _maxJump) _maxJump = jump;
        _prevCam = cam;
        _havePrev = true;

        bool shifted = bridge.LastShift != Vector3.Zero;
        if (shifted)
        {
            _shiftFrames++;
            GD.Print($"[GeodeticFlightTest] origin shift #{bridge.OriginShiftCount} {bridge.LastShift} at t={_simTime:0.0}s, loaded tiles {bridge.TotalLoaded}, camera {cam}");
            if (bridge.SignalShiftCount != bridge.OriginShiftCount || bridge.LastSignalShift != bridge.LastShift)
            {
                _shiftChecksFailed++;
                Fail($"origin_shifted signal ({bridge.SignalShiftCount}x, last {bridge.LastSignalShift}) disagrees with take_origin_shift ({bridge.OriginShiftCount}x, {bridge.LastShift})");
            }
        }
        if (shifted || _frames % TileCheckEveryFrames == 0) CheckTiles(shifted);
        if (shifted && _loadedBeforeShift > 0 && bridge.TotalLoaded == 0) Fail("all tiles gone after an origin shift");
        _loadedBeforeShift = bridge.TotalLoaded;

        if (_pathDone)
        {
            CheckTiles(false);
            Finish();
        }
    }

    /// <summary>Every held tile root must sit on the centre of its debug_tiles rect (both in the current local frame).</summary>
    private void CheckTiles(bool afterShift)
    {
        var bridge = Bridge!;
        if (!bridge.Streamer.HasDebugTiles) return;
        bridge.Streamer.DebugTiles(_tiles);
        for (int i = 0; i < _tiles.Count; i++)
        {
            var t = _tiles[i];
            // Terrain tiles (api 1.5) live in their own tile map; the key z/x/y alone would match a vector tile of the same slippy tile.
            if (t.Packed == 0 || t.State != TileState.Ready) continue;
            TileNodes? nodes;
            if (t.Layer == TileLayer.Landmarks) { if (!bridge.LandmarkCover.TryGet(t.Packed, out nodes)) continue; } // api 1.11: the landmark tile map
            else if (!(t.Terrain ? bridge.TerrainCover : bridge.Cover).TryGet(t.Packed, out nodes)) continue;
            var c = (t.Min + t.Max) * 0.5f;
            double err = new Vector2(nodes.Root.Position.X - c.X, nodes.Root.Position.Z - c.Y).Length();
            _tilesChecked++;
            if (err > _maxTileErr) _maxTileErr = err;
            // Terrain roots are placed at the tile centre of the Mercator projection at the tile's latitude, the debug rect is the projected corners:
            // the two differ by about 0.1 % of the tile width on 22 km tiles (23 m at z10) - a Rust-side projection detail, tolerated up to 0.15 %.
            double limit = t.Terrain || t.Layer == TileLayer.Landmarks ? System.Math.Max(TileLimitM, 0.0015 * (t.Max.X - t.Min.X)) : TileLimitM;
            if (err > limit)
            {
                _tileMismatches++;
                if (_tileMismatches <= 3) GD.PushError($"[GeodeticFlightTest] tile {t.Key} root {nodes.Root.Position} is {err:0.0} m off its debug_tiles rect centre {c}{(afterShift ? " (right after a shift)" : "")}");
            }
        }
    }

    private void Fail(string reason)
    {
        _failures.Add(reason);
        GD.PushError("[GeodeticFlightTest] " + reason);
    }

    private void Finish()
    {
        if (_finished) return;
        _finished = true;
        var bridge = Bridge;
        int shifts = bridge?.OriginShiftCount ?? 0;
        int delivered = bridge?.ReadyTotal ?? 0;
        string status = bridge?.Status.State ?? "n/a";
        if (_started)
        {
            if (shifts < 1) Fail("no origin shift happened (path too short?)");
            if (delivered < 1) Fail("no tile was delivered");
            if (bridge != null && bridge.Cover.Loaded < 1) Fail("no tile loaded at the end");
            if (_maxJump >= JumpLimitM) Fail($"camera jump {_maxJump:0.000} m >= {JumpLimitM} m");
            if (_tileMismatches > 0 || _tilesChecked == 0) Fail($"tile roots vs debug_tiles: {_tileMismatches} mismatches over {_tilesChecked} checks (max {_maxTileErr:0.000} m)");
            if (status is "error" or "offline") Fail($"status {status}: {bridge?.Status.Message}");
            if (bridge != null && bridge.ConsistencyErrors > 0) Fail($"{bridge.ConsistencyErrors} cover consistency error(s)");
            if (!_pathDone) Fail("path did not finish");
        }
        bool pass = _failures.Count == 0;
        foreach (var f in _failures) GD.Print("[GeodeticFlightTest] failure: " + f);
        GD.Print($"GEODETIC TEST {(pass ? "PASS" : "FAIL")}: shifts={shifts} tilesDelivered={delivered} maxJump={_maxJump:0.000} status={status}"
            + $" signalShifts={bridge?.SignalShiftCount ?? 0} maxTileErr={_maxTileErr:0.000} tilesChecked={_tilesChecked} frames={_frames} simSeconds={_simTime:0.0} loaded={bridge?.Cover.Loaded ?? 0}");
        if (bridge != null) bridge.ExternalCameraUpdate = null;
        GetTree().Quit(pass ? 0 : 1);
    }
}
