using System.Collections.Generic;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// The F1 overlay's top-down minimap around the camera (north up, camera in the centre). It shows only the tiles that are currently visible
/// (the quadtree cover), coloured by band; with <see cref="ShowHidden"/> it also outlines hidden tiles (hidden by their children / waiting for
/// siblings) and tiles still in flight. Everything is batched into one mesh and one multiline so the map costs about two draw calls.
/// </summary>
public partial class DebugMinimap : Control
{
    /// <summary>Side of the square map in pixels.</summary>
    public const float SizePx = 320f;

    // Total minimap extent in meters (map is square, camera in the center).
    private static readonly float[] ZoomSpansM = { 16000f, 4000f, 1000f };

    private static readonly Color[] RingColors =
    {
        new(1f, 1f, 1f),        // R0 white
        new(0.2f, 0.9f, 1f),    // R1 cyan
        new(1f, 0.4f, 0.9f),    // R2 magenta
        new(1f, 0.65f, 0.15f),  // R3 orange
    };

    private static readonly Color[] StateColors =
    {
        new(0.5f, 0.5f, 0.55f, 0.35f),  // queued   gray
        new(0.25f, 0.45f, 1f, 0.5f),    // fetching blue
        new(1f, 0.9f, 0.2f, 0.5f),      // meshing  yellow
        new(0.2f, 0.85f, 0.3f, 0.5f),   // ready    green
        new(1f, 0.15f, 0.15f, 0.6f),    // failed   red
        new(0.6f, 0.25f, 0.8f, 0.5f),   // unloading purple
        new(0.3f, 0.3f, 0.3f, 0.3f),    // unknown
    };

    private static readonly string[] StateNames =
        { "queued", "fetching", "meshing", "ready", "failed", "unloading" };

    /// <summary>The tiles the library reported at the last refresh (read when drawing).</summary>
    public IReadOnlyList<DebugTile> Tiles { get; set; } = System.Array.Empty<DebugTile>();
    /// <summary>The vector tile map, to tell visible from hidden tiles; null = nothing is visible.</summary>
    public TileCover? Cover { get; set; }
    /// <summary>Camera position in the world frame (x east, z south) and its horizontal heading (unit vector).</summary>
    public Vector3 CamPos { get; set; }
    public Vector2 Heading { get; set; } = new(0, -1);
    /// <summary>Outline hidden tiles and the tiles in flight too (H).</summary>
    public bool ShowHidden { get; private set; }
    /// <summary>Extent of the map in metres at the current zoom.</summary>
    public float SpanM => ZoomSpansM[_zoom];

    private readonly Vector2[] _arrow = new Vector2[3];
    private readonly List<Vector2> _fillV = new(1024);
    private readonly List<Color> _fillC = new(1024);
    private readonly List<int> _fillI = new(2048);
    private readonly List<Vector2> _lineP = new(2048);
    private readonly List<Color> _lineC = new(1024);
    private readonly ArrayMesh _fillMesh = new();
    private Font _font = null!;
    private int _zoom;

    public override void _Ready()
    {
        _font = ThemeDB.FallbackFont;
    }

    /// <summary>M: next zoom level.</summary>
    public void CycleZoom()
    {
        _zoom = (_zoom + 1) % ZoomSpansM.Length;
        QueueRedraw();
    }

    /// <summary>H: outline hidden tiles or not.</summary>
    public void ToggleHidden()
    {
        ShowHidden = !ShowHidden;
        QueueRedraw();
    }

    private void AddFill(Rect2 r, Color c)
    {
        int b = _fillV.Count;
        _fillV.Add(r.Position); _fillV.Add(new Vector2(r.End.X, r.Position.Y)); _fillV.Add(r.End); _fillV.Add(new Vector2(r.Position.X, r.End.Y));
        for (int i = 0; i < 4; i++) _fillC.Add(c);
        _fillI.Add(b); _fillI.Add(b + 1); _fillI.Add(b + 2); _fillI.Add(b); _fillI.Add(b + 2); _fillI.Add(b + 3);
    }

    private void AddLine(Vector2 a, Vector2 b, Color c)
    {
        _lineP.Add(a); _lineP.Add(b); _lineC.Add(c);
    }

    private void AddOutline(Rect2 r, Color c)
    {
        var tr = new Vector2(r.End.X, r.Position.Y);
        var bl = new Vector2(r.Position.X, r.End.Y);
        AddLine(r.Position, tr, c); AddLine(tr, r.End, c); AddLine(r.End, bl, c); AddLine(bl, r.Position, c);
    }

