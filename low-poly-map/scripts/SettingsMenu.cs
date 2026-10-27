using System.Collections.Generic;
using System.Threading.Tasks;
using Godot;

namespace LowPolyMap;

/// <summary>
/// F3-toggled settings panel (bottom-left; hidden at startup, launch arg <c>--settings-open</c> shows it): flight speed
/// (live, two-way with <see cref="FreeFlyCamera.Speed"/>), the four band edges (how far each LOD ring of 3D buildings reaches)
/// and the per-ring minimum building height. Edges and heights are profile values the library reads at configure time, so
/// they are applied by <see cref="StreamerBridge.ApplyProfileOverrides"/> (world restart at the camera position): with the
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
    private const double EdgeMinM = 50.0;
    private const double EdgeMaxM = 100000.0;
    /// <summary>Smallest allowed gap between neighbouring band edges (they must stay strictly increasing).</summary>
    private const double EdgeGapM = 10.0;
    private const double SpeedMin = 1.0;
    private const double SpeedMax = 2000.0;
    private const double SpeedDefault = 80.0;
    private const double MinHeightMax = 100.0;
    private const string Attribution = "© OpenStreetMap contributors";

    private static readonly double[] EdgeDefaults = { 600.0, 2500.0, 7000.0, 25000.0 };
    private static readonly string[] EdgeKeys = { "r0_edge_m", "r1_edge_m", "r2_edge_m", "r3_edge_m" };
    private static readonly string[] EdgeNames = { "R0 detailed buildings", "R1 boxes", "R2 merged", "R3 lights only" };

    [Export] public StreamerBridge? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }

    /// <summary>Apply range / filter changes 0.6 s after the last change (default on); off = Apply button only.</summary>
    public bool AutoApply { get; set; } = true;
    /// <summary>Band edges in metres as shown on the panel (may be ahead of <see cref="AppliedEdges"/>).</summary>
    public IReadOnlyList<double> Edges => _edges;
    public IReadOnlyList<double> AppliedEdges => _appliedEdges;
    public IReadOnlyList<double> MinHeights => _minH;
    public IReadOnlyList<double> AppliedMinHeights => _appliedMinH;
    /// <summary>True from <see cref="Apply"/> until the bridge reports the result.</summary>
    public bool ApplyInFlight { get; private set; }
    public bool LastApplyOk { get; private set; } = true;
    public string LastError { get; private set; } = "";
    /// <summary>True when the library's profile has the buildings.rN.min_height_m keys (profile_version 2).</summary>
    public bool HasMinHeight { get; private set; }

    private readonly double[] _edges = new double[4];
    private readonly double[] _appliedEdges = new double[4];
    private readonly double[] _minH = new double[3];
    private readonly double[] _appliedMinH = new double[3];
    private readonly StreamerStats _stats = new();

    private readonly HSlider[] _edgeSlider = new HSlider[4];
    private readonly SpinBox[] _edgeBox = new SpinBox[4];
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

        // -- generation range
        AddHeading(col, "3D building range (band edges)", ResetRange);
        for (int i = 0; i < 4; i++)
        {
            int ring = i;
            (_edgeSlider[i], _edgeBox[i]) = AddRow(col, EdgeNames[i], System.Math.Log10(EdgeMinM), System.Math.Log10(EdgeMaxM), 0.001, EdgeMinM, EdgeMaxM, 1, " m");
            _edgeSlider[i].ValueChanged += v => { if (!_syncing) SetEdge(ring, RoundEdge(System.Math.Pow(10, v))); };
            _edgeBox[i].ValueChanged += v => { if (!_syncing) SetEdge(ring, v); };
        }

        // -- building filter
        AddHeading(col, "Hide buildings lower than", ResetFilter);
        for (int i = 0; i < 3; i++)
        {
            int ring = i;
            (_minSlider[i], _minBox[i]) = AddRow(col, $"R{i} min height", 0, MinHeightMax, 0.5, 0, MinHeightMax, 0.5, " m");
            _minSlider[i].ValueChanged += v => { if (!_syncing) SetMinHeight(ring, v); };
            _minBox[i].ValueChanged += v => { if (!_syncing) SetMinHeight(ring, v); };
        }

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

    private static double RoundEdge(double m) => m < 1000 ? System.Math.Round(m / 5) * 5 : System.Math.Round(m / 10) * 10;

    /// <summary>Pushes the model into every slider and number box without raising their change signals.</summary>
    private void SyncWidgets()
    {
        _syncing = true;
        for (int i = 0; i < 4; i++)
        {
            _edgeSlider[i].SetValueNoSignal(System.Math.Log10(_edges[i]));
            _edgeBox[i].SetValueNoSignal(_edges[i]);
        }
        for (int i = 0; i < 3; i++)
        {
            _minSlider[i].SetValueNoSignal(_minH[i]);
            _minBox[i].SetValueNoSignal(_minH[i]);
        }
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

    /// <summary>
    /// Sets band edge <paramref name="ring"/> (0..3) in metres, clamped to 50..100000 and kept strictly increasing: neighbours that
    /// would cross are pushed along. Marks the panel dirty (auto-apply starts its timer).
    /// </summary>
    public void SetEdge(int ring, double metres)
    {
        metres = System.Math.Clamp(metres, EdgeMinM + ring * EdgeGapM, EdgeMaxM - (3 - ring) * EdgeGapM);
        if (_edges[ring] == metres) return;
        _edges[ring] = metres;
        for (int j = ring + 1; j < 4; j++)
            if (_edges[j] < _edges[j - 1] + EdgeGapM) _edges[j] = _edges[j - 1] + EdgeGapM;
        for (int j = ring - 1; j >= 0; j--)
            if (_edges[j] > _edges[j + 1] - EdgeGapM) _edges[j] = _edges[j + 1] - EdgeGapM;
        SyncWidgets();
        MarkDirty();
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

    public void ResetRange()
    {
        for (int i = 0; i < 4; i++) _edges[i] = EdgeDefaults[i];
        SyncWidgets();
        MarkDirty();
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
        System.Array.Copy(EdgeDefaults, _edges, 4);
        System.Array.Clear(_minH);
        HasMinHeight = false;
        if (profile != null)
        {
            if (profile.TryGetValue("bands", out var bv) && bv.VariantType == Variant.Type.Dictionary)
            {
                var b = bv.AsGodotDictionary();
                for (int i = 0; i < 4; i++)
                    if (b.TryGetValue(EdgeKeys[i], out var e)) _edges[i] = System.Math.Clamp(e.AsDouble(), EdgeMinM, EdgeMaxM);
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
        System.Array.Copy(_edges, _appliedEdges, 4);
        System.Array.Copy(_minH, _appliedMinH, 3);
        _effective = Bridge != null && Bridge.Streamer.IsAvailable ? Bridge.EffectiveProfileText() : "no world";
    }

    // ---- apply --------------------------------------------------------------------------------

    /// <summary>Queues a world restart with the panel's band edges and minimum heights (see <see cref="StreamerBridge.ApplyProfileOverrides"/>).</summary>
    public void Apply()
    {
        if (Bridge == null) return;
        var bands = new Godot.Collections.Dictionary();
        for (int i = 0; i < 4; i++) bands[EdgeKeys[i]] = _edges[i];
        var o = new Godot.Collections.Dictionary { ["bands"] = bands };
        if (HasMinHeight)
        {
            var b = new Godot.Collections.Dictionary();
            for (int i = 0; i < 3; i++) b["r" + i] = new Godot.Collections.Dictionary { ["min_height_m"] = _minH[i] };
            o["buildings"] = b;
        }
        _dirty = false;
        ApplyInFlight = true;
        Bridge.ApplyProfileOverrides(o);
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
            System.Array.Copy(_appliedEdges, _edges, 4);
            System.Array.Copy(_appliedMinH, _minH, 3);
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

        _statsTimer -= delta;
        if (_statsTimer <= 0 && Bridge != null && Bridge.Streamer.DebugStats(_stats))
        {
            _statsTimer = 0.25;
            int q = 0, l = 0;
            for (int i = 0; i < StreamerStats.Rings; i++) { q += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i]; l += _stats.Loaded[i]; }
            _tiles.Text = $"tiles loaded {l}, queued {q}";
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
        return $"queued+fetching+meshing R0..R3 {Ring(busy)}, loaded {Ring(_stats.Loaded)}, cover {b.Cover.Loaded}, tile roots {b.TileRootCount} (in use {b.PoolCreated - b.PoolFree})";
    }

    private int LoadedTotal() { int n = 0; foreach (var v in _stats.Loaded) n += v; return n; }

    /// <summary>Idle = nothing queued, fetching or meshing for 1.5 s in a row and no reconfigure pending (finished tiles can still wait for their poll_ready).</summary>
    private async Task<bool> WaitIdle(double timeoutS)
    {
        ulong calmSince = 0;
        return await WaitFor(() =>
        {
            Bridge!.Streamer.DebugStats(_stats);
            int busy = 0;
            for (int i = 0; i < 4; i++) busy += _stats.Queued[i] + _stats.Fetching[i] + _stats.Meshing[i];
            if (busy != 0 || Bridge.ReconfigurePending || ApplyInFlight) { calmSince = 0; return false; }
            if (calmSince == 0) calmSince = Time.GetTicksMsec();
            return Time.GetTicksMsec() - calmSince >= 1500;
        }, timeoutS);
    }

    private async void RunSelfTest()
    {
        var fails = new List<string>();
        try { await SelfTestBody(fails); }
        catch (System.Exception e) { fails.Add("exception: " + e.Message); }
        GD.Print(fails.Count == 0
            ? "SETTINGS SELFTEST PASS: 12 reconfigures (1 via widgets, 11 alternating), one rejected profile, no pool growth, origin kept, tiles released and reloaded each time"
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
        bridge.Reconfigured += (_, _) => atEventLoaded = bridge.Cover.Loaded;

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
        _edgeBox[3].Value = 12000;
        _minSlider[1].Value = 4;
        await Delay(0.2);
        if (ApplyInFlight || bridge.ReconfigureCount != cw) fails.Add("auto-apply fired before its 0.6 s delay");
        if (!await WaitFor(() => bridge.ReconfigureCount == cw + 1 && !ApplyInFlight, 10)) { fails.Add("auto-apply did not run after the widget changes"); return; }
        if (System.Math.Abs(_appliedEdges[3] - 12000) > 0.5 || System.Math.Abs(_appliedMinH[1] - 4) > 0.01) fails.Add($"widget apply: effective R3 {_appliedEdges[3]}, R1 min {_appliedMinH[1]}");
        GD.Print($"[SelfTest] widgets: speed slider/box/wheel ok, auto-apply ran once; effective {_effective}");
        AutoApply = false;
        await WaitIdle(150);

        // Config A: smaller ranges plus a height filter. Config B: the library defaults.
        double[] edgesA = { 600, 2500, 3000, 6000 }, minA = { 0, 8, 0 };
        double[] edgesB = EdgeDefaults, minB = { 0, 0, 0 };
        int nodesBase = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
        long loadedA = -1, loadedB = -1;
        int rootsWarm = 0;

        // Fly off the origin first so the re-centering is a real test (1.5 km east, 0.8 km north).
        cam.GlobalPosition = new Vector3(1500f, cam.GlobalPosition.Y, -800f);
        await Delay(0.5);

        for (int k = 0; k < 11; k++)
        {
            bool a = k % 2 == 0;
            var edges = a ? edgesA : edgesB;
            var minH = a ? minA : minB;
            if (k > 0) cam.GlobalPosition += new Vector3(250f, 0f, -150f); // keep moving between applies
            string before = Snapshot();
            var geo0 = bridge.Streamer.LocalToGeodetic(cam.GlobalPosition);
            float y0 = cam.GlobalPosition.Y;
            int count0 = bridge.ReconfigureCount;

            for (int i = 0; i < 4; i++) SetEdge(i, edges[i]);
            for (int i = 0; i < 3; i++) SetMinHeight(i, minH[i]);
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
            for (int i = 0; i < 4; i++) if (System.Math.Abs(_appliedEdges[i] - edges[i]) > 0.5) fails.Add($"apply {k}: effective edge R{i} {_appliedEdges[i]}, sent {edges[i]}");
            if (!HasMinHeight) fails.Add("profile has no buildings.rN.min_height_m keys");
            else for (int i = 0; i < 3; i++)
                if (System.Math.Abs(_appliedMinH[i] - minH[i]) > 0.01) fails.Add($"apply {k}: effective min height R{i} {_appliedMinH[i]}, sent {minH[i]}");

            // Leak check: every pooled set in use is a tile the cover knows, and the host holds what the library says it delivered.
            await WaitFor(() => { bridge.Streamer.DebugStats(_stats); return LoadedTotal() == bridge.Cover.Loaded; }, 5);
            bridge.Streamer.DebugStats(_stats);
            if (LoadedTotal() != bridge.Cover.Loaded) fails.Add($"apply {k}: library holds {LoadedTotal()} tiles, host cover {bridge.Cover.Loaded}");
            if (bridge.PoolCreated - bridge.PoolFree != bridge.Cover.Loaded) fails.Add($"apply {k}: pool in use {bridge.PoolCreated - bridge.PoolFree} != cover {bridge.Cover.Loaded}");
            if (bridge.TileRootCount != bridge.PoolCreated) fails.Add($"apply {k}: tile roots {bridge.TileRootCount} != pool created {bridge.PoolCreated}");
            if (bridge.Cover.Loaded == 0) fails.Add($"apply {k}: nothing loaded after the restart");
            if (k == 1) rootsWarm = bridge.TileRootCount;
            if (a) loadedA = bridge.Cover.Loaded; else loadedB = bridge.Cover.Loaded;
        }

        // Rejection path: an out-of-range edge sent behind the sliders' back must leave the running world alone and revert the panel.
        int loadedBefore = bridge.Cover.Loaded, countBefore = bridge.ReconfigureCount, unloadedBefore = bridge.UnloadedTotal;
        bridge.Streamer.DebugStats(_stats);
        double originLat = _stats.OriginLat, originLon = _stats.OriginLon;
        var posBefore = cam.GlobalPosition;
        bridge.ApplyProfileOverrides(new Godot.Collections.Dictionary { ["bands"] = new Godot.Collections.Dictionary { ["r3_edge_m"] = 5.0e6 } });
        ApplyInFlight = true;
        if (!await WaitFor(() => !ApplyInFlight, 30)) { fails.Add("rejection test: bridge never answered"); return; }
        await Delay(1.0);
        bridge.Streamer.DebugStats(_stats);
        GD.Print($"[SelfTest] rejection: ok={LastApplyOk}, error text '{LastError}', ReconfigureCount {countBefore} -> {bridge.ReconfigureCount}, "
            + $"tiles held {loadedBefore} -> {bridge.Cover.Loaded} ({bridge.UnloadedTotal - unloadedBefore} unloaded meanwhile by normal streaming), library origin unchanged: {_stats.OriginLat == originLat && _stats.OriginLon == originLon}");
        if (LastApplyOk || LastError.Length == 0) fails.Add("rejection test: an out-of-range edge was not rejected with an error text");
        if (bridge.ReconfigureCount != countBefore) fails.Add("rejection test: counted as a reconfigure");
        if (bridge.UnloadedTotal - unloadedBefore > loadedBefore / 3 || bridge.Cover.Loaded < loadedBefore / 2) fails.Add($"rejection test: the world was dropped ({loadedBefore} -> {bridge.Cover.Loaded} tiles); the previous world must keep running");
        if (_stats.OriginLat != originLat || _stats.OriginLon != originLon) fails.Add("rejection test: library origin changed");
        if (cam.GlobalPosition != posBefore) fails.Add("rejection test: camera moved");
        for (int i = 0; i < 4; i++) if (System.Math.Abs(_edges[i] - _appliedEdges[i]) > 0.01) fails.Add("rejection test: sliders not reverted");

        int roots = bridge.TileRootCount;
        int peak = System.Math.Max(24, bridge.PeakActiveTiles);
        int nodesEnd = (int)Performance.GetMonitor(Performance.Monitor.ObjectNodeCount);
        GD.Print($"[SelfTest] tile pool: {roots} node sets at the end (after first A+B: {rootsWarm}, peak tiles held {bridge.PeakActiveTiles}, prewarmed 24), "
            + $"{bridge.PoolCreated - bridge.PoolFree} in use; scene nodes {nodesBase} -> {nodesEnd}; last A loaded {loadedA}, last B loaded {loadedB}");
        if (roots > rootsWarm * 1.15 + 8) fails.Add($"tile pool grew from {rootsWarm} (after the first A+B) to {roots} node sets");
        if (roots != peak) fails.Add($"tile pool has {roots} node sets, expected exactly the peak {peak} (leak or double allocation)");
        if (loadedA >= loadedB) fails.Add($"config A (smaller ranges) loaded {loadedA} tiles, not fewer than config B's {loadedB}: the ranges had no effect");
        if (bridge.ConsistencyErrors > 0) fails.Add($"{bridge.ConsistencyErrors} cover consistency errors");
    }
}
