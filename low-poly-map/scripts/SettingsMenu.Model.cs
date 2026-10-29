using Godot;

namespace LowPolyMap;

// The panel's model: the setters the widgets and the self-test call, the profile read, apply and the revert on a rejected profile.
public partial class SettingsMenu
{
    /// <summary>Camera speed in m/s (live; no cap unless the host set <see cref="FreeFlyCamera.MaxSpeed"/>).</summary>
    public void SetSpeed(double v)
    {
        if (Camera == null) return;
        Camera.Speed = (float)System.Math.Clamp(v, 0.0, FreeFlyCamera.MaxSpeed);
    }

    /// <summary>bands.max_distance_m in metres (50..100000): the outer limit of the cover. Marks the panel dirty (auto-apply starts its timer).</summary>
    public void SetMaxDistance(double metres)
    {
        if (!HasMaxDistance) return;
        metres = System.Math.Clamp(metres, MaxDistMinM, MaxDistMaxM);
        if (_maxDist == metres) return;
        _maxDist = metres;
        SyncWidgets();
        MarkDirty();
    }

    /// <summary>The user-facing detail (0.25..4, right = more detail): sets the quality scale 1 / <paramref name="detail"/>.</summary>
    public void SetDetail(double detail)
    {
        if (!(detail > 0.0) || double.IsInfinity(detail)) return;
        SetQuality(1.0 / detail);
    }

    /// <summary>set_lod_quality, live (no restart, no teleport): the pixel error threshold multiplier (the raw library scale, 2 = coarser), clamped to 0.25..4; debounced 0.3 s while the slider moves.</summary>
    public void SetQuality(double scale)
    {
        if (!HasLodQuality) return;
        scale = System.Math.Clamp(scale, QualityMin, QualityMax);
        if (_quality == scale) return;
        _quality = scale;
        _qualityDirty = true;
        _qualityTimer = 0.3;
        SyncWidgets();
    }

    /// <summary>Sends the panel's quality to the library now (the debounce skipped); returns the scale in effect.</summary>
    public double ApplyQualityNow()
    {
        _qualityDirty = false;
        if (Bridge == null || !HasLodQuality) return double.NaN;
        double eff = Bridge.SetLodQuality(_quality);
        if (!double.IsNaN(eff)) _quality = eff;
        SyncWidgets();
        return eff;
    }

    /// <summary>Hide buildings lower than <paramref name="metres"/> (0..100) in ring 0..2.</summary>
    public void SetMinHeight(int ring, double metres)
    {
        if (!HasMinHeight) return;
        metres = System.Math.Clamp(metres, 0.0, MinHeightMax);
        if (_minH[ring] == metres) return;
        _minH[ring] = metres;
        SyncWidgets();
        MarkDirty();
    }

    /// <summary>Finest band (0..3) that gets light sprites.</summary>
    public void SetLightMinRing(int ring)
    {
        if (!HasLightMinRing) return;
        ring = System.Math.Clamp(ring, 0, 3);
        if (_lightRing == ring) return;
        _lightRing = ring;
        SyncWidgets();
        MarkDirty();
    }

    public void ResetLights() => SetLightMinRing(LightRingDefault);

    /// <summary>Terrain layer on / off; applied with the world restart (Apply). Needs a terrain-capable library.</summary>
    public void SetTerrainOn(bool on)
    {
        if (!HasTerrain || _terrainOn == on) return;
        _terrainOn = on;
        SyncWidgets();
        MarkDirty();
    }

    /// <summary>Hills amplitude in metres (profile terrain.amplitude_m, 0..3000); applied with the world restart.</summary>
    public void SetTerrainAmplitude(double metres)
    {
        if (!HasTerrain) return;
        metres = System.Math.Clamp(metres, 0.0, TerrainAmpMax);
        if (_terrainAmp == metres) return;
        _terrainAmp = metres;
        SyncWidgets();
        MarkDirty();
    }

