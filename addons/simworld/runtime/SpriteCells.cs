#nullable enable
using System.Collections.Generic;
using System.Diagnostics;
using Godot;

namespace SimWorld;

/// <summary>
/// The spatial cells that merge the point sprites of the R2 / R3 tiles (<see cref="SpriteCell"/>): placement of a tile into the cell of its key, removal, the end-of-frame
/// <see cref="Flush"/> (rebuilds of cells that must be redrawn) and <see cref="Sync"/> (the per-slot fade tables, the cull margins), the origin shift and the day switch.
/// R0 / R1 tiles (and the landmark tiles) keep their own points mesh. A cell is keyed by the tile's ancestor at the cell zoom and its ring, so the swap partners of the cover
/// (R2 under R3) are separate meshes with separate tables. Main thread only.
/// </summary>
internal sealed class SpriteCells
{
    private readonly Node3D _root;
    private readonly SpriteMaterials _materials;
    private readonly float _compactShare, _pad;
    private readonly int _minRing, _zoomR2, _zoomR3;
    private readonly Dictionary<long, SpriteCell> _cells = new();
    private readonly List<SpriteCell> _list = new();
    private readonly Stack<SpriteCell> _free = new();
    private readonly Stack<CellMember> _spareMembers = new();
    private readonly PointBuffers _scratch = new();
    private readonly PointSurface _surface = new();
    private bool _on = true;
    /// <summary>Something changed since the last <see cref="Sync"/> (a tile placed, committed or removed): the cells are not up to date until the frame's sync.</summary>
    private bool _unsynced;
    private Vector3 _camera;
    private float _invTwoR;
    private long _nextMarginTicks;
    private static readonly long MarginIntervalTicks = Stopwatch.Frequency / 4;

    public SpriteCells(Node3D container, SpriteMaterials materials, WorldRenderSettings settings, float pad)
    {
        _root = new Node3D { Name = "SpriteCells" };
        container.AddChild(_root);
        _materials = materials;
        _pad = pad;
        _minRing = settings.SpriteCellMinRing;
        _zoomR2 = settings.SpriteCellZoomR2;
        _zoomR3 = settings.SpriteCellZoomR3;
        _compactShare = settings.SpriteCellCompactShare;
    }

    /// <summary>Whether tiles of this ring are merged.</summary>
    public bool Merges(int ring) => ring >= _minRing;

    // ---- stats (overlay, benchmark) ----
    public int CellCount => _list.Count;
    public int MemberCount { get; private set; }
    public long Rebuilds { get; private set; }
    public long RebuiltPoints { get; private set; }
    public long RebuildUs { get; private set; }
    public long RebuildUsMax { get; private set; }
    public long TableWrites { get; private set; }
    public int DeadPoints { get; private set; }
    public int MeshPoints { get; private set; }
    public IReadOnlyList<SpriteCell> Cells => _list;

    private int CellZoom(int ring, int tileZ)
    {
        int z = ring == 2 ? _zoomR2 : _zoomR3;
        // The table has 64 slots: at most 3 zoom levels between the cell and its tiles (4^3 = 64); never finer than the tile itself.
        return System.Math.Clamp(z, tileZ - 3, tileZ);
    }

    private long KeyOf(TileNodes t) => TileKey.Ancestor(t.Packed, CellZoom(t.Ring, TileKey.Z(t.Packed))) | ((long)t.Ring << 56);

