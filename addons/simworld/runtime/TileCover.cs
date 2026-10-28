using System.Collections.Generic;
using System.Text;
using Godot;

namespace SimWorld;

/// <summary>
/// The set of uploaded tiles plus the sibling-swap visibility rule of the quadtree cover:
/// a tile is visible iff (z == root zoom or all 4 siblings are uploaded) and not all 4 of its children are uploaded.
/// One instance per layer: the vector layer (root z13, leaf z16, 4 rings) and, since api 1.5, the terrain layer (root =
/// terrain.far_zoom, leaf = terrain.near_zoom, default z10..z14, 5 rings; see <see cref="Configure"/>).
/// Dithered LOD fades add two exceptions: a tile that has a poll_fades entry (<see cref="TileNodes.Fading"/>) is visible
/// whenever it is uploaded (both sides of a swap show at once), and a freshly uploaded tile stays hidden
/// (<see cref="Vis.AwaitingFade"/>) until poll_fades has run once after its upload, so its shader fade is set before its
/// first visible frame. Godot never hides tiles any other way. On every upload / unload only the affected keys (the tile itself,
/// its parent, its siblings and its children) are re-evaluated; everything is key arithmetic on packed longs,
/// so a frame allocates nothing. The ground mesh is a child of the same tile root as the buildings, so it follows the
/// same rule by construction (one Root.Visible flag, one origin shift); this class only counts it.
/// </summary>
public sealed class TileCover
{
    public enum Vis { Visible = 0, HiddenByChildren = 1, WaitingSiblings = 2, AwaitingFade = 3 }

    /// <summary>Coarsest zoom of this layer (the sibling rule does not apply to it): 13 for the vector layer.</summary>
    public int RootZ { get; private set; } = TileKey.RootZ;
    /// <summary>Finest zoom of this layer (tiles of it have no children): 16 for the vector layer.</summary>
    public int LeafZ { get; private set; } = TileKey.LeafZ;
    /// <summary>Number of rings = zooms of this layer (ring = LeafZ - z).</summary>
    public int Rings => LeafZ - RootZ + 1;

    public TileCover() { }

    public TileCover(int rootZ, int leafZ) { Configure(rootZ, leafZ); }

    /// <summary>
    /// Sets the zoom range (root = coarsest, leaf = finest) and resizes the per-ring counters. Only allowed while no tile is held
    /// (the terrain layer's range comes from the profile after each configure, when everything has been unloaded).
    /// </summary>
    public void Configure(int rootZ, int leafZ)
    {
        if (_tiles.Count != 0) throw new System.InvalidOperationException("TileCover.Configure with tiles held");
        if (leafZ < rootZ) throw new System.ArgumentException("leafZ < rootZ");
        RootZ = rootZ;
        LeafZ = leafZ;
        VisibleByRing = new int[Rings];
        LoadedByRing = new int[Rings];
        HiddenByChildren = 0;
        WaitingSiblings = 0;
        AwaitingFadeTiles = 0;
        FadingTiles = 0;
        TrianglesLoaded = TrianglesVisible = 0;
        GroundTilesLoaded = GroundTilesVisible = 0;
        GroundTrianglesLoaded = GroundTrianglesVisible = 0;
        LightsLoaded = LightsVisible = 0;
        LightTilesLoaded = ClusteredTilesLoaded = ClusteredTilesVisible = LightTilesVisible = 0;
        _awaiting.Clear();
    }

    /// <summary>
    /// True once the Rust build exports poll_fades(): new tiles then wait for the first poll_fades after their upload.
    /// False (default, and in the cover self test) = the pre-fade behaviour, visibility is the sibling-swap rule only.
    /// </summary>
    public bool FadesEnabled { get; set; }
    private readonly List<TileNodes> _awaiting = new(32);
    /// <summary>Tiles currently in a poll_fades transition (visible by the fade exception).</summary>
    public int FadingTiles { get; private set; }

    private readonly Dictionary<long, TileNodes> _tiles = new();

