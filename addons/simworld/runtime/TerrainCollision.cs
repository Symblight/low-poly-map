#nullable enable
using Godot;

namespace SimWorld;

/// <summary>
/// Optional, very simple ground collision (default off, see <see cref="WorldView.TerrainCollisionEnabled"/>): ONE <c>StaticBody3D</c> with ONE
/// <c>HeightMapShape3D</c> patch (default 33 x 33 samples, 40 m apart, about 1.3 km wide) centred under the camera. Heights come from the library's
/// <c>ground_height_at</c> (the exact procedural heightfield), sampled on a grid snapped to the spacing. The patch is re-centred and refilled when the
/// camera is more than a quarter of the patch half-size from its centre (rows are filled within <see cref="FillBudgetUs"/> per frame and the new data is
/// swapped in as a whole, so the old patch keeps working meanwhile) and moves with every origin shift (<see cref="Shift"/>); a HeightMapShape3D has unit
/// sample spacing, so the body is scaled in x / z by the spacing (y scale stays 1).
/// <para>The surface is the exact heightfield interpolated linearly between the samples: it differs from the drawn terrain triangles by up to the band's
/// <c>max_error_m</c> (plus the interpolation error between samples, which grows with the spacing). There is no collision for buildings or for terrain
/// beyond the patch; with the terrain layer off the ground is flat at 0 m.</para>
/// </summary>
public sealed class TerrainCollision
{
    private readonly StaticBody3D _body;
    private readonly CollisionShape3D _cs;
    private readonly HeightMapShape3D _shape = new();
    private readonly System.Func<double, double, double> _height;
    private float[] _data = System.Array.Empty<float>();
    private int _n = 33, _row;
    private float _sp = 40f;
    private float _cx, _cz, _nx, _nz;
    private bool _valid, _filling;

    /// <summary>Samples per side (odd, 3..129) and spacing in metres; a change restarts the patch.</summary>
    public int Samples { get; set; } = 33;
    public float SpacingM { get; set; } = 40f;
    /// <summary>Time (us) a frame may spend sampling heights; at least one row is filled per frame.</summary>
    public long FillBudgetUs { get; set; } = 1000;

    /// <summary>Patch centre in the local frame (valid after the first fill).</summary>
    public Vector2 Center => new(_cx, _cz);
    public bool Ready => _valid;
    /// <summary>Completed fills, the longest single-frame cost of filling (us), and the total cost of the last fill including the commit (us).</summary>
    public int Fills { get; private set; }
    public long FillFrameUsMax { get; private set; }
    public long LastFillUs { get; private set; }
    public long CommitUsMax { get; private set; }
    private long _fillUs;

    public TerrainCollision(Node parent, System.Func<double, double, double> groundHeight)
    {
        _height = groundHeight;
        _cs = new CollisionShape3D { Name = "Shape", Shape = _shape, Disabled = true };
        _body = new StaticBody3D { Name = "TerrainCollision" };
        _body.AddChild(_cs);
        parent.AddChild(_body);
    }

    /// <summary>Removes the body (the feature was turned off).</summary>
    public void Free() => _body.QueueFree();

    /// <summary>The world was reconfigured: the patch is stale, refill on the next update.</summary>
    public void Invalidate()
    {
        _valid = false;
        _filling = false;
        _cs.Disabled = true;
    }

    /// <summary>Origin shift: the patch (and a fill in progress) move with the world; the heights stay valid.</summary>
    public void Shift(Vector3 shift)
    {
        _cx -= shift.X; _cz -= shift.Z; _nx -= shift.X; _nz -= shift.Z;
        _body.Position = new Vector3(_cx, 0f, _cz);
    }

    /// <summary>Once per frame after the camera update / origin shift; <paramref name="camera"/> is in the local frame.</summary>
    public void Update(Vector3 camera)
    {
        int n = System.Math.Clamp(Samples | 1, 3, 129);
        float sp = System.Math.Max(SpacingM, 1f);
        if (n != _n || sp != _sp) { _n = n; _sp = sp; Invalidate(); }
        if (!_filling)
        {
            float quarter = (n - 1) * sp * 0.5f * 0.25f;
            if (_valid && System.Math.Abs(camera.X - _cx) <= quarter && System.Math.Abs(camera.Z - _cz) <= quarter) return;
            _nx = Mathf.Round(camera.X / sp) * sp;
            _nz = Mathf.Round(camera.Z / sp) * sp;
            if (_data.Length != n * n) _data = new float[n * n];
            _row = 0;
            _fillUs = 0;
            _filling = true;
        }
        long t0 = (long)Time.GetTicksUsec();
        float half = (n - 1) * 0.5f;
        while (_row < n)
        {
            double z = _nz + (_row - half) * sp;
            for (int i = 0; i < n; i++) _data[_row * n + i] = (float)_height(_nx + (i - half) * sp, z);
            _row++;
            if ((long)Time.GetTicksUsec() - t0 >= FillBudgetUs) break;
        }
        if (_row >= n)
        {
            long c0 = (long)Time.GetTicksUsec();
            _shape.MapWidth = n;
            _shape.MapDepth = n;
            _shape.MapData = _data;
            _body.Scale = new Vector3(sp, 1f, sp);
            _body.Position = new Vector3(_nx, 0f, _nz);
            _cx = _nx; _cz = _nz;
            _cs.Disabled = false;
            _valid = true;
            _filling = false;
            Fills++;
            long commit = (long)Time.GetTicksUsec() - c0;
            if (commit > CommitUsMax) CommitUsMax = commit;
            _fillUs += (long)Time.GetTicksUsec() - t0;
            LastFillUs = _fillUs;
        }
        else _fillUs += (long)Time.GetTicksUsec() - t0;
        long frame = (long)Time.GetTicksUsec() - t0;
        if (frame > FillFrameUsMax) FillFrameUsMax = frame;
    }
}
