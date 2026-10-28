using System.Collections.Generic;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// <c>--fade-selftest</c> (headless works, no world needed): guards the fix of the tile-shaped holes caused by instance uniforms. (1) No addon shader may declare
/// <c>instance uniform</c> (they reserve 16 entries of a 4096-node buffer per node, never give them back, and read as 0 once it is full: invisible faded
/// tiles). (2) A <see cref="TilePool"/> / <see cref="TerrainPool"/> of 1500 node sets fades 20 tiles at a time for 300 rounds: a fade swaps every drawn node onto its own
/// dither clone (distinct per tile, the plain material restored afterwards), the clones go back to their pools, and no more than the concurrent fades ever existed.
/// (3) A parameter set on the pool afterwards reaches every clone (no stale window shares / fog / emission). Prints <c>FADE SELFTEST PASS/FAIL</c>, exit 0 / 1.
/// </summary>
public partial class FadeSelfTest : Node
{
    public static bool Requested() => System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--fade-selftest");

    public override void _Ready() => CallDeferred(nameof(Run));

    private void Run()
    {
        var fails = new List<string>();
        void Check(bool ok, string what) { if (!ok) fails.Add(what); }

        // (1) shader sources
        int scanned = 0;
        foreach (var f in DirAccess.GetFilesAt(AddonPaths.Root + "/shaders"))
        {
            if (!f.EndsWith(".gdshader") && !f.EndsWith(".gdshaderinc")) continue;
            scanned++;
            foreach (var line in FileAccess.GetFileAsString(AddonPaths.Root + "/shaders/" + f).Split('\n'))
            {
                var t = line.Trim();
                Check(!(t.StartsWith("instance uniform") || t.StartsWith("instance  uniform")), $"{f}: declares an instance uniform: {t}");
            }
        }

        // (2) + (3) pools
        var root = new Node3D { Name = "FadeTestRoot" };
        AddChild(root);
        var pool = new TilePool(root, new WorldRenderSettings());
        var tpool = new TerrainPool(root, new WorldRenderSettings());
        const int Sets = 1500;
        var sets = new List<TileNodes>(Sets);
        var tsets = new List<TerrainNodes>(Sets);
        for (int i = 0; i < Sets; i++)
        {
            var n = pool.Acquire(TileKey.ToText(TileKey.Pack(15, i, 7)), TileKey.Pack(15, i, 7), 1);
            sets.Add(n);
            var t = tpool.Acquire(TileKey.ToTerrainText(TileKey.Pack(12, i, 3)), TileKey.Pack(12, i, 3), 2);
            tsets.Add(t);
        }
        var rng = new RandomNumberGenerator { Seed = 7 };
        var active = new List<TileNodes>();
        for (int round = 0; round < 300; round++)
        {
            for (int k = 0; k < 20; k++)
            {
                int i = rng.RandiRange(0, Sets - 1);
                var n = sets[i];
                if (n.FadeMaterialActive) continue;
                pool.SetFade(n, 0.5f);
                tpool.SetFade(tsets[i], 0.5f);
                active.Add(n);
                Check(n.MeshInstance.MaterialOverride != pool.BuildingMaterial && n.MeshInstance.MaterialOverride != pool.BuildingFadeMaterial, "building node uses the plain or the template material while fading");
                Check(n.Points.MaterialOverride != pool.Sprites.Point && n.Pools.MaterialOverride != pool.Sprites.Pool, "sprite node still on the plain material while fading");
                Check((float)((ShaderMaterial)n.MeshInstance.MaterialOverride!).GetShaderParameter("fade") == 0.5f, "fade value not on the node's own clone");
            }
            if (active.Count >= 2) // two tiles fading together must not share a material (each has its own fade)
                Check(active[0].MeshInstance.MaterialOverride != active[1].MeshInstance.MaterialOverride, "two fading tiles share one dither material");
            foreach (var n in active)
            {
                pool.EndFade(n);
                int i = sets.IndexOf(n);
                tpool.EndFade(tsets[i]);
                Check(n.MeshInstance.MaterialOverride == pool.BuildingMaterial && n.GroundInstance.MaterialOverride == pool.GroundMaterial && n.Points.MaterialOverride == pool.Sprites.Point
                      && n.Pools.MaterialOverride == pool.Sprites.Pool, "plain materials not restored after the fade");
            }
            active.Clear();
        }
        Check(pool.BuildingFades.Clones <= 25 && pool.BuildingFades.FreeClones == pool.BuildingFades.Clones, $"building dither clones: {pool.BuildingFades.Clones} created, {pool.BuildingFades.FreeClones} free (expected at most 25, all returned)");

        // (3) a later parameter reaches templates and clones alike
        var held = new List<TileNodes> { sets[0], sets[1] };
        foreach (var n in held) pool.SetFade(n, 0.25f);
        pool.SetEmissionLevel(3.5f);
        pool.SetWindowShares(new float[] { 0.1f, 0.2f, 0.3f, 0f, 0.5f, 0f, 0f, 0f });
        pool.SetLightFog(1000f, 9000f, 2f);
        foreach (var n in held)
        {
            var bm = (ShaderMaterial)n.MeshInstance.MaterialOverride!;
            var lm = (ShaderMaterial)n.Points.MaterialOverride!;
            Check((float)bm.GetShaderParameter("emission_level") == 3.5f, "emission_level not on a dither clone");
            Check(bm.GetShaderParameter("window_share").AsFloat32Array()[2] == 0.3f, "window_share not on a dither clone");
            Check((float)lm.GetShaderParameter("fog_end_m") == 9000f, "fog_end_m not on a sprite dither clone");
        }
        Check(pool.ReadWindowShares(true)[1] == 0.2f && pool.ReadWindowShares(false)[1] == 0.2f, "window shares differ between the plain and the dither template");
        foreach (var n in held) pool.EndFade(n);
        // a clone rented after the change starts from the current values
        pool.SetFade(sets[2], 0.5f);
        Check((float)((ShaderMaterial)sets[2].MeshInstance.MaterialOverride!).GetShaderParameter("emission_level") == 3.5f, "a clone rented later missed emission_level");
        pool.EndFade(sets[2]);

        GD.Print(fails.Count == 0
            ? $"FADE SELFTEST PASS: {scanned} shader files without instance uniforms; {Sets} vector + {Sets} terrain sets, 300 fade rounds, building clones {pool.BuildingFades.Clones}"
            : "FADE SELFTEST FAIL: " + string.Join("; ", new HashSet<string>(fails)));
        GetTree().Quit(fails.Count == 0 ? 0 : 1);
    }
}