    // Counters kept incrementally (see Account).
    public int Loaded => _tiles.Count;
    public int[] VisibleByRing { get; private set; } = new int[4];
    public int[] LoadedByRing { get; private set; } = new int[4];
    public int HiddenByChildren { get; private set; }
    public int WaitingSiblings { get; private set; }
    public int AwaitingFadeTiles { get; private set; }
    public long TrianglesLoaded { get; private set; }
    public long TrianglesVisible { get; private set; }
    /// <summary>Ground meshes (one draw call each when visible) and their triangles.</summary>
    public int GroundTilesLoaded { get; private set; }
    public int GroundTilesVisible { get; private set; }
    public long GroundTrianglesLoaded { get; private set; }
    public long GroundTrianglesVisible { get; private set; }
    public long LightsLoaded { get; private set; }
    public long LightsVisible { get; private set; }
    public int LightTilesLoaded { get; private set; }
    public int ClusteredTilesLoaded { get; private set; }
    public int ClusteredTilesVisible { get; private set; }
    public int LightTilesVisible { get; private set; }

    public IEnumerable<TileNodes> All => _tiles.Values;

    public bool TryGet(long key, out TileNodes nodes) => _tiles.TryGetValue(key, out nodes!);

    /// <summary>-1 = not uploaded, otherwise the <see cref="Vis"/> state.</summary>
    public int StateOf(long key) => _tiles.TryGetValue(key, out var t) ? (int)t.Vis : -1;

    /// <summary>Registers a freshly uploaded tile and re-evaluates the affected keys.</summary>
    public void Add(TileNodes t)
    {
        t.Vis = Vis.WaitingSiblings; // provisional; Account counts it, Refresh below fixes it up
        t.Applied = false;
        t.Fading = false;
        t.Fade = 1f;
        t.FadeAwait = FadesEnabled;
        if (FadesEnabled) _awaiting.Add(t);
        _tiles[t.Packed] = t;
        Account(t, +1);
        Refresh(t.Packed, force: true);
        RefreshAround(t.Packed);
    }

    /// <summary>Call around an in-place re-upload of the same key: remove its contribution to the counters.</summary>
    public void BeginReplace(TileNodes t) => Account(t, -1);

    /// <summary>Counterpart of <see cref="BeginReplace"/> once the new mesh/lights are in the node.</summary>
    public void EndReplace(TileNodes t) => Account(t, +1);

    /// <summary>Removes a tile and re-evaluates the affected keys. Returns its nodes (or null) for pool release.</summary>
    public TileNodes? Remove(long key)
    {
        if (!_tiles.Remove(key, out var t)) return null;
        Account(t, -1);
        if (t.Fading) FadingTiles--;
        t.Fading = false;
        t.FadeAwait = false;
        t.Fade = 1f;
        RefreshAround(key);
        return t;
    }

    /// <summary>
    /// Records the poll_fades state of an uploaded tile: <paramref name="fading"/> true = it has an entry below 1
    /// (visible whenever uploaded), false = no entry or the final 1.0 entry (back to the sibling-swap rule).
    /// </summary>
    public void SetFade(TileNodes t, bool fading, float fade)
    {
        t.Fade = fade;
        if (t.Fading == fading) return;
        t.Fading = fading;
        FadingTiles += fading ? 1 : -1;
        Refresh(t.Packed, force: true);
    }

    /// <summary>Call after poll_fades ran: tiles uploaded since the previous call may now be shown.</summary>
    public void ReleaseAwaiting()
    {
        for (int i = 0; i < _awaiting.Count; i++)
        {
            var t = _awaiting[i];
            // The node may have been unloaded (and even re-acquired) since; only act on the live registered tile.
            if (!t.FadeAwait || !_tiles.TryGetValue(t.Packed, out var live) || live != t) continue;
            t.FadeAwait = false;
            Refresh(t.Packed, force: true);
        }
        _awaiting.Clear();
    }

    public void Shift(Vector3 shift)
    {
        foreach (var t in _tiles.Values) t.Root.Position -= shift;
    }

    // ---- the rule --------------------------------------------------------------------------

    private bool Has(long k) => _tiles.ContainsKey(k);

    /// <summary>
    /// A tile counts as present when it is uploaded, or (after Rust refined it and unloaded it) when all 4 of its
    /// children are present. With a literal "sibling uploaded" test, refining one tile of a sibling group would
    /// unload it and hide its 3 siblings (a hole); with this definition the two readings agree whenever no member of
    /// the group has been refined, and the cover stays hole-free when one has.
    /// </summary>
    private bool Present(long k)
    {
        if (_tiles.ContainsKey(k)) return true;
        return TileKey.Z(k) < LeafZ && AllChildrenPresent(k);
    }

    private bool AllChildrenPresent(long k)
    {
        for (int q = 0; q < 4; q++) if (!Present(TileKey.Child(k, q))) return false;
        return true;
    }

