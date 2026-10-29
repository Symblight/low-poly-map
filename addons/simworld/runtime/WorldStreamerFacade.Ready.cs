using System.Collections.Generic;
using Godot;
using Godot.Collections;

namespace SimWorld;

// poll_ready: the delivered tile dictionaries and their typed parse.
public sealed partial class WorldStreamerFacade
{
    private bool _warnedPollReadyType;
    private readonly Variant[] _argsReady = new Variant[1];

    /// <summary>
    /// Polls up to <paramref name="maxTiles"/> ready tiles into <paramref name="into"/> (cleared first) and returns how many there are. Every requested tile is delivered, even when empty.
    /// An entry the host cannot place (no usable key) comes back with <see cref="ReadyTile.Usable"/> false and its keys and types in <see cref="ReadyTile.Problem"/>. The facade releases the
    /// call's Variant and collection wrappers itself; the mesh handles inside the tiles are the caller's to release with <see cref="ReadyTile.ReleaseHandles"/> once the mesh is uploaded.
    /// </summary>
    public int PollReady(int maxTiles, List<ReadyTile> into)
    {
        into.Clear();
        if (!IsAvailable) return 0;
        using var timing = Timings.Time(StreamerCall.PollReady);
        _argsReady[0] = maxTiles;
        Variant v;
        Godot.Collections.Array? entries = null;
        using (AllocProbe.Measure(AllocSite.PollReadyCall))
        {
            v = _node!.Call(MPollReady, _argsReady);
            if (v.VariantType == Variant.Type.Array) entries = v.AsGodotArray();
        }
        using var variant = v;
        if (entries == null)
        {
            if (!_warnedPollReadyType)
            {
                _warnedPollReadyType = true;
                GD.PushWarning($"[WorldStreamerFacade] poll_ready returned {v.VariantType}, expected an Array; no tile is delivered (library {ApiVersionText})");
            }
            return 0;
        }
        using (entries)
        {
            for (int i = 0; i < entries.Count; i++)
            {
                using (AllocProbe.Measure(AllocSite.ReadyParse))
                {
                    using var ev = entries[i];
                    using var d = ev.VariantType == Variant.Type.Dictionary ? ev.AsGodotDictionary() : new Dictionary();
                    if (!TryParseReady(d, out var tile)) tile = new ReadyTile { Problem = DescribeEntry(d) };
                    into.Add(tile);
                }
            }
        }
        return into.Count;
    }

    /// <summary>Typed parse of one poll_ready() entry. False if it has no usable key (the caller reports it: <see cref="DescribeEntry"/>).</summary>
    public static bool TryParseReady(Dictionary d, out ReadyTile tile)
    {
        tile = default;
        if (!d.TryGetValue(KKey, out var k)) return false;
        bool keyOk = k.VariantType == Variant.Type.String;
        if (keyOk) tile.Key = k.AsString();
        k.Dispose();
        if (!keyOk) return false;
        string layerName = Txt(d, KLayer);
        tile.IsTerrain = layerName == "terrain";
        tile.IsLandmark = layerName == "landmarks";
        if (tile.IsTerrain)
        {
            // Terrain entries (api 1.5) carry only the terrain mesh; every vector key is empty.
            tile.Ring = (int)Lng(d, KRing);
            if (d.TryGetValue(KOrigin, out var to)) tile.Origin = to.AsVector3();
            tile.Terrain = d.TryGetValue(KTerrain, out var tm) ? AsMeshArrays(tm, KTerrain, ref tile.Problem) : null;
            tile.TerrainSurface = Surface(d, KTerrainSurface, ref tile.Problem);
            tile.HMin = (float)Dbl(d, KHMin);
            tile.HMax = (float)Dbl(d, KHMax);
            tile.Lights = System.Array.Empty<Vector3>();
            tile.LightColors = System.Array.Empty<Color>();
            tile.AirportLights = System.Array.Empty<Vector3>();
            tile.AirportLightColors = System.Array.Empty<Color>();
            return true;
        }
        tile.Ring = (int)Lng(d, KRing);
        if (d.TryGetValue(KOrigin, out var o)) tile.Origin = o.AsVector3();
        using (AllocProbe.Measure(AllocSite.ReadyLights))
        {
            tile.Lights = Points(d, KLights);
            tile.LightColors = Colors(d, KLightColors);
            tile.StreetLights = Points(d, KStreetLights);
            tile.StreetLightColors = Colors(d, KStreetLightColors);
            tile.BeaconLights = Points(d, KBeaconLights);
            tile.BeaconLightColors = Colors(d, KBeaconLightColors);
            tile.AirportLights = Points(d, KAirportLights);
            tile.AirportLightColors = Colors(d, KAirportLightColors);
        }

        tile.Mesh = d.TryGetValue(KMesh, out var m) ? AsMeshArrays(m, KMesh, ref tile.Problem) : null;
        tile.HasGroundKey = d.TryGetValue(KGround, out var g);
        tile.Ground = tile.HasGroundKey ? AsMeshArrays(g, KGround, ref tile.Problem) : null;
        tile.MeshSurface = Surface(d, KMeshSurface, ref tile.Problem);
        tile.GroundSurface = Surface(d, KGroundSurface, ref tile.Problem);
        return true;
    }

