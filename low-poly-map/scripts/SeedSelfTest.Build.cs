using System.Collections.Generic;
using System.Globalization;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// The S2 and S3-S4 groups of <c>--seed-selftest</c> (docs/spec-world-seed.md 11a.3), each run only when <c>seed_world_info()["phases"]</c> lists the phase.
/// Only data the library already delivers is used: the raw <c>seed_world_info()</c> (city plan statistics), <c>airports()</c>, the world profile, and the
/// ready-tile meshes the host holds (vertex positions, COLOR, TEX_UV, light counts).
/// <list type="bullet">
/// <item>S2: every city has a plan (cells, districts, arterials, radials, the access road of each linked airport), the cells by type add up to the land cells,
/// the arterial drop never splits the network (components after = before). At <c>eyvi-field</c> / <c>eyka-field</c>: no ground triangle of a district area
/// (residential, commercial, industrial or a road) overlaps a runway strip; every ground triangle there is class 7 (aeroway).</item>
/// <item>S3-S4: at <c>vilnius</c> / <c>kaunas</c>: R0 tiles with building meshes, windowed walls in R0, building lights in R1..R3 (R3 has no mesh), street lamps
/// in R0, night: sprites on. At the airport spots: no building roof triangle overlaps a runway strip (any airport), every roof vertex inside an approach
/// trapezoid is at or below the surface (base = ground at the threshold, slope per class, from <c>seed.airport.approach</c>).</item>
/// </list>
/// </summary>
public partial class SeedSelfTest
{
    private static readonly StringName MSeedWorldInfo = new("seed_world_info");

    /// <summary>A convex 2D quad in the local x/z plane (a strip or an approach trapezoid).</summary>
    private sealed class Quad
    {
        public string Tag = "";
        public Vector2[] P = new Vector2[4];
        public Vector2 Min, Max;
        // Approach only:
        public bool Approach;
        public Vector2 Org, Out;
        public float Len, Half0, Div, Slope, BaseY;

        public void Bounds()
        {
            Min = P[0]; Max = P[0];
            foreach (var p in P) { Min = Min.Min(p); Max = Max.Max(p); }
        }

        public float? Limit(Vector2 p)
        {
            var d = p - Org;
            float along = d.Dot(Out);
            if (along < 0f || along > Len) return null;
            float across = System.MathF.Abs(d.X * -Out.Y + d.Y * Out.X);
            if (across > Half0 + Div * along) return null;
            return BaseY + Slope * along;
        }
    }

    // ---- S2: city plans -----------------------------------------------------------------------------

    private void CheckPlans()
    {
        var node = _view.Streamer.Node;
        if (node == null) { Fail("city plans", "no library node"); return; }
        using var raw = node.Call(MSeedWorldInfo);
        if (raw.VariantType != Variant.Type.Dictionary || !raw.AsGodotDictionary().TryGetValue("cities", out var cv) || cv.VariantType != Variant.Type.Array)
        { Fail("city plans", "seed_world_info() has no cities array"); return; }
        var cities = cv.AsGodotArray();
        GD.Print("SKIP arterial graph single component: needs a per-city arterial edge list (debug call); checked instead: the drop never splits the network (components after = before)");
        if (cities.Count != 2) Fail("city plans", $"{cities.Count} cities in seed_world_info(), expected 2");
        foreach (var item in cities)
        {
            var c = item.AsGodotDictionary();
            string id = c.TryGetValue("id", out var idv) ? idv.AsString() : "?";
            if (!c.TryGetValue("plan", out var pv) || pv.VariantType != Variant.Type.Dictionary) { Fail("city plan " + id, "seed_world_info() city has no plan (S2 built?)"); continue; }
            var p = pv.AsGodotDictionary();
            double land = Num(p, "land_cells"), water = Num(p, "water_cells"), airportCells = Num(p, "airport_cells"), districts = Num(p, "districts");
            double edges = Num(p, "arterial_edges"), comps = Num(p, "arterial_components"), before = Num(p, "arterial_components_before");
            double radials = Num(p, "radials"), access = Num(p, "airport_access");
            if (land < 50) Fail("city plan " + id, $"only {land} land cells");
            if (districts < 1) Fail("city plan " + id, "no districts");
            if (edges < 1) Fail("city plan " + id, "no arterial edges");
            if (radials < 3) Fail("city plan " + id, $"{radials} radials (expected at least 3)");
            double byType = 0;
            string types = "";
            if (p.TryGetValue("cells_by_type", out var tv) && tv.VariantType == Variant.Type.Dictionary)
                foreach (var kv in tv.AsGodotDictionary()) { byType += kv.Value.AsDouble(); types += $" {kv.Key.AsString()}={kv.Value.AsInt32()}"; }
            if (byType != land) Fail("city plan " + id, $"cells_by_type adds up to {byType}, land_cells is {land}");
            // The spec's "arterial graph connected" is what the library measures: dropping arterials never splits the network.
            if (comps != before) Fail("arterials " + id, $"the drop split the network: {before} components before, {comps} after");
            int linked = c.TryGetValue("airports", out var av) && av.VariantType == Variant.Type.PackedStringArray ? av.AsStringArray().Length : 0;
            if (access != linked) Fail("city plan " + id, $"{access} airport access roads for {linked} linked airports");
            string warn = p.TryGetValue("warnings", out var wv) && wv.VariantType == Variant.Type.PackedStringArray ? string.Join("; ", wv.AsStringArray()) : "";
            Note($"plan {id}: land {land}, water {water}, airport {airportCells}, districts {districts} [{types.Trim()}], arterial edges {edges}, components {before}->{comps}, "
                + $"bridges {Num(p, "bridges")}, radials {radials}, access {access}, build {Num(p, "build_ms"):0.0} ms, {Num(p, "kb"):0} KB{(warn.Length > 0 ? ", warnings: " + warn : "")}");
        }
    }

