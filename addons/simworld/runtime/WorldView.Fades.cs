#nullable enable
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>poll_fades: dither fades of the sibling swaps, applied to the tiles' pooled materials.</summary>
public partial class WorldView
{
    /// <summary>Tiles in a poll_fades transition (below 1) with their last poll stamp; allocation-free in steady state.</summary>
    private readonly List<TileNodes> _fading = new(64);
    private int _fadeFrame;
    public int FadeEntriesLastPoll { get; private set; }
    /// <summary>Test aid (user arg --fake-fade=&lt;0..1&gt;): pretends poll_fades lists every R1 tile with this fade (checks the dither path without Rust).</summary>
    private float _fakeFade = -1f;
    /// <summary>True when the loaded Rust build exports poll_fades(); false = no transitions, instant swaps (old behaviour).</summary>
    public bool FadesSupported => Cover.FadesEnabled;

    // One entry of the last poll_fades dictionary: its key as the Variant the library gave (kept so the next frame can read the value without turning the
    // string into a managed one again), and the parsed tile key.
    private struct FadeKey
    {
        public Variant Key;
        public long Packed;
        public TileLayer Layer;
        public bool Valid;
    }

    private readonly List<FadeKey> _fadeKeys = new(64);
    private float[] _fadeValues = new float[64];

    /// <summary>
    /// poll_fades step (after poll_ready, before poll_unload): applies each listed tile's fade to its pooled nodes, ends the
    /// transition of tiles that were listed before but are not any more (or listed with 1.0), then lets tiles uploaded
    /// this frame become visible.
    /// <para>The dictionary is read without allocating while its key set is the same as last frame's (the usual case: a fade runs for dozens of frames): the
    /// remembered key Variants look the new values up. Only a frame in which a fade starts or ends enumerates it and parses the key strings again.</para>
    /// </summary>
    private void PollFades()
    {
        var pool = _pool!;
        Godot.Collections.Dictionary? fades;
        using (AllocProbe.Measure(AllocSite.PollFades)) fades = Streamer.PollFades();
        using var probe = AllocProbe.Measure(AllocSite.FadeApply);
        long applyStart = (long)Time.GetTicksUsec(); // the library call is the "fades" marker, what follows is fade_apply
        _fadeFrame++;
        int entries = fades?.Count ?? 0;
        if (entries > 0)
        {
            using (AllocProbe.Measure(AllocSite.FadeRead)) if (!ReadKnownFades(fades!)) ReadFadesAfresh(fades!);
            for (int i = 0; i < _fadeKeys.Count; i++) ApplyFade(_fadeKeys[i], _fadeValues[i]);
        }
        else ClearFadeKeys();
        if (_fakeFade >= 0f)
        {
            foreach (var nodes in Cover.All)
            {
                if (nodes.Ring != 1) continue;
                entries++;
                nodes.FadeStamp = _fadeFrame;
                if (!nodes.Fading) _fading.Add(nodes);
                pool.SetFade(nodes, _fakeFade);
                Cover.SetFade(nodes, true, _fakeFade);
            }
        }
        FadeEntriesLastPoll = entries;
        // Transitions that disappeared from the dictionary without a 1.0 entry (e.g. force-completed) end here.
        for (int i = _fading.Count - 1; i >= 0; i--)
        {
            var n = _fading[i];
            if (!n.Fading || CoverOf(n).TryGet(n.Packed, out var live) && live != n)
            {
                _fading.RemoveAt(i); // unloaded in the meantime
                continue;
            }
            if (n.FadeStamp != _fadeFrame) EndTileFade(n);
        }
        Cover.ReleaseAwaiting();
        TerrainCover.ReleaseAwaiting();
        fades?.Dispose(); // the remembered key Variants above hold their own references
        _fadeApplyUs += (long)Time.GetTicksUsec() - applyStart;
    }