    public override void _Draw()
    {
        float span = ZoomSpansM[_zoom];
        float scale = SizePx / span;
        var center = new Vector2(SizePx, SizePx) * 0.5f;
        var cam2 = new Vector2(CamPos.X, CamPos.Z);

        DrawRect(new Rect2(0, 0, SizePx, SizePx), new Color(0, 0, 0, 0.55f));

        // Distance circles.
        var grid = new Color(1, 1, 1, 0.12f);
        for (float r = 1000f; r * scale < SizePx * 0.75f; r *= 2f)
            DrawArc(center, r * scale, 0f, Mathf.Tau, 48, grid, 1f);

        // Coarse rings first so finer rings stay on top. Visible tiles (= the quadtree cover) are filled with
        // their band colour; with [H], hidden tiles are outlined dimly and tiles in flight use their state colour.
        var cover = Cover;
        _fillV.Clear(); _fillC.Clear(); _fillI.Clear(); _lineP.Clear(); _lineC.Clear();
        for (int ring = 3; ring >= 0; ring--)
        {
            var band = RingColors[ring];
            for (int i = 0; i < Tiles.Count; i++)
            {
                var t = Tiles[i];
                if (t.Terrain || t.Layer == TileLayer.Landmarks || t.Ring != ring) continue; // landmark tiles (api 1.11, overview-sized, flat set) and terrain tiles (api 1.5) live in their own tile map and are 22 km wide: not on this minimap
                int vis = cover != null && t.Packed != 0 ? cover.StateOf(t.Packed) : -1;
                bool isVisible = vis == (int)TileCover.Vis.Visible;
                if (!isVisible && !ShowHidden) continue;
                var p = center + (t.Min - cam2) * scale;
                var sz = (t.Max - t.Min) * scale;
                if (p.X > SizePx || p.Y > SizePx || p.X + sz.X < 0 || p.Y + sz.Y < 0) continue;
                if (sz.X < 1f) sz.X = 1f;
                if (sz.Y < 1f) sz.Y = 1f;
                var rect = new Rect2(p, sz);
                if (isVisible)
                {
                    var fill = band;
                    fill.A = 0.35f;
                    AddFill(rect, fill);
                    var outline = band;
                    outline.A = 0.95f;
                    AddOutline(rect, outline);
                }
                else if (vis < 0)
                {
                    var sc = StateColors[(int)t.State];
                    sc.A = 0.6f;
                    AddOutline(rect, sc); // not uploaded yet (queued / fetching / meshing / ...)
                }
                else
                {
                    var outline = band;
                    outline.A = vis == (int)TileCover.Vis.HiddenByChildren ? 0.28f : 0.5f;
                    AddOutline(rect, outline);
                    if (vis == (int)TileCover.Vis.WaitingSiblings)
                        AddLine(rect.Position, rect.End, outline); // uploaded, waiting for its siblings
                }
            }
        }
        // Everything above is batched into one mesh + one multiline so the minimap costs ~2 draw calls
        // (hundreds of DrawRect calls would swamp the 3D draw-call count shown in the overlay).
        if (_fillV.Count > 0)
        {
            var arrays = new Godot.Collections.Array();
            arrays.Resize((int)Mesh.ArrayType.Max);
            arrays[(int)Mesh.ArrayType.Vertex] = _fillV.ToArray();
            arrays[(int)Mesh.ArrayType.Color] = _fillC.ToArray();
            arrays[(int)Mesh.ArrayType.Index] = _fillI.ToArray();
            _fillMesh.ClearSurfaces();
            _fillMesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays);
            DrawMesh(_fillMesh, null);
        }
        if (_lineP.Count > 0) DrawMultilineColors(_lineP.ToArray(), _lineC.ToArray(), 1f);

        // Camera marker + heading (north up: screen up = -z).
        var h = Heading;
        var side = new Vector2(-h.Y, h.X);
        _arrow[0] = center + h * 9f;
        _arrow[1] = center - h * 5f + side * 5f;
        _arrow[2] = center - h * 5f - side * 5f;
        DrawColoredPolygon(_arrow, new Color(1f, 0.2f, 0.2f));
        DrawLine(center, center + h * 28f, new Color(1f, 0.2f, 0.2f, 0.8f), 1f);

        // Legend: rings (top-left), states (bottom).
        for (int i = 0; i < RingColors.Length; i++)
        {
            float x = 6f + i * 34f;
            DrawString(_font, new Vector2(x, 14f), "R" + i, HorizontalAlignment.Left, -1, 12, RingColors[i]);
        }
        for (int i = 0; i < StateNames.Length; i++)
        {
            float x = 6f + (i % 3) * 104f;
            float y = SizePx - 24f + (i / 3) * 14f;
            var c = StateColors[i];
            c.A = 1f;
            DrawRect(new Rect2(x, y - 8f, 8f, 8f), c);
            DrawString(_font, new Vector2(x + 12f, y), StateNames[i], HorizontalAlignment.Left, -1, 11, Colors.White);
        }
        DrawRect(new Rect2(0, 0, SizePx, SizePx), new Color(1, 1, 1, 0.5f), false, 1f);
    }
}