    /// <summary>The keys and value types of an entry, for the log line of an entry <see cref="TryParseReady"/> rejected.</summary>
    public static string DescribeEntry(Dictionary d)
    {
        var sb = new System.Text.StringBuilder("{");
        foreach (var kv in d)
        {
            sb.Append(sb.Length > 1 ? ", " : "").Append(kv.Key.AsString()).Append(':').Append(kv.Value.VariantType);
            kv.Key.Dispose();
            kv.Value.Dispose();
        }
        return sb.Append('}').ToString();
    }

    // Empty arrays share one instance; a managed copy (Variant.AsVector3Array) per call is the unavoidable cost of reading a list of points.
    private static readonly Vector3[] NoPoints = System.Array.Empty<Vector3>();
    private static readonly Color[] NoColors = System.Array.Empty<Color>();

    private static Vector3[] Points(Dictionary d, StringName key)
    {
        if (!d.TryGetValue(key, out var v)) return NoPoints;
        var points = v.VariantType == Variant.Type.PackedVector3Array ? v.AsVector3Array() : NoPoints;
        v.Dispose();
        return points;
    }

    private static Color[] Colors(Dictionary d, StringName key)
    {
        if (!d.TryGetValue(key, out var v)) return NoColors;
        var colors = v.VariantType == Variant.Type.PackedColorArray ? v.AsColorArray() : NoColors;
        v.Dispose();
        return colors;
    }

    /// <summary>
    /// A mesh Array is usable when it is an Array whose ARRAY_VERTEX slot is a PackedVector3Array. Nothing is copied: the Array goes straight into
    /// <c>ArrayMesh.AddSurfaceFromArrays</c>, which rejects a degenerate one itself (the uploader checks the surface count). Padded to Mesh.ARRAY_MAX if
    /// Rust sent fewer slots. An empty Array (the contract's "no mesh") or nil gives null; anything else that cannot be drawn gives null and a
    /// <paramref name="problem"/> text.
    /// </summary>
    private static Godot.Collections.Array? AsMeshArrays(Variant v, StringName what, ref string? problem)
    {
        using var owner = v; // the wrapper below holds its own reference
        if (v.VariantType == Variant.Type.Nil) return null;
        if (v.VariantType != Variant.Type.Array)
        {
            problem ??= $"'{what}' is a {v.VariantType}, expected an Array";
            return null;
        }
        var a = v.AsGodotArray();
        if (a.Count == 0) { a.Dispose(); return null; }
        using var vtx = a[(int)Mesh.ArrayType.Vertex];
        if (vtx.VariantType != Variant.Type.PackedVector3Array)
        {
            problem ??= $"'{what}' vertex slot is a {vtx.VariantType}, expected a PackedVector3Array";
            a.Dispose();
            return null;
        }
        if (a.Count < (int)Mesh.ArrayType.Max) a.Resize((int)Mesh.ArrayType.Max);
        return a;
    }
}
