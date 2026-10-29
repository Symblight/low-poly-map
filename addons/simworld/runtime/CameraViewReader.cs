#nullable enable
using Godot;

namespace SimWorld;

/// <summary>What the camera renders: its vertical fov and the size of the 3D render target in pixels.</summary>
public readonly record struct CameraView(float FovYDeg, float ViewportHeightPx, float RenderScale)
{
    /// <summary>Height of the 3D render target in pixels: the viewport height times its 3D scale (<c>Viewport.Scaling3DScale</c>, project setting <c>rendering/scaling_3d/scale</c>).</summary>
    public float RenderHeightPx => ViewportHeightPx * RenderScale;
}

/// <summary>
/// Reads the view of a <see cref="Camera3D"/> for the screen-space LOD (<c>set_view</c>, api 1.10) and the light sprites: vertical fov and the height in
/// pixels of the target the 3D scene is rendered to. That is the physical viewport height (a window with content scale mode "viewport": the content
/// scale height, a <see cref="SubViewport"/>: its size) times the viewport's 3D render scale, because the screen-space error is measured on the pixels
/// that are actually drawn (a phone that renders at 0.5 scale and upsamples has half the pixels to refine for).
/// </summary>
public static class CameraViewReader
{
    /// <summary>
    /// False without a camera or with an orthogonal / frustum projection. A camera that keeps the width has a horizontal fov, converted with the
    /// viewport aspect. <paramref name="fallback"/> supplies the window when the camera's viewport is neither a window nor a SubViewport.
    /// </summary>
    public static bool TryRead(Camera3D? cam, Node fallback, out CameraView view)
    {
        view = default;
        if (cam == null || cam.Projection != Camera3D.ProjectionType.Perspective) return false;
        var vp = cam.GetViewport();
        float fov = cam.Fov;
        if (cam.KeepAspect == Camera3D.KeepAspectEnum.Width)
        {
            var rect = vp.GetVisibleRect().Size;
            float aspect = rect.Y > 0f ? rect.X / rect.Y : 16f / 9f;
            fov = Mathf.RadToDeg(2f * Mathf.Atan(Mathf.Tan(Mathf.DegToRad(fov) * 0.5f) / aspect));
        }
        float h = vp switch
        {
            SubViewport sv => sv.Size.Y,
            Window w => w.ContentScaleMode == Window.ContentScaleModeEnum.Viewport && w.ContentScaleSize.Y > 0 ? w.ContentScaleSize.Y : w.Size.Y,
            _ => fallback.GetWindow().Size.Y,
        };
        float scale = vp.Scaling3DScale;
        if (!(scale > 0.01f)) scale = 1f;
        view = new CameraView(fov, h, scale);
        return true;
    }
}
