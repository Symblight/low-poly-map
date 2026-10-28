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
/// <see cref="TileCover"/> and its own root zoom for the sibling rule, and a third time on the landmark layer's flat set (api 1.11,
/// <see cref="LandmarkCover"/>). With a running world <see cref="CoverLiveCheck"/> then compares the live landmark tile map to <c>debug_tiles</c>.
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
        ok &= RunLandmarkLayer(container);
        container.QueueFree();
        return ok;
    }

    /// <summary>
    /// The landmark layer's flat tile map (api 1.11): random add / replace / remove / shift of "l/z/x/y" tiles over a private <see cref="TilePool"/> and
    /// <see cref="LandmarkCover"/>, verified after every step (visible whenever held, counters equal a recount, the pool gets every node back).
    /// </summary>
    private static bool RunLandmarkLayer(Node3D container)
    {
        var pool = new TilePool(container);
        var cover = new LandmarkCover();
        var rng = new RandomNumberGenerator { Seed = 777 };
        var report = new StringBuilder();
        var held = new List<long>();
        int errors = 0, steps = 0, replaced = 0, maxHeld = 0;
        void Check(string what)
        {
            steps++;
            report.Clear();
            int e = cover.Verify(report);
            if (e > 0) { errors += e; GD.PushError($"[CoverSelfTest] landmarks after {what}: {e} problem(s)\n{report}"); }
            if (cover.Loaded != held.Count) { errors++; GD.PushError($"[CoverSelfTest] landmarks after {what}: held {cover.Loaded} != expected {held.Count}"); }
            if (cover.Loaded > maxHeld) maxHeld = cover.Loaded;
        }
        for (int i = 0; i < 3000; i++)
        {
            float r = rng.Randf();
            long k = TileKey.Pack(11, 1160 + (int)(rng.Randi() % 12), 645 + (int)(rng.Randi() % 12));
            if (r < 0.45f)
            {
                if (cover.TryGet(k, out var existing)) // never expected from the library; must replace in place, never add a second copy
                {
                    cover.BeginReplace(existing);
                    existing.Tris = (int)(rng.Randi() % 5000);
                    existing.LightCount = (int)(rng.Randi() % 400);
                    cover.EndReplace(existing);
                    replaced++;
                    Check("replace " + TileKey.ToLandmarkText(k));
                    continue;
                }
                var n = pool.Acquire(TileKey.ToLandmarkText(k), k, 0);
                n.Tris = (int)(rng.Randi() % 5000);
                n.LightCount = (int)(rng.Randi() % 400);
                n.BeaconCount = (int)(rng.Randi() % 50);
                n.Root.Position = new Vector3(i, 0, -i);
                cover.Add(n);
                held.Add(k);
                Check("add " + TileKey.ToLandmarkText(k));
            }
            else if (r < 0.9f)
            {
                if (held.Count == 0) continue;
                long d = held[(int)(rng.Randi() % (uint)held.Count)];
                var n = cover.Remove(d);
                if (n == null) { errors++; GD.PushError("[CoverSelfTest] landmarks: held key not found"); continue; }
                n.Tris = 0; n.LightCount = 0; n.BeaconCount = 0;
                pool.Release(n);
                held.Remove(d);
                if (cover.Remove(d) != null) { errors++; GD.PushError("[CoverSelfTest] landmarks: removed key still present"); }
                Check("remove " + TileKey.ToLandmarkText(d));
            }
            else
            {
                var before = new Dictionary<long, Vector3>();
                foreach (var n in cover.All) before[n.Packed] = n.Root.Position;
                var shift = new Vector3(rng.Randf() * 5000f, 0f, rng.Randf() * 5000f);
                cover.Shift(shift);
                foreach (var n in cover.All)
                    if ((n.Root.Position - (before[n.Packed] - shift)).Length() > 1e-3f) { errors++; GD.PushError("[CoverSelfTest] landmarks: shift moved a tile wrongly"); break; }
                Check("shift");
            }
        }
        foreach (long k in held.ToArray())
        {
            var n = cover.Remove(k);
            if (n != null) { n.Tris = 0; n.LightCount = 0; n.BeaconCount = 0; pool.Release(n); }
        }
        bool poolOk = pool.FreeCount == pool.TotalCreated && cover.Loaded == 0 && cover.TrianglesLoaded == 0 && cover.LightsLoaded == 0 && cover.BeaconsLoaded == 0;
        if (!poolOk) { errors++; GD.PushError($"[CoverSelfTest] landmarks drain: pool free {pool.FreeCount}/{pool.TotalCreated}, loaded {cover.Loaded}, counters {cover.TrianglesLoaded}/{cover.LightsLoaded}/{cover.BeaconsLoaded}"); }
        GD.Print($"[CoverSelfTest] landmarks flat set {(errors == 0 ? "PASS" : "FAIL")}: {steps} verified steps, {errors} errors, max {maxHeld} tiles held, {replaced} in-place re-uploads, pool created {pool.TotalCreated}");
        return errors == 0;
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
