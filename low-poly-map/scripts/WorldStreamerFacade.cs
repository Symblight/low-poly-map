using System.Collections.Generic;
using Godot;
using Godot.Collections;

namespace LowPolyMap;

public enum TileState { Queued, Fetching, Meshing, Ready, Failed, Unloading, Unknown }

/// <summary>One entry of debug_tiles(): ring, state and the tile rect in local x/z meters.</summary>
public struct DebugTile
{
    public string Key;
    /// <summary>Packed key (see <see cref="TileKey"/>), 0 when the key could not be parsed.</summary>
    public long Packed;
    public int Ring;
    public TileState State;
    public Vector2 Min;
    public Vector2 Max;
}

/// <summary>
/// Typed view of one poll_ready() dictionary. <see cref="Mesh"/> (buildings) and <see cref="Ground"/> are Mesh.ARRAY_MAX
/// arrays (null when missing/empty, e.g. empty tiles and all of R3).
/// </summary>
public struct ReadyTile
{
    public string Key;
    public int Ring;
    public Vector3 Origin;
    public Vector3[] Lights;
    public Color[] LightColors;
    public Godot.Collections.Array? Mesh;
    /// <summary>
    /// True when the dictionary carried a "ground" key at all (an empty Array still counts). Lets the uploader tell
    /// "no ground for this tile" (R3, or an empty tile) from "this delivery does not speak about ground" (older Rust build).
    /// </summary>
    public bool HasGroundKey;
    /// <summary>Ground mesh Array (VERTEX, NORMAL, COLOR, INDEX), relative to <see cref="Origin"/>. Null when absent or empty (R3).</summary>
    public Godot.Collections.Array? Ground;
}

/// <summary>Typed, reusable result of debug_stats(). Per-ring arrays have length <see cref="Rings"/>.</summary>
public sealed class StreamerStats
{
    public const int Rings = 4;
    public readonly int[] Queued = new int[Rings];
    public readonly int[] Fetching = new int[Rings];
    public readonly int[] Meshing = new int[Rings];
    public readonly int[] Loaded = new int[Rings];
    public double CacheMb;
    public double OriginLat;
    public double OriginLon;
    public string Version = "?";
}

/// <summary>
/// The single typed C# wrapper over the Rust "WorldStreamer" node (GDExtension).
/// Nothing else in the project touches method-name strings. If the class is not
/// registered (Rust library not built / not loaded) the facade stays unavailable
/// and every method is a harmless no-op returning an empty value.
/// </summary>
public sealed class WorldStreamerFacade
{
    public const string ClassNameText = "WorldStreamer";

    /// <summary>Contract major this facade is written against (docs/contract.md). A different library major is never called into.</summary>
    public const int ExpectedApiMajor = 0;
    /// <summary>Lowest contract minor the facade fully supports. Below it, the features in <see cref="FeatureMinors"/> may be missing.</summary>
    public const int MinApiMinor = 5;

    /// <summary>Facade features that need a minor above the baseline, with the api minor that introduces each (docs/contract.md).</summary>
    private static readonly (string Feature, string Method, int Minor)[] FeatureMinors =
    {
        ("HasPollFades (dithered LOD fades)", "poll_fades", 5),
        ("HasGroundHeight (height above ground)", "ground_height_at", 5),
        ("HasDebugTiles (tile rects in the overlay)", "debug_tiles", 5),
    };

    private static readonly StringName ClassNameSn = new(ClassNameText);
    private static readonly StringName MConfigure = new("configure");
    private static readonly StringName MUpdateCamera = new("update_camera");
    private static readonly StringName MPollReady = new("poll_ready");
    private static readonly StringName MPollFades = new("poll_fades");
    private static readonly StringName MPollUnload = new("poll_unload");
    private static readonly StringName MTakeOriginShift = new("take_origin_shift");
    private static readonly StringName MDebugStats = new("debug_stats");
    private static readonly StringName MDebugTiles = new("debug_tiles");
    private static readonly StringName MGroundHeightAt = new("ground_height_at");

