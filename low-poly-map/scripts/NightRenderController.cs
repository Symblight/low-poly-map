using Godot;

namespace LowPolyMap;

/// <summary>
/// Night rendering (docs/spec-night-rendering.md): lives on the WorldEnvironment node and drives its Environment.
/// - Bloom: only things above the HDR glow threshold glow. At night the threshold sits just below the emission of lit
///   windows / light sprites (<see cref="EmissionLevel"/>); concrete, roofs and sky stay far below it. By day
///   it is raised out of reach of sunlit surfaces. Glow intensity rises with altitude so the light carpet blooms at cruise.
/// - Tone map: Filmic or AgX, switchable (export, T key, <c>--tonemap=filmic|agx</c>).
/// - Eye adaptation, external view only (<see cref="ExternalView"/>): Godot's auto exposure is Forward+ only (the engine
///   warns on Mobile), so exposure is a CPU model: a metered-luminance estimate drives <c>Environment.TonemapExposure</c>
///   through an asymmetric low-pass (dark to bright faster than bright to dark).
/// - Fog: depth fog tuned so R2 blocks (2.5..7 km) dissolve instead of reading as boxes, plus a thin ground-haze height fog.
/// - Placeholder day/night look (sun, ambient, sky, fog colour) from <see cref="StreamerBridge.NightFactor"/> until the M6
///   TimeOfDay script owns the sun/moon; switch off with <see cref="DriveLighting"/>.
/// Mobile-renderer features only.
/// </summary>
public partial class NightRenderController : WorldEnvironment
{
    public enum ToneMapper { Filmic = 0, AgX = 1 }

