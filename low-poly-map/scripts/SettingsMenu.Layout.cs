using Godot;

namespace LowPolyMap;

// The panel's controls: the section builders, the slider / number box rows and the model-to-widget sync.
public partial class SettingsMenu
{
    /// <summary>Creates the controls: one section per setting, each with a Reset button, then Apply, the status lines and the attribution.</summary>
    private void BuildWidgets()
    {
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
    }

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
}
