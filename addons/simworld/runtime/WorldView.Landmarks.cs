using Godot;

namespace SimWorld;

// Landmark layer (api 1.11, contract 9b): the flat tile set and its counters.
public partial class WorldView
{
    /// <summary>Landmark layer tile map (api 1.11): a flat set keyed by the "l/z/x/y" key (zoom = the archive's overview zoom). Empty unless <see cref="LandmarksActive"/>.</summary>
    public LandmarkCover LandmarkCover { get; } = new();
    /// <summary>True when this view asked for the landmark layer in the last configure (<see cref="LandmarksEnabled"/> and a library with api 1.11). Whether tiles come depends on the archive (a compatibility archive or the seed world has none).</summary>
    public bool LandmarksActive { get; private set; }
    /// <summary>Landmark ready deliveries, unloads, and "l/" keys seen in poll_ready, poll_fades or poll_unload since start (the library must never put one in poll_fades, nor send any to a host that did not opt in).</summary>
    public int LandmarkReadyTotal { get; private set; }
    public int LandmarkReplacedTotal { get; private set; }
    public int LandmarkUnloadedTotal { get; private set; }
    public int LandmarkKeysSeen { get; private set; }
    public int LandmarkFadeKeysSeen { get; private set; }
    /// <summary>Per landmark tile upload cost in microseconds: last, max, sum and count.</summary>
    public long LandmarkUploadUsLast { get; private set; }
    public long LandmarkUploadUsMax { get; private set; }
    public long LandmarkUploadUsSum { get; private set; }
    public int LandmarkUploadCount { get; private set; }
    /// <summary>Landmark tile node sets handed out by the pool (held).</summary>
    public int LandmarkNodesHeld => LandmarkCover.Loaded;
}
