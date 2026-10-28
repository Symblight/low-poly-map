using System.Collections.Generic;
using Godot;

namespace SimWorld;

// The light sprite sets of a tile: creation, upload into MultiMeshes, the day / night switch.
public sealed partial class TilePool
{
    private const int FloatsPerInstance = 16;
    private const int CapacityStep = 256;
    /// <summary>Alpha of a single light is clamp(sqrt(1)/8) = 0.125; anything clearly above is a cluster.</summary>
    private const float ClusterAlpha = 0.13f;
    /// <summary>A MultiMesh keeps its instance count (and so its native buffer) while a smaller tile reuses it, until it is more than twice what the tile needs and above this floor.</summary>
    private const int ShrinkFloorInstances = 1024;
    /// <summary>A released set keeps at most this many instances per sprite set; bigger ones are shrunk to nothing when the set is cleared.</summary>
    private const int MaxRetainedInstances = 4096;
    /// <summary>Staging arrays are cached up to this many instances (256 KB); bigger uploads allocate a temporary one.</summary>
    private const int ScratchCacheMaxInstances = 4096;

    // One managed staging array per instance count, shared by every tile: MultiMesh.Buffer must hold exactly InstanceCount x 16 floats, and a copy of it
    // is made natively, so no set needs to keep its own array (they used to, and reallocated it whenever the count crossed a 256 bucket).
    private readonly Dictionary<int, float[]> _scratch = new();
    private bool _spritesOn = true;

    /// <summary>Problems seen while filling sprites (short colour arrays) and uploading meshes; logged once per key, counted for the overlay.</summary>
    public OnceLog Problems { get; } = new();

    /// <summary>True while the sprite sets are shown (the night factor is above the switch-on level). See <see cref="SetSpritesVisible"/>.</summary>
    public bool SpritesOn => _spritesOn;

    private float[] Scratch(int instances)
    {
        int len = instances * FloatsPerInstance;
        if (instances > ScratchCacheMaxInstances) return new float[len];
        if (!_scratch.TryGetValue(instances, out var a)) _scratch[instances] = a = new float[len];
        return a;
    }

    /// <summary>
    /// Shows or hides the sprite container of every held tile (<paramref name="held"/>) and of every set acquired later. The light sprites, street lamp
    /// pools, beacons and airport lights all multiply their brightness by the night factor, so by day they only cost draw calls and additive black
    /// pixels; hidden, the engine does not cull, sort or rasterize them. Beacons go with the rest (their shader is also zero by day). One call per tile,
    /// only when the state changes. Returns the number of tiles touched.
    /// </summary>
    public int SetSpritesVisible(bool on, IEnumerable<TileNodes> held)
    {
        if (on == _spritesOn) return 0;
        _spritesOn = on;
        int n = 0;
        foreach (var t in held)
        {
            ApplySprites(t);
            n++;
        }
        return n;
    }

    private void ApplySprites(TileNodes t)
    {
        if (t.SpritesShown == _spritesOn) return;
        t.Sprites.Visible = _spritesOn;
        t.SpritesShown = _spritesOn;
    }

    private static (MultiMeshInstance3D, MultiMesh) CreateSprites(string name, Mesh quad, ShaderMaterial material)
    {
        var mm = new MultiMesh
        {
            TransformFormat = MultiMesh.TransformFormatEnum.Transform3D,
            UseColors = true,
            Mesh = quad,
        };
        mm.InstanceCount = 0; // after format/colors are set
        var mmi = new MultiMeshInstance3D
        {
            Name = name,
            Multimesh = mm,
            // Also as override so the per-instance fade_mul uniform is found on the MultiMesh instance.
            MaterialOverride = material,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            Visible = false,
            // The billboard is expanded in the vertex shader; keep distant sprites out of any frustum-edge pop.
            ExtraCullMargin = SpriteCullPad,
        };
        return (mmi, mm);
    }

    /// <summary>Extra cull margin of a sprite instance over the tile's curvature margin (m); the street pool discs reach below and around the lamps and need less.</summary>
    private const float SpriteCullPad = 200f, PoolCullPad = 50f;

    private static void ClearSet(MultiMeshInstance3D mmi, MultiMesh mm, ref int count)
    {
        if (count > 0)
        {
            mm.VisibleInstanceCount = 0;
            mmi.Visible = false;
            count = 0;
        }
        if (mm.InstanceCount > MaxRetainedInstances) mm.InstanceCount = 0;
    }

    private static void ClearLights(TileNodes t)
    {
        t.Clustered = false;
        ClearSet(t.Lights, t.LightsMm, ref t.LightCount);
        if (t.StreetLightCount > 0) t.StreetPools.Visible = false;
        ClearSet(t.StreetLights, t.StreetLightsMm, ref t.StreetLightCount);
        ClearSet(t.Beacons, t.BeaconsMm, ref t.BeaconCount);
        ClearSet(t.AirportLights, t.AirportLightsMm, ref t.AirportLightCount);
    }