    private static double Num(Godot.Collections.Dictionary d, string k) => d.TryGetValue(k, out var v) && (v.VariantType == Variant.Type.Float || v.VariantType == Variant.Type.Int) ? v.AsDouble() : 0.0;

    // ---- S3-S4: buildings and lights in a city --------------------------------------------------------

    private string CheckCity(string name)
    {
        int r0Tiles = 0, r0Tris = 0, r1Tris = 0, r2Tris = 0, windowVerts = 0, street = 0, r0Lamps = 0;
        long lightsR1 = 0, lightsR2 = 0, lightsR3 = 0;
        int shown = 0, withSprites = 0;
        foreach (var n in _view.Cover.All)
        {
            switch (n.Ring)
            {
                case 0:
                    if (n.HasMesh && n.Tris > 0) { r0Tiles++; r0Tris += n.Tris; windowVerts += WindowedWallVertices(n); }
                    r0Lamps += n.StreetLightCount;
                    break;
                case 1: r1Tris += n.Tris; lightsR1 += n.LightCount; break;
                case 2: r2Tris += n.Tris; lightsR2 += n.LightCount; break;
                case 3: lightsR3 += n.LightCount; break;
            }
            street += n.StreetLightCount;
            if (n.SpriteCount > 0) { withSprites++; if (n.SpritesShown) shown++; }
        }
        if (r0Tiles == 0) Fail("R0 buildings " + name, "no R0 tile with a building mesh is held");
        if (r1Tris == 0) Fail("R1 buildings " + name, "no R1 building mesh is held");
        if (r2Tris == 0) Fail("R2 buildings " + name, "no R2 building mesh is held");
        if (windowVerts == 0) Fail("windows " + name, "no windowed wall (TEX_UV u >= 0) in the R0 building meshes");
        if (lightsR1 + lightsR2 == 0) Fail("building lights " + name, "no building light in R1 / R2 tiles");
        if (lightsR3 == 0) Fail("R3 lights " + name, "no building light in R3 tiles");
        if (r0Lamps == 0) Fail("street lamps " + name, "no street lamp in R0 tiles");
        bool night = _view.NightFactor > 0.5f;
        if (night && (!_view.SpritesVisible || shown == 0)) Fail("night sprites " + name, $"night factor {_view.NightFactor:0.00} but sprites are off ({shown} of {withSprites} tiles with sprites shown)");
        return $"; buildings R0 {r0Tiles} tiles {r0Tris} tris, R1 {r1Tris}, R2 {r2Tris}; windowed wall vertices {windowVerts}; building lights R1 {lightsR1} R2 {lightsR2} R3 {lightsR3}; "
            + $"street lamps {street} (R0 {r0Lamps}); {(night ? "night" : "day")} (factor {_view.NightFactor:0.00}, {shown}/{withSprites} sprite tiles shown)";
    }