    private static readonly StringName KKey = new("key");
    private static readonly StringName KOrigin = new("origin");
    private static readonly StringName KMesh = new("mesh");
    private static readonly StringName KGround = new("ground");
    private static readonly StringName KLights = new("lights");
    private static readonly StringName KLightColors = new("light_colors");
    private static readonly StringName KRing = new("ring");
    private static readonly StringName KState = new("state");
    private static readonly StringName KMin = new("min");
    private static readonly StringName KMax = new("max");
    private static readonly StringName KQueued = new("queued");
    private static readonly StringName KFetching = new("fetching");
    private static readonly StringName KMeshing = new("meshing");
    private static readonly StringName KLoaded = new("loaded");
    private static readonly StringName KCacheMb = new("cache_mb");
    private static readonly StringName KOriginLat = new("origin_lat");
    private static readonly StringName KOriginLon = new("origin_lon");
    private static readonly StringName KVersion = new("version");
    private static readonly StringName MApiVersion = new("api_version");
    private static readonly StringName KApiMajor = new("api_major");
    private static readonly StringName KApiMinor = new("api_minor");

    private static readonly Array<Dictionary> EmptyReady = new();
    private static readonly string[] EmptyUnload = System.Array.Empty<string>();

    private Node? _node;

    /// <summary>The Rust node to add to the scene tree; null when unavailable.</summary>
    public Node? Node => IsAvailable ? _node : null;

    /// <summary>True when the node exists and is still a live Godot instance.</summary>
    public bool IsAvailable => _node != null && GodotObject.IsInstanceValid(_node);

    /// <summary>Why the facade is unavailable (empty when it is).</summary>
    public string Error { get; private set; } = "";

    /// <summary>Contract major of the loaded library; meaningful only when <see cref="ApiVersionKnown"/>.</summary>
    public int ApiMajor { get; private set; }
    /// <summary>Contract minor of the loaded library; meaningful only when <see cref="ApiVersionKnown"/>.</summary>
    public int ApiMinor { get; private set; }
    /// <summary>False for a library that predates api_version (feature probing by method name stays in effect).</summary>
    public bool ApiVersionKnown { get; private set; }
    /// <summary>One-line description of the resolved version for the startup log.</summary>
    public string ApiVersionText => ApiVersionKnown ? $"{ApiMajor}.{ApiMinor}" : "unknown (library predates api_version)";

    /// <summary>Known version that satisfies <see cref="MinApiMinor"/>: the facade features are guaranteed, no probing.</summary>
    private bool VersionGuaranteesFeatures => ApiVersionKnown && ApiMajor == ExpectedApiMajor && ApiMinor >= MinApiMinor;

    public static bool ClassExists() => ClassDB.ClassExists(ClassNameSn);

    /// <summary>Instantiate the Rust class. Returns null (and logs) if it is not registered.</summary>
    public static WorldStreamerFacade Create()
    {
        var facade = new WorldStreamerFacade();
        if (!ClassExists())
        {
            facade.Error = "class 'WorldStreamer' is not registered (Rust library missing, not built, or "
                + "simplane_world.gdextension not loaded). Build ../rust (cargo build) and restart/reload the editor.";
            GD.PushError("[WorldStreamerFacade] " + facade.Error);
            return facade;
        }

        var variant = ClassDB.Instantiate(ClassNameSn);
        var obj = variant.AsGodotObject();
        if (obj is Node node)
        {
            facade._node = node;
        }
        else
        {
            facade.Error = "ClassDB.Instantiate('WorldStreamer') did not return a Node (got "
                + (obj == null ? "null" : obj.GetClass()) + ").";
            GD.PushError("[WorldStreamerFacade] " + facade.Error);
            obj?.Free();
        }
        if (facade._node != null) facade.CheckApiVersion();
        return facade;
    }