    private bool AllSiblingsPresent(long k)
    {
        for (int q = 0; q < 4; q++) if (!Present(TileKey.Sibling(k, q))) return false;
        return true;
    }

    /// <summary>Visibility of an uploaded key by the rule.</summary>
    public Vis Evaluate(long k)
    {
        if (_tiles.TryGetValue(k, out var self))
        {
            if (self.FadeAwait) return Vis.AwaitingFade;
            if (self.Fading) return Vis.Visible;
        }
        int z = TileKey.Z(k);
        if (z > RootZ && !AllSiblingsPresent(k)) return Vis.WaitingSiblings;
        if (z < LeafZ && AllChildrenPresent(k)) return Vis.HiddenByChildren;
        return Vis.Visible;
    }

    /// <summary>
    /// Re-evaluates the keys whose state can depend on <paramref name="k"/>: the tile itself, its siblings, its
    /// children, its parent, and (because "present" looks through unloaded refined tiles) the ancestors with
    /// their sibling groups. At most ~25 dictionary lookups, no allocation.
    /// </summary>
    private void RefreshAround(long k)
    {
        int z = TileKey.Z(k);
        if (z < LeafZ)
            for (int q = 0; q < 4; q++) Refresh(TileKey.Child(k, q));
        long a = k;
        for (int az = z; az >= RootZ; az--)
        {
            if (az > RootZ)
                for (int q = 0; q < 4; q++)
                {
                    long s = TileKey.Sibling(a, q);
                    if (s != k) Refresh(s);
                }
            if (az > RootZ) a = TileKey.Parent(a);
            if (az > RootZ) Refresh(a); // ancestor: hidden-by-children state
        }
    }

    private void Refresh(long k, bool force = false)
    {
        if (!_tiles.TryGetValue(k, out var t)) return;
        var want = Evaluate(k);
        if (want == t.Vis && t.Applied && !force) return;
        Account(t, -1);
        t.Vis = want;
        t.Applied = true;
        Account(t, +1);
        t.Root.Visible = want == Vis.Visible;
    }

    /// <summary>Adds (+1) or removes (-1) a tile's contribution to all counters, using its current state.</summary>
    private void Account(TileNodes t, int s)
    {
        int ring = t.Ring;
        if ((uint)ring >= (uint)LoadedByRing.Length) ring = LoadedByRing.Length - 1;
        bool vis = t.Vis == Vis.Visible;
        LoadedByRing[ring] += s;
        TrianglesLoaded += s * t.Tris;
        if (t.HasGround) { GroundTilesLoaded += s; GroundTrianglesLoaded += s * t.GroundTris; }
        LightsLoaded += s * t.LightCount;
        if (t.LightCount > 0) LightTilesLoaded += s;
        if (t.Clustered) ClusteredTilesLoaded += s;
        if (vis)
        {
            VisibleByRing[ring] += s;
            TrianglesVisible += s * t.Tris;
            if (t.HasGround) { GroundTilesVisible += s; GroundTrianglesVisible += s * t.GroundTris; }
            LightsVisible += s * t.LightCount;
            if (t.LightCount > 0) LightTilesVisible += s;
            if (t.Clustered) ClusteredTilesVisible += s;
        }
        else if (t.Vis == Vis.HiddenByChildren) HiddenByChildren += s;
        else if (t.Vis == Vis.AwaitingFade) AwaitingFadeTiles += s;
        else WaitingSiblings += s;
    }

    // ---- consistency check ----------------------------------------------------------------