    [Export] public StreamerBridge? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }
    [Export] public DirectionalLight3D? Sun { get; set; }

    /// <summary>Default tone map, see the report: AgX keeps hue in saturated highlights, Filmic crushes blacks harder.</summary>
    [Export] public ToneMapper Tonemap { get; set; } = ToneMapper.Filmic;
    /// <summary>External (chase / free-fly) view: camera-like, so auto exposure is on. Pilot view (future) = eyes: exposure fixed.</summary>
    [Export] public bool ExternalView { get; set; } = true;
    [Export] public bool DriveLighting { get; set; } = true;

    // ---- bloom ----------------------------------------------------------------------------------
    /// <summary>
    /// HDR value (linear, before tone map) a lit window / sprite / lit road reaches. The glow threshold is derived from it;
    /// the building night shader (M6) must emit at least this much. Sprites emit 1.6 * colour; roads do not glow.
    /// </summary>
    [Export] public float EmissionLevel { get; set; } = 1.0f;
    /// <summary>Glow threshold = EmissionLevel * this at night ("just below" the emission).</summary>
    [Export(PropertyHint.Range, "0.3,1,0.01")] public float ThresholdBelowEmission { get; set; } = 0.82f;
    /// <summary>Daytime threshold: above anything a sunlit surface or the sky reaches.</summary>
    [Export] public float DayThreshold { get; set; } = 3.0f;
    [Export] public float GlowIntensityGround { get; set; } = 0.4f;
    [Export] public float GlowIntensityCruise { get; set; } = 1.4f;
    [Export] public float GlowAltLowM { get; set; } = 300f;
    [Export] public float GlowAltHighM { get; set; } = 10000f;

    // ---- exposure (CPU model, see class comment) ---------------------------------------------------
    /// <summary>Scene luminance (linear, pre exposure) that should map to mid grey.</summary>
    [Export] public float MidGrey { get; set; } = 0.18f;
    /// <summary>Modelled average scene luminance by day / at night over dark ground, and the extra a full light carpet adds.</summary>
    [Export] public float LuminanceDay { get; set; } = 0.55f;
    [Export] public float LuminanceNight { get; set; } = 0.035f;
    [Export] public float LuminanceCarpet { get; set; } = 0.06f;
    /// <summary>Visible light sprites at which the carpet term saturates.</summary>
    [Export] public float CarpetLights { get; set; } = 25000f;
    [Export] public float ExposureMin { get; set; } = 0.8f;
    [Export] public float ExposureMax { get; set; } = 1.6f;
    /// <summary>Adaptation time constants in seconds. Scene gets brighter (exposure falls): ~2 s; gets darker (exposure rises): ~8 s.</summary>
    [Export] public float TauDarkToBright { get; set; } = 2.0f;
    [Export] public float TauBrightToDark { get; set; } = 8.0f;

    // ---- fog ------------------------------------------------------------------------------------
    /// <summary>Depth fog is pow(smoothstep(begin, end, slant distance), curve): ~5 % at the R1/R2 border (2.5 km), ~80 % at 7 km.</summary>
    [Export] public float FogBeginM { get; set; } = 1500f;
    [Export] public float FogEndM { get; set; } = 9000f;
    [Export] public float FogCurve { get; set; } = 1.0f;
    /// <summary>Height fog (per fragment height, not per ray): fragments below this world y get 1 - exp(-(h - y) * density).</summary>
    [Export] public float HazeTopM { get; set; } = 150f;
    [Export] public float HazeDensity { get; set; } = 0.0006f;

    private Godot.Environment _env = null!;
    private ProceduralSkyMaterial? _sky;

    private float _night = -1f;
    private float _exposure = 1f;
    // Test aids (user args --cam-alt=<m AGL> --cam-pitch=<deg> --night=<0..1> --shot=<png>[,frames]); no effect without them.
    private float _testAlt = -1f, _testPitch = float.NaN;
    private bool _testNoGlow;
    private string _testBlend = "";
    private string _shotPath = "";
    private int _shotFrames = -1;

    // Public for the overlay.
    public float GlowIntensityNow { get; private set; }
    public float GlowThresholdNow { get; private set; }
    public float ExposureNow => _exposure;
    public float MeteredLuminanceNow { get; private set; }

    private static readonly Color DayFog = new(0.65f, 0.68f, 0.74f);
    private static readonly Color NightFog = new(0.030f, 0.040f, 0.075f);
    private static readonly Color DaySkyTop = new(0.30f, 0.50f, 0.85f);
    private static readonly Color NightSkyTop = new(0.004f, 0.007f, 0.022f);
    private static readonly Color DayAmbient = new(0.5f, 0.55f, 0.65f);
    private static readonly Color NightAmbient = new(0.10f, 0.13f, 0.22f);
    private static readonly Color DaySun = new(1f, 0.96f, 0.88f);
    private static readonly Color NightMoon = new(0.55f, 0.65f, 1f);

    public override void _Ready()
    {
        _env = Environment ?? new Godot.Environment();
        Environment = _env;
        _sky = _env.Sky?.SkyMaterial as ProceduralSkyMaterial;

        foreach (var arg in OS.GetCmdlineUserArgs())
        {
            if (arg.StartsWith("--tonemap=", System.StringComparison.Ordinal))
                Tonemap = arg.Substring(10).ToLowerInvariant() == "agx" ? ToneMapper.AgX : ToneMapper.Filmic;
            else if (arg == "--no-exposure") ExternalView = false;
            else if (arg == "--no-glow") _testNoGlow = true;
            else if (arg.StartsWith("--glow-blend=", System.StringComparison.Ordinal)) _testBlend = arg.Substring(13);
            else if (arg.StartsWith("--cam-alt=", System.StringComparison.Ordinal) && float.TryParse(arg.Substring(10), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var ca)) _testAlt = ca;
            else if (arg.StartsWith("--cam-pitch=", System.StringComparison.Ordinal) && float.TryParse(arg.Substring(12), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var cp)) _testPitch = cp;
            else if (arg.StartsWith("--night=", System.StringComparison.Ordinal) && float.TryParse(arg.Substring(8), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var nf) && Bridge != null) Bridge.NightFactor = nf;
            else if (arg.StartsWith("--shot=", System.StringComparison.Ordinal))
            {
                // --shot=<png>[,<frames>]: save the viewport after N frames (default 90) and quit. Verification aid.
                var parts = arg.Substring(7).Split(',');
                _shotPath = parts[0];
                _shotFrames = parts.Length > 1 && int.TryParse(parts[1], out var f) ? f : 90;
            }
        }

        ConfigureStatic();
        ApplyTonemap();
        if (_testNoGlow) _env.GlowEnabled = false;
        if (_testBlend.Length > 0) _env.GlowBlendMode = _testBlend switch { "additive" => Godot.Environment.GlowBlendModeEnum.Additive, "screen" => Godot.Environment.GlowBlendModeEnum.Screen, "replace" => Godot.Environment.GlowBlendModeEnum.Replace, _ => Godot.Environment.GlowBlendModeEnum.Softlight };
        if (Camera != null)
        {
            if (_testAlt > 0f) Camera.SetAltitude(_testAlt);
            if (!float.IsNaN(_testPitch)) Camera.Rotation = new Vector3(Mathf.DegToRad(_testPitch), 0f, 0f);
        }
        if (_shotFrames > 0 && !System.Linq.Enumerable.Contains(OS.GetCmdlineUserArgs(), "--shot-hud") && GetParent()?.GetNodeOrNull<CanvasLayer>("HUD") is { } hud) hud.Visible = false;
    }

    /// <summary>Everything that does not change per frame.</summary>
    private void ConfigureStatic()
    {
        // Bloom: half-resolution first level, three levels (1/2, 1/4, 1/8); no bloom term, so only the threshold decides.
        _env.GlowEnabled = true;
        _env.GlowNormalized = false;
        _env.GlowBloom = 0f;
        _env.GlowStrength = 1.0f;
                _env.GlowBlendMode = Godot.Environment.GlowBlendModeEnum.Additive; // Softlight showed no visible bloom on Mobile
        _env.GlowHdrScale = 0.45f;
        _env.GlowHdrLuminanceCap = 12f;
        // SetGlowLevel is 0-based (glow_levels/1 == index 0); 7 levels exist.
        for (int i = 0; i < 7; i++) _env.SetGlowLevel(i, 0f);
        _env.SetGlowLevel(0, 0.6f); // 1/2 resolution
        _env.SetGlowLevel(1, 1.0f); // 1/4
        _env.SetGlowLevel(2, 0.8f); // 1/8

        // Fog.
        _env.FogEnabled = true;
        _env.FogMode = Godot.Environment.FogModeEnum.Depth;
        _env.FogDensity = 1.0f;
        _env.FogDepthBegin = FogBeginM;
        _env.FogDepthEnd = FogEndM;
        _env.FogDepthCurve = FogCurve;
        _env.FogHeight = HazeTopM;
        _env.FogHeightDensity = HazeDensity;
        _env.FogSkyAffect = 0f; // the sky horizon is matched to the fog colour instead
        _env.FogSunScatter = 0f;
        _env.FogLightEnergy = 1f;

        // No depth of field: not enabled anywhere (spec: too heavy for the phone, pilots do not see it).
    }

    private void ApplyTonemap()
    {
        switch (Tonemap)
        {
            case ToneMapper.AgX:
                _env.TonemapMode = Godot.Environment.ToneMapper.Agx;
                _env.TonemapWhite = 16.0f;
                break;
            default:
                _env.TonemapMode = Godot.Environment.ToneMapper.Filmic;
                _env.TonemapWhite = 16.0f;
                break;
        }
        _env.TonemapExposure = _exposure;
    }

    public override void _UnhandledInput(InputEvent @event)
    {
        if (@event is InputEventKey { Pressed: true, Echo: false } k)
        {
            if (k.PhysicalKeycode == Key.T)
            {
                Tonemap = Tonemap == ToneMapper.Filmic ? ToneMapper.AgX : ToneMapper.Filmic;
                ApplyTonemap();
                GD.Print($"[NightRender] tone map -> {Tonemap}");
            }
            else if (k.PhysicalKeycode == Key.X)
            {
                ExternalView = !ExternalView;
                GD.Print($"[NightRender] external view (auto exposure) -> {ExternalView}");
            }
        }
    }

    public override void _Process(double delta)
    {
        float night = Bridge != null ? Bridge.NightFactor : 1f;
        float alt = Camera != null ? (float)Camera.AltitudeAgl : 0f;

        // ---- glow: threshold below the emission at night, out of reach by day; stronger the higher we are ----
        float altK = Mathf.SmoothStep(GlowAltLowM, GlowAltHighM, alt);
        float threshold = Mathf.Lerp(DayThreshold, EmissionLevel * ThresholdBelowEmission, night);
        float intensity = Mathf.Lerp(GlowIntensityGround, GlowIntensityCruise, altK);
        _env.GlowHdrThreshold = threshold;
        _env.GlowIntensity = intensity;
        GlowThresholdNow = threshold;
        GlowIntensityNow = intensity;

        // ---- eye adaptation (external view only) ----
        float target = 1f;
        if (ExternalView)
        {
            // Metered luminance model: sky/ambient by day/night plus the glow of the light carpet in view at night.
            float carpet = Bridge != null ? Mathf.Clamp(Bridge.Cover.LightsVisible / CarpetLights, 0f, 1f) : 0f;
            float lum = Mathf.Lerp(LuminanceDay, LuminanceNight, night) + LuminanceCarpet * carpet * night;
            MeteredLuminanceNow = lum;
            target = Mathf.Clamp(MidGrey / Mathf.Max(lum, 1e-4f), ExposureMin, ExposureMax);
        }
        // Asymmetric low-pass in the log domain: falling exposure (scene got brighter) is fast, rising (darker) is slow.
        float tau = target < _exposure ? TauDarkToBright : TauBrightToDark;
        float k = 1f - Mathf.Exp(-(float)delta / Mathf.Max(tau, 0.01f));
        _exposure = Mathf.Exp(Mathf.Lerp(Mathf.Log(_exposure), Mathf.Log(target), k));
        _env.TonemapExposure = _exposure;

        // ---- placeholder day/night look ----
        if (DriveLighting && !Mathf.IsEqualApprox(night, _night))
        {
            _night = night;
            ApplyLook(night);
        }

        if (_shotFrames > 0 && --_shotFrames == 0)
        {
            var img = GetViewport().GetTexture().GetImage();
            img.SavePng(_shotPath);
            GD.Print($"[NightRender] screenshot {_shotPath} {img.GetWidth()}x{img.GetHeight()} tonemap={Tonemap} exposure={ExternalView}");
            GetTree().Quit();
        }
    }

    private void ApplyLook(float night)
    {
        var fog = DayFog.Lerp(NightFog, night);
        _env.FogLightColor = fog;
        _env.AmbientLightColor = DayAmbient.Lerp(NightAmbient, night);
        _env.AmbientLightEnergy = Mathf.Lerp(0.6f, 0.35f, night);
        if (_sky != null)
        {
            _sky.SkyTopColor = DaySkyTop.Lerp(NightSkyTop, night);
            _sky.SkyHorizonColor = fog;
            _sky.GroundHorizonColor = fog;
            _sky.GroundBottomColor = fog; // same as the horizon: no dark band between the far-plane clipped ground and the sky
            _sky.SkyEnergyMultiplier = Mathf.Lerp(1f, 0.6f, night);
        }
        if (Sun != null)
        {
            Sun.LightColor = DaySun.Lerp(NightMoon, night);
            Sun.LightEnergy = Mathf.Lerp(1f, 0.05f, night);
            Sun.ShadowEnabled = night < 0.5f; // shadows off at night
        }
    }
}
