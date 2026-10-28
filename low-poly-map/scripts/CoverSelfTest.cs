using System.Collections.Generic;
using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--cover-selftest</c>: simulates a Rust side that obeys the delivery guarantees (refine = 4 children
/// delivered as a group, parent unloaded only after all 4 children, coarsen = parent re-delivered before its children
/// are unloaded), with random interleaving, and runs <see cref="TileCover.Verify"/> after every atomic step.
/// Uses a private TilePool/TileCover, so the live scene is untouched. Prints PASS/FAIL. Runs the same simulation twice: on the vector
/// layer (z13..z16, <see cref="TilePool"/>) and on the terrain layer (api 1.5, z10..z14, five rings, <see cref="TerrainPool"/>), each with its own
/// <see cref="TileCover"/> and its own root zoom for the sibling rule.
/// </summary>
public static class CoverSelfTest
{
    private const string Arg = "--cover-selftest";

    public static bool Requested()
    {
        foreach (var a in OS.GetCmdlineUserArgs()) if (a == Arg) return true;
        return false;
    }

    private sealed class Op
    {
        public long Tile;
        public bool Refine;
        public int Step;
    }

    public static bool Run(Node3D host)
    {
        var container = new Node3D { Name = "CoverSelfTest" };
        host.AddChild(container);
        bool ok = true;
        {
            var pool = new TilePool(container);
            ok &= RunLayer("vector z13..z16", 13, 16, 4660, 2600, (k, ring) => pool.Acquire(TileKey.ToText(k), k, ring), n => pool.Release(n), () => (pool.FreeCount, pool.TotalCreated));
        }
        {
            var pool = new TerrainPool(container);
            ok &= RunLayer("terrain z10..z14", 10, 14, 583, 325, (k, ring) => pool.Acquire(TileKey.ToTerrainText(k), k, ring), n => pool.Release((TerrainNodes)n), () => (pool.FreeCount, pool.TotalCreated));
        }
        container.QueueFree();
        return ok;
    }

    private static bool RunLayer(string name, int rootZ, int leafZ, int baseX, int baseY,
        System.Func<long, int, TileNodes> acquire, System.Action<TileNodes> release, System.Func<(int Free, int Created)> poolState)
    {
        var cover = new TileCover(rootZ, leafZ);
        var rng = new RandomNumberGenerator { Seed = 12345 };
        var leaves = new HashSet<long>();      // tiles currently part of the (settled) cover
        var ops = new List<Op>();              // in-flight refine / coarsen operations
        var busy = new HashSet<long>();        // tiles touched by an in-flight op
        var pending = new HashSet<long>();
        var none = new HashSet<long>(); // strict: the delivery guarantees mean no hole even while operations are in flight
        var suspect = new HashSet<long>();
        var report = new StringBuilder();
        int errors = 0, steps = 0, replaced = 0, maxLoaded = 0;
        var visSeen = new bool[3];

        void Deliver(long k)
        {
            if (cover.TryGet(k, out var existing)) // re-delivery under the same key: in place
            {
                cover.BeginReplace(existing);
                cover.EndReplace(existing);
                replaced++;
                return;
            }
            var n = acquire(k, leafZ - TileKey.Z(k));
            n.Root.Position = Vector3.Zero;
            cover.Add(n);
        }
        void Unload(long k)
        {
            var n = cover.Remove(k);
            if (n != null) release(n);
        }
        void Check(string what)
        {
            steps++;
            report.Clear();
            int e = cover.Verify(none, suspect, report);
            e += cover.Verify(none, suspect, report); // second pass promotes suspects (holes) to errors
            if (e > 0)
            {
                errors += e;
                GD.PushError($"[CoverSelfTest] {name} after {what}: {e} problem(s)\n{report}");
            }
            foreach (var t in cover.All) visSeen[(int)t.Vis] = true;
            if (cover.Loaded > maxLoaded) maxLoaded = cover.Loaded;
        }

        // Settled start: a 3x3 block of root-zoom tiles delivered in random order (the root zoom is visible alone).
        var roots = new List<long>();
        for (int dx = 0; dx < 3; dx++) for (int dy = 0; dy < 3; dy++) roots.Add(TileKey.Pack(rootZ, baseX + dx, baseY + dy));
        while (roots.Count > 0)
        {
            int i = (int)(rng.Randi() % (uint)roots.Count);
            long k = roots[i]; roots.RemoveAt(i);
            Deliver(k); leaves.Add(k);
            Check("root " + TileKey.ToText(k));
        }

        for (int iter = 0; iter < 6000; iter++)
        {
            // Start a new operation when fewer than 4 are in flight.
            if (ops.Count < 4 && rng.Randf() < 0.5f)
            {
                var list = new List<long>(leaves);
                long k = list[(int)(rng.Randi() % (uint)list.Count)];
                bool tryRefine = rng.Randf() < 0.55f;
                if (tryRefine && TileKey.Z(k) < leafZ && !busy.Contains(k))
                {
                    busy.Add(k); ops.Add(new Op { Tile = k, Refine = true });
                    pending.Add(TileKey.Ancestor(k, rootZ));
                }
                else if (!tryRefine && TileKey.Z(k) > rootZ)
                {
                    long p = TileKey.Parent(k);
                    bool allLeaves = true;
                    for (int q = 0; q < 4; q++) if (!leaves.Contains(TileKey.Child(p, q)) || busy.Contains(TileKey.Child(p, q))) allLeaves = false;
                    if (allLeaves && !busy.Contains(p))
                    {
                        busy.Add(p);
                        for (int q = 0; q < 4; q++) busy.Add(TileKey.Child(p, q));
                        ops.Add(new Op { Tile = p, Refine = false });
                        pending.Add(TileKey.Ancestor(p, rootZ));
                    }
                }
            }
            if (ops.Count == 0) continue;

            var op = ops[(int)(rng.Randi() % (uint)ops.Count)];
            long t = op.Tile;
            if (op.Refine)
            {
                // steps 0..3: deliver child q (in a random but fixed order = by q here), 4: unload parent
                if (op.Step < 4)
                {
                    Deliver(TileKey.Child(t, (op.Step + (int)(t % 4)) % 4));
                    Check("refine deliver " + TileKey.ToText(t) + " #" + op.Step);
                }
                else
                {
                    Unload(t);
                    leaves.Remove(t);
                    for (int q = 0; q < 4; q++) leaves.Add(TileKey.Child(t, q));
                    Check("refine unload parent " + TileKey.ToText(t));
                }
                op.Step++;
                if (op.Step > 4) { ops.Remove(op); busy.Remove(t); pending.Remove(TileKey.Ancestor(t, rootZ)); RecomputePending(pending, ops, rootZ); }
            }
            else
            {
                if (op.Step == 0)
                {
                    Deliver(t); // parent delivered again before its children go
                    Check("coarsen deliver parent " + TileKey.ToText(t));
                    if (rng.Randf() < 0.3f) { Deliver(t); Check("same-key re-upload " + TileKey.ToText(t)); }
                }
                else
                {
                    Unload(TileKey.Child(t, op.Step - 1));
                    Check("coarsen unload child " + TileKey.ToText(TileKey.Child(t, op.Step - 1)));
                }
                op.Step++;
                if (op.Step > 4)
                {
                    for (int q = 0; q < 4; q++) { leaves.Remove(TileKey.Child(t, q)); busy.Remove(TileKey.Child(t, q)); }
                    leaves.Add(t);
                    ops.Remove(op); busy.Remove(t); RecomputePending(pending, ops, rootZ);
                }
            }
        }

        // Drain: unload everything and make sure the pool got every node back.
        var all = new List<long>();
        foreach (var t in cover.All) all.Add(t.Packed);
        foreach (long k in all) Unload(k);
        var (poolFree, poolCreated) = poolState();
        bool poolOk = poolFree == poolCreated && cover.Loaded == 0
            && cover.HiddenByChildren == 0 && cover.WaitingSiblings == 0 && cover.TrianglesLoaded == 0;
        if (!poolOk) { errors++; GD.PushError($"[CoverSelfTest] {name} drain: pool free {poolFree}/{poolCreated}, loaded {cover.Loaded}, counters {cover.HiddenByChildren}/{cover.WaitingSiblings}"); }

        bool allStates = visSeen[0] && visSeen[1] && visSeen[2];
        GD.Print($"[CoverSelfTest] {name} {(errors == 0 && allStates ? "PASS" : "FAIL")}: {steps} verified steps, {errors} errors, max {maxLoaded} tiles loaded, "
            + $"{replaced} in-place re-uploads, pool created {poolCreated}, states seen visible/hiddenByChildren/waiting = {visSeen[0]}/{visSeen[1]}/{visSeen[2]}");
        return errors == 0 && allStates;
    }

    private static void RecomputePending(HashSet<long> pending, List<Op> ops, int rootZ)
    {
        pending.Clear();
        foreach (var o in ops) pending.Add(TileKey.Ancestor(o.Tile, rootZ));
    }
}