    /// <summary>
    /// Walks every uploaded tile. Errors: (a) stored state differs from the rule; (b) a visible tile has a visible
    /// ancestor (ground covered twice); (c) a root-zoom tile that has uploaded tiles is not fully covered by visible tiles
    /// (ground covered 0 times) while it is not in <paramref name="pendingRoots"/> and was already suspect on the
    /// previous call (<paramref name="suspectRoots"/> is updated in place); (d) counters differ from a recount.
    /// Returns the number of errors; details are appended to <paramref name="report"/>.
    /// </summary>
    public int Verify(HashSet<long> pendingRoots, HashSet<long> suspectRoots, StringBuilder report)
    {
        int errors = 0;
        var roots = new HashSet<long>();
        var recountVis = new int[VisibleByRing.Length];
        int recountHidden = 0, recountWait = 0, recountAwait = 0, recountFading = 0;
        foreach (var kv in _tiles)
        {
            long k = kv.Key;
            var t = kv.Value;
            roots.Add(TileKey.Ancestor(k, RootZ));
            var want = Evaluate(k);
            if (want != t.Vis || t.Root.Visible != (want == Vis.Visible))
            {
                errors++;
                report.Append("state mismatch ").Append(TileKey.ToText(k)).Append(": stored ").Append(t.Vis)
                      .Append(" rootVisible ").Append(t.Root.Visible).Append(" rule ").Append(want).Append('\n');
            }
            if (t.Fading) recountFading++;
            if (t.Vis == Vis.Visible)
            {
                recountVis[System.Math.Clamp(t.Ring, 0, recountVis.Length - 1)]++;
                // Double cover is intended while a swap fades: skip pairs where either side is in a transition.
                for (int z = TileKey.Z(k) - 1; z >= RootZ; z--)
                {
                    if (!t.Fading && _tiles.TryGetValue(TileKey.Ancestor(k, z), out var a) && a.Vis == Vis.Visible && !a.Fading)
                    {
                        errors++;
                        report.Append("double cover: ").Append(TileKey.ToText(k)).Append(" and visible ancestor ")
                              .Append(TileKey.ToText(TileKey.Ancestor(k, z))).Append('\n');
                        break;
                    }
                }
            }
            else if (t.Vis == Vis.HiddenByChildren) recountHidden++;
            else if (t.Vis == Vis.AwaitingFade) recountAwait++;
            else recountWait++;
        }

        for (int r = 0; r < recountVis.Length; r++)
            if (recountVis[r] != VisibleByRing[r]) { errors++; report.Append("counter VisibleByRing[").Append(r).Append("] ").Append(VisibleByRing[r]).Append(" != ").Append(recountVis[r]).Append('\n'); }
        if (recountHidden != HiddenByChildren) { errors++; report.Append("counter HiddenByChildren ").Append(HiddenByChildren).Append(" != ").Append(recountHidden).Append('\n'); }
        if (recountWait != WaitingSiblings) { errors++; report.Append("counter WaitingSiblings ").Append(WaitingSiblings).Append(" != ").Append(recountWait).Append('\n'); }

        if (recountAwait != AwaitingFadeTiles) { errors++; report.Append("counter AwaitingFadeTiles ").Append(AwaitingFadeTiles).Append(" != ").Append(recountAwait).Append('\n'); }
        if (recountFading != FadingTiles) { errors++; report.Append("counter FadingTiles ").Append(FadingTiles).Append(" != ").Append(recountFading).Append('\n'); }

        var nowSuspect = new HashSet<long>();
        foreach (long root in roots)
        {
            if (pendingRoots.Contains(root)) continue;
            float cov = Coverage(root);
            if (cov == 1f) continue;
            nowSuspect.Add(root);
            if (suspectRoots.Contains(root))
            {
                errors++;
                report.Append("hole: root ").Append(TileKey.ToText(root)).Append(" covered ").Append((cov * 100f).ToString("0")).Append("% by visible tiles (settled)\n");
            }
        }
        suspectRoots.Clear();
        foreach (long r in nowSuspect) suspectRoots.Add(r);
        return errors;
    }

    /// <summary>
    /// Debug: root-zoom tiles that have uploaded tiles below them but whose ground is not fully covered by visible tiles right now
    /// (a hole), ignoring roots in <paramref name="pending"/> (still being built). <paramref name="worst"/> = the lowest coverage found.
    /// </summary>
    public int CountUncoveredRoots(HashSet<long>? pending, out float worst)
    {
        worst = 1f;
        var roots = new HashSet<long>();
        foreach (var k in _tiles.Keys) roots.Add(TileKey.Ancestor(k, RootZ));
        int n = 0;
        foreach (long root in roots)
        {
            if (pending != null && pending.Contains(root)) continue;
            float c = Coverage(root);
            if (c < 1f) { n++; if (c < worst) worst = c; }
        }
        return n;
    }

    /// <summary>Fraction of the tile's ground covered by visible tiles at or below it.</summary>
    private float Coverage(long k)
    {
        if (_tiles.TryGetValue(k, out var t) && t.Vis == Vis.Visible) return 1f;
        if (TileKey.Z(k) >= LeafZ) return 0f;
        float sum = 0f;
        for (int q = 0; q < 4; q++) sum += Coverage(TileKey.Child(k, q));
        return sum * 0.25f;
    }
}