    /// <summary>
    /// Reads the library's contract version once (api_version(), else debug_stats api_major/api_minor, else unknown)
    /// and applies the policy: major mismatch = unavailable, low minor = warning naming the missing features,
    /// unknown = one warning and method probing as before, higher minor = one info line.
    /// </summary>
    private void CheckApiVersion()
    {
        var node = _node!;
        if (ClassDB.ClassHasMethod(ClassNameSn, MApiVersion))
        {
            var v = node.Call(MApiVersion);
            if (v.VariantType == Variant.Type.Vector2I)
            {
                var vi = v.AsVector2I();
                ApiMajor = vi.X;
                ApiMinor = vi.Y;
                ApiVersionKnown = true;
            }
        }
        else
        {
            var sv = node.Call(MDebugStats);
            if (sv.VariantType == Variant.Type.Dictionary)
            {
                var d = sv.AsGodotDictionary();
                if (d.TryGetValue(KApiMajor, out var maj) && d.TryGetValue(KApiMinor, out var min))
                {
                    ApiMajor = (int)maj.AsInt64();
                    ApiMinor = (int)min.AsInt64();
                    ApiVersionKnown = true;
                }
            }
        }

        if (!ApiVersionKnown)
        {
            GD.PushWarning($"[WorldStreamerFacade] the Rust library predates api_version; expected contract {ExpectedApiMajor}.{MinApiMinor}+. "
                + "Falling back to probing methods by name (poll_fades, ground_height_at, debug_tiles). Rebuild ../rust.");
            return;
        }

        if (ApiMajor != ExpectedApiMajor)
        {
            Error = $"Rust library api {ApiMajor}.{ApiMinor} has a different major than this facade expects ({ExpectedApiMajor}.x): "
                + "the contract is breaking across majors, so the library is not used. Update the C# facade or rebuild ../rust at a matching version.";
            GD.PushError("[WorldStreamerFacade] " + Error);
            node.Free();
            _node = null;
            return;
        }

        if (ApiMinor < MinApiMinor)
        {
            var missing = new System.Text.StringBuilder();
            foreach (var f in FeatureMinors)
                if (ApiMinor < f.Minor) missing.Append("\n  - ").Append(f.Feature).Append(": needs ").Append(f.Method).Append(" (api ").Append(ExpectedApiMajor).Append('.').Append(f.Minor).Append(')');
            GD.PushWarning($"[WorldStreamerFacade] Rust library api {ApiMajor}.{ApiMinor} is older than the required {ExpectedApiMajor}.{MinApiMinor}. Missing facade features:{missing}\n"
                + "These stay off (probed by method name). Rebuild ../rust.");
        }
        else if (ApiMinor > MinApiMinor)
        {
            GD.Print($"[WorldStreamerFacade] Rust library api {ApiMajor}.{ApiMinor} is newer than the facade's {ExpectedApiMajor}.{MinApiMinor}; additive changes only, fine.");
        }
    }

    // ---- contract -------------------------------------------------------

    public void Configure(double originLat, double originLon, string tileUrl, string cacheDir)
    {
        if (!IsAvailable) return;
        _node!.Call(MConfigure, originLat, originLon, tileUrl, cacheDir);
    }

    public void UpdateCamera(Vector3 localPos, Vector3 velocity, double altAglM)
    {
        if (!IsAvailable) return;
        _node!.Call(MUpdateCamera, localPos, velocity, altAglM);
    }

    /// <summary>Ready tiles (key, ring, origin, mesh, lights, light_colors). Every requested tile is delivered, even when empty.</summary>
    public Array<Dictionary> PollReady(int maxTiles)
    {
        if (!IsAvailable) return EmptyReady;
        var v = _node!.Call(MPollReady, maxTiles);
        return v.VariantType == Variant.Type.Array ? v.AsGodotArray<Dictionary>() : EmptyReady;
    }

