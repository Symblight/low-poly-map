#nullable enable
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Runway markings for a seed world (api 1.7, docs/spec-world-seed.md 5.1 step 8): the ground mesh has no UV, so the host draws the
/// markings as white quads from the runway ends of <c>airports()</c>. Paved runways get edge lines, a dashed centreline, threshold bars at both
/// landing thresholds and aiming-point bars 300 m in (long runways); no designators. One mesh holds every airport within
/// <see cref="WorldRenderSettings.MarkingRangeM"/> of the camera and is rebuilt only when that set changes or the airports were set again
/// (<see cref="WorldView"/> does that on configure and on every origin shift, so the quads follow the floating origin).
/// </summary>
public sealed class AirportMarkings
{
    private readonly MeshInstance3D _instance;
    private readonly ArrayMesh _mesh = new();
    private readonly ShaderMaterial _material;
    private readonly WorldRenderSettings _settings;
    private AirportInfo[] _airports = System.Array.Empty<AirportInfo>();
    private bool[] _selected = System.Array.Empty<bool>();
    private bool _dirty = true;
    private double _sinceCheck;

    // Reused build buffers.
    private readonly List<Vector3> _verts = new(2048);
    private readonly List<Vector3> _normals = new(2048);
    private readonly List<int> _indices = new(4096);

    /// <summary>Airports in the mesh now, and its quads (tests, overlay).</summary>
    public int DrawnAirports { get; private set; }
    public int Quads { get; private set; }
    /// <summary>Mesh rebuilds so far.</summary>
    public int Rebuilds { get; private set; }
    public ShaderMaterial Material => _material;
    /// <summary>The values last given to <see cref="SetRingLifts"/> (what the shader adds to the mesh y, before <see cref="WorldRenderSettings.MarkingLiftM"/>).</summary>
    public Vector4 RingLift { get; private set; }
    public Vector3 RingEdge { get; private set; }
    /// <summary>Vertices of the current mesh (frame of airports(); the shader adds the ring lift to y). For tests.</summary>
    public IReadOnlyList<Vector3> Vertices => _verts;
    public MeshInstance3D Instance => _instance;

    public AirportMarkings(Node3D parent, WorldRenderSettings settings, string shadersPath)
    {
        _settings = settings;
        _material = new ShaderMaterial { Shader = GD.Load<Shader>(shadersPath + "/airport_marking.gdshader") };
        settings.ApplyToMarkings(_material);
        _instance = new MeshInstance3D
        {
            Name = "AirportMarkings",
            Mesh = _mesh,
            MaterialOverride = _material,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            ExtraCullMargin = TilePool.CurveCullMargin + 20f,
            Visible = false,
        };
        parent.AddChild(_instance);
    }

    /// <summary>
    /// The height of the drawn runway above ground_height_at per ring R0..R3 (<paramref name="ringLift"/> = the runway's stacking level x step since generator 2,
    /// plus the band's drape lift for a generator-1 library) and the view distances beyond which
    /// ring 1, 2, 3 applies (<paramref name="ringEdge"/>); see <see cref="WorldView"/> <c>ComputeRunwayLifts</c> and the shader's header.
    /// </summary>
    public void SetRingLifts(Vector4 ringLift, Vector3 ringEdge)
    {
        RingLift = ringLift;
        RingEdge = ringEdge;
        _material.SetShaderParameter("ring_lift", ringLift);
        _material.SetShaderParameter("ring_edge", ringEdge);
    }

    /// <summary>Replaces the airports (local positions of the current frame); the mesh is rebuilt on the next <see cref="Update"/>.</summary>
    public void SetAirports(AirportInfo[] airports)
    {
        _airports = airports;
        _selected = new bool[airports.Length];
        _dirty = true;
        _sinceCheck = 1e9;
    }

    /// <summary>Re-selects the airports near the camera (twice a second) and rebuilds the mesh when the selection changed.</summary>
    public void Update(Vector3 cameraLocal, double delta)
    {
        _sinceCheck += delta;
        if (_sinceCheck < 0.5 && !_dirty) return;
        _sinceCheck = 0;
        bool changed = _dirty;
        if (!_settings.MarkingsEnabled)
        {
            if (DrawnAirports != 0 || _instance.Visible) { _mesh.ClearSurfaces(); _instance.Visible = false; DrawnAirports = 0; Quads = 0; }
            _dirty = false;
            return;
        }
        for (int i = 0; i < _airports.Length; i++)
        {
            var a = _airports[i];
            double reach = _settings.MarkingRangeM + HalfExtent(a);
            double dx = a.Position.X - cameraLocal.X, dz = a.Position.Z - cameraLocal.Z;
            bool sel = dx * dx + dz * dz < reach * reach && a.Runways.Length > 0;
            if (sel != _selected[i]) { _selected[i] = sel; changed = true; }
        }
        if (!changed) return;
        _dirty = false;
        Rebuild();
    }

