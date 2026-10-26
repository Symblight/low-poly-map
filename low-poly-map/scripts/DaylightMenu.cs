using Godot;

namespace LowPolyMap;

/// <summary>
/// F2-toggled daylight menu (bottom-right): time of day, date, clock speed, presets and a night-factor override, with a
/// readout of sun elevation / azimuth / night factor. Drives <see cref="TimeOfDay"/>; owns no lighting itself.
/// Controls never take keyboard focus, so WASD / F / T keep reaching the camera and the render controller.
/// </summary>
public partial class DaylightMenu : PanelContainer
{
    private const float Margin = 12f;
    private const float Width = 340f;

    private static readonly (string Name, double Speed)[] Speeds =
        { ("paused", 0), ("1x", 1), ("1 min/s", 60), ("10 min/s", 600), ("1 h/s", 3600) };

    /// <summary>Presets follow the sun on the selected date; the fixed hour is the fallback when the sun never gets there.</summary>
    private static readonly (string Name, System.Func<TimeOfDay, double> Hour, double Fallback)[] Presets =
    {
        ("Sunrise", c => c.HourOfElevation(0.0, rising: true), 6.0),
        ("Noon", c => c.HourOfExtremeSun(highest: true), 12.0),
        ("Dusk", c => c.HourOfElevation(-3.0, rising: false), 19.0), // mid civil twilight: night factor ~0.75
        ("Night", c => c.HourOfExtremeSun(highest: false), 0.0),
    };

    [Export] public TimeOfDay? Clock { get; set; }

    private HSlider _time = null!, _date = null!, _night = null!;
    private Label _timeLabel = null!, _dateLabel = null!, _nightLabel = null!, _readout = null!;
    private OptionButton _speed = null!;
    private CheckBox _override = null!;
    private bool _syncing;

    public override void _Ready()
    {
        // Bottom-right: the F3 text fills the left column and the minimap the top-right corner.
        AnchorLeft = 1f; AnchorRight = 1f; AnchorTop = 1f; AnchorBottom = 1f;
        GrowHorizontal = GrowDirection.Begin; GrowVertical = GrowDirection.Begin;
        OffsetLeft = -Margin - Width; OffsetRight = -Margin; OffsetBottom = -Margin - 22f; OffsetTop = -Margin - 22f; // above the key-hint line
        AddThemeStyleboxOverride("panel", new StyleBoxFlat
        {
            BgColor = new Color(0f, 0f, 0f, 0.6f),
            ContentMarginLeft = 10, ContentMarginRight = 10, ContentMarginTop = 8, ContentMarginBottom = 8,
            CornerRadiusTopLeft = 4, CornerRadiusTopRight = 4, CornerRadiusBottomLeft = 4, CornerRadiusBottomRight = 4,
        });

        var col = new VBoxContainer();
        col.AddThemeConstantOverride("separation", 4);
        AddChild(col);
        col.AddChild(new Label { Text = "Daylight  [F2]" });

        _time = Slider(0, 24, 0.25 / 6); // 2.5 min steps
        _timeLabel = Row(col, "Time", _time);
        _time.ValueChanged += v => { if (!_syncing && Clock != null) Clock.LocalHour = v; };

        _date = Slider(1, 365, 1);
        _dateLabel = Row(col, "Date", _date);
        _date.ValueChanged += v => { if (!_syncing && Clock != null) Clock.DayOfYear = (int)v; };

        var speedRow = new HBoxContainer();
        speedRow.AddChild(new Label { Text = "Speed", CustomMinimumSize = new Vector2(52, 0) });
        _speed = new OptionButton { FocusMode = FocusModeEnum.None, SizeFlagsHorizontal = SizeFlags.ExpandFill };
        foreach (var (name, _) in Speeds) _speed.AddItem(name);
        _speed.ItemSelected += i => { if (Clock != null) Clock.TimeSpeed = Speeds[i].Speed; };
        speedRow.AddChild(_speed);
        col.AddChild(speedRow);

        var presets = new HBoxContainer();
        foreach (var (name, hour, fallback) in Presets)
        {
            var b = new Button { Text = name, FocusMode = FocusModeEnum.None, SizeFlagsHorizontal = SizeFlags.ExpandFill };
            b.Pressed += () =>
            {
                if (Clock == null) return;
                double h = hour(Clock);
                Clock.LocalHour = h >= 0.0 ? h : fallback;
                Clock.OverrideNight = false;
            };
            presets.AddChild(b);
        }
        col.AddChild(presets);

        _override = new CheckBox { Text = "Override night factor", FocusMode = FocusModeEnum.None };
        _override.Toggled += on => { if (!_syncing && Clock != null) Clock.OverrideNight = on; };
        col.AddChild(_override);
        _night = Slider(0, 1, 0.01);
        _nightLabel = Row(col, "Night", _night);
        _night.ValueChanged += v => { if (!_syncing && Clock != null) { Clock.NightOverride = (float)v; Clock.OverrideNight = true; } };

        _readout = new Label();
        _readout.AddThemeFontSizeOverride("font_size", 13);
        col.AddChild(_readout);

        if (Clock == null) col.AddChild(new Label { Text = "no TimeOfDay node assigned", Modulate = new Color(1f, 0.4f, 0.3f) });
    }

    private static HSlider Slider(double min, double max, double step) => new()
    {
        MinValue = min, MaxValue = max, Step = step, FocusMode = FocusModeEnum.None,
        SizeFlagsHorizontal = SizeFlags.ExpandFill, SizeFlagsVertical = SizeFlags.ShrinkCenter,
    };

    private static Label Row(VBoxContainer col, string name, HSlider slider)
    {
        var row = new HBoxContainer();
        row.AddChild(new Label { Text = name, CustomMinimumSize = new Vector2(52, 0) });
        row.AddChild(slider);
        var value = new Label { CustomMinimumSize = new Vector2(64, 0), HorizontalAlignment = HorizontalAlignment.Right };
        row.AddChild(value);
        col.AddChild(row);
        return value;
    }

    public override void _UnhandledInput(InputEvent @event)
    {
        if (@event is InputEventKey { Pressed: true, Echo: false, PhysicalKeycode: Key.F2 })
        {
            Visible = !Visible;
            GetViewport().SetInputAsHandled();
        }
    }

    public override void _Process(double delta)
    {
        if (!Visible || Clock == null) return;
        // Mirror the clock (it advances on its own and can be changed by launch args or presets).
        _syncing = true;
        _time.SetValueNoSignal(Clock.LocalHour);
        _date.SetValueNoSignal(Clock.DayOfYear);
        _night.SetValueNoSignal(Clock.NightFactor);
        _override.SetPressedNoSignal(Clock.OverrideNight);
        _syncing = false;
        int si = System.Array.FindIndex(Speeds, s => s.Speed == Clock.TimeSpeed);
        if (si >= 0 && _speed.Selected != si) _speed.Select(si);

        _timeLabel.Text = Clock.TimeText;
        _dateLabel.Text = Clock.DateText;
        _nightLabel.Text = Clock.NightFactor.ToString("0.00");
        _readout.Text = $"sun elev {Clock.SunElevationDeg:0.0}°  az {Clock.SunAzimuthDeg:0}°  night (sun) {Clock.SunNightFactor:0.00}\n"
                      + $"{Clock.Latitude:0.0000}, {Clock.Longitude:0.0000}  UTC{(Clock.UtcOffset >= 0 ? "+" : "")}{Clock.UtcOffset:0.#}";
    }
}
