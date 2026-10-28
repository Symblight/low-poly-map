using Godot;

namespace SimWorld;

/// <summary>
/// Draws every world material once, tiny and in front of the camera, for a few dozen frames after the view starts. The engine compiles a shader and the
/// pipeline of each (material, vertex layout) pair the first time it is drawn, which costs 10 to 25 ms: without this the first LOD fade of a flight paid it
/// for the dither variants (14 permutations: building, ground and five sprite sets, plain and dithering, plus the terrain's two), in the middle of a frame.
/// Now it happens while the first tiles are still being fetched. The nodes are one-off and freed afterwards (the pool is not involved).
/// </summary>
public sealed class MaterialPrewarm
{
    /// <summary>Frames the nodes stay (the engine may finish a pipeline a frame or two after the first draw).</summary>
    public const int Frames = 24;
    /// <summary>Edge of the warm-up triangle in metres, and its distance in front of the camera.</summary>
    private const float TriangleM = 0.02f, AheadM = 3f;

    private Node3D? _root;
    private int _framesLeft = Frames;

    /// <summary>Materials drawn (tests, log).</summary>
    public int MaterialCount { get; private set; }

    /// <summary>Builds the warm-up nodes under <paramref name="camera"/> (so they are always in view). The terrain pool may be null.</summary>
    public MaterialPrewarm(Camera3D camera, TilePool pool, TerrainPool? terrain)
    {
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
            default: AddSprite(m); break;
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

    // Same MultiMesh layout as a tile's sprite set (transform 3D + colour), one instance.
    private void AddSprite(Material m)
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

    /// <summary>Once per frame; frees the nodes after <see cref="Frames"/> frames and returns true then (and afterwards).</summary>
    public bool Tick()
    {
        if (_root == null) return true;
        if (--_framesLeft > 0) return false;
        _root.GetParent()?.RemoveChild(_root);
        _root.QueueFree();
        _root = null;
        return true;
    }
}
