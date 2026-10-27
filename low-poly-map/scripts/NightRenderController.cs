using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Night rendering (docs/spec-night-rendering.md): lives on the WorldEnvironment node and drives its Environment.
/// - Bloom and fog: the addon's <see cref="NightLook"/> (glow threshold below the emission at night, glow intensity by altitude,
///   depth and haze fog). This host owns everything else below.
/// - Tone map: Filmic or AgX, switchable (export, T key, <c>--tonemap=filmic|agx</c>).
/// - Eye adaptation, external view only (<see cref="ExternalView"/>): Godot's auto exposure is Forward+ only (the engine
///   warns on Mobile), so exposure is a CPU model: a metered-luminance estimate drives <c>Environment.TonemapExposure</c>
///   through an asymmetric low-pass (dark to bright faster than bright to dark).
/// - Placeholder day/night look (sun, ambient, sky, fog colour) from <see cref="WorldView.NightFactor"/>;
///   switch off with <see cref="DriveLighting"/>.
/// Mobile-renderer features only.
/// </summary>
public partial class NightRenderController : WorldEnvironment
{
    public enum ToneMapper { Filmic = 0, AgX = 1 }

    [Export] public WorldView? Bridge { get; set; }
    [Export] public FreeFlyCamera? Camera { get; set; }
    [Export] public DirectionalLight3D? Sun { get; set; }

    /// <summary>Default tone map, see the report: AgX keeps hue in saturated highlights, Filmic crushes blacks harder.</summary>
    [Export] public ToneMapper Tonemap { get; set; } = ToneMapper.Filmic;
    /// <summary>External (chase / free-fly) view: camera-like, so auto exposure is on. Pilot view (future) = eyes: exposure fixed.</summary>
    [Export] public bool ExternalView { get; set; } = true;
    [Export] public bool DriveLighting { get; set; } = true;

    /// <summary>The addon's bloom and fog look; null = defaults.</summary>
    [Export] public NightLook? Look { get; set; }

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
    public float GlowIntensityNow => Look?.GlowIntensityNow ?? 0f;
    public float GlowThresholdNow => Look?.GlowThresholdNow ?? 0f;
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
        Look ??= new NightLook();
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
            else if (arg.StartsWith("--shot=", System.StringComparison.Ordinal))
            {
                // --shot=<png>[,<frames>]: save the viewport after N frames (default 90) and quit. Verification aid.
                var parts = arg.Substring(7).Split(',');
                _shotPath = parts[0];
                _shotFrames = parts.Length > 1 && int.TryParse(parts[1], out var f) ? f : 90;
            }
        }

        Look.ApplyStatic(_env);
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

        // ---- glow and fog: the addon's look ----
        Look!.Update(_env, night, alt);

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
