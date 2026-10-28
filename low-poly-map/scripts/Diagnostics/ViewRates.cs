using SimWorld;

namespace LowPolyMap.Diagnostics;

/// <summary>The view's cumulative churn and upload counters at one instant (tiles shown / hidden, fades, re-deliveries, uploads), per layer.</summary>
public readonly record struct ViewTotals(
    long VectorShown, long VectorHidden, long VectorQuick, long TerrainShown, long TerrainHidden, long TerrainQuick,
    long FadesStarted, long FadesEnded, long Redeliveries,
    long UploadedVector, long UploadedTerrain, long UploadedLandmarks, long UploadFrames, long UploadFramesOverBudget)
{
    public static ViewTotals Read(WorldView v) => new(
        v.Cover.ShownTotal, v.Cover.HiddenTotal, v.Cover.QuickTogglesTotal,
        v.TerrainCover.ShownTotal, v.TerrainCover.HiddenTotal, v.TerrainCover.QuickTogglesTotal,
        v.Cover.FadesStartedTotal + v.TerrainCover.FadesStartedTotal, v.Cover.FadesEndedTotal + v.TerrainCover.FadesEndedTotal,
        v.Cover.RedeliveriesTotal + v.TerrainCover.RedeliveriesTotal,
        v.ReadyTotal, v.TerrainReadyTotal, v.LandmarkReadyTotal, v.UploadFrames, v.UploadFramesOverBudget);

    public static ViewTotals operator -(ViewTotals a, ViewTotals b) => new(
        a.VectorShown - b.VectorShown, a.VectorHidden - b.VectorHidden, a.VectorQuick - b.VectorQuick,
        a.TerrainShown - b.TerrainShown, a.TerrainHidden - b.TerrainHidden, a.TerrainQuick - b.TerrainQuick,
        a.FadesStarted - b.FadesStarted, a.FadesEnded - b.FadesEnded, a.Redeliveries - b.Redeliveries,
        a.UploadedVector - b.UploadedVector, a.UploadedTerrain - b.UploadedTerrain, a.UploadedLandmarks - b.UploadedLandmarks,
        a.UploadFrames - b.UploadFrames, a.UploadFramesOverBudget - b.UploadFramesOverBudget);
}

/// <summary>
/// Turns the view's cumulative counters into per-second rates, and counts the shown tiles inside the camera frustum per layer (the
/// proxy for draw calls per layer: one mesh instance per tile). Once a second, main thread.
/// </summary>
public sealed class ViewRates
{
    readonly WorldView _view;
    ViewTotals _last;

    public ViewRates(WorldView view)
    {
        _view = view;
        _last = ViewTotals.Read(view);
    }

    /// <summary>Writes the churn, upload and frustum metrics for the <paramref name="wallSeconds"/> since the previous call into <paramref name="values"/>.</summary>
    public void Sample(double wallSeconds, double[] values)
    {
        var now = ViewTotals.Read(_view);
        var d = now - _last;
        _last = now;
        double perS = 1.0 / System.Math.Max(wallSeconds, 1e-3);
        values[(int)Metric.ShownVectorPerS] = d.VectorShown * perS;
        values[(int)Metric.HiddenVectorPerS] = d.VectorHidden * perS;
        values[(int)Metric.ShownTerrainPerS] = d.TerrainShown * perS;
        values[(int)Metric.HiddenTerrainPerS] = d.TerrainHidden * perS;
        values[(int)Metric.QuickTogglesVectorPerS] = d.VectorQuick * perS;
        values[(int)Metric.QuickTogglesTerrainPerS] = d.TerrainQuick * perS;
        values[(int)Metric.FadesStartedPerS] = d.FadesStarted * perS;
        values[(int)Metric.FadesEndedPerS] = d.FadesEnded * perS;
        values[(int)Metric.FadesActive] = _view.Cover.FadingTiles + _view.TerrainCover.FadingTiles;
        values[(int)Metric.RedeliveriesPerS] = d.Redeliveries * perS;
        values[(int)Metric.UploadedTilesPerS] = (d.UploadedVector + d.UploadedTerrain + d.UploadedLandmarks) * perS;
        values[(int)Metric.UploadFramesOverBudgetPerS] = d.UploadFramesOverBudget * perS;
        if (_view.Camera == null) return;
        values[(int)Metric.VectorInFrustum] = _view.CountVectorInFrustum(_view.Camera, out _, out long vectorTris);
        values[(int)Metric.VectorTrisInFrustum] = vectorTris;
        values[(int)Metric.TerrainInFrustum] = _view.CountTerrainInFrustum(_view.Camera, out _, out long terrainTris);
        values[(int)Metric.TerrainTrisInFrustum] = terrainTris;
    }
}
