using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>One tile's points inside a <see cref="SpriteCell"/>: its slot in the cell's fade table and the bytes (cell-relative) the cell mesh is rebuilt from.</summary>
internal sealed class CellMember
{
    public TileNodes Tile = null!;
    public int Slot;
    public SpriteCell Cell = null!;
    public readonly PointBuffers Data = new(pooled: true);
}

/// <summary>
/// The merged point sprites of the R2 / R3 tiles of one spatial cell (a tile of the cell zoom, see <see cref="WorldRenderSettings.SpriteCellZoomR2"/>) and ring: ONE points mesh
/// and one draw call instead of one per tile. Each member tile owns a <b>slot</b> (0..63) that its points carry in CUSTOM0; the cell's material reads <c>slot_fade[slot]</c> =
/// <c>tile shown ? tile fade : 0</c> (<see cref="SyncTable"/>, once per frame), so a tile's points appear, fade and vanish in the frame its root does, whatever the mesh holds.
/// <list type="bullet">
/// <item>A tile joining or being re-delivered changes the mesh: <see cref="Rebuild"/> concatenates the members' cached bytes into a new surface (a memcpy, no repacking). It must
/// happen before the frame is drawn (<see cref="Required"/>), so <see cref="SpriteCells.Flush"/> runs it at the end of the frame's updates.</item>
/// <item>A tile leaving only zeroes its table entry; its points stay in the mesh (dead, drawn collapsed) and its slot is not reused until the next rebuild, which drops them.
/// The rebuild is done when the dead points are a large share, the cell is empty, or the slots ran out.</item>
/// </list>
/// Points are relative to <see cref="Origin"/> (the cell instance's position, kept in step with the floating origin by <see cref="SpriteCells.Shift"/>). Main thread only.
/// </summary>
internal sealed class SpriteCell
{
    public const int MaxSlots = 64;
    private const int TableVectors = MaxSlots / 4;

    public readonly MeshInstance3D Instance;
    public readonly ArrayMesh Mesh;
    public long Key;
    public int Ring;
    public readonly List<CellMember> Members = new();
    /// <summary>The mesh is out of date and a tile that should be drawn this frame is missing from it (or holds old points): rebuild before drawing.</summary>
    public bool Required;
    /// <summary>Points in the mesh, of which <see cref="DeadPoints"/> belong to tiles that left.</summary>
    public int MeshPoints, DeadPoints;
    public float MarginWritten = float.NaN;
    public Vector3 BoxMin, BoxMax;

    private ShaderMaterial? _material;
    private ulong _freeSlots = ulong.MaxValue, _deadSlots;
    private readonly float[] _values = new float[MaxSlots], _sent = new float[MaxSlots];
    private readonly Vector4[] _table = new Vector4[TableVectors];
    private bool _tableSent, _shown;
    private static readonly StringName PSlotFade = new("slot_fade");

    public SpriteCell(Node3D parent)
    {
        Mesh = new ArrayMesh();
        Instance = new MeshInstance3D
        {
            Name = "SpriteCell",
            Mesh = Mesh,
            CastShadow = GeometryInstance3D.ShadowCastingSetting.Off,
            Visible = false,
        };
        parent.AddChild(Instance);
    }

    public Vector3 Origin => Instance.Position;
    public int LivePoints => MeshPoints - DeadPoints;
    public bool HasMesh => MeshPoints > 0;
    public bool Empty => Members.Count == 0;

    /// <summary>Starts a cell for key <paramref name="key"/> at <paramref name="origin"/> with a material clone.</summary>
    public void Begin(long key, int ring, Vector3 origin, ShaderMaterial material)
    {
        Key = key;
        Ring = ring;
        Instance.Position = origin;
        Instance.MaterialOverride = _material = material;
        MarginWritten = float.NaN;
        _tableSent = false;
    }

    /// <summary>Takes a free slot (0..63) or -1 when every slot is taken or dead; dead ones come back with a rebuild, so ask <see cref="Rebuild"/> first if it returns -1 and <see cref="DeadPoints"/> is above 0.</summary>
    public int TakeSlot()
    {
        if (_freeSlots == 0) return -1;
        int s = System.Numerics.BitOperations.TrailingZeroCount(_freeSlots);
        _freeSlots &= ~(1UL << s);
        return s;
    }

