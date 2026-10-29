#nullable enable
using System;
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Where the world is viewed from. <see cref="WorldView"/> reads the pose every frame (the input of update_camera) and writes
/// back the terrain height and origin shifts, so the camera can be a free-fly camera, an aircraft node or anything else.
/// Implemented by the host's node; a plain <see cref="Camera3D"/> works without any code through <see cref="Camera3DSource"/>,
/// a host that switches between several cameras (chase, cockpit, tower) uses <see cref="CameraSwitchSource"/>.
/// All positions are in the current floating-origin frame (Godot world coordinates, meters).
/// </summary>
public interface IWorldCameraSource
{
    /// <summary>Position in the current frame (what update_camera receives).</summary>
    Vector3 GlobalPosition { get; }
    /// <summary>Velocity in meters per second in the current frame.</summary>
    Vector3 Velocity { get; }
    /// <summary>Height above the ground in meters (position y minus the terrain height last reported to <see cref="SetGroundHeight"/>).</summary>
    double AltitudeAgl { get; }
    /// <summary>The library's terrain height under the camera (ground_height_at; 0 unless the terrain layer is active). Called every frame before the update.</summary>
    void SetGroundHeight(double groundY);
    /// <summary>
    /// The origin moved: subtract <paramref name="shift"/> from the position (and from anything else attached to the source). Called in
    /// the same frame the library delivers the shift, before any tile of the new frame is placed. A reconfigure uses it to move the
    /// camera back to local x/z = 0 as well.
    /// </summary>
    void ApplyOriginShift(Vector3 shift);
}

/// <summary>A source the view ticks once per frame before it reads the pose (velocity from the position change, a camera switch).</summary>
public interface IWorldCameraTick
{
    void Tick(double delta);
}

/// <summary>
/// A source that also says which <see cref="Camera3D"/> is looking right now. <see cref="WorldView"/> reads the field of view, the render height
/// and the look direction (set_view, the sprite pixel size, the material prewarm) from it instead of from its <see cref="WorldView.Camera"/> export,
/// so a host with several cameras keeps the screen-space LOD right across a view switch.
/// </summary>
public interface IWorldCameraProvider
{
    Camera3D? CurrentCamera { get; }
}

/// <summary>
/// Adapter for a bare <see cref="Camera3D"/>: velocity from the position change between frames, AGL = y minus the reported
/// terrain height, shifts applied to the camera node. <see cref="WorldView"/> uses it when its camera is not an
/// <see cref="IWorldCameraSource"/> itself.
/// </summary>
public sealed class Camera3DSource : IWorldCameraSource, IWorldCameraTick, IWorldCameraProvider
{
    private readonly Camera3D _camera;
    private Vector3 _last;
    private bool _haveLast;
    private double _groundY;

    public Camera3DSource(Camera3D camera) => _camera = camera;

    public Camera3D? CurrentCamera => _camera;
    public Vector3 GlobalPosition => _camera.GlobalPosition;
    public Vector3 Velocity { get; private set; }
    public double AltitudeAgl => _camera.GlobalPosition.Y - _groundY;

    public void SetGroundHeight(double groundY) => _groundY = groundY;

    public void ApplyOriginShift(Vector3 shift)
    {
        _camera.GlobalPosition -= shift;
        _last -= shift;
    }

    /// <summary>Called once per frame by the view before it reads the pose.</summary>
    public void Tick(double delta)
    {
        var p = _camera.GlobalPosition;
        Velocity = _haveLast && delta > 1e-6 ? (p - _last) / (float)delta : Vector3.Zero;
        _last = p;
        _haveLast = true;
    }
}

/// <summary>
/// A source over whichever <see cref="Camera3D"/> the host says is current (a view switcher: chase, cockpit, tower). The pose and the view
/// come from the current camera; the frame a different camera becomes current reports zero velocity (the jump between two cameras is not
/// a speed), and the next frame measures again. An origin shift moves every camera the host listed in <paramref name="all"/>, or only the
/// current one when the list is null: a camera the host recomputes from its aircraft every frame needs no shift, one that keeps its own
/// position in local space must be listed (or shifted by the host through <see cref="WorldView.OriginShifted"/>).
/// <para><b>Velocity.</b> The library's cover looks ahead by velocity x <c>bands.look_ahead_s</c> (20 s, clamped per band). A chase or orbit camera
/// swinging on its arm reports hundreds of metres per second that are not travel, and every swing then moves the whole streamed set (tiles
/// stream in and out and fade: "everything blinks when I move the camera"). A host with a vehicle passes its velocity through
/// <paramref name="velocity"/>; without one the camera's own position change is used, as <see cref="Camera3DSource"/> does.</para>
/// </summary>
public sealed class CameraSwitchSource : IWorldCameraSource, IWorldCameraTick, IWorldCameraProvider
{
    private readonly Func<Camera3D?> _current;
    private readonly IReadOnlyList<Camera3D>? _all;
    private readonly Func<Vector3>? _velocity;
    private Camera3D? _camera;
    private Vector3 _last;
    private bool _haveLast;
    private double _groundY;

    /// <param name="current">Returns the camera that is looking now (null = none yet; the pose then holds the last camera's position).</param>
    /// <param name="all">Every camera to move on an origin shift; null = only the current one.</param>
    /// <param name="velocity">The travel velocity in local axes (m/s), e.g. the aircraft's; null = the camera's own position change.</param>
    public CameraSwitchSource(Func<Camera3D?> current, IReadOnlyList<Camera3D>? all = null, Func<Vector3>? velocity = null)
    {
        _current = current;
        _all = all;
        _velocity = velocity;
    }

    public Camera3D? CurrentCamera => _camera;
    /// <summary>Frames in which the current camera changed (diagnostics).</summary>
    public int Switches { get; private set; }

    public Vector3 GlobalPosition => _camera != null && GodotObject.IsInstanceValid(_camera) ? _camera.GlobalPosition : _last;
    public Vector3 Velocity { get; private set; }
    public double AltitudeAgl => GlobalPosition.Y - _groundY;

    public void SetGroundHeight(double groundY) => _groundY = groundY;

    public void ApplyOriginShift(Vector3 shift)
    {
        if (_all != null)
        {
            foreach (var c in _all)
                if (GodotObject.IsInstanceValid(c)) c.GlobalPosition -= shift;
        }
        else if (_camera != null && GodotObject.IsInstanceValid(_camera))
        {
            _camera.GlobalPosition -= shift;
        }
        _last -= shift;
    }

    public void Tick(double delta)
    {
        var cam = _current();
        if (cam != null && !GodotObject.IsInstanceValid(cam)) cam = null;
        if (!ReferenceEquals(cam, _camera))
        {
            if (_camera != null) Switches++;
            _camera = cam;
            _haveLast = false; // the jump to the other camera is not a velocity
        }
        if (_camera == null) { Velocity = Vector3.Zero; return; }
        var p = _camera.GlobalPosition;
        Velocity = _velocity != null ? _velocity() : _haveLast && delta > 1e-6 ? (p - _last) / (float)delta : Vector3.Zero;
        _last = p;
        _haveLast = true;
    }
}