    private static int WindowedWallVertices(TileNodes n)
    {
        if (n.Mesh.GetSurfaceCount() == 0) return 0;
        var arrays = n.Mesh.SurfaceGetArrays(0);
        var nor = arrays[(int)Mesh.ArrayType.Normal].AsVector3Array();
        var uv = arrays[(int)Mesh.ArrayType.TexUV];
        if (uv.VariantType != Variant.Type.PackedVector2Array) return 0;
        var u = uv.AsVector2Array();
        int c = 0;
        for (int i = 0; i < nor.Length && i < u.Length; i++) if (System.MathF.Abs(nor[i].Y) < 0.7f && u[i].X >= 0f) c++;
        return c;
    }

    // ---- airport geometry from airports() and the profile ---------------------------------------------------

    private double ProfileNum(string a, string b, string c, double fallback)
    {
        try
        {
            var prof = _view.Streamer.WorldProfile();
            var d = prof["seed"].AsGodotDictionary()["airport"].AsGodotDictionary();
            if (b.Length == 0) return d[a].AsDouble();
            var e = d[a].AsGodotDictionary();
            if (c.Length == 0) return e[b].AsDouble();
            return e[b].AsGodotDictionary()[c].AsDouble();
        }
        catch (System.Exception) { return fallback; }
    }

    /// <summary>Runway strips (physical ends + strip_end_m, strip_half_m by type) and approach trapezoids (spec 5.1 steps 2-3) of every airport, in the current frame.</summary>
    private void BuildAirportQuads(List<Quad> strips, List<Quad> apps)
    {
        double endM = ProfileNum("strip_end_m", "", "", 60.0);
        double longMin = ProfileNum("approach", "long_min_runway_m", "", 1200.0);
        foreach (var a in _view.Airports)
        {
            string cls = a.Type.StartsWith("large", System.StringComparison.Ordinal) ? "large" : a.Type.StartsWith("medium", System.StringComparison.Ordinal) ? "medium" : "small";
            float half = (float)ProfileNum("strip_half_m", cls, "", 40.0);
            foreach (var r in a.Runways)
            {
                var le = new Vector2(r.Le.Position.X, r.Le.Position.Z);
                var he = new Vector2(r.He.Position.X, r.He.Position.Z);
                float len = le.DistanceTo(he);
                if (len < 50f) continue;
                var dir = (he - le) / len;
                var nrm = new Vector2(-dir.Y, dir.X);
                var s0 = le - dir * (float)endM;
                var s1 = he + dir * (float)endM;
                var q = new Quad { Tag = $"{a.Ident} {r.Designator}" };
                q.P[0] = s0 + nrm * half; q.P[1] = s0 - nrm * half; q.P[2] = s1 - nrm * half; q.P[3] = s1 + nrm * half;
                q.Bounds();
                strips.Add(q);

                bool longClass = r.Paved && r.LengthM >= longMin;
                string k = longClass ? "long" : "short";
                float div = (float)ProfileNum("approach", k, "divergence", longClass ? 0.15 : 0.1);
                float alen = (float)ProfileNum("approach", k, "length_m", longClass ? 3000.0 : 1600.0);
                float slope = (float)ProfileNum("approach", k, "slope", longClass ? 0.02 : 0.05);
                foreach (bool leEnd in new[] { true, false })
                {
                    var end = leEnd ? r.Le : r.He;
                    var thr = new Vector2(end.ThresholdPosition.X, end.ThresholdPosition.Z);
                    var outDir = leEnd ? -dir : dir;
                    var org = thr + outDir * (float)endM;
                    var far = org + outDir * alen;
                    float half1 = half + div * alen;
                    var ap = new Quad
                    {
                        Tag = $"{a.Ident} {end.Ident}", Approach = true, Org = org, Out = outDir, Len = alen, Half0 = half, Div = div, Slope = slope,
                        BaseY = (float)(System.Math.Round(end.ThresholdPosition.Y * 100.0) / 100.0),
                    };
                    ap.P[0] = org + nrm * half; ap.P[1] = org - nrm * half; ap.P[2] = far - nrm * half1; ap.P[3] = far + nrm * half1;
                    ap.Bounds();
                    apps.Add(ap);
                }
            }
        }
    }

