#nullable enable
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// The tile map of the landmark layer (api 1.11, contract section 9b): a flat set of landmark tiles keyed by their packed "l/z/x/y" key
/// (z = the overview zoom, ring 0). Root zoom = leaf zoom, so the sibling-swap rule degenerates to "visible while uploaded": a tile is shown
/// from its upload until the library unloads it, never hidden, never dithered (the library keeps landmarks out of poll_fades). The nodes are
/// ordinary <see cref="TileNodes"/> from the vector <see cref="TilePool"/> (building mesh with the shared building material, light sprites,
/// beacons; the ground and street sets stay empty), so a landmark tile costs the same draw calls as an R0 tile and its sprites cross-fade with
/// the windows like any building's. Counters are kept incrementally like <see cref="TileCover"/>.
/// </summary>
public sealed class LandmarkCover
{
    private readonly Dictionary<long, TileNodes> _tiles = new();

    /// <summary>Tiles held (uploaded, visible).</summary>
    public int Loaded => _tiles.Count;
    public long TrianglesLoaded { get; private set; }
    public long LightsLoaded { get; private set; }
    public long BeaconsLoaded { get; private set; }
    public IEnumerable<TileNodes> All => _tiles.Values;
    public bool TryGet(long key, out TileNodes nodes) => _tiles.TryGetValue(key, out nodes!);

    /// <summary>Registers a freshly uploaded tile and shows it.</summary>
    public void Add(TileNodes t)
    {
        t.Vis = TileCover.Vis.Visible;
        t.Applied = true;
        t.Fading = false;
        t.FadeAwait = false;
        t.Fade = 1f;
        t.Root.Visible = true;
        _tiles[t.Packed] = t;
        Account(t, +1);
    }

    /// <summary>Call around an in-place re-upload of the same key (not expected from the library, handled anyway): removes its contribution to the counters.</summary>
    public void BeginReplace(TileNodes t) => Account(t, -1);

    public void EndReplace(TileNodes t) => Account(t, +1);

    /// <summary>Removes a tile; returns its nodes (or null) for the pool.</summary>
    public TileNodes? Remove(long key)
    {
        if (!_tiles.Remove(key, out var t)) return null;
        Account(t, -1);
        return t;
    }

    public void Shift(Vector3 shift)
    {
        foreach (var t in _tiles.Values) t.Root.Position -= shift;
    }

    private void Account(TileNodes t, int s)
    {
        TrianglesLoaded += s * t.Tris;
        LightsLoaded += s * t.LightCount;
        BeaconsLoaded += s * t.BeaconCount;
    }

    /// <summary>Recounts the counters from the tiles and checks the root visibility; returns the number of problems (details appended to <paramref name="report"/>).</summary>
    public int Verify(System.Text.StringBuilder report)
    {
        int errors = 0;
        long tris = 0, lights = 0, beacons = 0;
        foreach (var kv in _tiles)
        {
            var t = kv.Value;
            tris += t.Tris;
            lights += t.LightCount;
            beacons += t.BeaconCount;
            if (!t.Root.Visible || t.Fading || t.FadeAwait)
            {
                errors++;
                report.Append("landmark tile ").Append(TileKey.ToText(kv.Key)).Append(" not plainly visible: rootVisible ").Append(t.Root.Visible)
                      .Append(" fading ").Append(t.Fading).Append(" await ").Append(t.FadeAwait).Append('\n');
            }
        }
        if (tris != TrianglesLoaded || lights != LightsLoaded || beacons != BeaconsLoaded)
        {
            errors++;
            report.Append("landmark counters tris ").Append(TrianglesLoaded).Append('/').Append(tris).Append(" lights ").Append(LightsLoaded).Append('/').Append(lights)
                  .Append(" beacons ").Append(BeaconsLoaded).Append('/').Append(beacons).Append('\n');
        }
        return errors;
    }
}
