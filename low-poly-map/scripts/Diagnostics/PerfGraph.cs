using System;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>Frame-time graph: one line per frame, newest on the right; 0..50 ms, red line at the 33.3 ms budget, green at 16.7 ms.</summary>
public partial class PerfGraph : Control
{
    const double GraphMaxMs = 50.0;
    readonly Vector2[] _points = new Vector2[PerfMonitor.GraphFrames];

    public override void _Draw()
    {
        using var _ = PerfMarker.Scope(PerfCat.Overlay);
        DrawRect(new Rect2(Vector2.Zero, Size), new Color(0f, 0f, 0f, 0.35f));
        float budgetY = Y(PerfStats.BudgetFrameMs);
        DrawLine(new Vector2(0, budgetY), new Vector2(Size.X, budgetY), new Color(1f, 0.3f, 0.3f, 0.9f), 1f);
        float y60 = Y(1000.0 / 60.0);
        DrawLine(new Vector2(0, y60), new Vector2(Size.X, y60), new Color(0.3f, 1f, 0.3f, 0.5f), 1f);
        int n = PerfMonitor.GraphCount;
        if (n < 2) return;
        float step = Size.X / (PerfMonitor.GraphFrames - 1);
        for (int i = 0; i < PerfMonitor.GraphFrames; i++)
        {
            int age = PerfMonitor.GraphFrames - 1 - i;
            _points[i] = new Vector2(i * step, Y(age < n ? PerfMonitor.GraphSample(age) : 0f));
        }
        DrawPolyline(_points, new Color(1f, 0.9f, 0.3f), 1f);
    }

    float Y(double ms) => Size.Y * (1f - (float)Math.Clamp(ms / GraphMaxMs, 0.0, 1.0));
}