    /// <summary>
    /// Finds the member of <paramref name="t"/> in its cell, adding it (a new cell if needed) and returning the offset to add to the tile's point positions so that they are
    /// relative to the cell. False when no slot could be found (the caller draws the tile's own mesh). The tile's points must then be packed into <c>member.Data</c> and
    /// <see cref="Commit"/> called.
    /// </summary>
    public bool TryPlace(TileNodes t, Vector3 tileOrigin, out CellMember member, out Vector3 offset)
    {
        member = null!;
        offset = default;
        if (t.CellMember != null)
        {
            // Re-delivered key: the same slot, the new bytes. A key never changes cell.
            member = t.CellMember;
            offset = tileOrigin - member.Cell.Origin;
            return true;
        }
        long key = KeyOf(t);
        if (!_cells.TryGetValue(key, out var cell)) cell = StartCell(key, t.Ring, tileOrigin);
        int slot = cell.TakeSlot();
        if (slot < 0 && cell.HasDeadSlots)
        {
            Rebuild(cell); // frees the dead slots
            slot = cell.TakeSlot();
        }
        if (slot < 0) return false;
        member = _spareMembers.Count > 0 ? _spareMembers.Pop() : new CellMember();
        member.Tile = t;
        member.Slot = slot;
        member.Cell = cell;
        _unsynced = true;
        cell.Members.Add(member);
        t.CellMember = member;
        MemberCount++;
        offset = tileOrigin - cell.Origin;
        return true;
    }

    /// <summary>The member's bytes are written: the cell must be rebuilt before it is drawn.</summary>
    public void Commit(CellMember m)
    {
        m.Cell.Required = true;
        _unsynced = true;
    }

    /// <summary>Takes the tile's points out of its cell (it was released, or a re-delivery has none). The cell is rebuilt later, see <see cref="Flush"/>.</summary>
    public void Remove(TileNodes t)
    {
        var m = t.CellMember;
        if (m == null) return;
        t.CellMember = null;
        _unsynced = true;
        var cell = m.Cell;
        cell.Remove(m);
        MemberCount--;
        m.Data.Release(); // the cell's mesh still holds a copy of the points until its rebuild
        m.Tile = null!;
        m.Cell = null!;
        _spareMembers.Push(m);
    }

    private SpriteCell StartCell(long key, int ring, Vector3 origin)
    {
        var cell = _free.Count > 0 ? _free.Pop() : new SpriteCell(_root);
        cell.Begin(key, ring, origin, _materials.CellFades.Rent());
        _cells[key] = cell;
        _list.Add(cell);
        return cell;
    }

    private void EndCell(SpriteCell cell)
    {
        _cells.Remove(cell.Key);
        _list.Remove(cell);
        _materials.CellFades.Return(cell.End());
        _free.Push(cell);
    }

    private void Rebuild(SpriteCell cell)
    {
        long t0 = Stopwatch.GetTimestamp();
        cell.Rebuild(_scratch, _surface);
        long us = (Stopwatch.GetTimestamp() - t0) * 1_000_000 / Stopwatch.Frequency;
        Rebuilds++;
        RebuiltPoints += cell.MeshPoints;
        RebuildUs += us;
        if (us > RebuildUsMax) RebuildUsMax = us;
        UpdateMargin(cell);
    }

    /// <summary>
    /// End of the frame's tile updates, before drawing: rebuilds every cell that has a tile to draw that its mesh lacks, and, when dead points are a large share of a cell (or it
    /// is empty), the cell too; an empty cell is recycled.
    /// </summary>
    public void Flush()
    {
        for (int i = _list.Count - 1; i >= 0; i--)
        {
            var cell = _list[i];
            if (cell.Empty) { EndCell(cell); continue; }
            if (cell.Required || cell.DeadPoints > 0 && cell.DeadPoints >= _compactShare * cell.MeshPoints) Rebuild(cell);
        }
    }

    /// <summary>
    /// Once per frame after <see cref="Flush"/>: the fade tables (<c>shown ? fade : 0</c> per slot) and the instances' visibility; a few times a second the cull margins.
    /// </summary>
    public void Sync(Vector3 camera, float invTwoR)
    {
        _camera = camera;
        _invTwoR = invTwoR;
        _unsynced = false;
        int dead = 0, points = 0;
        for (int i = 0; i < _list.Count; i++)
        {
            var c = _list[i];
            if (c.SyncTable(_on)) TableWrites++;
            dead += c.DeadPoints;
            points += c.MeshPoints;
        }
        DeadPoints = dead;
        MeshPoints = points;
        long now = Stopwatch.GetTimestamp();
        if (now < _nextMarginTicks) return;
        _nextMarginTicks = now + MarginIntervalTicks;
        for (int i = 0; i < _list.Count; i++) UpdateMargin(_list[i]);
    }

