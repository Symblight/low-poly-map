using System.Collections.Generic;
using System.Threading.Tasks;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// F3-toggled settings panel (bottom-left; hidden at startup, launch arg <c>--settings-open</c> shows it): flight speed
/// (live, two-way with <see cref="FreeFlyCamera.Speed"/>), the level of detail (api 1.10: a live "Detail" slider = set_lod_quality(1 / detail), right = more detail, a restart-type
/// "Max distance" = bands.max_distance_m and a read-only readout of the live refine distances; the fixed band edges are gone), the per-ring minimum building height and, with a terrain-capable library (api 1.5), the terrain layer (enable toggle and hills amplitude
/// apply with the world restart, the detail slider is live through set_terrain_detail without a teleport). Max distance and heights are profile values the library reads at configure time, so
/// they are applied by <see cref="WorldView.ApplyProfileOverrides"/> (world restart at the camera position): with the
/// auto-apply box on, 0.6 s after the last change, otherwise with the Apply button. A rejected profile keeps the previous world
/// running, shows the library's error in red and reverts the sliders. Controls never take keyboard focus (only the speed
/// number box does while it is being typed in), so flying keeps working. The logic (model, apply, revert) does not depend on the
/// panel being visible; <c>--settings-selftest</c> drives it without any input (see <see cref="RunSelfTest"/>).
/// </summary>
public partial class SettingsMenu : PanelContainer
{
    private const float Margin = 12f;
    private const float Width = 420f;
    private const double AutoApplyDelay = 0.6;
    private const double MaxDistMinM = 50.0;
    private const double MaxDistMaxM = 100000.0;
    private const double MaxDistDefaultM = 25000.0;
    private const double QualityMin = 0.25, QualityMax = 4.0;
    private const double SpeedMin = 1.0;
    private const double SpeedMax = 2000.0;
    private const double SpeedDefault = 80.0;
    private const double MinHeightMax = 100.0;
    private const string Attribution = "© OpenStreetMap contributors";