    public bool HasDeadSlots => _deadSlots != 0;

    /// <summary>The tile left: its slot goes dead until the next rebuild, its points stay in the mesh (drawn at 0). Returns the member's buffers to the caller to recycle.</summary>
    public void Remove(CellMember m)
    {
        Members.Remove(m);
        _deadSlots |= 1UL << m.Slot;
        DeadPoints += m.Data.Count;
        _tableSent = false;
    }

    /// <summary>
    /// Rebuilds the mesh from the members' cached bytes into <paramref name="scratch"/> (a reusable buffer) and frees the dead slots. An empty cell clears its mesh.
    /// </summary>
    public void Rebuild(PointBuffers scratch)
    {
        int total = 0;
        for (int i = 0; i < Members.Count; i++) total += Members[i].Data.Count;
        _freeSlots |= _deadSlots;
        _deadSlots = 0;
        DeadPoints = 0;
        Required = false;
        if (total == 0)
        {
            if (MeshPoints > 0) Mesh.ClearSurfaces();
            MeshPoints = 0;
            return;
        }
        scratch.Reset(total);
        var min = scratch.Min;
        var max = scratch.Max;
        int at = 0;
        for (int i = 0; i < Members.Count; i++)
        {
            var d = Members[i].Data;
            System.Buffer.BlockCopy(d.Pos, 0, scratch.Pos, at * PointBuffers.PosStride, d.Count * PointBuffers.PosStride);
            System.Buffer.BlockCopy(d.Attr, 0, scratch.Attr, at * PointBuffers.AttrStride, d.Count * PointBuffers.AttrStride);
            min = min.Min(d.Min);
            max = max.Max(d.Max);
            at += d.Count;
        }
        scratch.Count = total;
        PointSurface.Set(Mesh, scratch.PosBytes, scratch.AttrBytes, total, min, max);
        MeshPoints = total;
        BoxMin = min;
        BoxMax = max;
        _tableSent = false;
    }

    /// <summary>
    /// Writes <c>slot_fade</c> (<c>tile shown ? tile fade : 0</c> per slot) into the material when it changed, and shows the instance while any slot is above 0 (and sprites are
    /// on). Returns true when the table was written.
    /// </summary>
    public bool SyncTable(bool spritesOn)
    {
        System.Array.Clear(_values);
        bool any = false;
        for (int i = 0; i < Members.Count; i++)
        {
            var m = Members[i];
            float v = m.Tile.Shown ? m.Tile.Fade : 0f;
            _values[m.Slot] = v;
            any |= v > 0f;
        }
        bool changed = !_tableSent;
        if (!changed) for (int i = 0; i < MaxSlots; i++) if (_values[i] != _sent[i]) { changed = true; break; }
        if (changed)
        {
            for (int i = 0; i < TableVectors; i++) _table[i] = new Vector4(_values[4 * i], _values[4 * i + 1], _values[4 * i + 2], _values[4 * i + 3]);
            _material!.SetShaderParameter(PSlotFade, _table);
            System.Array.Copy(_values, _sent, MaxSlots);
            _tableSent = true;
        }
        bool show = spritesOn && any && MeshPoints > 0;
        if (show != _shown) Instance.Visible = _shown = show;
        return changed;
    }

    /// <summary>True when the instance is shown now (mirror of the node).</summary>
    public bool Shown => _shown;

    /// <summary>The table value last written for <paramref name="slot"/> (tests).</summary>
    public float SentValue(int slot) => _sent[slot];

    /// <summary>Takes the cell out of play: members must be gone; hides the instance and returns the material.</summary>
    public ShaderMaterial End()
    {
        if (MeshPoints > 0) Mesh.ClearSurfaces();
        MeshPoints = DeadPoints = 0;
        _freeSlots = ulong.MaxValue;
        _deadSlots = 0;
        Required = false;
        Instance.Visible = _shown = false;
        var m = _material!;
        _material = null;
        Instance.MaterialOverride = null;
        return m;
    }

    /// <summary>Writes the earth-curvature cull margin (plus the sprite pad) to the instance.</summary>
    public void SetMargin(float margin, float pad)
    {
        MarginWritten = margin;
        Instance.ExtraCullMargin = margin + pad;
    }
}