    /// <summary>Typed parse of one poll_ready() entry. False if it has no key.</summary>
    public static bool TryParseReady(Dictionary d, out ReadyTile tile)
    {
        tile = default;
        if (!d.TryGetValue(KKey, out var k)) return false;
        tile.Key = k.AsString();
        if (d.TryGetValue(KRing, out var r)) tile.Ring = (int)r.AsInt64();
        if (d.TryGetValue(KOrigin, out var o)) tile.Origin = o.AsVector3();
        tile.Lights = d.TryGetValue(KLights, out var l) && l.VariantType == Variant.Type.PackedVector3Array
            ? l.AsVector3Array() : System.Array.Empty<Vector3>();
        tile.LightColors = d.TryGetValue(KLightColors, out var c) && c.VariantType == Variant.Type.PackedColorArray
            ? c.AsColorArray() : System.Array.Empty<Color>();

        tile.Mesh = d.TryGetValue(KMesh, out var m) ? AsMeshArrays(m) : null;
        tile.HasGroundKey = d.TryGetValue(KGround, out var g);
        tile.Ground = tile.HasGroundKey ? AsMeshArrays(g) : null;
        return true;
    }

    /// <summary>
    /// A mesh Array is usable when it is an Array whose ARRAY_VERTEX slot is a non-empty
    /// PackedVector3Array. Returned without copying; padded to Mesh.ARRAY_MAX if Rust sent fewer slots.
    /// </summary>
    private static Godot.Collections.Array? AsMeshArrays(Variant v)
    {
        if (v.VariantType != Variant.Type.Array) return null;
        var a = v.AsGodotArray();
        if (a.Count == 0) return null;
        var vtx = a[(int)Mesh.ArrayType.Vertex];
        if (vtx.VariantType != Variant.Type.PackedVector3Array || vtx.AsVector3Array().Length < 3) return null;
        if (a.Count < (int)Mesh.ArrayType.Max) a.Resize((int)Mesh.ArrayType.Max);
        return a;
    }

    private bool? _hasGroundHeight;