    /// <summary>The user-facing terrain detail (1 / detail_scale_max..4, right = more detail, above 1 expensive): sets the terrain scale 1 / <paramref name="detail"/>.</summary>
    public void SetTerrainDetailLevel(double detail)
    {
        if (!(detail > 0.0) || double.IsInfinity(detail)) return;
        SetTerrainDetail(1.0 / detail);
    }

    /// <summary>set_terrain_detail, live (no restart): the raw library scale of the terrain cover's error threshold, clamped to 0.25..detail_scale_max; above 1 is coarser, below 1 finer and expensive. The cover follows through ordinary swaps; debounced 0.3 s while the slider moves.</summary>
    public void SetTerrainDetail(double scale)
    {
        if (!HasTerrain) return;
        scale = System.Math.Clamp(scale, TerrainDetailMin, _terrainDetailMax);
        if (_terrainDetail == scale) return;
        _terrainDetail = scale;
        _detailDirty = true;
        _detailTimer = 0.3;
        SyncWidgets();
    }

    public void ResetTerrain()
    {
        if (!HasTerrain) return;
        _terrainOn = false;
        _terrainAmp = TerrainAmpDefault;
        _terrainDetail = 1.0;
        _detailDirty = true;
        _detailTimer = 0.0;
        SyncWidgets();
        MarkDirty();
    }

    public void ResetRange()
    {
        if (HasMaxDistance) _maxDist = MaxDistDefaultM;
        if (HasLodQuality) { _quality = 1.0; _qualityDirty = true; _qualityTimer = 0.0; }
        SyncWidgets();
        if (HasMaxDistance) MarkDirty();
    }

    public void ResetFilter()
    {
        if (!HasMinHeight) return;
        System.Array.Clear(_minH);
        SyncWidgets();
        MarkDirty();
    }

    private void MarkDirty()
    {
        _dirty = true;
        _dirtyTimer = AutoApplyDelay;
        RefreshStatus();
    }

    /// <summary>Reads the effective values (what the library runs with, incl. launch args) into the model and the applied copies.</summary>
    private void LoadFromProfile()
    {
        var profile = Bridge?.Streamer.WorldProfile();
        Bridge?.ReadLod();
        HasLodQuality = Bridge != null && Bridge.Streamer.IsAvailable && Bridge.Streamer.HasLodQuality;
        _quality = HasLodQuality ? System.Math.Clamp(Bridge!.LodQuality, QualityMin, QualityMax) : 1.0;
        HasMaxDistance = false;
        _maxDist = MaxDistDefaultM;
        System.Array.Clear(_minH);
        HasMinHeight = false;
        HasLightMinRing = false;
        _lightRing = LightRingDefault;
        if (profile != null)
        {
            if (profile.TryGetValue("lights", out var lv) && lv.VariantType == Variant.Type.Dictionary && lv.AsGodotDictionary().TryGetValue("min_ring", out var mr))
            {
                HasLightMinRing = true;
                _lightRing = System.Math.Clamp((int)mr.AsInt64(), 0, 3);
            }
            if (profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary)
            {
                var b = bv.AsGodotDictionary();
                if (b.TryGetValue("max_distance_m", out var e))
                {
                    HasMaxDistance = true;
                    _maxDist = System.Math.Clamp(e.AsDouble(), MaxDistMinM, MaxDistMaxM);
                }
            }
            if (profile.TryGetValue("buildings", out var bd) && bd.VariantType == Variant.Type.Dictionary)
            {
                var d = bd.AsGodotDictionary();
                HasMinHeight = true;
                for (int i = 0; i < 3; i++)
                    if (d.TryGetValue("r" + i, out var rv) && rv.VariantType == Variant.Type.Dictionary && rv.AsGodotDictionary().TryGetValue("min_height_m", out var mh))
                        _minH[i] = System.Math.Clamp(mh.AsDouble(), 0.0, MinHeightMax);
                    else HasMinHeight = false;
            }
        }
        HasTerrain = false;
        _terrainOn = Bridge?.TerrainActive ?? false;
        _terrainAmp = TerrainAmpDefault;
        _terrainDetailMax = 2.0;
        if (profile != null && Bridge != null && Bridge.Streamer.HasTerrainLayer
            && profile.TryGetValue("terrain", out var tv) && tv.VariantType == Variant.Type.Dictionary)
        {
            var t = tv.AsGodotDictionary();
            HasTerrain = true;
            if (t.TryGetValue("amplitude_m", out var amp)) _terrainAmp = System.Math.Clamp(amp.AsDouble(), 0.0, TerrainAmpMax);
            if (t.TryGetValue("detail_scale_max", out var dm)) _terrainDetailMax = System.Math.Max(dm.AsDouble(), 1.0);
            _terrainDetail = System.Math.Clamp(Bridge.TerrainDetail, TerrainDetailMin, _terrainDetailMax);
        }
        _appliedTerrainOn = _terrainOn;
        _appliedTerrainAmp = _terrainAmp;
        _appliedMaxDist = _maxDist;
        System.Array.Copy(_minH, _appliedMinH, 3);
        _appliedLightRing = _lightRing;
        _effective = Bridge != null && Bridge.Streamer.IsAvailable ? Bridge.EffectiveProfileText() : "no world";
    }