    /// <summary>True when the triangle penetrates the convex quad by more than <paramref name="pen"/> metres along every separating-axis candidate.</summary>
    private static bool Overlaps(Vector2 a, Vector2 b, Vector2 c, Quad q, float pen)
    {
        for (int pass = 0; pass < 2; pass++)
        {
            int edges = pass == 0 ? 3 : 4;
            for (int e = 0; e < edges; e++)
            {
                Vector2 p0, p1;
                if (pass == 0) { p0 = e == 0 ? a : e == 1 ? b : c; p1 = e == 0 ? b : e == 1 ? c : a; }
                else { p0 = q.P[e]; p1 = q.P[(e + 1) & 3]; }
                var ax = new Vector2(-(p1.Y - p0.Y), p1.X - p0.X);
                float l = ax.Length();
                if (l < 1e-4f) continue;
                ax /= l;
                float t0 = a.Dot(ax), t1 = b.Dot(ax), t2 = c.Dot(ax);
                float amin = System.MathF.Min(t0, System.MathF.Min(t1, t2)), amax = System.MathF.Max(t0, System.MathF.Max(t1, t2));
                float bmin = float.MaxValue, bmax = float.MinValue;
                for (int i = 0; i < 4; i++) { float t = q.P[i].Dot(ax); bmin = System.MathF.Min(bmin, t); bmax = System.MathF.Max(bmax, t); }
                if (amax - bmin <= pen || bmax - amin <= pen) return false;
            }
        }
        return true;
    }

    private static bool BoundsTouch(Quad q, Vector2 mn, Vector2 mx) => q.Min.X <= mx.X && mn.X <= q.Max.X && q.Min.Y <= mx.Y && mn.Y <= q.Max.Y;

    // ---- the S2 / S3-S4 airport rules on held tiles -------------------------------------------------------------

