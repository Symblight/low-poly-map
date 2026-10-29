#nullable enable
using System.Collections.Generic;
using Godot;

namespace SimWorld;

// The point sprites of a tile: creation, the upload into its one points mesh or its merged cell (PointPacker, SpriteCells) and its pool MultiMesh (PoolWriter), the day / night switch.
public sealed partial class TilePool
{
    private bool _spritesOn = true;
    private readonly PointBuffers _tileBuf = new();
    private readonly PointSurface _pointSurface = new();
    private static readonly Vector3[] NoPoints = System.Array.Empty<Vector3>();
    private static readonly Color[] NoColors = System.Array.Empty<Color>();

    /// <summary>Problems seen while filling sprites (short colour arrays) and uploading meshes; logged once per key, counted for the overlay.</summary>
    public OnceLog Problems { get; } = new();

    /// <summary>True while the sprites are shown (the night factor is above the switch-on level). See <see cref="SetSpritesVisible"/>.</summary>
    public bool SpritesOn => _spritesOn;

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
        _cells.SetVisible(on);
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
        ClearOwnPoints(t);
        t.Clustered = false;
        t.LightCount = t.StreetLightCount = t.AirportLightCount = t.BeaconCount = 0;
        ClearPools(t);
    }

    private void ClearOwnPoints(TileNodes t)
    {
        if (t.PointsFilled) t.PointsMesh.ClearSurfaces();
        t.PointsFilled = false;
        t.Points.Visible = false;
    }

    private void ClearPools(TileNodes t)
    {
        if (t.PoolsFilled) t.PoolsMm.VisibleInstanceCount = 0;
        t.PoolsFilled = false;
        t.Pools.Visible = false;
        if (t.PoolsMm.InstanceCount > _maxRetainedSprites) t.PoolsMm.InstanceCount = 0;
    }

    /// <summary>
    /// Fills the tile's point sprites (building lights, street lamps, airport lights and beacons of the ready tile): R0 / R1 tiles into their own points mesh, R2 / R3 tiles (rings
    /// from <see cref="WorldRenderSettings.SpriteCellMinRing"/>) into the merged <see cref="SpriteCell"/> of their area, whose rebuild <see cref="SyncSprites"/> does at the end of the
    /// frame; and, when the tile's ring can show pools, the pool MultiMesh with its street lamps. Colour rgb = light colour, alpha = intensity weight (0.125 single light .. 1 big cluster;
    /// for a beacon the blink phase; Rust sends airport lights with their intensity weight, which scales the brightness instead). See <see cref="PointPacker"/>.
    /// </summary>
    public void SetSprites(TileNodes t, in ReadyTile tile)
    {
        var lights = _buildingLights ? tile.Lights ?? NoPoints : NoPoints;
        var street = tile.StreetLights ?? NoPoints;
        int total = lights.Length + street.Length + (tile.AirportLights?.Length ?? 0) + (tile.BeaconLights?.Length ?? 0);
        t.LightCount = lights.Length;
        t.StreetLightCount = street.Length;
        t.AirportLightCount = tile.AirportLights?.Length ?? 0;
        t.BeaconCount = tile.BeaconLights?.Length ?? 0;
        t.Clustered = false;
        int missing = 0;
        if (total > 0 && _cells.Merges(t.Ring) && _cells.TryPlace(t, tile.Origin, out var member, out var offset))
        {
            ClearOwnPoints(t);
            missing = Pack(member.Data, total, member.Slot, offset, lights, in tile);
            t.Clustered = member.Data.Clustered;
            _cells.Commit(member);
        }
        else
        {
            _cells.Remove(t);
            if (total > 0)
            {
                missing = Pack(_tileBuf, total, -1, default, lights, in tile);
                _pointSurface.Set(t.PointsMesh, _tileBuf);
                t.PointsFilled = true;
                t.Clustered = _tileBuf.Clustered;
                t.Points.Visible = true;
            }
            else ClearOwnPoints(t);
        }
        if (missing > 0) Problems.Warn("short-colours", $"[TilePool] tile {t.Key}: {missing} light colours missing for {total} light points; the rest are drawn warm white (the library must send one colour per point)");
        if (street.Length > 0 && t.Ring <= _streetPoolMaxRing)
        {
            _poolWriter.Fill(t.PoolsMm, street, tile.StreetLightColors ?? NoColors);
            t.PoolsFilled = true;
            t.Pools.Visible = t.PoolsInRange;
        }
        else ClearPools(t);
    }

    private int Pack(PointBuffers into, int total, int slot, Vector3 offset, Vector3[] lights, in ReadyTile tile)
    {
        _packer.Begin(into, total, slot, offset);
        int missing = _packer.Add(SpriteKind.Light, lights, tile.LightColors ?? NoColors);
        missing += _packer.Add(SpriteKind.Street, tile.StreetLights ?? NoPoints, tile.StreetLightColors ?? NoColors);
        missing += _packer.Add(SpriteKind.Airport, tile.AirportLights ?? NoPoints, tile.AirportLightColors ?? NoColors);
        missing += _packer.Add(SpriteKind.Beacon, tile.BeaconLights ?? NoPoints, tile.BeaconLightColors ?? NoColors);
        return missing;
    }

    /// <summary>
    /// End of the frame's tile updates, before drawing: rebuilds the merged cells that gained or changed a tile (a tile's points are in their cell in the frame its root is shown), writes
    /// the cells' per-tile fade tables (<c>shown ? fade : 0</c>) and, a few times a second, their cull margins. <paramref name="camera"/> and <paramref name="invTwoR"/> as for <see cref="CullMargins"/>.
    /// </summary>
    public void SyncSprites(Vector3 camera, float invTwoR)
    {
        _cells.Flush();
        _cells.Sync(camera, invTwoR);
    }

    /// <summary>The floating origin moved by <paramref name="shift"/>: moves the merged cells with the tile roots.</summary>
    public void ShiftSprites(Vector3 shift) => _cells.Shift(shift);

    /// <summary>Children of the container that are not tile roots (the root of the merged sprite cells).</summary>
    public int HelperNodes => 1;

    /// <summary>The merged point sprite cells (counters for the overlay and the benchmark, consistency checks).</summary>
    internal SpriteCells Cells => _cells;

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
