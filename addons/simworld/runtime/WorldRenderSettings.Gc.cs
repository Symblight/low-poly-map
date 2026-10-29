#nullable enable
using Godot;

namespace SimWorld;

// Garbage collector pacing (see GcPacer).
public partial class WorldRenderSettings
{
    /// <summary>
    /// Megabytes of managed allocation the view lets the garbage collector skip between two collections (<see cref="GcPacer"/>); 0 = off, the runtime's own schedule (a gen0 collection every 8 MB, a pause of
    /// 5 to 10 ms in a world this size). Costs up to that much extra working set, and more: a whole region's finalizable wrappers survive their rotation and are freed only at the next one. Measured in a 600 s soak at 64 MB: 5 rotations instead of 30 collections, but one rotation paused 184 ms, the managed heap grew from 38 to 226 MB and the frame p50 doubled by the end; leave it at 0 unless a soak on the target device says otherwise (see low-poly-map/CLAUDE.md "GC pacing").
    /// </summary>
    [Export] public int GcQuietBudgetMb { get; set; }
}
