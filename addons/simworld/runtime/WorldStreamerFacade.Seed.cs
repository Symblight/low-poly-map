#nullable enable
using System.Collections.Generic;
using Godot;
using Godot.Collections;

namespace SimWorld;

// World seed (api 1.7): airports() and seed_world_info() as typed records.
public sealed partial class WorldStreamerFacade
{
    private static readonly AirportInfo[] NoAirports = System.Array.Empty<AirportInfo>();

    /// <summary>
    /// airports() (api 1.7): the seed world's airports sorted by ident, positions in the CURRENT local frame (call again after an origin shift).
    /// Empty without a seed world, on an older library or when unavailable. Allocates (parses a few hundred light points): call it on
    /// configure and on origin shifts, not every frame.
    /// </summary>
    public AirportInfo[] Airports()
    {
        if (!HasSeedWorld) return NoAirports;
        var v = _node!.Call(MAirports);
        if (v.VariantType != Variant.Type.Array) return NoAirports;
        var arr = v.AsGodotArray();
        var list = new List<AirportInfo>(arr.Count);
        foreach (var item in arr)
        {
            if (item.VariantType != Variant.Type.Dictionary) continue;
            var d = item.AsGodotDictionary();
            var runways = new List<RunwayInfo>();
            if (d.TryGetValue("runways", out var rv) && rv.VariantType == Variant.Type.Array)
                foreach (var r in rv.AsGodotArray())
                    if (r.VariantType == Variant.Type.Dictionary) runways.Add(ParseRunway(r.AsGodotDictionary()));
            list.Add(new AirportInfo(Str(d, "ident"), Str(d, "name"), Str(d, "type"), Num(d, "lat"), Num(d, "lon"), Vec(d, "position"),
                d.TryGetValue("elevation_ft", out var el) && (el.VariantType == Variant.Type.Float || el.VariantType == Variant.Type.Int) ? el.AsDouble() : null,
                runways.ToArray()));
        }
        return list.ToArray();
    }

    private static RunwayInfo ParseRunway(Dictionary d)
    {
        return new RunwayInfo(Str(d, "designator"), ParseEnd(d, "le"), ParseEnd(d, "he"), Num(d, "length_m"), Num(d, "width_m"), Str(d, "surface"),
            Bool(d, "paved"), Bool(d, "lighted"), Bool(d, "approximate"),
            d.TryGetValue("lights", out var l) && l.VariantType == Variant.Type.PackedVector3Array ? l.AsVector3Array() : System.Array.Empty<Vector3>(),
            d.TryGetValue("light_colors", out var c) && c.VariantType == Variant.Type.PackedColorArray ? c.AsColorArray() : System.Array.Empty<Color>());
    }

    private static RunwayEnd ParseEnd(Dictionary runway, string key)
    {
        var e = runway.TryGetValue(key, out var v) && v.VariantType == Variant.Type.Dictionary ? v.AsGodotDictionary() : new Dictionary();
        return new RunwayEnd(Str(e, "ident"), Num(e, "lat"), Num(e, "lon"), Vec(e, "position"), Num(e, "threshold_lat"), Num(e, "threshold_lon"),
            Vec(e, "threshold_position"), Num(e, "displaced_threshold_m"), Vec(e, "touchdown_position"), Num(e, "heading_deg"));
    }

    private static string Str(Dictionary d, string k) => d.TryGetValue(k, out var v) && v.VariantType == Variant.Type.String ? v.AsString() : "";
    private static double Num(Dictionary d, string k) => d.TryGetValue(k, out var v) && (v.VariantType == Variant.Type.Float || v.VariantType == Variant.Type.Int) ? v.AsDouble() : 0.0;
    private static bool Bool(Dictionary d, string k) => d.TryGetValue(k, out var v) && v.VariantType == Variant.Type.Bool && v.AsBool();
    private static Vector3 Vec(Dictionary d, string k) => d.TryGetValue(k, out var v) && v.VariantType == Variant.Type.Vector3 ? v.AsVector3() : Vector3.Zero;
    private static string[] Strs(Dictionary d, string k) => d.TryGetValue(k, out var v)
        ? v.VariantType == Variant.Type.PackedStringArray ? v.AsStringArray()
        : v.VariantType == Variant.Type.Array ? System.Linq.Enumerable.ToArray(System.Linq.Enumerable.Select(v.AsGodotArray(), x => x.AsString()))
        : System.Array.Empty<string>()
        : System.Array.Empty<string>();

    /// <summary>
    /// seed_world_info() (api 1.7): generator version, built phases, cities, counts, load-time warnings, memory. Null without a seed world
    /// (empty dictionary), on an older library or when unavailable.
    /// </summary>
    public SeedWorldInfo? SeedInfo()
    {
        if (!HasSeedWorld) return null;
        var v = _node!.Call(MSeedWorldInfo);
        if (v.VariantType != Variant.Type.Dictionary) return null;
        var d = v.AsGodotDictionary();
        if (d.Count == 0) return null;
        var cities = new List<SeedCityInfo>();
        if (d.TryGetValue("cities", out var cv) && cv.VariantType == Variant.Type.Array)
            foreach (var c in cv.AsGodotArray())
            {
                if (c.VariantType != Variant.Type.Dictionary) continue;
                var cd = c.AsGodotDictionary();
                cities.Add(new SeedCityInfo(Str(cd, "id"), Str(cd, "name"), Num(cd, "lat"), Num(cd, "lon"), Num(cd, "radius_km"), (int)Num(cd, "version"), Str(cd, "source"), Strs(cd, "airports")));
            }
        string mem = "";
        double loadMs = 0;
        if (d.TryGetValue("memory", out var mv) && mv.VariantType == Variant.Type.Dictionary)
        {
            var md = mv.AsGodotDictionary();
            loadMs = Num(md, "load_ms");
            mem = $"water index {Num(md, "water_index_mb"):0.##} MB + rasters {Num(md, "water_raster_mb"):0.##} MB, strips {Num(md, "strip_index_kb"):0.#} KB, airports {Num(md, "airports_kb"):0.#} KB, load {loadMs:0} ms";
        }
        return new SeedWorldInfo(Str(d, "manifest"), (int)Num(d, "generator_version"), Strs(d, "phases"), (int)Num(d, "world_version"), Str(d, "world_seed"),
            cities.ToArray(), (int)Num(d, "airports"), (int)Num(d, "water_polygons"), Strs(d, "warnings"), mem, loadMs);
    }
}
