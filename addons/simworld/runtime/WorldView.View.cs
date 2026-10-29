#nullable enable
using Godot;

namespace SimWorld;

// The camera view the library and the sprite shaders work with: vertical fov, the 3D render height and the look direction.
public partial class WorldView
{
    /// <summary>Vertical fov (degrees), the 3D render height sent to set_view (pixels: viewport height x 3D render scale x <see cref="WorldRenderSettings.LodPixelScale"/>, or the override) and the look direction that set_view carries / would carry; 0 until the first frame.</summary>
    public float ViewFovYDeg { get; private set; }
    public int ViewHeightPx { get; private set; }
    public Vector3 ViewForward { get; private set; }
    /// <summary>Physical height of the camera's viewport in pixels and its 3D render scale (<c>Viewport.Scaling3DScale</c>, project setting <c>rendering/scaling_3d/scale</c>) as measured last; <see cref="ViewHeightPx"/> is their product (times <see cref="WorldRenderSettings.LodPixelScale"/>) unless an override is set.</summary>
    public float ViewportHeightPx { get; private set; }
    public float RenderScale { get; private set; } = 1f;

    private const float ViewFovEpsDeg = 0.05f;
    private static readonly float ViewForwardCos = Mathf.Cos(Mathf.DegToRad(2f));
    private float? _viewFovOverride;
    private int? _viewHOverride;
    private bool _viewForce = true;
    private float _lastFov = -1f, _lastHeight = -1f;
    private int _viewLogs;

    /// <summary>The camera the view is measured from: the source's current camera when the source names one (<see cref="IWorldCameraProvider"/>), else the <see cref="Camera"/> export.</summary>
    public Camera3D? ViewCamera => (_source as IWorldCameraProvider)?.CurrentCamera ?? Camera;

    /// <summary>
    /// Test aid: replaces the camera's vertical fov (degrees) and / or the render height (pixels, used as is: neither the 3D render scale nor
    /// <see cref="WorldRenderSettings.LodPixelScale"/> applies) that go into set_view; null = measure again. Takes effect with the next frame.
    /// </summary>
    public void SetViewOverride(float? fovYDeg, int? heightPx)
    {
        _viewFovOverride = fovYDeg;
        _viewHOverride = heightPx;
        _viewForce = true;
    }

    /// <summary>Tells the sprite shader how many metres one pixel spans per metre of depth (changes with fov / render size; a sprite is a few pixels of the rendered image).</summary>
    private void UpdateSpriteScale()
    {
        var cam = ViewCamera;
        if (cam == null || _pool == null) return;
        if (!CameraViewReader.TryRead(cam, this, out var v)) return;
        float h = v.RenderHeightPx;
        if (h < 1f) h = 720f;
        if (v.FovYDeg == _lastFov && h == _lastHeight) return;
        _lastFov = v.FovYDeg;
        _lastHeight = h;
        _pool.SetSpritePxPerDepth(2f * Mathf.Tan(Mathf.DegToRad(v.FovYDeg) * 0.5f) / h);
    }

    /// <summary>
    /// set_view (api 1.10) step, before update_camera: sends the camera's vertical fov, the 3D render height in pixels and the look direction
    /// when one of them changed (fov by more than 0.05 degrees, height at all, direction by more than 2 degrees) and after every configure. The values
    /// are measured even on a library without set_view (overlay), but only a library with it gets the call. The height follows the viewport size and
    /// its 3D render scale, so a window resize or a scale change is picked up here.
    /// </summary>
    private void UpdateView()
    {
        var cam = ViewCamera;
        if (cam == null || !CameraViewReader.TryRead(cam, this, out var v)) return;
        float fov = _viewFovOverride ?? v.FovYDeg;
        int hp = _viewHOverride ?? Mathf.Max(1, Mathf.RoundToInt(v.RenderHeightPx * _settings.LodPixelScale));
        var fwd = -cam.GlobalTransform.Basis.Z;
        fwd = fwd.LengthSquared() > 1e-8f ? fwd.Normalized() : Vector3.Forward;
        ViewFovYDeg = fov;
        ViewHeightPx = hp;
        ViewForward = fwd;
        ViewportHeightPx = v.ViewportHeightPx;
        RenderScale = v.RenderScale;
        if (fov <= 0f || !Streamer.HasSetView) return;
        var s = Streamer;
        if (!_viewForce && Mathf.Abs(fov - (float)s.LastViewFovYDeg) <= ViewFovEpsDeg && hp == s.LastViewHeightPx && fwd.Dot(s.LastViewForward) >= ViewForwardCos) return;
        if (!s.SetView(fov, hp, fwd)) return;
        if (_viewForce || hp != _lastLoggedViewH) LogView(hp);
        _viewForce = false;
    }

    private int _lastLoggedViewH;

    /// <summary>One line per change of the render height (first send, window resize, scale change; the first 12).</summary>
    private void LogView(int hp)
    {
        _lastLoggedViewH = hp;
        if (_viewLogs++ >= 12) return;
        string how = _viewHOverride.HasValue ? "override" : $"viewport {ViewportHeightPx:0} px x 3D scale {RenderScale:0.###} x lod pixel scale {_settings.LodPixelScale:0.##}";
        GD.Print($"[WorldView] set_view fov {ViewFovYDeg:0.#} deg, render height {hp} px ({how})");
    }
}