    /// <summary>Reads the values of the remembered keys. False when the dictionary's key set is not exactly the remembered one (a fade started or ended).</summary>
    private bool ReadKnownFades(Godot.Collections.Dictionary fades)
    {
        int n = _fadeKeys.Count;
        if (n != fades.Count) return false;
        for (int i = 0; i < n; i++)
        {
            if (!fades.TryGetValue(_fadeKeys[i].Key, out var v)) return false;
            _fadeValues[i] = (float)v.AsDouble();
        }
        return true;
    }

    /// <summary>The key set changed: enumerates the dictionary, parses the key strings once, and remembers the key Variants for the frames that follow.</summary>
    private void ReadFadesAfresh(Godot.Collections.Dictionary fades)
    {
        ClearFadeKeys();
        if (_fadeValues.Length < fades.Count) _fadeValues = new float[fades.Count * 2];
        int i = 0;
        foreach (var kv in fades)
        {
            var e = new FadeKey { Key = kv.Key };
            e.Valid = TileKey.TryParse(kv.Key.AsString(), out e.Packed, out e.Layer);
            _fadeKeys.Add(e);
            _fadeValues[i++] = (float)kv.Value.AsDouble();
        }
    }

    private void ClearFadeKeys()
    {
        for (int i = 0; i < _fadeKeys.Count; i++) _fadeKeys[i].Key.Dispose();
        _fadeKeys.Clear();
    }

    /// <summary>One poll_fades entry: the tile's fade value (a tile listed but not uploaded yet is picked up on the call after its upload).</summary>
    private void ApplyFade(in FadeKey e, float value)
    {
        if (!e.Valid) return;
        if (e.Layer == TileLayer.Landmarks) { LandmarkKeysSeen++; LandmarkFadeKeysSeen++; return; } // contract 9b: never listed; a stray key is counted (CoverSelfTest) and ignored
        bool terrainKey = e.Layer == TileLayer.Terrain;
        if (terrainKey) TerrainKeysSeen++;
        var cover = terrainKey ? TerrainCover : Cover;
        if (!cover.TryGet(e.Packed, out var nodes)) return;
        nodes.FadeStamp = _fadeFrame;
        float f = Mathf.Clamp(value, 0f, 1f);
        if (f >= 1f)
        {
            // Fully shown: no dithering. But an entry still makes the tile visible whatever its siblings and children are (contract 5, "Visibility
            // rule"): the outgoing parent of a refine swap reads exactly 1.0 until the camera has moved by half a fade step, while its 4 children are
            // uploaded and read 0.0. Ending the transition here hid the parent behind its children (rule: "all 4 children present") for those frames,
            // up to fades.cap_s = 0.75 s: a whole tile blank, the terrain showing through. The entry is dropped on the first call without it.
            if (nodes.FadeMaterialActive) { if (nodes is TerrainNodes tn1) _terrainPool!.EndFade(tn1); else _pool!.EndFade(nodes); }
            if (!nodes.Fading) _fading.Add(nodes);
            cover.SetFade(nodes, true, 1f);
            return;
        }
        if (!nodes.Fading) _fading.Add(nodes);
        if (!nodes.Fading || nodes.Fade != f || !nodes.FadeMaterialActive)
        {
            if (nodes is TerrainNodes tn) _terrainPool!.SetFade(tn, f);
            else _pool!.SetFade(nodes, f);
        }
        cover.SetFade(nodes, true, f);
    }

    private TileCover CoverOf(TileNodes nodes) => nodes is TerrainNodes ? TerrainCover : Cover;

    private void EndTileFade(TileNodes nodes)
    {
        if (nodes.Fading || nodes.FadeMaterialActive)
        {
            if (nodes is TerrainNodes tn) _terrainPool!.EndFade(tn);
            else _pool!.EndFade(nodes);
            CoverOf(nodes).SetFade(nodes, false, 1f);
        }
        _fading.Remove(nodes);
    }
}