    private void UpdateMargin(SpriteCell c)
    {
        if (!c.HasMesh) return;
        var size = c.BoxMax - c.BoxMin;
        float half = 0.5f * Mathf.Sqrt(size.X * size.X + size.Z * size.Z);
        var centre = c.Origin + (c.BoxMin + c.BoxMax) * 0.5f;
        if (CullMargins.TryUpdate(c.MarginWritten, _camera, centre, half, _invTwoR, out float margin)) c.SetMargin(margin, _pad);
    }

    /// <summary>The floating origin moved by <paramref name="shift"/>: every cell moves with the tile roots.</summary>
    public void Shift(Vector3 shift)
    {
        for (int i = 0; i < _list.Count; i++) _list[i].Instance.Position -= shift;
    }

    /// <summary>The day / night switch: by day the cells are hidden (their tables stay current, so they come back right).</summary>
    public void SetVisible(bool on)
    {
        _on = on;
        for (int i = 0; i < _list.Count; i++) _list[i].SyncTable(on);
    }

    /// <summary>
    /// Diagnostic for a tile the cover shows (<c>--hole-check</c>): are all of its point sprites ready to be drawn in this frame? A merged tile needs its member in a cell that is not waiting for a
    /// rebuild, holds all its points, is shown, and has a fade table entry equal to the tile's; a tile with its own mesh needs the mesh drawn. True while the sprites are off (by day) or the tile has none.
    /// </summary>
    public bool LightsDrawn(TileNodes t, out string why)
    {
        why = "";
        if (!_on || t.SpriteCount == 0) return true;
        if (t.CellMember is not { } m)
        {
            if (t.PointsFilled && t.Points.Visible) return true;
            why = Merges(t.Ring) ? "ring merges its lights but the tile is in no cell and draws none of its own" : "the tile's own mesh of lights is not drawn";
            return false;
        }
        var c = m.Cell;
        if (m.Data.Count != t.SpriteCount) why = $"cell holds {m.Data.Count} of the tile's {t.SpriteCount} lights";
        else if (c.Required) why = "cell waits for a rebuild";
        else if (c.LivePoints < m.Data.Count) why = $"cell mesh has {c.LivePoints} live points, fewer than the tile's {m.Data.Count}";
        else
        {
            float want = t.Shown ? t.Fade : 0f;
            if (c.SentValue(m.Slot) != want) why = $"fade table entry {c.SentValue(m.Slot):0.###} for a tile shown at {want:0.###}";
            else if (want > 0f && !c.Shown) why = "cell instance hidden";
        }
        return why.Length == 0;
    }

    /// <summary>Test / diagnostic: the first problem found in the cells' bookkeeping, or null. Checks accounting and that each shown slot carries what its tile shows.</summary>
    public string? Verify()
    {
        if (_unsynced) return null; // called between a tile update and the frame's sync: nothing to compare yet
        int members = 0;
        foreach (var c in _list)
        {
            if (c.Required) return $"cell {TileKey.ToText(c.Key & 0x00FFFFFFFFFFFFFF)} ring {c.Ring} still needs a rebuild after the frame's flush";
            if (c.LivePoints < 0) return $"cell {TileKey.ToText(c.Key & 0x00FFFFFFFFFFFFFF)} has more dead points ({c.DeadPoints}) than points ({c.MeshPoints})";
            int live = 0;
            foreach (var m in c.Members)
            {
                members++;
                live += m.Data.Count;
                if (m.Tile.CellMember != m || m.Cell != c) return $"member of {m.Tile.Key} is not linked back to its cell";
                float want = m.Tile.Shown ? m.Tile.Fade : 0f;
                if (c.SentValue(m.Slot) != want) return $"tile {m.Tile.Key}: fade table says {c.SentValue(m.Slot):0.###}, the tile shows {want:0.###}";
            }
            if (live != c.LivePoints) return $"cell {TileKey.ToText(c.Key & 0x00FFFFFFFFFFFFFF)} ring {c.Ring}: members hold {live} points, the mesh has {c.LivePoints} live";
        }
        return members == MemberCount ? null : $"{members} members in the cells, {MemberCount} counted";
    }
}
