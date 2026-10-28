using System.Diagnostics;
using System.Text;
using Godot;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--upload-microbench</c> (windowed, real renderer): times <c>ArrayMesh.AddSurfaceFromArrays</c> of a synthetic building tile
/// (20000 vertices: VERTEX, NORMAL, COLOR, TEX_UV, INDEX) and of a few variants (fewer attributes, the compress flag) into a reused mesh, plus
/// <c>ClearSurfaces</c>, so a change of the upload path can be judged on what the engine costs apart from the library and the managed copies.
/// Prints a table and quits. The mesh is shown (hidden meshes are not uploaded lazily in 4.x, but the instance keeps the renderer path honest).
/// </summary>
public partial class UploadMicroBench : Node
{
    const int Vertices = 20000, Triangles = 30000, Runs = 40;

    public static bool Requested() => System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--upload-microbench");

    public static void Attach(Node parent) => parent.CallDeferred(Node.MethodName.AddChild, new UploadMicroBench { Name = "UploadMicroBench" });

    int _frame;

    public override void _Process(double delta)
    {
        if (++_frame < 400) return; // let the synthetic city finish loading
        Run();
        GetTree().Quit();
    }

    static Godot.Collections.Array Make(bool normal, bool color, bool uv)
    {
        var rng = new RandomNumberGenerator { Seed = 7 };
        var v = new Vector3[Vertices];
        var n = new Vector3[Vertices];
        var c = new Color[Vertices];
        var t = new Vector2[Vertices];
        for (int i = 0; i < Vertices; i++)
        {
            v[i] = new Vector3(rng.RandfRange(-250, 250), rng.RandfRange(0, 80), rng.RandfRange(-250, 250));
            n[i] = new Vector3(rng.Randf() - 0.5f, rng.Randf() - 0.5f, rng.Randf() - 0.5f).Normalized();
            c[i] = new Color(rng.Randf(), rng.Randf(), rng.Randf(), 1f);
            t[i] = new Vector2(rng.Randf() * 20, rng.Randf() * 10);
        }
        var idx = new int[Triangles * 3];
        for (int i = 0; i < idx.Length; i++) idx[i] = (int)(rng.Randi() % Vertices);
        var a = new Godot.Collections.Array();
        a.Resize((int)Mesh.ArrayType.Max);
        a[(int)Mesh.ArrayType.Vertex] = v;
        if (normal) a[(int)Mesh.ArrayType.Normal] = n;
        if (color) a[(int)Mesh.ArrayType.Color] = c;
        if (uv) a[(int)Mesh.ArrayType.TexUV] = t;
        a[(int)Mesh.ArrayType.Index] = idx;
        return a;
    }

    static (double mean, double min, double max, double clear) Time(Godot.Collections.Array arrays, Mesh.ArrayFormat flags)
    {
        var mesh = new ArrayMesh();
        double sum = 0, min = double.MaxValue, max = 0, clearSum = 0;
        for (int i = 0; i < Runs; i++)
        {
            long t0 = Stopwatch.GetTimestamp();
            mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays, null, null, flags);
            double ms = (Stopwatch.GetTimestamp() - t0) * 1000.0 / Stopwatch.Frequency;
            long t1 = Stopwatch.GetTimestamp();
            mesh.ClearSurfaces();
            clearSum += (Stopwatch.GetTimestamp() - t1) * 1000.0 / Stopwatch.Frequency;
            sum += ms;
            if (ms < min) min = ms;
            if (ms > max) max = ms;
        }
        return (sum / Runs, min, max, clearSum / Runs);
    }

    void Run()
    {
        var sb = new StringBuilder("\nUPLOAD MICROBENCH (20000 vertices, 30000 triangles, ms per AddSurfaceFromArrays into a reused mesh)\n");
        void Row(string name, Godot.Collections.Array arrays, Mesh.ArrayFormat flags = 0)
        {
            var (mean, min, max, clear) = Time(arrays, flags);
            sb.AppendLine($"  {name,-34} mean {mean,6:0.00}  min {min,6:0.00}  max {max,6:0.00}   clear {clear,5:0.00}");
        }
        Row("full (vertex normal color uv idx)", Make(true, true, true));
        Row("no normal", Make(false, true, true));
        Row("no color", Make(true, false, true));
        Row("no uv", Make(true, true, false));
        Row("vertex + index only", Make(false, false, false));
        Row("full + compress attributes", Make(true, true, true), Mesh.ArrayFormat.FlagCompressAttributes);
        Row("full + dynamic update", Make(true, true, true), Mesh.ArrayFormat.FlagUseDynamicUpdate);
        var withAabb = Make(true, true, true);
        // Cost of the array marshalling alone: reading the vertex array back as a managed copy (what AsMeshArrays did).
        long t0 = Stopwatch.GetTimestamp();
        for (int i = 0; i < Runs; i++) { var copy = withAabb[(int)Mesh.ArrayType.Vertex].AsVector3Array(); if (copy.Length == 0) break; }
        sb.AppendLine($"  managed copy of the vertex array   {(Stopwatch.GetTimestamp() - t0) * 1000.0 / Stopwatch.Frequency / Runs,6:0.000} ms");
        GD.Print(sb.ToString());
    }
}