    /// <summary>Queues a world restart with the panel's maximum distance and minimum heights (see <see cref="WorldView.ApplyProfileOverrides"/>).</summary>
    public void Apply()
    {
        if (Bridge == null) return;
        var o = new Godot.Collections.Dictionary();
        if (HasMaxDistance) o["bands"] = new Godot.Collections.Dictionary { ["max_distance_m"] = _maxDist };
        if (HasMinHeight)
        {
            var b = new Godot.Collections.Dictionary();
            for (int i = 0; i < 3; i++) b["r" + i] = new Godot.Collections.Dictionary { ["min_height_m"] = _minH[i] };
            o["buildings"] = b;
        }
        if (HasLightMinRing) o["lights"] = new Godot.Collections.Dictionary { ["min_ring"] = _lightRing };
        if (HasTerrain) o["terrain"] = new Godot.Collections.Dictionary { ["enabled"] = _terrainOn, ["amplitude_m"] = _terrainAmp };
        _dirty = false;
        ApplyInFlight = true;
        // The terrain layer flag travels with the request: the view sends / drops the layers option and keeps its old flags if the profile is rejected.
        Bridge.ApplyProfileOverrides(o, HasTerrain ? _terrainOn : null);
        RefreshStatus();
    }

    private void OnReconfigured(bool ok, string error)
    {
        ApplyInFlight = false;
        LastApplyOk = ok;
        LastError = error;
        if (ok)
        {
            // Show what the library actually runs with (it may normalise values), not what was asked for.
            LoadFromProfile();
            _error.Visible = false;
        }
        else
        {
            // Rejected: the previous world still runs; go back to its values.
            _maxDist = _appliedMaxDist;
            System.Array.Copy(_appliedMinH, _minH, 3);
            _lightRing = _appliedLightRing;
            _terrainOn = _appliedTerrainOn;
            _terrainAmp = _appliedTerrainAmp;
            _dirty = false;
            _error.Text = "REJECTED: " + error;
            _error.Visible = true;
        }
        SyncWidgets();
        RefreshStatus();
    }

    private void RefreshStatus()
    {
        if (_status == null) return;
        string state = ApplyInFlight ? "applying..." : _dirty ? (AutoApply ? $"changed, applies in {System.Math.Max(_dirtyTimer, 0):0.0} s" : "changed, press Apply") : "applied";
        _status.Text = $"{state}  |  effective: {_effective}" + (Bridge != null && Bridge.ReconfigureCount > 0 ? $"  (#{Bridge.ReconfigureCount})" : "");
    }
}
