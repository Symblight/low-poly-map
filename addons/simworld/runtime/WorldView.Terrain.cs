using System.Collections.Generic;
using Godot;

namespace SimWorld;

// Terrain layer (api 1.5): the tile map, the profile section it reads and set_terrain_detail.
public partial class WorldView
{
    /// <summary>Terrain layer tile map (api 1.5): its own quadtree, root / leaf zoom from the profile (terrain.far_zoom / near_zoom, default 10..14),
    /// ring statistics for 5 rings. Empty unless <see cref="TerrainActive"/>.</summary>
    public TileCover TerrainCover { get; } = new(10, 14);

    /// <summary>True when the terrain layer runs: the host asked for it, the library has it (api 1.5) and the profile enables it.</summary>
    public bool TerrainActive { get; private set; }
    /// <summary>Slant reach of the terrain layer in metres (last band edge), 0 while inactive: the camera far plane and the day fog follow it.</summary>
    public float TerrainReachM { get; private set; }
    /// <summary>Rings of the terrain layer (terrain.bands entries), 0 while inactive.</summary>
    public int TerrainRingCount => TerrainActive ? TerrainCover.Rings : 0;
    /// <summary>The set_terrain_detail scale in effect (1 = profile).</summary>
    public double TerrainDetail { get; private set; } = 1.0;
    /// <summary>Profile <c>terrain.tau_px</c> (the terrain cover's screen-space-error threshold before the <see cref="TerrainDetail"/> scale; 0 on a library without it) and the effective tau = tau_px x scale.</summary>
    public double TerrainTauPx { get; private set; }
    public double TerrainTauEff => TerrainTauPx * TerrainDetail;
    /// <summary>Terrain ready deliveries (first and re-deliveries), re-deliveries replaced in place, and unloads.</summary>
    public int TerrainReadyTotal { get; private set; }
    public int TerrainReplacedTotal { get; private set; }
    public int TerrainUnloadedTotal { get; private set; }
    /// <summary>Terrain keys ("t/z/x/y") seen in poll_ready, poll_fades or poll_unload since start. Must stay 0 for a host that did not opt in.</summary>
    public int TerrainKeysSeen { get; private set; }
    /// <summary>Per tile upload cost in microseconds for the terrain layer, indexed by ring (up to 8).</summary>
    public readonly long[] TerrainUploadUsLast = new long[StreamerStats.TerrainRingsMax];
    public readonly long[] TerrainUploadUsMax = new long[StreamerStats.TerrainRingsMax];
    public readonly long[] TerrainUploadUsSum = new long[StreamerStats.TerrainRingsMax];
    public readonly int[] TerrainUploadCount = new int[StreamerStats.TerrainRingsMax];

    /// <summary>The ground collision helper; null while <see cref="TerrainCollisionEnabled"/> is off.</summary>
    public TerrainCollision? TerrainCollision => _collision;

    /// <summary>The shared opaque terrain material (tests, debug UI).</summary>
    public ShaderMaterial? TerrainMaterial => _terrainPool?.Material;

    public int TerrainPoolFree => _terrainPool?.FreeCount ?? 0;
    public int TerrainPoolCreated => _terrainPool?.TotalCreated ?? 0;

    public int TerrainPoolTrimmed => _terrainPool?.Trimmed ?? 0;

    private TerrainCollision? _collision;
    private bool _loggedFirstTerrainTile;
    private bool _warnedNoTerrainLayer;

    /// <summary>After a configure: reads the profile's terrain section, resets the terrain tile map to its zoom range and the pool to its shading.</summary>
    private void ConfigureTerrainLayer(Godot.Collections.Dictionary profile)
    {
        TerrainActive = false;
        TerrainReachM = 0f;
        TerrainTauPx = 0.0;
        if (EnableTerrain && Streamer.HasTerrainLayer
            && profile.TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary)
        {
            var t = tv.AsGodotDictionary();
            // api 1.6: the library says whether the terrain is active; before that the host re-derives the rule from the profile.
            bool enabled = Streamer.HasTerrainActiveFlag ? Status.TerrainActive : t.TryGetValue("enabled", out var ev) && ev.AsBool();
            if (enabled && t.TryGetValue("near_zoom", out var nz) && t.TryGetValue("far_zoom", out var fz))
            {
                if (TerrainCover.Loaded != 0)
                    GD.PushError($"[WorldView] terrain tile map still holds {TerrainCover.Loaded} tiles at configure; releasing them");
                var stray = new List<long>();
                foreach (var n in TerrainCover.All) stray.Add(n.Packed);
                foreach (long k in stray) ReleaseTerrainTile(k);
                TerrainCover.Configure((int)fz.AsInt64(), (int)nz.AsInt64());
                _terrainPool!.SetSmoothShading(t.TryGetValue("shading", out var sh) && sh.AsString() == "smooth");
                if (t.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Array)
                {
                    var bands = bv.AsGodotArray();
                    if (bands.Count > 0 && bands[bands.Count - 1].VariantType == Variant.Type.Dictionary
                        && bands[bands.Count - 1].AsGodotDictionary().TryGetValue("edge_m", out var edge))
                        TerrainReachM = (float)edge.AsDouble();
                }
                TerrainTauPx = t.TryGetValue("tau_px", out var tau) ? tau.AsDouble() : 0.0;
                TerrainActive = true;
            }
        }
        if (!TerrainActive && TerrainCover.Loaded == 0) TerrainCover.Configure(TerrainCover.RootZ, TerrainCover.LeafZ);
        _collision?.Invalidate(); // new world: the patch was filled from the old one
        GD.Print(TerrainActive
            ? $"[WorldView] terrain layer on: zoom {TerrainCover.RootZ}..{TerrainCover.LeafZ} ({TerrainCover.Rings} rings), reach {TerrainReachM / 1000f:0.#} km, shading {(_terrainPool != null ? "per profile" : "-")}"
            : EnableTerrain ? "[WorldView] terrain layer requested but the profile has terrain.enabled = false (or the library lacks it): no terrain tiles"
                            : "[WorldView] terrain layer off");
    }

    /// <summary>
    /// set_terrain_detail: a scale of the terrain cover's screen-space-error threshold (tau = <c>terrain.tau_px</c> x scale; the library clamps to
    /// [0.25, terrain.detail_scale_max]). Above 1 is coarser (fewer, larger tiles), below 1 finer and much more expensive (0.25 is about 2300 tiles).
    /// Since api 1.10 the cover follows through ordinary refines / coarsens (new keys, fades, unloads): held tiles are not re-delivered. No teleport, the
    /// vector world is untouched. Returns the scale in effect (NaN on a library without the method).
    /// </summary>
    public double SetTerrainDetail(double scale)
    {
        double r = Streamer.SetTerrainDetail(scale);
        if (!double.IsNaN(r)) TerrainDetail = r;
        return r;
    }
}