    /// <summary>
    /// Scans every held tile: ground triangles against the strips (S2: only class 7 may lie on a runway strip), building roofs against the strips (S3-S4: none) and
    /// roof vertices against the approach trapezoids (S3-S4: at or below the surface). Per-ring counts are reported; a violation is a failure.
    /// </summary>
    private string CheckAirportRules(string spot, bool phaseS2, bool phaseS3)
    {
        var strips = new List<Quad>();
        var apps = new List<Quad>();
        BuildAirportQuads(strips, apps);
        var relS = new List<Quad>();
        var relA = new List<Quad>();
        int[] groundTris = new int[4], badGround = new int[4], roofTris = new int[4], badRoof = new int[4], roofVerts = new int[4], badCap = new int[4];
        float[] worstCap = new float[4];
        float worstOverAll = 0f;
        string worstWho = "";
        string firstGround = "", firstRoof = "", firstCap = "";
        const float pen = 0.05f; // touching edges are not an overlap
        const float capTol = 0.01f; // metres over the surface (numeric tolerance; the generator caps against the mesher base, the highest terrain under the footprint)
        foreach (var n in _view.Cover.All)
        {
            if (n.Ring < 0 || n.Ring > 3) continue;
            var off = n.Root.Position;
            Vector2 mn = new(float.MaxValue, float.MaxValue), mx = new(float.MinValue, float.MinValue);
            // Tile extent from the ground mesh (always present with ground) or the building mesh.
            foreach (var m in new[] { n.Mesh, n.GroundMesh })
            {
                if (m.GetSurfaceCount() == 0) continue;
                var bb = m.GetAabb();
                mn = mn.Min(new Vector2(bb.Position.X + off.X, bb.Position.Z + off.Z));
                mx = mx.Max(new Vector2(bb.End.X + off.X, bb.End.Z + off.Z));
            }
            if (mn.X > mx.X) continue;
            relS.Clear(); relA.Clear();
            foreach (var q in strips) if (BoundsTouch(q, mn, mx)) relS.Add(q);
            foreach (var q in apps) if (BoundsTouch(q, mn, mx)) relA.Add(q);
            if (relS.Count == 0 && relA.Count == 0) continue;

            if (phaseS2 && relS.Count > 0 && n.GroundMesh.GetSurfaceCount() > 0)
            {
                var g = n.GroundMesh.SurfaceGetArrays(0);
                var v = g[(int)Mesh.ArrayType.Vertex].AsVector3Array();
                var col = g[(int)Mesh.ArrayType.Color].AsColorArray();
                var idx = g[(int)Mesh.ArrayType.Index].AsInt32Array();
                for (int t = 0; t + 2 < idx.Length; t += 3)
                {
                    var a = new Vector2(v[idx[t]].X + off.X, v[idx[t]].Z + off.Z);
                    var b = new Vector2(v[idx[t + 1]].X + off.X, v[idx[t + 1]].Z + off.Z);
                    var c = new Vector2(v[idx[t + 2]].X + off.X, v[idx[t + 2]].Z + off.Z);
                    foreach (var q in relS)
                    {
                        if (!Overlaps(a, b, c, q, pen)) continue;
                        groundTris[n.Ring]++;
                        int cls = Mathf.RoundToInt(col[idx[t]].R * 16f);
                        if (cls != 7)
                        {
                            badGround[n.Ring]++;
                            if (firstGround.Length == 0) firstGround = $"R{n.Ring} tile {n.Key} class {cls} triangle on strip {q.Tag}";
                        }
                        break;
                    }
                }
            }

            if (phaseS3 && n.Mesh.GetSurfaceCount() > 0)
            {
                var g = n.Mesh.SurfaceGetArrays(0);
                var v = g[(int)Mesh.ArrayType.Vertex].AsVector3Array();
                var nor = g[(int)Mesh.ArrayType.Normal].AsVector3Array();
                var idx = g[(int)Mesh.ArrayType.Index].AsInt32Array();
                for (int t = 0; t + 2 < idx.Length; t += 3)
                {
                    if (nor[idx[t]].Y < 0.7f) continue; // roofs only: they cover the footprint
                    var a = new Vector2(v[idx[t]].X + off.X, v[idx[t]].Z + off.Z);
                    var b = new Vector2(v[idx[t + 1]].X + off.X, v[idx[t + 1]].Z + off.Z);
                    var c = new Vector2(v[idx[t + 2]].X + off.X, v[idx[t + 2]].Z + off.Z);
                    roofTris[n.Ring]++;
                    foreach (var q in relS)
                    {
                        if (!Overlaps(a, b, c, q, pen)) continue;
                        badRoof[n.Ring]++;
                        if (firstRoof.Length == 0) firstRoof = $"R{n.Ring} tile {n.Key} roof on strip {q.Tag}";
                        break;
                    }
                    for (int k = 0; k < 3 && relA.Count > 0; k++)
                    {
                        var p = k == 0 ? a : k == 1 ? b : c;
                        float y = v[idx[t + k]].Y + off.Y;
                        float? lim = null;
                        string who = "";
                        foreach (var q in relA)
                        {
                            var l = q.Limit(p);
                            if (l is { } lv && (lim == null || lv < lim)) { lim = lv; who = q.Tag; }
                        }
                        if (lim is not { } cap) continue;
                        roofVerts[n.Ring]++;
                        float over = y - cap;
                        if (over > capTol)
                        {
                            badCap[n.Ring]++;
                            if (over > worstCap[n.Ring]) worstCap[n.Ring] = over;
                            if (over > worstOverAll) { worstOverAll = over; worstWho = who; }
                            if (firstCap.Length == 0) firstCap = $"R{n.Ring} tile {n.Key} roof y {y:0.00} is {over:0.00} m over the {who} surface ({cap:0.00})";
                        }
                    }
                }
            }
        }
        string sum = "";
        int totBadG = 0, totBadR = 0, totBadC = 0;
        for (int r = 0; r < 4; r++) { totBadG += badGround[r]; totBadR += badRoof[r]; totBadC += badCap[r]; }
        if (phaseS2)
        {
            if (totBadG > 0) Fail("district ground on strip " + spot, $"{totBadG} non-aeroway ground triangle(s) overlap a runway strip, e.g. {firstGround}");
            sum += $"; strip ground tris {groundTris[0]}/{groundTris[1]}/{groundTris[2]}/{groundTris[3]} (R0..R3) {totBadG} not aeroway";
        }
        if (phaseS3)
        {
            if (totBadR > 0) Fail("building on strip " + spot, $"{totBadR} building roof triangle(s) overlap a runway strip, e.g. {firstRoof}");
            if (totBadC > 0) Fail("approach cap " + spot, $"{totBadC} roof vertices over the approach surface at {spot} (worst {worstOverAll:0.00} m, airport/end {worstWho}; per ring {badCap[0]}/{badCap[1]}/{badCap[2]}/{badCap[3]}), e.g. {firstCap}");
            sum += $"; roofs near strips {roofTris[0]}/{roofTris[1]}/{roofTris[2]}/{roofTris[3]} {totBadR} on a strip; approach roof vertices {roofVerts[0]}/{roofVerts[1]}/{roofVerts[2]}/{roofVerts[3]} {totBadC} over the cap (worst {worstOverAll:0.00} m)";
        }
        return sum;
    }
}
