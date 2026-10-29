using Godot;

namespace SimWorld;

// Garbage collector pacing (see GcPacer).
public partial class WorldRenderSettings
{
    /// <summary>
    /// Megabytes of managed allocation the view lets the garbage collector skip between two collections (<see cref="GcPacer"/>); 0 = off, the runtime's own schedule (a gen0 collection every 8 MB, a pause of
    /// 5 to 10 ms in a world this size). Costs up to that much extra working set. A starting point: 64 (one collection per 5 minutes at 0.2 MB/s).
    /// </summary>
    [Export] public int GcQuietBudgetMb { get; set; }
}
