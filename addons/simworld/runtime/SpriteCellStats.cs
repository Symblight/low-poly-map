#nullable enable
namespace SimWorld;

/// <summary>Counters of the merged point sprite cells (<see cref="SpriteCells"/>), for the overlay and the benchmark report: cells and tiles in them, points in the cell meshes (dead = of tiles that left, not yet rebuilt away), rebuilds since start and what they cost.</summary>
public readonly record struct SpriteCellStats(int Cells, int Tiles, int Points, int DeadPoints, long Rebuilds, long RebuiltPoints, long RebuildUs, long RebuildUsMax, long TableWrites)
{
    public override string ToString() => $"sprite cells {Cells} ({Tiles} tiles, {Points} points, {DeadPoints} dead)  rebuilds {Rebuilds} ({RebuildUs / 1000.0:0.0} ms, worst {RebuildUsMax / 1000.0:0.00} ms)  table writes {TableWrites}";
}
