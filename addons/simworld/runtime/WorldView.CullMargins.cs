using Godot;

namespace SimWorld;

// Per-tile frustum-cull margins for the earth curvature (see CullMargins).
public partial class WorldView
{
    /// <summary>Margin writes since start (a margin is written when the tile arrives, when it drifts out of range of its distance, or when it is twice what it needs).</summary>
    public int CullMarginWrites { get; private set; }

    private float InvTwoR => (float)(Curvature / (2.0 * EarthRadiusM));

    /// <summary>Called right after a tile's mesh went in: sizes its half-span from its zoom and writes its first margin.</summary>
    private void InitCullMargin(TileNodes nodes)
    {
        if (_source == null) return;
        nodes.HalfSpanM = CullMargins.HalfSpanM(TileKey.Z(nodes.Packed), OriginLat);
        nodes.CullMargin = float.NaN; // a reused set carries the margin of its last tile
        RefreshCullMargin(nodes, _source.GlobalPosition, InvTwoR);
    }

    private void RefreshCullMargin(TileNodes nodes, Vector3 camera, float invTwoR)
    {
        if (!CullMargins.TryUpdate(nodes.CullMargin, camera, nodes.Root.Position, nodes.HalfSpanM, invTwoR, out float margin)) return;
        if (nodes is TerrainNodes tn) TerrainPool.SetCullMargin(tn, margin);
        else _pool!.SetCullMargin(nodes, margin);
        CullMarginWrites++;
    }

    /// <summary>Four times a second: keeps every held tile's margin in step with its distance to the camera (the loops are plain dictionary walks; a write is rare).</summary>
    private void RefreshCullMargins()
    {
        var cam = _source!.GlobalPosition;
        float invTwoR = InvTwoR;
        foreach (var n in Cover.All) RefreshCullMargin(n, cam, invTwoR);
        foreach (var n in LandmarkCover.All) RefreshCullMargin(n, cam, invTwoR);
        foreach (var n in TerrainCover.All) RefreshCullMargin(n, cam, invTwoR);
    }
}