    [Export] public WorldView? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }

    /// <summary>Apply range / filter changes 0.6 s after the last change (default on); off = Apply button only.</summary>
    public bool AutoApply { get; set; } = true;
    /// <summary>bands.max_distance_m in metres (outer limit of the cover) as shown on the panel (may be ahead of <see cref="AppliedMaxDistance"/>) and as applied.</summary>
    public double MaxDistance => _maxDist;
    public double AppliedMaxDistance => _appliedMaxDist;
    /// <summary>set_lod_quality scale as shown on the panel (live; the library clamps to 0.25..4; 2 = coarser) and whether the library has the method (api 1.10).</summary>
    public double LodQuality => _quality;
    /// <summary>The user-facing "Detail" of the vector LOD = 1 / <see cref="LodQuality"/> (the slider's right = more detail).</summary>
    public double Detail => 1.0 / _quality;
    public bool HasLodQuality { get; private set; }
    /// <summary>True when the profile has bands.max_distance_m (profile_version 7).</summary>
    public bool HasMaxDistance { get; private set; }
    public IReadOnlyList<double> MinHeights => _minH;
    public IReadOnlyList<double> AppliedMinHeights => _appliedMinH;
    /// <summary>True from <see cref="Apply"/> until the bridge reports the result.</summary>
    public bool ApplyInFlight { get; private set; }
    public bool LastApplyOk { get; private set; } = true;
    public string LastError { get; private set; } = "";
    /// <summary>True when the library's profile has the buildings.rN.min_height_m keys (profile_version 2).</summary>
    public bool HasMinHeight { get; private set; }
    /// <summary>Finest band that carries light sprites as shown on the panel (0..3; library default 1) and as applied.</summary>
    public int LightMinRing => _lightRing;
    public int AppliedLightMinRing => _appliedLightRing;
    /// <summary>True when the library's profile has lights.min_ring (profile_version 3).</summary>
    public bool HasLightMinRing { get; private set; }

    /// <summary>True when the library has the terrain layer (api 1.5) and its profile has the terrain section.</summary>
    public bool HasTerrain { get; private set; }
    /// <summary>Terrain layer switch and hills amplitude (m) as shown on the panel and as applied.</summary>
    public bool TerrainOn => _terrainOn;
    public bool AppliedTerrainOn => _appliedTerrainOn;
    public double TerrainAmplitude => _terrainAmp;
    public double AppliedTerrainAmplitude => _appliedTerrainAmp;
    /// <summary>set_terrain_detail scale as shown on the panel (live; the raw library scale, above 1 = coarser).</summary>
    public double TerrainDetailScale => _terrainDetail;
    /// <summary>The user-facing "Terrain detail" = 1 / <see cref="TerrainDetailScale"/> (the slider's right = more detail, above 1 is expensive).</summary>
    public double TerrainDetailLevel => 1.0 / _terrainDetail;

    private const double TerrainAmpMax = 3000.0, TerrainAmpDefault = 160.0, TerrainDetailMin = 0.25;
    private bool _terrainOn, _appliedTerrainOn;
    private double _terrainAmp = TerrainAmpDefault, _appliedTerrainAmp = TerrainAmpDefault;
    private double _terrainDetail = 1.0, _terrainDetailMax = 2.0;
    private bool _detailDirty;
    private double _detailTimer;
    private CheckBox _terrainCheck = null!;
    private HSlider _ampSlider = null!, _detailSlider = null!;
    private SpinBox _ampBox = null!, _detailBox = null!;
    private double _maxDist = MaxDistDefaultM, _appliedMaxDist = MaxDistDefaultM;
    private double _quality = 1.0;
    private bool _qualityDirty;
    private double _qualityTimer;
    private Label _lodLabel = null!, _terrainLabel = null!;
    private readonly double[] _minH = new double[3];
    private readonly double[] _appliedMinH = new double[3];
    private const int LightRingDefault = 2;
    private int _lightRing = LightRingDefault, _appliedLightRing = LightRingDefault;
    private HSlider _ringSlider = null!;
    private SpinBox _ringBox = null!;
    private readonly StreamerStats _stats = new();

    private HSlider _qualitySlider = null!, _maxSlider = null!;
    private SpinBox _qualityBox = null!, _maxBox = null!;
    private readonly HSlider[] _minSlider = new HSlider[3];
    private readonly SpinBox[] _minBox = new SpinBox[3];
    private HSlider _speedSlider = null!;
    private SpinBox _speedBox = null!;
    private CheckBox _auto = null!;
    private Label _status = null!, _error = null!, _tiles = null!;
    private bool _syncing;
    private bool _speedDrag;
    private bool _dirty;
    private double _dirtyTimer;
    private double _statsTimer;
    private string _effective = "";

    public override void _Ready()
    {
        Visible = System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--settings-open");
        // Bottom-left; the daylight menu sits bottom-right and the minimap top-right (the F1 text block fills the left column and is overdrawn).
        AnchorLeft = 0f; AnchorRight = 0f; AnchorTop = 1f; AnchorBottom = 1f;
        GrowHorizontal = GrowDirection.End; GrowVertical = GrowDirection.Begin;
        OffsetLeft = Margin; OffsetRight = Margin + Width; OffsetBottom = -Margin; OffsetTop = -Margin;
        CustomMinimumSize = new Vector2(Width, 0);
        AddThemeStyleboxOverride("panel", new StyleBoxFlat
        {
            BgColor = new Color(0f, 0f, 0f, 0.9f), // nearly opaque: the F1 text block runs underneath this panel
            ContentMarginLeft = 10, ContentMarginRight = 10, ContentMarginTop = 8, ContentMarginBottom = 8,
            CornerRadiusTopLeft = 4, CornerRadiusTopRight = 4, CornerRadiusBottomLeft = 4, CornerRadiusBottomRight = 4,
        });
        var theme = new Theme();
        foreach (var type in new[] { "Label", "Button", "CheckBox", "LineEdit" }) theme.SetFontSize("font_size", type, 13);
        Theme = theme;

        var col = new VBoxContainer();
        col.AddThemeConstantOverride("separation", 3);
        AddChild(col);
        col.AddChild(new Label { Text = "Settings  [F3]" });

        // -- movement (live)
        AddHeading(col, "Movement", () => SetSpeed(SpeedDefault));
        (_speedSlider, _speedBox) = AddRow(col, "Speed", System.Math.Log10(SpeedMin), System.Math.Log10(SpeedMax), 0.001, 0, 1e6, 0.1, " m/s");
        _speedSlider.ValueChanged += v => { if (!_syncing) SetSpeed(SliderToSpeed(v)); };
        _speedSlider.DragStarted += () => _speedDrag = true;
        _speedSlider.DragEnded += _ => _speedDrag = false;
        _speedBox.ValueChanged += v => { if (!_syncing) SetSpeed(v); };

        // -- level of detail: screen-space error (api 1.10). Quality is live, the maximum distance restarts the world.
        AddHeading(col, "Level of detail (screen-space error)", ResetRange);
        // Right = more detail. The slider value d is the user-facing detail; the library gets scale = 1 / d (set_lod_quality, which multiplies tau).
        (_qualitySlider, _qualityBox) = AddRow(col, "Detail", System.Math.Log10(1.0 / QualityMax), System.Math.Log10(1.0 / QualityMin), 0.001, 1.0 / QualityMax, 1.0 / QualityMin, 0.01, "x");
        _qualityBox.TooltipText = "Level of detail of the buildings and ground: set_lod_quality(1 / detail). 1 = profile; 2 doubles every refine distance (finer, more tiles), 0.5 halves them (coarser, fewer tiles); live, no restart";
        _qualitySlider.ValueChanged += v => { if (!_syncing) SetDetail(System.Math.Round(System.Math.Pow(10, v), 2)); };
        _qualityBox.ValueChanged += v => { if (!_syncing) SetDetail(v); };
        (_maxSlider, _maxBox) = AddRow(col, "Max distance", System.Math.Log10(MaxDistMinM), System.Math.Log10(MaxDistMaxM), 0.001, MaxDistMinM, MaxDistMaxM, 1, " m");
        _maxBox.TooltipText = "bands.max_distance_m: how far the cover reaches (R3 edge); applies with the world restart";
        _maxSlider.ValueChanged += v => { if (!_syncing) SetMaxDistance(RoundDistance(System.Math.Pow(10, v))); };
        _maxBox.ValueChanged += v => { if (!_syncing) SetMaxDistance(v); };
        _lodLabel = new Label { AutowrapMode = TextServer.AutowrapMode.WordSmart, CustomMinimumSize = new Vector2(Width - 20, 0), Modulate = new Color(1f, 1f, 1f, 0.75f) };
        col.AddChild(_lodLabel);

        // -- building filter
        AddHeading(col, "Hide buildings lower than", ResetFilter);
        for (int i = 0; i < 3; i++)
        {
            int ring = i;
            (_minSlider[i], _minBox[i]) = AddRow(col, $"R{i} min height", 0, MinHeightMax, 0.5, 0, MinHeightMax, 0.5, " m");
            _minSlider[i].ValueChanged += v => { if (!_syncing) SetMinHeight(ring, v); };
            _minBox[i].ValueChanged += v => { if (!_syncing) SetMinHeight(ring, v); };
        }

        // -- light sprites: finest band that carries them (windows and sprites cross-fade over the WorldRenderSettings blend range)
        AddHeading(col, "Light sprites", ResetLights);
        (_ringSlider, _ringBox) = AddRow(col, "Lights from band", 0, 3, 1, 0, 3, 1, "");
        _ringBox.Prefix = "R";
        _ringBox.TooltipText = "Finest band with light sprites: R3 = only far away, R2 = R2/R3 (R1 windows then fade to nothing), R1 = default (sprites fade in over the windows between 0.9 and 2 km), R0 = everywhere";
        _ringSlider.ValueChanged += v => { if (!_syncing) SetLightMinRing((int)v); };
        _ringBox.ValueChanged += v => { if (!_syncing) SetLightMinRing((int)v); };

        // -- terrain (api 1.5): layer switch + hills amplitude restart the world like the ranges; detail is live
        AddHeading(col, "Terrain (procedural hills)", ResetTerrain);
        _terrainCheck = new CheckBox { Text = "terrain layer (restarts the world)", FocusMode = FocusModeEnum.None, TooltipText = "Draw the procedural terrain tiles (layers: terrain + profile terrain.enabled)" };
        _terrainCheck.Toggled += on => { if (!_syncing) SetTerrainOn(on); };
        col.AddChild(_terrainCheck);
        (_ampSlider, _ampBox) = AddRow(col, "Hills amplitude", 0, TerrainAmpMax, 10, 0, TerrainAmpMax, 10, " m");
        _ampSlider.ValueChanged += v => { if (!_syncing) SetTerrainAmplitude(v); };
        _ampBox.ValueChanged += v => { if (!_syncing) SetTerrainAmplitude(v); };
        // set_terrain_detail is a scale of the terrain cover's screen-space-error threshold (tau = terrain.tau_px x scale, api 1.10), above 1 = coarser. The slider
        // shows the user-facing detail d = 1 / scale (right = more detail, like the vector row above); the range follows the library clamp: scale 0.25..detail_scale_max.
        (_detailSlider, _detailBox) = AddRow(col, "Terrain detail", System.Math.Log10(1.0 / 2.0), System.Math.Log10(1.0 / TerrainDetailMin), 0.001, 1.0 / 2.0, 1.0 / TerrainDetailMin, 0.01, "x");
        _detailBox.TooltipText = "Terrain level of detail: set_terrain_detail(1 / detail) (tau = terrain.tau_px x scale). 1 = profile; below 1 = coarser (fewer, larger tiles, cheaper); "
            + "ABOVE 1 IS EXPENSIVE: 2 roughly doubles the terrain tiles, 4 (the maximum, scale 0.25) is about 2300 tiles and can stall the streaming and the draw call budget. Live, no restart; the cover changes through ordinary swaps";
        _detailSlider.ValueChanged += v => { if (!_syncing) SetTerrainDetailLevel(System.Math.Round(System.Math.Pow(10, v), 2)); };
        _detailBox.ValueChanged += v => { if (!_syncing) SetTerrainDetailLevel(v); };
        _terrainLabel = new Label { AutowrapMode = TextServer.AutowrapMode.WordSmart, CustomMinimumSize = new Vector2(Width - 20, 0), Modulate = new Color(1f, 1f, 1f, 0.75f) };
        col.AddChild(_terrainLabel);

        // -- apply
        var applyRow = new HBoxContainer();
        var apply = new Button { Text = "Apply", FocusMode = FocusModeEnum.None, TooltipText = "Restart the world at the camera position with these ranges" };
        apply.Pressed += Apply;
        applyRow.AddChild(apply);
        _auto = new CheckBox { Text = "auto-apply 0.6 s after last change", FocusMode = FocusModeEnum.None, ButtonPressed = AutoApply };
        _auto.Toggled += on => AutoApply = on;
        applyRow.AddChild(_auto);
        col.AddChild(applyRow);

        _status = new Label { AutowrapMode = TextServer.AutowrapMode.WordSmart, CustomMinimumSize = new Vector2(Width - 20, 0) };
        col.AddChild(_status);
        _tiles = new Label();
        col.AddChild(_tiles);
        _error = new Label { AutowrapMode = TextServer.AutowrapMode.WordSmart, CustomMinimumSize = new Vector2(Width - 20, 0), Visible = false };
        _error.AddThemeColorOverride("font_color", new Color(1f, 0.3f, 0.25f));
        col.AddChild(_error);
        var attribution = new Label { Text = Attribution, Modulate = new Color(1f, 1f, 1f, 0.6f) };
        attribution.AddThemeFontSizeOverride("font_size", 11);
        col.AddChild(attribution);

        LoadFromProfile();
        SyncWidgets();
        if (Bridge != null) Bridge.Reconfigured += OnReconfigured;
        if (!HasMinHeight)
            for (int i = 0; i < 3; i++) { _minSlider[i].Editable = false; _minBox[i].Editable = false; }
        _ringSlider.Editable = _ringBox.Editable = HasLightMinRing;
        _qualitySlider.Editable = _qualityBox.Editable = HasLodQuality;
        _maxSlider.Editable = _maxBox.Editable = HasMaxDistance;
        _terrainCheck.Disabled = !HasTerrain;
        _ampSlider.Editable = _ampBox.Editable = HasTerrain;
        _detailSlider.Editable = _detailBox.Editable = HasTerrain && _appliedTerrainOn;
        RefreshStatus();

        if (System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--settings-selftest")) RunSelfTest();
    }

    public override void _ExitTree()
    {
        if (Bridge != null) Bridge.Reconfigured -= OnReconfigured;
    }

    // ---- widgets ----------------------------------------------------------------------------

    private static void AddHeading(VBoxContainer col, string text, System.Action reset)
    {
        var row = new HBoxContainer();
        var l = new Label { Text = text, SizeFlagsHorizontal = SizeFlags.ExpandFill, Modulate = new Color(0.75f, 0.85f, 1f) };
        row.AddChild(l);
        var b = new Button { Text = "Reset", FocusMode = FocusModeEnum.None, TooltipText = "Back to the library defaults" };
        b.Pressed += reset;
        row.AddChild(b);
        col.AddChild(row);
    }

    private static (HSlider, SpinBox) AddRow(VBoxContainer col, string name, double sMin, double sMax, double sStep, double bMin, double bMax, double bStep, string suffix)
    {
        var row = new HBoxContainer();
        row.AddChild(new Label { Text = name, CustomMinimumSize = new Vector2(132, 0) });
        var slider = new HSlider
        {
            MinValue = sMin, MaxValue = sMax, Step = sStep, FocusMode = FocusModeEnum.None, Scrollable = false,
            SizeFlagsHorizontal = SizeFlags.ExpandFill, SizeFlagsVertical = SizeFlags.ShrinkCenter,
        };
        row.AddChild(slider);
        var box = new SpinBox
        {
            MinValue = bMin, MaxValue = bMax, Step = bStep, Suffix = suffix, CustomMinimumSize = new Vector2(112, 0),
            UpdateOnTextChanged = false, SelectAllOnFocus = true,
        };
        var edit = box.GetLineEdit();
        edit.FocusMode = FocusModeEnum.Click; // typing needs focus; released again on Enter or on a click elsewhere
        edit.TextSubmitted += _ => edit.ReleaseFocus();
        row.AddChild(box);
        col.AddChild(row);
        return (slider, box);
    }

    private static double SliderToSpeed(double v)
    {
        double s = System.Math.Pow(10, v);
        return s < 10 ? System.Math.Round(s * 10) / 10 : System.Math.Round(s);
    }

    private static double RoundDistance(double m) => m < 1000 ? System.Math.Round(m / 5) * 5 : System.Math.Round(m / 10) * 10;

    /// <summary>Pushes the model into every slider and number box without raising their change signals.</summary>
    private void SyncWidgets()
    {
        _syncing = true;
        _qualitySlider.SetValueNoSignal(-System.Math.Log10(_quality));
        _qualityBox.SetValueNoSignal(System.Math.Round(1.0 / _quality, 3));
        _maxSlider.SetValueNoSignal(System.Math.Log10(_maxDist));
        _maxBox.SetValueNoSignal(_maxDist);
        for (int i = 0; i < 3; i++)
        {
            _minSlider[i].SetValueNoSignal(_minH[i]);
            _minBox[i].SetValueNoSignal(_minH[i]);
        }
        _ringSlider.SetValueNoSignal(_lightRing);
        _ringBox.SetValueNoSignal(_lightRing);
        _terrainCheck.SetPressedNoSignal(_terrainOn);
        _ampSlider.SetValueNoSignal(_terrainAmp);
        _ampBox.SetValueNoSignal(_terrainAmp);
        _detailSlider.MinValue = -System.Math.Log10(_terrainDetailMax);
        _detailSlider.SetValueNoSignal(-System.Math.Log10(System.Math.Clamp(_terrainDetail, TerrainDetailMin, _terrainDetailMax)));
        _detailBox.MinValue = 1.0 / _terrainDetailMax;
        _detailBox.SetValueNoSignal(System.Math.Round(1.0 / _terrainDetail, 3));
        _syncing = false;
    }

    public override void _Input(InputEvent @event)
    {
        // A click anywhere outside the panel ends typing in a number box (otherwise it keeps the keyboard focus).
        if (@event is InputEventMouseButton { Pressed: true } mb && GetViewport().GuiGetFocusOwner() is { } owner
            && IsAncestorOf(owner) && !GetGlobalRect().HasPoint(mb.Position))
            owner.ReleaseFocus();
    }

    public override void _UnhandledInput(InputEvent @event)
    {
        if (@event is InputEventKey { Pressed: true, Echo: false, PhysicalKeycode: Key.F3 })
        {
            Visible = !Visible;
            if (!Visible && GetViewport().GuiGetFocusOwner() is { } owner && IsAncestorOf(owner)) owner.ReleaseFocus();
            GetViewport().SetInputAsHandled();
        }
    }

    // ---- model --------------------------------------------------------------------------------

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

    // ---- apply --------------------------------------------------------------------------------

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

    public override void _Process(double delta)
    {
        if (_qualityDirty && Bridge != null)
        {
            _qualityTimer -= delta;
            if (_qualityTimer <= 0) ApplyQualityNow();
        }
        if (_detailDirty && Bridge != null)
        {
            _detailTimer -= delta;
            if (_detailTimer <= 0)
            {
                _detailDirty = false;
                if (Bridge.TerrainActive)
                {
                    double eff = Bridge.SetTerrainDetail(_terrainDetail);
                    if (!double.IsNaN(eff)) _terrainDetail = eff; // the library clamps (and ignores non-finite values)
                    SyncWidgets();
                }
            }
        }
        if (_dirty && AutoApply && !ApplyInFlight)
        {
            _dirtyTimer -= delta;
            if (_dirtyTimer <= 0) Apply();
        }
        if (!Visible) return;

        // Speed mirrors the camera (the mouse wheel changes it too), except while the slider is dragged / the box is typed in.
        if (Camera != null)
        {
            _syncing = true;
            if (!_speedDrag) _speedSlider.SetValueNoSignal(System.Math.Log10(System.Math.Clamp(Camera.Speed, SpeedMin, SpeedMax)));
            if (!_speedBox.GetLineEdit().HasFocus()) _speedBox.SetValueNoSignal(System.Math.Round(Camera.Speed, 1));
            _syncing = false;
        }
        if (_dirty) RefreshStatus();
        if (Bridge != null && Bridge.HasLodStats)
        {
            var e = Bridge.LodEdgesM;
            _lodLabel.Text = $"live: detail x{1.0 / System.Math.Max(Bridge.LodQuality, 1e-6):0.##}, tau {Bridge.TauEff:0.##} px, fov {Bridge.LibraryViewFovYDeg:0.#} deg, H {Bridge.LibraryViewHeightPx} px\nrefine distances R0|R1|R2: {e[0]:0}|{e[1]:0}|{e[2]:0} m, max {e[3]:0} m";
        }

        if (_terrainLabel != null)
        {
            _terrainLabel.Visible = HasTerrain;
            if (HasTerrain)
                _terrainLabel.Text = Bridge != null && Bridge.TerrainActive
                    ? $"live: terrain detail x{1.0 / System.Math.Max(Bridge.TerrainDetail, 1e-6):0.##}" + (Bridge.TerrainTauPx > 0 ? $", tau {Bridge.TerrainTauEff:0.##} px (terrain.tau_px {Bridge.TerrainTauPx:0.##} x scale {Bridge.TerrainDetail:0.##})" : "")
                    : "terrain layer off";
        }

        _statsTimer -= delta;
        if (_statsTimer <= 0 && Bridge != null && Bridge.Streamer.DebugStats(_stats))
        {
            _statsTimer = 0.25;
            int q = 0, l = 0;
            for (int i = 0; i < StreamerStats.Rings; i++) { q += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i]; l += _stats.Loaded[i]; }
            string terrain = "";
            if (_stats.HasTerrain)
            {
                int tq = 0, tl = 0;
                for (int i = 0; i < _stats.TerrainRings; i++) { tq += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i]; tl += _stats.TerrainLoaded[i]; }
                terrain = $", terrain loaded {tl}, queued {tq}, {_stats.TerrainTris:N0} tris";
            }
            _tiles.Text = $"tiles loaded {l}, queued {q}{terrain}";
            _detailSlider.Editable = _detailBox.Editable = HasTerrain && Bridge.TerrainActive;
            RefreshStatus();
        }
    }

    // ---- self-test (--settings-selftest) --------------------------------------------------------------

    private async Task<bool> WaitFor(System.Func<bool> cond, double timeoutS)
    {
        ulong end = Time.GetTicksMsec() + (ulong)(timeoutS * 1000);
        while (!cond())
        {
            if (Time.GetTicksMsec() > end) return false;
            await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame);
        }
        return true;
    }

    private async Task Delay(double s) => await ToSignal(GetTree().CreateTimer(s), SceneTreeTimer.SignalName.Timeout);

    private string Snapshot()
    {
        var b = Bridge!;
        b.Streamer.DebugStats(_stats);
        string Ring(int[] a) => $"{a[0]}/{a[1]}/{a[2]}/{a[3]}";
        var busy = new int[4];
        for (int i = 0; i < 4; i++) busy[i] = _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
        string terrain = "";
        if (b.TerrainActive)
        {
            int tb = 0;
            for (int i = 0; i < _stats.TerrainRings; i++) tb += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i];
            terrain = $", terrain busy {tb}, library holds {TerrainLoadedTotal()}, host cover {b.TerrainCover.Loaded} (pool in use {b.TerrainPoolCreated - b.TerrainPoolFree})";
        }
        return $"queued+fetching+meshing R0..R3 {Ring(busy)}, loaded {Ring(_stats.Loaded)}, cover {b.Cover.Loaded}{(b.LandmarksActive ? $" + landmarks {b.LandmarkCover.Loaded}" : "")}, tile roots {b.TileRootCount} (in use {b.PoolCreated - b.PoolFree}){terrain}";
    }

    private int LoadedTotal() { int n = 0; foreach (var v in _stats.Loaded) n += v; return n; }
    private int TerrainLoadedTotal() { int n = 0; for (int i = 0; i < _stats.TerrainRings; i++) n += _stats.TerrainLoaded[i]; return n; }

    /// <summary>Idle = nothing queued, fetching or meshing for 1.5 s in a row and no reconfigure pending (finished tiles can still wait for their poll_ready).</summary>
    private async Task<bool> WaitIdle(double timeoutS)
    {
        ulong calmSince = 0;
        return await WaitFor(() =>
        {
            Bridge!.Streamer.DebugStats(_stats);
            int busy = 0;
            for (int i = 0; i < 4; i++) busy += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
            for (int i = 0; i < _stats.TerrainRings; i++) busy += _stats.TerrainQueued[i] + _stats.TerrainFetching[i] + _stats.TerrainMeshing[i];
            if (busy != 0 || Bridge.ReconfigurePending || ApplyInFlight) { calmSince = 0; return false; }
            if (calmSince == 0) calmSince = Time.GetTicksMsec();
            return Time.GetTicksMsec() - calmSince >= 1500;
        }, timeoutS);
    }

    private const int NodeGrowthSlack = 8;

    /// <summary>Number of alternating A/B applies of <c>--settings-selftest</c>: 11 by default, <c>--settings-selftest-applies=&lt;n&gt;</c> (4..500) for a leak run.</summary>
    private static int SelfTestApplies()
    {
        foreach (var a in OS.GetCmdlineUserArgs())
            if (a.StartsWith("--settings-selftest-applies=", System.StringComparison.Ordinal) && int.TryParse(a.Substring(28), out var n))
                return System.Math.Clamp(n, 4, 500);
        return 11;
    }

    private async void RunSelfTest()
    {
        var fails = new List<string>();
        try { await SelfTestBody(fails); }
        catch (System.Exception e) { fails.Add("exception: " + e.Message); }
        GD.Print(fails.Count == 0
            ? $"SETTINGS SELFTEST PASS: {1 + SelfTestApplies()} reconfigures (1 via widgets, {SelfTestApplies()} alternating), one rejected profile, no pool growth, origin kept, tiles released and reloaded each time, no scene node growth outside the pools"
            : "SETTINGS SELFTEST FAIL: " + string.Join("; ", fails));
        GetTree().Quit(fails.Count == 0 ? 0 : 1);
    }

    private async Task SelfTestBody(List<string> fails)
    {
        var bridge = Bridge;
        var cam = Camera;
        if (bridge == null || cam == null || !bridge.WorldRunning) { fails.Add("bridge/camera missing or world not running: " + bridge?.ConfigureError); return; }
        AutoApply = false;
        int atEventLoaded = -1;
        bridge.Reconfigured += (_, _) => atEventLoaded = bridge.TotalLoaded; // both layers

        if (!await WaitFor(() => bridge.ReadyTotal > 0, 90)) { fails.Add("no tile arrived within 90 s"); return; }
        await Delay(3.0);
        GD.Print($"[SelfTest] start: {Snapshot()}; effective {bridge.EffectiveProfileText()}");

        // Widget path first: the same signals a mouse or the keyboard would raise, with auto-apply on (0.6 s timer).
        AutoApply = true;
        int cw = bridge.ReconfigureCount;
        _speedSlider.Value = System.Math.Log10(500);
        if (System.Math.Abs(cam.Speed - 500) > 1) fails.Add($"speed slider -> camera speed {cam.Speed}, expected 500");
        _speedBox.Value = 2500; // beyond the slider's 2000 m/s end: no cap
        if (System.Math.Abs(cam.Speed - 2500) > 1) fails.Add($"speed box -> camera speed {cam.Speed}, expected 2500");
        cam.Speed = 123f; // the mouse wheel path: the panel must follow
        bool wasVisible = Visible;
        Visible = true;
        await Delay(0.3);
        if (System.Math.Abs(_speedBox.Value - 123) > 0.2 || System.Math.Abs(_speedSlider.Value - System.Math.Log10(123)) > 0.01) fails.Add($"panel did not follow the camera speed (box {_speedBox.Value}, slider {_speedSlider.Value})");
        Visible = wasVisible;
        cam.Speed = 40f;
        _maxBox.Value = 12000;
        _minSlider[1].Value = 4;
        await Delay(0.2);
        if (ApplyInFlight || bridge.ReconfigureCount != cw) fails.Add("auto-apply fired before its 0.6 s delay");
        if (!await WaitFor(() => bridge.ReconfigureCount == cw + 1 && !ApplyInFlight, 10)) { fails.Add("auto-apply did not run after the widget changes"); return; }
        if (System.Math.Abs(_appliedMaxDist - 12000) > 0.5 || System.Math.Abs(_appliedMinH[1] - 4) > 0.01) fails.Add($"widget apply: effective max distance {_appliedMaxDist}, R1 min {_appliedMinH[1]}");
        GD.Print($"[SelfTest] widgets: speed slider/box/wheel ok, auto-apply ran once; effective {_effective}");
        AutoApply = false;
        await WaitIdle(150);

        // Live quality (set_lod_quality through the slider): changes the refine distances without a reconfigure, and survives one.
        if (!HasLodQuality) fails.Add("library has no set_lod_quality (api < 1.10)");
        else
        {
            await Delay(0.8);
            bridge.ReadLod();
            double[] e0 = (double[])bridge.LodEdgesM.Clone();
            int rc0 = bridge.ReconfigureCount;
            _qualitySlider.Value = System.Math.Log10(0.5); // detail 0.5 = scale 2 = coarser
            await Delay(1.0);
            bridge.ReadLod();
            for (int i = 0; i < 3; i++)
                if (e0[i] <= 0 || System.Math.Abs(bridge.LodEdgesM[i] / e0[i] - 0.5) > 0.01) fails.Add($"quality x2 (slider): R{i} edge {e0[i]:0.#} -> {bridge.LodEdgesM[i]:0.#} m, expected half");
            if (System.Math.Abs(bridge.LodEdgesM[3] - e0[3]) > 0.5) fails.Add($"quality changed the max distance {e0[3]} -> {bridge.LodEdgesM[3]}");
            if (System.Math.Abs(bridge.LodQuality - 2.0) > 1e-9 || System.Math.Abs(bridge.TauEff - 8.0) > 0.01) fails.Add($"quality x2: library quality {bridge.LodQuality}, tau_eff {bridge.TauEff} (expected 2 and 8 px)");
            if (bridge.ReconfigureCount != rc0 || ApplyInFlight) fails.Add("the quality slider caused a reconfigure");
            GD.Print($"[SelfTest] quality x2 live: edges {e0[0]:0}/{e0[1]:0}/{e0[2]:0}/{e0[3]:0} -> {bridge.LodEdgesM[0]:0}/{bridge.LodEdgesM[1]:0}/{bridge.LodEdgesM[2]:0}/{bridge.LodEdgesM[3]:0} m, tau_eff {bridge.TauEff:0.##} px, no reconfigure");
            if (System.Math.Abs(Detail - 0.5) > 1e-9 || System.Math.Abs(_qualityBox.Value - 0.5) > 1e-6) fails.Add($"detail slider 0.5: panel detail {Detail}, box {_qualityBox.Value}");
            _qualityBox.Value = 1.0;
            await Delay(1.0);
            bridge.ReadLod();
            for (int i = 0; i < 3; i++)
                if (System.Math.Abs(bridge.LodEdgesM[i] / e0[i] - 1.0) > 0.01) fails.Add($"quality x1 (box): R{i} edge {bridge.LodEdgesM[i]:0.#} m, expected back to {e0[i]:0.#}");
            // Right = more detail: detail 2 (box) = scale 0.5 = every refine distance doubles (clamped to the max distance), tau_eff halves.
            _qualityBox.Value = 2.0;
            await Delay(1.0);
            bridge.ReadLod();
            if (System.Math.Abs(bridge.LodQuality - 0.5) > 1e-9 || System.Math.Abs(bridge.TauEff - 2.0) > 0.01) fails.Add($"detail 2 (box): library quality {bridge.LodQuality}, tau_eff {bridge.TauEff} (expected 0.5 and 2 px)");
            for (int i = 0; i < 3; i++)
                if (bridge.LodEdgesM[i] < e0[i] * 1.5 && bridge.LodEdgesM[i] < bridge.LodEdgesM[3] - 1) fails.Add($"detail 2 (box): R{i} edge {bridge.LodEdgesM[i]:0.#} m, expected about double {e0[i]:0.#}");
            GD.Print($"[SelfTest] detail 2 (finer) live: edges -> {bridge.LodEdgesM[0]:0}/{bridge.LodEdgesM[1]:0}/{bridge.LodEdgesM[2]:0}/{bridge.LodEdgesM[3]:0} m, tau_eff {bridge.TauEff:0.##} px");
            _qualitySlider.Value = 0.0;
            await Delay(1.0);
            bridge.ReadLod();
            if (System.Math.Abs(bridge.LodQuality - 1.0) > 1e-9) fails.Add($"detail 1 (slider): library quality {bridge.LodQuality}, expected 1");
            await WaitIdle(150);
        }

        // Config A: smaller ranges plus a height filter. Config B: the library defaults.
        double maxA = 6000, qualityA = 2.0, minA8 = 8;
        double maxB = MaxDistDefaultM, qualityB = 1.0;
        double[] minA = { 0, minA8, 0 }, minB = { 0, 0, 0 };
        const int ringA = 1, ringB = LightRingDefault;
        int nodesBase = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
        long loadedA = -1, loadedB = -1;
        int rootsWarm = 0;

        // Fly off the origin first so the re-centering is a real test (1.5 km east, 0.8 km north).
        cam.GlobalPosition = new Vector3(1500f, cam.GlobalPosition.Y, -800f);
        await Delay(0.5);

        int applies = SelfTestApplies();
        var nodeSeries = new List<int>();
        var otherSeries = new List<int>();
        for (int k = 0; k < applies; k++)
        {
            bool a = k % 2 == 0;
            var minH = a ? minA : minB;
            if (k > 0) cam.GlobalPosition += new Vector3(250f, 0f, -150f); // keep moving between applies
            string before = Snapshot();
            var geo0 = bridge.Streamer.LocalToGeodetic(cam.GlobalPosition);
            float y0 = cam.GlobalPosition.Y;
            int count0 = bridge.ReconfigureCount;

            SetMaxDistance(a ? maxA : maxB);
            SetQuality(a ? qualityA : qualityB);
            ApplyQualityNow(); // live, before the restart: the library keeps the scale across configure_world
            for (int i = 0; i < 3; i++) SetMinHeight(i, minH[i]);
            int lightRing = a ? ringA : ringB;
            SetLightMinRing(lightRing);
            Apply();
            if (!await WaitFor(() => !ApplyInFlight, 30)) { fails.Add($"apply {k}: bridge never answered"); return; }
            if (!LastApplyOk || bridge.ReconfigureCount != count0 + 1) { fails.Add($"apply {k}: rejected: {LastError}"); return; }
            if (atEventLoaded != 0) fails.Add($"apply {k}: {atEventLoaded} tiles still held when the reconfigure finished");
            var p = cam.GlobalPosition;
            if (System.Math.Abs(p.X) > 0.01f || System.Math.Abs(p.Z) > 0.01f || System.Math.Abs(p.Y - y0) > 0.01f) fails.Add($"apply {k}: camera at {p}, expected (0, {y0}, 0)");
            var geo1 = bridge.Streamer.LocalToGeodetic(p);
            if (System.Math.Abs(geo1.X - geo0.X) > 3e-5 || System.Math.Abs(geo1.Y - geo0.Y) > 3e-5) fails.Add($"apply {k}: geodetic position jumped from {geo0} to {geo1}");

            if (!await WaitIdle(150)) { fails.Add($"apply {k}: queues not idle after 150 s: {Snapshot()}"); return; }
            string after = Snapshot();
            GD.Print($"[SelfTest] apply {k} ({(a ? "A" : "B")}): origin moved to camera, tiles held at finish {atEventLoaded}\n  before: {before}\n  after:  {after}");

            // Applied values: what the library reports must be what was sent.
            double wantMax = a ? maxA : maxB, wantQ = a ? qualityA : qualityB;
            if (System.Math.Abs(_appliedMaxDist - wantMax) > 0.5) fails.Add($"apply {k}: effective max distance {_appliedMaxDist}, sent {wantMax}");
            bridge.ReadLod();
            if (System.Math.Abs(bridge.LodQuality - wantQ) > 1e-9) fails.Add($"apply {k}: library LOD quality {bridge.LodQuality} after the restart, expected {wantQ} (kept across configure)");
            if (System.Math.Abs(bridge.LodEdgesM[3] - wantMax) > 0.5) fails.Add($"apply {k}: lod_edges_m R3 {bridge.LodEdgesM[3]}, expected the max distance {wantMax}");
            if (!HasLightMinRing) fails.Add("profile has no lights.min_ring key");
            else if (_appliedLightRing != lightRing) fails.Add($"apply {k}: effective lights.min_ring {_appliedLightRing}, sent {lightRing}");
            if (!HasMinHeight) fails.Add("profile has no buildings.rN.min_height_m keys");
            else for (int i = 0; i < 3; i++)
                if (System.Math.Abs(_appliedMinH[i] - minH[i]) > 0.01) fails.Add($"apply {k}: effective min height R{i} {_appliedMinH[i]}, sent {minH[i]}");

            // Leak check: every pooled set in use is a tile the cover knows, and the host holds what the library says it delivered.
            await WaitFor(() => { bridge.Streamer.DebugStats(_stats); return LoadedTotal() == bridge.Cover.Loaded; }, 5);
            bridge.Streamer.DebugStats(_stats);
            if (LoadedTotal() != bridge.Cover.Loaded) fails.Add($"apply {k}: library holds {LoadedTotal()} tiles, host cover {bridge.Cover.Loaded}");
            if (bridge.PoolCreated - bridge.PoolFree != bridge.Cover.Loaded + bridge.LandmarkCover.Loaded) fails.Add($"apply {k}: pool in use {bridge.PoolCreated - bridge.PoolFree} != cover {bridge.Cover.Loaded} + landmarks {bridge.LandmarkCover.Loaded}");
            if (bridge.TileRootCount != bridge.PoolCreated) fails.Add($"apply {k}: tile roots {bridge.TileRootCount} != pool created {bridge.PoolCreated}");
            if (bridge.Cover.Loaded == 0) fails.Add($"apply {k}: nothing loaded after the restart");
            if (bridge.TerrainActive)
            {
                // The terrain layer is torn down and rebuilt with every restart too: nothing leaks, the host holds what the library delivered.
                await WaitFor(() => { bridge.Streamer.DebugStats(_stats); return TerrainLoadedTotal() == bridge.TerrainCover.Loaded; }, 5);
                bridge.Streamer.DebugStats(_stats);
                if (TerrainLoadedTotal() != bridge.TerrainCover.Loaded) fails.Add($"apply {k}: library holds {TerrainLoadedTotal()} terrain tiles, host cover {bridge.TerrainCover.Loaded}");
                if (bridge.TerrainPoolCreated - bridge.TerrainPoolFree != bridge.TerrainCover.Loaded) fails.Add($"apply {k}: terrain pool in use {bridge.TerrainPoolCreated - bridge.TerrainPoolFree} != cover {bridge.TerrainCover.Loaded}");
                if (bridge.TerrainCover.Loaded == 0) fails.Add($"apply {k}: no terrain tile after the restart");
            }
            if (k == 1) rootsWarm = bridge.TileRootCount;
            int nodesNow = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
            nodeSeries.Add(nodesNow);
            otherSeries.Add(nodesNow - bridge.PooledNodeCount());
            GD.Print($"[SelfTest] apply {k} nodes: scene {nodesNow}, objects {(int)Performance.GetMonitor(Performance.Monitor.ObjectCount)}, orphans {(int)Performance.GetMonitor(Performance.Monitor.ObjectOrphanNodeCount)}, "
                + $"outside pools {otherSeries[^1]}, tile roots {bridge.TileRootCount}, pool created {bridge.PoolCreated} free {bridge.PoolFree}, terrain pool created {bridge.TerrainPoolCreated} free {bridge.TerrainPoolFree}");
            if (a) loadedA = bridge.Cover.Loaded; else loadedB = bridge.Cover.Loaded;
        }

        // Rejection path: an out-of-range tau sent behind the sliders' back must leave the running world alone and revert the panel.
        int loadedBefore = bridge.Cover.Loaded, countBefore = bridge.ReconfigureCount, unloadedBefore = bridge.UnloadedTotal;
        bridge.Streamer.DebugStats(_stats);
        double originLat = _stats.OriginLat, originLon = _stats.OriginLon;
        var posBefore = cam.GlobalPosition;
        bridge.ApplyProfileOverrides(new Godot.Collections.Dictionary { ["lod"] = new Godot.Collections.Dictionary { ["tau_px"] = 1000.0 } });
        ApplyInFlight = true;
        if (!await WaitFor(() => !ApplyInFlight, 30)) { fails.Add("rejection test: bridge never answered"); return; }
        await Delay(1.0);
        bridge.Streamer.DebugStats(_stats);
        GD.Print($"[SelfTest] rejection: ok={LastApplyOk}, error text '{LastError}', ReconfigureCount {countBefore} -> {bridge.ReconfigureCount}, "
            + $"tiles held {loadedBefore} -> {bridge.Cover.Loaded} ({bridge.UnloadedTotal - unloadedBefore} unloaded meanwhile by normal streaming), library origin unchanged: {_stats.OriginLat == originLat && _stats.OriginLon == originLon}");
        if (LastApplyOk || LastError.Length == 0) fails.Add("rejection test: an out-of-range lod.tau_px was not rejected with an error text");
        if (bridge.ReconfigureCount != countBefore) fails.Add("rejection test: counted as a reconfigure");
        if (bridge.TerrainActive && bridge.TerrainCover.Loaded == 0) fails.Add("rejection test: the terrain layer was dropped");
        if (bridge.UnloadedTotal - unloadedBefore > loadedBefore / 3 || bridge.Cover.Loaded < loadedBefore / 2) fails.Add($"rejection test: the world was dropped ({loadedBefore} -> {bridge.Cover.Loaded} tiles); the previous world must keep running");
        if (_stats.OriginLat != originLat || _stats.OriginLon != originLon) fails.Add("rejection test: library origin changed");
        if (cam.GlobalPosition != posBefore) fails.Add("rejection test: camera moved");
        if (System.Math.Abs(_maxDist - _appliedMaxDist) > 0.01 || System.Math.Abs(_maxBox.Value - _appliedMaxDist) > 0.5) fails.Add("rejection test: sliders not reverted");

        int roots = bridge.TileRootCount;
        int peak = System.Math.Max(24, bridge.PeakActiveTiles);
        int nodesEnd = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
        GD.Print($"[SelfTest] tile pool: {roots} node sets at the end ({bridge.PoolTrimmed} trimmed; after first A+B: {rootsWarm}, peak tiles held {bridge.PeakActiveTiles}, prewarmed 24), "
            + $"{bridge.PoolCreated - bridge.PoolFree} in use; scene nodes {nodesBase} -> {nodesEnd}; last A loaded {loadedA}, last B loaded {loadedB}");
        // Scene nodes outside the tile pools: pooled tiles may grow to a high-water mark during the first applies (that is not a leak), but nothing else may be created per reconfigure.
        if (nodeSeries.Count > 4)
        {
            int hw = 0;
            for (int i = 0; i < 4; i++) hw = System.Math.Max(hw, otherSeries[i]);
            int worst = hw;
            for (int i = 4; i < otherSeries.Count; i++) worst = System.Math.Max(worst, otherSeries[i]);
            GD.Print($"[SelfTest] scene nodes per apply: {string.Join(" ", nodeSeries)}; outside the tile pools: {string.Join(" ", otherSeries)}; high-water of the first 4 applies {hw}, max afterwards {worst} (allowed +{NodeGrowthSlack})");
            if (worst > hw + NodeGrowthSlack) fails.Add($"scene nodes outside the tile pools grew after the warm-up: {worst} vs {hw} in the first four applies (leak of {worst - hw} nodes over {nodeSeries.Count} applies)");
        }
        if (roots > rootsWarm * 1.15 + 8) fails.Add($"tile pool grew from {rootsWarm} (after the first A+B) to {roots} node sets");
        // Without trimming the pool holds exactly the peak (no leak, no double allocation). With it (PoolTrimmer, e.g. --pool-trim-hold=3) sets that were destroyed
        // and re-allocated make "ever allocated" larger, so only the sets that exist now are bounded by the peak.
        if (bridge.PoolTrimmed == 0 ? roots != peak : roots > peak) fails.Add($"tile pool has {roots} node sets ({bridge.PoolTrimmed} trimmed), expected {(bridge.PoolTrimmed == 0 ? "exactly" : "at most")} the peak {peak} (leak or double allocation)");
        if (loadedA >= loadedB) fails.Add($"config A (max distance 6000, quality x2) loaded {loadedA} tiles, not fewer than config B's {loadedB}: the ranges had no effect");
        if (bridge.ConsistencyErrors > 0) fails.Add($"{bridge.ConsistencyErrors} cover consistency errors");
    }
}
