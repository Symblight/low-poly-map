using System.Collections.Generic;
using Godot;

namespace SimWorld;

// The point sprites of a tile: creation, the upload into its one points mesh (PointPacker) and its pool MultiMesh (PoolWriter), the day / night switch.
public sealed partial class TilePool
{
    private bool _spritesOn = true;
    private static readonly Vector3[] NoPoints = System.Array.Empty<Vector3>();
    private static readonly Color[] NoColors = System.Array.Empty<Color>();

    /// <summary>Problems seen while filling sprites (short colour arrays) and uploading meshes; logged once per key, counted for the overlay.</summary>
    public OnceLog Problems { get; } = new();

    /// <summary>True while the sprites are shown (the night factor is above the switch-on level). See <see cref="SetSpritesVisible"/>.</summary>
    public bool SpritesOn => _spritesOn;

    /// <summary>Pool MultiMesh buffer reallocations (a pool buffer had to grow) since start.</summary>
    public int PoolReallocations => _poolWriter.Reallocations;

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

    /// <summary>Extra cull margin of a sprite instance over the tile's curvature margin (m); the street pool discs reach below and around the lamps and need less.</summary>
    private const float SpriteCullPad = 200f, PoolCullPad = 50f;
    /// <summary>Distance the camera can cover between two range refreshes (a quarter of a second at 250 m/s is 63 m) before a pool just past the fade end would have been needed.</summary>
    private const float PoolRangeSlackM = 100f;

    private static MeshInstance3D CreatePoints(ArrayMesh mesh, ShaderMaterial material) => new()
    {
        Name = "Points",
        Mesh = mesh,
        // Also the material the dither clone of a fading tile replaces per node.
        MaterialOverride = material,
        CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
        Visible = false,
        // A point is expanded by the vertex shader (POINT_SIZE, lift); keep distant sprites out of any frustum-edge pop.
        ExtraCullMargin = SpriteCullPad,
    };

    private static (MultiMeshInstance3D, MultiMesh) CreatePools(Mesh quad, ShaderMaterial material)
    {
        var mm = new MultiMesh { TransformFormat = MultiMesh.TransformFormatEnum.Transform3D, UseColors = true, Mesh = quad };
        mm.InstanceCount = 0; // after format / colors are set
        var mmi = new MultiMeshInstance3D
        {
            Name = "Pools",
            Multimesh = mm,
            MaterialOverride = material,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            Visible = false,
            ExtraCullMargin = PoolCullPad, // the disc reaches below and around the lamp's AABB; the curvature margin is added by SetCullMargin
        };
        return (mmi, mm);
    }

    /// <summary>
    /// Empties the tile's sprites. The pool MultiMesh keeps its buffer (the next tile of this node set reuses it without a reallocation) unless it holds more than
    /// <see cref="WorldRenderSettings.SpriteMaxRetainedInstances"/>.
    /// </summary>
    private void ClearSprites(TileNodes t)
    {
        if (t.SpriteCount > 0) t.PointsMesh.ClearSurfaces();
        t.Clustered = false;
        t.LightCount = t.StreetLightCount = t.AirportLightCount = t.BeaconCount = 0;
        t.Points.Visible = false;
        ClearPools(t);
    }

    private void ClearPools(TileNodes t)
    {
        if (t.PoolsFilled) t.PoolsMm.VisibleInstanceCount = 0;
        t.PoolsFilled = false;
        t.Pools.Visible = false;
        if (t.PoolsMm.InstanceCount > _maxRetainedSprites) t.PoolsMm.InstanceCount = 0;
    }

    /// <summary>
    /// Fills the tile's point sprites (building lights, street lamps, airport lights and beacons of the ready tile) into its one points mesh, and, when the tile's ring can
    /// show pools, the pool MultiMesh with its street lamps. Colour rgb = light colour, alpha = intensity weight (0.125 single light .. 1 big cluster; for a beacon the
    /// blink phase; Rust sends airport lights with their intensity weight, which scales the brightness instead). See <see cref="PointPacker"/>.
    /// </summary>
    public void SetSprites(TileNodes t, in ReadyTile tile)
    {
        var lights = _buildingLights ? tile.Lights ?? NoPoints : NoPoints;
        var street = tile.StreetLights ?? NoPoints;
        var airport = tile.AirportLights ?? NoPoints;
        var beacons = tile.BeaconLights ?? NoPoints;
        int total = lights.Length + street.Length + airport.Length + beacons.Length;
        if (t.SpriteCount > 0) t.PointsMesh.ClearSurfaces();
        t.LightCount = lights.Length;
        t.StreetLightCount = street.Length;
        t.AirportLightCount = airport.Length;
        t.BeaconCount = beacons.Length;
        t.Clustered = false;
        t.Points.Visible = total > 0;
        if (total > 0)
        {
            _packer.Begin(total);
            int missing = _packer.Add(SpriteKind.Light, lights, tile.LightColors ?? NoColors);
            missing += _packer.Add(SpriteKind.Street, street, tile.StreetLightColors ?? NoColors);
            missing += _packer.Add(SpriteKind.Airport, airport, tile.AirportLightColors ?? NoColors);
            missing += _packer.Add(SpriteKind.Beacon, beacons, tile.BeaconLightColors ?? NoColors);
            t.PointsMesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Points, _packer.Finish());
            t.Clustered = _packer.Clustered;
            if (missing > 0) Problems.Warn("short-colours", $"[TilePool] tile {t.Key}: {missing} light colours missing for {total} light points; the rest are drawn warm white (the library must send one colour per point)");
        }
        if (street.Length > 0 && t.Ring <= _streetPoolMaxRing)
        {
            _poolWriter.Fill(t.PoolsMm, street, tile.StreetLightColors ?? NoColors);
            t.PoolsFilled = true;
            t.Pools.Visible = t.PoolsInRange;
        }
        else ClearPools(t);
    }

    /// <summary>
    /// Shows the tile's pools only while the tile is within the fade range of the camera: <paramref name="nearestM"/> is the distance to the tile's nearest point; beyond
    /// the pool fade end <see cref="SetLodRanges"/> set, every pool of the tile is invisible (their shader collapses them), so the draw is skipped. Cheap; called a few times a second.
    /// </summary>
    public void UpdatePoolRange(TileNodes t, float nearestM)
    {
        bool inRange = nearestM < _poolFadeEndM + PoolRangeSlackM;
        if (inRange == t.PoolsInRange) return;
        t.PoolsInRange = inRange;
        if (t.PoolsFilled) t.Pools.Visible = inRange;
    }
}
