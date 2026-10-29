using Godot;
using Godot.Collections;

namespace SimWorld;

// The calls of the per-frame loop (contract section 3) and the geodetic conversions hosts make every frame. Each reuses its argument array and releases the Variant it gets
// back: a Variant that holds a collection owns a finalizable wrapper, and thousands of those per second (one per call) made every garbage collection scan a long
// finalization queue.
public sealed partial class WorldStreamerFacade
{
    private readonly Variant[] _argsCamera = new Variant[3], _argsGround = new Variant[2], _argsGeodetic = new Variant[4], _argsGeoToLocal = new Variant[3], _argsLocalToGeo = new Variant[1];
    private bool? _hasGroundHeight, _hasPollFades;

    public void UpdateCamera(Vector3 localPos, Vector3 velocity, double altAglM)
    {
        if (!IsAvailable) return;
        using var timing = Timings.Time(StreamerCall.UpdateCamera);
        _argsCamera[0] = localPos; _argsCamera[1] = velocity; _argsCamera[2] = altAglM;
        _node!.Call(MUpdateCamera, _argsCamera);
    }

    /// <summary>
    /// update_camera_geodetic (api 0.7+): WGS84 degrees, meters above MSL, velocity as (north, east, down) m/s. Use instead of
    /// <see cref="UpdateCamera"/> in a frame. Returns false when unavailable (nothing was sent).
    /// </summary>
    public bool UpdateCameraGeodetic(double lat, double lon, double altMslM, Vector3 velNed)
    {
        if (!HasHostApi) return false;
        using var timing = Timings.Time(StreamerCall.UpdateCamera);
        _argsGeodetic[0] = lat; _argsGeodetic[1] = lon; _argsGeodetic[2] = altMslM; _argsGeodetic[3] = velNed;
        _node!.Call(MUpdateCameraGeodetic, _argsGeodetic);
        return true;
    }

    /// <summary>geodetic_to_local (api 0.7+): WGS84 point to the current local frame; Vector3.Zero when unavailable or invalid.</summary>
    public Vector3 GeodeticToLocal(double lat, double lon, double altMsl)
    {
        if (!HasHostApi) return Vector3.Zero;
        _argsGeoToLocal[0] = lat; _argsGeoToLocal[1] = lon; _argsGeoToLocal[2] = altMsl;
        var v = _node!.Call(MGeodeticToLocal, _argsGeoToLocal);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }

    /// <summary>local_to_geodetic (api 0.7+): current local point to Vector3(lat, lon, alt_msl) (f32 resolution, about 0.1-0.4 m); Vector3.Zero when unavailable.</summary>
    public Vector3 LocalToGeodetic(Vector3 local)
    {
        if (!HasHostApi) return Vector3.Zero;
        _argsLocalToGeo[0] = local;
        var v = _node!.Call(MLocalToGeodetic, _argsLocalToGeo);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }

    /// <summary>True if the loaded Rust build exports ground_height_at() (checked once).</summary>
    public bool HasGroundHeight =>
        _hasGroundHeight ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MGroundHeightAt));

    /// <summary>
    /// Terrain height in meters under a local point (Rust's single ground_height function). Returns 0 when the
    /// facade is unavailable or the Rust build does not export ground_height_at() yet.
    /// </summary>
    public double GroundHeightAt(double localX, double localZ)
    {
        if (!IsAvailable || !HasGroundHeight) return 0.0;
        _argsGround[0] = localX; _argsGround[1] = localZ;
        var v = _node!.Call(MGroundHeightAt, _argsGround);
        return v.VariantType == Variant.Type.Float ? v.AsDouble() : 0.0;
    }

    /// <summary>True if the loaded Rust build exports poll_fades() (checked once; false = every tile is fade 1, no transitions).</summary>
    public bool HasPollFades =>
        _hasPollFades ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MPollFades));

    /// <summary>
    /// poll_fades(): key ("z/x/y") -> float fade (0..1) for every tile currently in a LOD transition (changed or not);
    /// a tile that is not listed has fade 1. Returns null when no tile is in a transition, when the Rust build does not export the
    /// method yet or when the facade is unavailable, which callers treat as "no tile is fading". The caller disposes a returned dictionary.
    /// </summary>
    public Dictionary? PollFades()
    {
        if (!IsAvailable || !HasPollFades) return null;
        using var timing = Timings.Time(StreamerCall.PollFades);
        using var v = _node!.Call(MPollFades);
        if (v.VariantType != Variant.Type.Dictionary) return null;
        var d = v.AsGodotDictionary();
        if (d.Count > 0) return d;
        d.Dispose();
        return null;
    }

    /// <summary>Keys ("z/x/y") of tiles Rust wants unloaded.</summary>
    public string[] PollUnload()
    {
        if (!IsAvailable) return EmptyUnload;
        using var timing = Timings.Time(StreamerCall.PollUnload);
        using var v = _node!.Call(MPollUnload);
        return v.VariantType == Variant.Type.PackedStringArray ? v.AsStringArray() : EmptyUnload;
    }

    /// <summary>Non-zero when the floating origin moved: subtract it from camera and all tile roots.</summary>
    public Vector3 TakeOriginShift()
    {
        if (!IsAvailable) return Vector3.Zero;
        using var timing = Timings.Time(StreamerCall.TakeOriginShift);
        var v = _node!.Call(MTakeOriginShift);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }
}
