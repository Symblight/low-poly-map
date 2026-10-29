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
    /// <summary>set_lod_quality scale as shown on the panel (live; the library clamps to 0.25..4; 2 = coarser) and whether the library has the method (api 1.10).</summary>
    public double LodQuality => _quality;
    /// <summary>The user-facing "Detail" of the vector LOD = 1 / <see cref="LodQuality"/> (the slider's right = more detail).</summary>
    public double Detail => 1.0 / _quality;
    public bool HasLodQuality { get; private set; }
    /// <summary>True when the profile has bands.max_distance_m (profile_version 7).</summary>
    public bool HasMaxDistance { get; private set; }
    /// <summary>True from <see cref="Apply"/> until the bridge reports the result.</summary>
    public bool ApplyInFlight { get; private set; }
    public bool LastApplyOk { get; private set; } = true;
    public string LastError { get; private set; } = "";
    /// <summary>True when the library's profile has the buildings.rN.min_height_m keys (profile_version 2).</summary>
    public bool HasMinHeight { get; private set; }
    /// <summary>True when the library's profile has lights.min_ring (profile_version 3).</summary>
    public bool HasLightMinRing { get; private set; }

    /// <summary>True when the library has the terrain layer (api 1.5) and its profile has the terrain section.</summary>
    public bool HasTerrain { get; private set; }
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
        BuildWidgets();

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
}