    /// <summary>
    /// Fills the tile's light MultiMesh from positions relative to the tile origin. Color rgb = light color,
    /// alpha = intensity weight (0.125 single light .. 1 big cluster); the sprite shader reads it from COLOR.
    /// </summary>
    public void SetLights(TileNodes t, Vector3[] lights, Color[] colors)
    {
        int prev = t.LightCount;
        t.LightCount = lights.Length;
        t.Clustered = Fill(t, t.Lights, t.LightsMm, prev, lights, colors);
    }

    /// <summary>Fills the tile's street lamp MultiMesh (api 1.1), same layout as <see cref="SetLights"/>.</summary>
    public void SetStreetLights(TileNodes t, Vector3[] lights, Color[] colors)
    {
        int prev = t.StreetLightCount;
        t.StreetLightCount = lights.Length;
        Fill(t, t.StreetLights, t.StreetLightsMm, prev, lights, colors);
        if (prev > 0 || lights.Length > 0) t.StreetPools.Visible = lights.Length > 0;
    }

    /// <summary>Fills the tile's beacon MultiMesh (api 1.2); colour alpha is the blink phase, passed through untouched.</summary>
    public void SetBeacons(TileNodes t, Vector3[] lights, Color[] colors)
    {
        int prev = t.BeaconCount;
        t.BeaconCount = lights.Length;
        Fill(t, t.Beacons, t.BeaconsMm, prev, lights, colors, rawAlpha: true);
    }

    /// <summary>
    /// Fills the tile's airport light MultiMesh (api 1.7). Rust sends the fixed colour (blue / white / green / red) and the intensity weight in the
    /// alpha (<c>seed.airport.lights.intensity</c>, 0.35); the sprite shader would read an alpha above 0.125 as a cluster, so the buffer carries
    /// alpha 0.125 (a single light) and the weight scales the colour instead (weight / reference weight, 1 at the default).
    /// </summary>
    public void SetAirportLights(TileNodes t, Vector3[] lights, Color[] colors)
    {
        int prev = t.AirportLightCount;
        t.AirportLightCount = lights.Length;
        Fill(t, t.AirportLights, t.AirportLightsMm, prev, lights, colors, airportWeight: _airportReferenceWeight);
    }

    /// <summary>Uploads one sprite set; returns true when any light carries a cluster weight.</summary>
    private bool Fill(TileNodes t, MultiMeshInstance3D mmi, MultiMesh mm, int previousCount, Vector3[] lights, Color[] colors, bool rawAlpha = false, float airportWeight = 0f)
    {
        int n = lights.Length;
        if (n == 0)
        {
            if (previousCount > 0)
            {
                mm.VisibleInstanceCount = 0;
                mmi.Visible = false;
            }
            return false;
        }

        int cap = (n + CapacityStep - 1) / CapacityStep * CapacityStep;
        int have = mm.InstanceCount;
        if (have < cap || have > 2 * cap && have > ShrinkFloorInstances)
        {
            mm.InstanceCount = cap;
            have = cap;
        }
        var buf = Scratch(have);

        int nc = colors.Length;
        bool clustered = false;
        for (int i = 0, o = 0; i < n; i++, o += FloatsPerInstance)
        {
            var p = lights[i];
            buf[o + 0] = 1f; buf[o + 1] = 0; buf[o + 2] = 0; buf[o + 3] = p.X;
            buf[o + 4] = 0; buf[o + 5] = 1f; buf[o + 6] = 0; buf[o + 7] = p.Y;
            buf[o + 8] = 0; buf[o + 9] = 0; buf[o + 10] = 1f; buf[o + 11] = p.Z;
            if (i < nc)
            {
                var c = colors[i];
                if (airportWeight > 0f)
                {
                    float k = c.A / airportWeight;
                    buf[o + 12] = c.R * k; buf[o + 13] = c.G * k; buf[o + 14] = c.B * k; buf[o + 15] = 0.125f;
                    continue;
                }
                float a = DebugLightAlpha >= 0f && !rawAlpha ? DebugLightAlpha : c.A;
                buf[o + 12] = c.R; buf[o + 13] = c.G; buf[o + 14] = c.B; buf[o + 15] = a;
                if (a > ClusterAlpha) clustered = true;
            }
            else
            {
                buf[o + 12] = 1; buf[o + 13] = 0.9f; buf[o + 14] = 0.7f; buf[o + 15] = 0.125f;
            }
        }
        if (nc < n) Problems.Warn("short-colours", $"[TilePool] tile {t.Key}: {nc} light colours for {n} light points; the rest are drawn warm white (the library must send one colour per point)");
        System.Array.Clear(buf, n * FloatsPerInstance, buf.Length - n * FloatsPerInstance); // instances past n are never drawn; keep them neutral for the bounds
        mm.Buffer = buf;
        mm.VisibleInstanceCount = n;
        mmi.Visible = true;
        return clustered;
    }
}
