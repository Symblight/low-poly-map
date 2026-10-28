using Godot;

namespace SimWorld;

/// <summary>
/// Draws every world material once, tiny and in front of the camera, for a few dozen frames after the view starts. The engine compiles a shader and the
/// pipeline of each (material, vertex layout) pair the first time it is drawn, which costs 10 to 25 ms: without this the first LOD fade of a flight paid it
/// for the dither variants (10 permutations: building, ground, point sprites and street pools, plain and dithering, plus the terrain's two), in the middle of a frame.
/// Now it happens while the first tiles are still being fetched. The nodes are one-off and freed afterwards (the pool is not involved).
/// In the same frames it makes the dither material clones the first fade bursts need (<see cref="ReserveTiles"/>), a few per frame, so no swap pays for creating them.
/// </summary>
public sealed class MaterialPrewarm
{
    /// <summary>Frames the nodes stay (the engine may finish a pipeline a frame or two after the first draw).</summary>
    public const int Frames = 24;
    /// <summary>Edge of the warm-up triangle in metres, and its distance in front of the camera.</summary>
    private const float TriangleM = 0.02f, AheadM = 3f;

    /// <summary>Clones made per frame (of each pool's four / one material kinds), and the frames after <see cref="Frames"/> the reserve may still take.</summary>
    private const int ClonesPerFrame = 6, ReserveExtraFrames = 240;

    private readonly TilePool _pool;
    private readonly TerrainPool? _terrain;
    private Node3D? _root;
    private int _framesLeft = Frames, _extraFrames = ReserveExtraFrames;
    private bool _reserveDone = true;
    private int _reserveTiles;

    /// <summary>Materials drawn (tests, log).</summary>
    public int MaterialCount { get; private set; }

    /// <summary>Fading tiles to hold dither clones for (<c>fades.max_active</c> swaps x tiles per swap); set it right after construction. 0 = none.</summary>
    public int ReserveTiles
    {
        get => _reserveTiles;
        set
        {
            _reserveTiles = value;
            _reserveDone = value <= 0;
        }
    }

    /// <summary>Builds the warm-up nodes under <paramref name="camera"/> (so they are always in view). The terrain pool may be null.</summary>
    public MaterialPrewarm(Camera3D camera, TilePool pool, TerrainPool? terrain)
    {
        _pool = pool;
        _terrain = terrain;
        _root = new Node3D { Name = "MaterialPrewarm", Position = new Vector3(0f, 0f, -AheadM) };
        camera.AddChild(_root);
        foreach (var w in pool.WarmupMaterials()) Add(w.Material, w.Kind);
        if (terrain != null)
        {
            AddMesh(terrain.Material, TriangleArrays(false, false, false), shadow: false);
            AddMesh(terrain.FadeMaterial, TriangleArrays(false, false, false), shadow: false);
        }
    }

    private void Add(Material m, WarmupKind kind)
    {
        switch (kind)
        {
            case WarmupKind.Building: AddMesh(m, TriangleArrays(true, true, true), shadow: true); break;
            case WarmupKind.Ground: AddMesh(m, TriangleArrays(true, true, false), shadow: false); break;
            case WarmupKind.Points: AddPoints(m); break;
            default: AddPool(m); break;
        }
    }

    private void AddMesh(Material m, Godot.Collections.Array arrays, bool shadow)
    {
        var mesh = new ArrayMesh();
        mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays);
        _root!.AddChild(new MeshInstance3D
        {
            Mesh = mesh,
            MaterialOverride = m,
            CastShadow = shadow ? GeometryInstance3D.ShadowCastingSetting.On : GeometryInstance3D.ShadowCastingSetting.Off,
        });
        MaterialCount++;
    }

    // Same layout as a tile's points mesh: one point with COLOR and CUSTOM0 (kind street).
    private void AddPoints(Material m)
    {
        var a = new Godot.Collections.Array();
        a.Resize((int)Mesh.ArrayType.Max);
        a[(int)Mesh.ArrayType.Vertex] = new[] { Vector3.Zero };
        a[(int)Mesh.ArrayType.Color] = new[] { new Color(1f, 1f, 1f, 0.125f) };
        a[(int)Mesh.ArrayType.Custom0] = new byte[] { 32, (byte)SpriteKind.Street, 0, 0 };
        var mesh = new ArrayMesh();
        mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Points, a);
        _root!.AddChild(new MeshInstance3D { Mesh = mesh, MaterialOverride = m, CastShadow = GeometryInstance3D.ShadowCastingSetting.Off });
        MaterialCount++;
    }

    // Same MultiMesh layout as a tile's pools (transform 3D + colour), one instance.
    private void AddPool(Material m)
    {
        var mm = new MultiMesh
        {
            TransformFormat = MultiMesh.TransformFormatEnum.Transform3D,
            UseColors = true,
            Mesh = new QuadMesh { Size = Vector2.One },
        };
        mm.InstanceCount = 1;
        mm.SetInstanceTransform(0, Transform3D.Identity);
        mm.SetInstanceColor(0, new Color(1f, 1f, 1f, 0.125f));
        _root!.AddChild(new MultiMeshInstance3D { Multimesh = mm, MaterialOverride = m, CastShadow = GeometryInstance3D.ShadowCastingSetting.Off });
        MaterialCount++;
    }

    private static Godot.Collections.Array TriangleArrays(bool normal, bool color, bool uv)
    {
        var a = new Godot.Collections.Array();
        a.Resize((int)Mesh.ArrayType.Max);
        a[(int)Mesh.ArrayType.Vertex] = new[] { new Vector3(0, 0, 0), new Vector3(TriangleM, 0, 0), new Vector3(0, TriangleM, 0) };
        if (normal) a[(int)Mesh.ArrayType.Normal] = new[] { Vector3.Back, Vector3.Back, Vector3.Back };
        if (color) a[(int)Mesh.ArrayType.Color] = new[] { new Color(0.1f, 0.5f, 0.2f, 0.3f), new Color(0.1f, 0.5f, 0.2f, 0.3f), new Color(0.1f, 0.5f, 0.2f, 0.3f) };
        if (uv) a[(int)Mesh.ArrayType.TexUV] = new[] { new Vector2(0, 0), new Vector2(1, 0), new Vector2(0, 1) };
        a[(int)Mesh.ArrayType.Index] = new[] { 0, 1, 2 };
        return a;
    }

    /// <summary>
    /// Once per frame: makes a few of the reserved clones, frees the nodes after <see cref="Frames"/> frames, and returns true when the nodes are gone and the
    /// reserve is complete (or has used its extra frames).
    /// </summary>
    public bool Tick()
    {
        if (!_reserveDone)
        {
            _reserveDone = _pool.ReserveFadeClones(_reserveTiles, ClonesPerFrame) & (_terrain?.ReserveFadeClones(_reserveTiles, ClonesPerFrame) ?? true);
            if (_reserveDone) GD.Print($"[WorldView] fade clone reserve ready: {_reserveTiles} tiles x {TilePool.FadeNodes} vector nodes and {_reserveTiles} terrain nodes");
        }
        if (_root != null && --_framesLeft <= 0)
        {
            _root.GetParent()?.RemoveChild(_root);
            _root.QueueFree();
            _root = null;
        }
        if (_root != null) return false;
        return _reserveDone || --_extraFrames <= 0;
    }
}
