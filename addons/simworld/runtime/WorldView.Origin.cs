using Godot;

namespace SimWorld;

// Floating origin: the re-centers the view applied and the origin_shifted signal (diagnostic only).
public partial class WorldView
{
    /// <summary>Raised from <c>_Process</c> after the view applied a re-center: the vector to subtract from every host node in the world frame.</summary>
    public event System.Action<Vector3>? OriginShifted;

    /// <summary>Re-centers seen through take_origin_shift() (the authoritative path) and the latest / summed shift.</summary>
    public int OriginShiftCount { get; private set; }
    public Vector3 LastShift { get; private set; }
    public Vector3 TotalShift { get; private set; }
    /// <summary>Re-centers seen through the origin_shifted signal (diagnostic; must equal <see cref="OriginShiftCount"/>) and its latest vector.</summary>
    public int SignalShiftCount { get; private set; }
    public Vector3 LastSignalShift { get; private set; }
    /// <summary>
    /// Host hook: when set, replaces the update_camera(...) call in each frame (same place in the call order, before
    /// take_origin_shift). The hook may move the camera (in the pre-shift frame; the view subtracts the shift
    /// afterwards) and must call one camera update on the Streamer (e.g. UpdateCameraGeodetic).
    /// </summary>
    public System.Action<double>? ExternalCameraUpdate { get; set; }

    // Diagnostic only: the bridge shifts from take_origin_shift() (same-frame ordering); never consume the shift here.
    private void OnOriginShifted(Vector3 shift)
    {
        SignalShiftCount++;
        LastSignalShift = shift;
    }
}