    /// <summary>True if the loaded Rust build exports ground_height_at() (checked once).</summary>
    public bool HasGroundHeight =>
        _hasGroundHeight ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MGroundHeightAt));

    /// <summary>
    /// Terrain height in meters under a local point (Rust's single ground_height function). Returns 0 when the
    /// facade is unavailable or the Rust build does not export ground_height_at() yet.
    /// </summary>
    public double GroundHeightAt(double localX, double localZ)
    {
        if (!IsAvailable || !HasGroundHeight) return 0.0;
        var v = _node!.Call(MGroundHeightAt, localX, localZ);
        return v.VariantType == Variant.Type.Float ? v.AsDouble() : 0.0;
    }

    private bool? _hasPollFades;

    /// <summary>True if the loaded Rust build exports poll_fades() (checked once; false = every tile is fade 1, no transitions).</summary>
    public bool HasPollFades =>
        _hasPollFades ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MPollFades));

    /// <summary>
    /// poll_fades(): key ("z/x/y") -> float fade (0..1) for every tile currently in a LOD transition (changed or not);
    /// a tile that is not listed has fade 1. Returns null when the Rust build does not export the method yet (or the
    /// facade is unavailable), which callers treat as "no tile is fading".
    /// </summary>
    public Dictionary? PollFades()
    {
        if (!IsAvailable || !HasPollFades) return null;
        var v = _node!.Call(MPollFades);
        return v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : null;
    }

    /// <summary>Keys ("z/x/y") of tiles Rust wants unloaded.</summary>
    public string[] PollUnload()
    {
        if (!IsAvailable) return EmptyUnload;
        var v = _node!.Call(MPollUnload);
        return v.VariantType == Variant.Type.PackedStringArray ? v.AsStringArray() : EmptyUnload;
    }

    /// <summary>Non-zero when the floating origin moved: subtract it from camera and all tile roots.</summary>
    public Vector3 TakeOriginShift()
    {
        if (!IsAvailable) return Vector3.Zero;
        var v = _node!.Call(MTakeOriginShift);
        return v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    }

    /// <summary>Raw debug_stats() dictionary (prefer the typed overload).</summary>
    public Dictionary DebugStats()
    {
        if (!IsAvailable) return new Dictionary();
        var v = _node!.Call(MDebugStats);
        return v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : new Dictionary();
    }

    /// <summary>Fills <paramref name="into"/> from debug_stats(). Returns false if unavailable.</summary>
    public bool DebugStats(StreamerStats into)
    {
        if (!IsAvailable) return false;
        var v = _node!.Call(MDebugStats);
        if (v.VariantType != Variant.Type.Dictionary) return false;
        var d = v.AsGodotDictionary();
        ReadRing(d, KQueued, into.Queued);
        ReadRing(d, KFetching, into.Fetching);
        ReadRing(d, KMeshing, into.Meshing);
        ReadRing(d, KLoaded, into.Loaded);
        if (d.TryGetValue(KCacheMb, out var cm)) into.CacheMb = cm.AsDouble();
        if (d.TryGetValue(KOriginLat, out var la)) into.OriginLat = la.AsDouble();
        if (d.TryGetValue(KOriginLon, out var lo)) into.OriginLon = lo.AsDouble();
        if (d.TryGetValue(KVersion, out var ver)) into.Version = ver.AsString();
        return true;
    }

    private static void ReadRing(Dictionary d, StringName key, int[] dst)
    {
        System.Array.Clear(dst);
        if (!d.TryGetValue(key, out var v)) return;
        if (v.VariantType == Variant.Type.PackedInt32Array)
        {
            var src = v.AsInt32Array();
            System.Array.Copy(src, dst, System.Math.Min(src.Length, dst.Length));
        }
        else if (v.VariantType == Variant.Type.Int)
        {
            dst[0] = (int)v.AsInt64(); // legacy scalar form: put everything on ring 0
        }
    }

    private bool? _hasDebugTiles;

    /// <summary>True if the loaded Rust build exports debug_tiles() (checked once).</summary>
    public bool HasDebugTiles =>
        _hasDebugTiles ??= IsAvailable && (VersionGuaranteesFeatures || ClassDB.ClassHasMethod(ClassNameSn, MDebugTiles));

    /// <summary>
    /// Clears <paramref name="into"/> and fills it from debug_tiles(). Returns the number of tiles
    /// (0 when the method is missing or the facade is unavailable).
    /// </summary>
    public int DebugTiles(List<DebugTile> into)
    {
        into.Clear();
        if (!IsAvailable || !HasDebugTiles) return 0;
        var v = _node!.Call(MDebugTiles);
        if (v.VariantType != Variant.Type.Array) return 0;
        var arr = v.AsGodotArray<Dictionary>();
        foreach (var d in arr)
        {
            var t = new DebugTile { State = TileState.Unknown, Key = "" };
            if (d.TryGetValue(KKey, out var key))
            {
                t.Key = key.AsString();
                if (!TileKey.TryParse(t.Key, out t.Packed)) t.Packed = 0;
            }
            if (d.TryGetValue(KRing, out var r)) t.Ring = (int)r.AsInt64();
            if (d.TryGetValue(KState, out var st)) t.State = ParseState(st.AsString());
            if (d.TryGetValue(KMin, out var mn)) t.Min = mn.AsVector2();
            if (d.TryGetValue(KMax, out var mx)) t.Max = mx.AsVector2();
            into.Add(t);
        }
        return into.Count;
    }

    private static TileState ParseState(string s) => s switch
    {
        "queued" => TileState.Queued,
        "fetching" => TileState.Fetching,
        "meshing" => TileState.Meshing,
        "ready" => TileState.Ready,
        "failed" => TileState.Failed,
        "unloading" => TileState.Unloading,
        _ => TileState.Unknown,
    };
}