    private static double HalfExtent(AirportInfo a)
    {
        double m = 0;
        foreach (var r in a.Runways) m = System.Math.Max(m, r.LengthM);
        return m;
    }

    private void Rebuild()
    {
        Rebuilds++;
        _verts.Clear();
        _normals.Clear();
        _indices.Clear();
        int airports = 0;
        for (int i = 0; i < _airports.Length; i++)
        {
            if (!_selected[i]) continue;
            airports++;
            foreach (var r in _airports[i].Runways) if (r.Paved) AddRunway(r);
        }
        _mesh.ClearSurfaces();
        DrawnAirports = airports;
        Quads = _verts.Count / 4;
        if (_verts.Count == 0) { _instance.Visible = false; return; }
        var arrays = new Godot.Collections.Array();
        arrays.Resize((int)Mesh.ArrayType.Max);
        arrays[(int)Mesh.ArrayType.Vertex] = _verts.ToArray();
        arrays[(int)Mesh.ArrayType.Normal] = _normals.ToArray();
        arrays[(int)Mesh.ArrayType.Index] = _indices.ToArray();
        _mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays);
        _instance.Visible = true;
    }

    // ---- one runway --------------------------------------------------------

    private void AddRunway(RunwayInfo r)
    {
        // Frame of the runway: s along the le -> he axis from the le landing threshold, t to the right of that direction, y linear
        // between the two landing thresholds (the height function is exactly that there, contract section 2).
        Vector3 a = r.Le.ThresholdPosition, b = r.He.ThresholdPosition;
        var axis = new Vector2(b.X - a.X, b.Z - a.Z);
        float len = axis.Length();
        if (len < 100f) return;
        var dir = axis / len;
        var side = new Vector2(-dir.Y, dir.X);
        float slope = (b.Y - a.Y) / len;
        float w = (float)r.WidthM;
        var o = new Vector2(a.X, a.Z);

        void Quad(float s0, float s1, float t0, float t1)
        {
            int v0 = _verts.Count;
            Vector3 P(float s, float t)
            {
                var p = o + dir * s + side * t;
                return new Vector3(p.X, a.Y + slope * s, p.Y);
            }
            _verts.Add(P(s0, t0)); _verts.Add(P(s1, t0)); _verts.Add(P(s1, t1)); _verts.Add(P(s0, t1));
            for (int k = 0; k < 4; k++) _normals.Add(Vector3.Up);
            _indices.Add(v0); _indices.Add(v0 + 1); _indices.Add(v0 + 2);
            _indices.Add(v0); _indices.Add(v0 + 2); _indices.Add(v0 + 3);
        }

        float half = w * 0.5f;
        // Edge lines, 0.9 m wide, 0.9 m inside the pavement edge, end to end between the thresholds.
        const float line = 0.9f;
        Quad(0f, len, half - 0.9f - line, half - 0.9f);
        Quad(0f, len, -half + 0.9f, -half + 0.9f + line);

        // Threshold bars: stripes 1.8 m wide on a 3.6 m pitch across the runway, 30 m long, starting 6 m past each landing threshold.
        float usable = w - 2f * 3f;
        int n = System.Math.Clamp((int)(usable / 3.6f), 2, 12);
        for (int i = 0; i < n; i++)
        {
            float tc = (i - (n - 1) * 0.5f) * 3.6f;
            Quad(6f, 36f, tc - 0.9f, tc + 0.9f);
            Quad(len - 36f, len - 6f, tc - 0.9f, tc + 0.9f);
        }

        // Centreline: 30 m dashes on a 50 m pitch between the threshold bars.
        float from = 36f + 12f, to = len - 36f - 12f;
        for (float s = from; s + 30f <= to; s += 50f) Quad(s, s + 30f, -0.45f, 0.45f);

        // Aiming point marks (runways of 1200 m and more): two bars 45 m long, 300 m past each landing threshold.
        if (len >= 1200f)
        {
            float tOff = System.Math.Max(2f, w * 0.2f);
            float bw = System.Math.Clamp(w * 0.12f, 1.5f, 6f);
            foreach (float s in new[] { 300f, len - 300f - 45f })
            {
                Quad(s, s + 45f, tOff - bw * 0.5f, tOff + bw * 0.5f);
                Quad(s, s + 45f, -tOff - bw * 0.5f, -tOff + bw * 0.5f);
            }
        }
    }
}
