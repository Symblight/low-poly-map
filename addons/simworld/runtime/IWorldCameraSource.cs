using Godot;

namespace SimWorld;

/// <summary>
/// Where the world is viewed from. <see cref="WorldView"/> reads the pose every frame (the input of update_camera) and writes
/// back the terrain height and origin shifts, so the camera can be a free-fly camera, an aircraft node or anything else.
/// Implemented by the host's node; a plain <see cref="Camera3D"/> works without any code through <see cref="Camera3DSource"/>.
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
    /// <summary>The library's terrain height under the camera (ground_height_at; 0 until a DEM exists). Called every frame before the update.</summary>
    void SetGroundHeight(double groundY);
    /// <summary>
    /// The origin moved: subtract <paramref name="shift"/> from the position (and from anything else attached to the source). Called in
    /// the same frame the library delivers the shift, before any tile of the new frame is placed. A reconfigure uses it to move the
    /// camera back to local x/z = 0 as well.
    /// </summary>
    void ApplyOriginShift(Vector3 shift);
}

/// <summary>
/// Adapter for a bare <see cref="Camera3D"/>: velocity from the position change between frames, AGL = y minus the reported
/// terrain height, shifts applied to the camera node. <see cref="WorldView"/> uses it when its camera is not an
/// <see cref="IWorldCameraSource"/> itself.
/// </summary>
public sealed class Camera3DSource : IWorldCameraSource
{
    private readonly Camera3D _camera;
    private Vector3 _last;
    private bool _haveLast;
    private double _groundY;

    public Camera3DSource(Camera3D camera) => _camera = camera;

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
