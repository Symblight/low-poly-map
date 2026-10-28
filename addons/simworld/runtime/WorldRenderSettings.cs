using Godot;

namespace SimWorld;

/// <summary>
/// Tuning of the addon's rendering side: upload budget, pooling and the material parameters of the building, ground and light
/// sprite shaders. A <c>.tres</c> of this resource goes into <see cref="WorldView.Settings"/>; without one the defaults below apply.
/// The material values are written when the view builds its pool, so change them before the view starts. The defaults equal the
/// shaders' own defaults. Streaming and meshing tuning is not here: that is the world profile (docs/contract.md 3.2).
/// </summary>
[GlobalClass]
public partial class WorldRenderSettings : Resource
{
    // ---- upload -------------------------------------------------------------------------------
    /// <summary>Microseconds per frame after which no further ready tile is polled (one big tile can still exceed it).</summary>
    [ExportGroup("Upload")]
    [Export] public long UploadBudgetUs { get; set; } = 2000;
    [Export] public int MaxTilesPerFrame { get; set; } = 2;
    /// <summary>
    /// Draw every world material once at start (tiny, in front of the camera, for 24 frames) so the engine compiles the 10 permutations the tiles use,
    /// including the dithering ones of the LOD fades, during loading and not on the first fade (10 to 25 ms). Needs <see cref="WorldView.Camera"/>; skipped headless.
    /// </summary>
    [Export] public bool PrewarmMaterials { get; set; } = true;
    /// <summary>Tile node sets created at start to avoid first-use hitches.</summary>
    [Export] public int PrewarmTiles { get; set; } = 24;
    /// <summary>
    /// Microseconds per frame spent freeing the meshes and sprite buffers of released tiles (a released tile is hidden at once, its node set is cleared and reused
    /// within this budget; at least one set per frame whatever the budget, so a long queue still drains).
    /// </summary>
    [Export] public long ReleaseBudgetUs { get; set; } = 1000;
    /// <summary>
    /// While many ready tiles are waiting (a cold start, a teleport; <see cref="UploadBacklogDeep"/> tiles or more behind the one being uploaded) and the camera
    /// is slow (below <see cref="UploadBurstMaxSpeedMps"/>) the per-frame upload budget is this many microseconds and <see cref="UploadBurstMaxTiles"/> tiles
    /// instead of <see cref="UploadBudgetUs"/> / <see cref="MaxTilesPerFrame"/>. Steady flight keeps the normal budget. 0 = no burst.
    /// </summary>
    [Export] public long UploadBudgetBurstUs { get; set; } = 4000;
    [Export] public int UploadBurstMaxTiles { get; set; } = 4;
    [Export] public int UploadBacklogDeep { get; set; } = 24;
    [Export] public float UploadBurstMaxSpeedMps { get; set; } = 60f;
    /// <summary>
    /// A pooled tile keeps the instance buffer of its street pool MultiMesh (64 bytes per lamp, grow-only while it is in use) when it is released only up to this
    /// many lamps; a bigger one is freed. The default is lower on a mobile OS, where the node pool has to stay small.
    /// </summary>
    [Export] public int SpriteMaxRetainedInstances { get; set; } = OS.HasFeature("mobile") ? 1024 : 4096;
    /// <summary>
    /// Draw the building light sprites at all (the far light carpet of R1..R3 and the clusters). Off for a low tier: street lamps, beacons and airport lights stay.
    /// Read when the view builds its pool. Windows still show on near buildings.
    /// </summary>
    [Export] public bool BuildingLightSprites { get; set; } = true;
    /// <summary>
    /// Ask the library for engine surfaces (api 1.11, contract 4a): it packs every building, ground and terrain mesh into Godot's vertex layout on its worker
    /// threads and the main thread only hands the bytes over. Off, or with an older library, Godot packs the Arrays on the main thread (about 1 ms per 20 000 vertices).
    /// Read at every configure.
    /// </summary>
    [Export] public bool EngineSurfaces { get; set; } = true;
    /// <summary>
    /// Dither material clones made up front for the fade bursts (during <see cref="PrewarmMaterials"/>): fading tiles at once = <c>fades.max_active</c>
    /// swaps x this many tiles per swap (the coarse tile and its four children). Without the reserve the clones are made when the first swaps start.
    /// </summary>
    [Export] public int FadeReserveTilesPerSwap { get; set; } = 5;
    /// <summary>
    /// Debug checks that cost frame time: the periodic cover verification (<see cref="VerifyIntervalS"/>, about a thousand dictionaries through
    /// <c>debug_tiles</c>) and the one-time <c>debug_stats</c> print at the first tile (7 to 21 ms). Off by default; a host turns it on for self-tests and smoke runs.
    /// </summary>
    [Export] public bool DebugChecks { get; set; }
    /// <summary>Seconds between cover consistency checks while <see cref="DebugChecks"/> is on (0 = never).</summary>
    [Export] public double VerifyIntervalS { get; set; } = 5.0;

    // ---- memory -------------------------------------------------------------------------------
    /// <summary>
    /// Seconds the free list of a node pool must stay above its margin before the surplus is freed (a few sets per frame); 0 = pools never shrink.
    /// The window is tumbling, so the real delay is between one and two times this.
    /// </summary>
    [ExportGroup("Memory")]
    [Export(PropertyHint.Range, "0,120,1,or_greater")] public double PoolTrimHoldS { get; set; } = 30.0;
    /// <summary>Free node sets a pool always keeps (the margin is the larger of this and <see cref="PoolTrimPeakShare"/> x the recent peak in use).</summary>
    [Export] public int PoolTrimMinFree { get; set; } = 64;
    /// <summary>Share of the recent peak of node sets in use that stays free as the margin (0.25 = a quarter).</summary>
    [Export(PropertyHint.Range, "0,1,0.05")] public float PoolTrimPeakShare { get; set; } = 0.25f;
    /// <summary>Node sets destroyed per frame while trimming (each is a root with up to 7 children).</summary>
    [Export(PropertyHint.Range, "1,32,1")] public int PoolTrimPerFrame { get; set; } = 4;
    /// <summary>
    /// Budget in MiB of the library's CPU-side pools (profile <c>memory.budget_mb</c>, api 1.10) that <see cref="WorldView"/> passes as a profile override
    /// at every configure, unless the host's profile overrides or profile file set the key. 0 = the library default (512). The default of this
    /// property is 224 on a mobile build (<c>OS.HasFeature("mobile")</c>, "ios" or "android") and 0 on desktop. Needs a library with api 1.10.
    /// </summary>
    [Export(PropertyHint.Range, "0,65536,1,or_greater")] public float MemoryBudgetMb { get; set; } = DefaultMemoryBudgetMb();

    /// <summary>Platform default of <see cref="MemoryBudgetMb"/>: 224 MiB on mobile (iOS, Android), 0 (library default) elsewhere.</summary>
    public static float DefaultMemoryBudgetMb() => OS.HasFeature("mobile") || OS.HasFeature("ios") || OS.HasFeature("android") ? MobileMemoryBudgetMb : 0f;
    public const float MobileMemoryBudgetMb = 224f;

    // ---- level of detail ----------------------------------------------------------------------
    /// <summary>
    /// Multiplies the viewport height sent to <c>set_view</c> (api 1.10, screen-space LOD): below 1 the library works with fewer pixels, so tiles refine later
    /// (a cheap load knob); 1 = the 3D render height. The base is already the viewport height times its 3D render scale (<c>rendering/scaling_3d/scale</c>),
    /// so a phone that renders at 0.5 scale gets half the pixels without touching this; use it only for an extra bias on top.
    /// </summary>
    [ExportGroup("Level of detail")]
    [Export(PropertyHint.Range, "0.1,4,0.05")] public float LodPixelScale { get; set; } = 1f;
    /// <summary>
    /// True (default): the window / sprite cross-fade, the street light pool fade and the light sprites' far end follow the live LOD edges of the library
    /// (<c>lod_edges_m</c>, api 1.10): with D = the R2 to R1 edge the blend runs <see cref="BlendStartShare"/> x D .. <see cref="BlendEndShare"/> x D, the pool fade
    /// <see cref="PoolStartShare"/> x D .. min(<see cref="PoolEndShare"/> x D, D / hysteresis), and the sprites fade out <see cref="LightFadeShare"/> x .. 1 x <c>bands.max_distance_m</c>.
    /// The shares reproduce the fixed values (900 / 2000, 1200 / 2500, 21000 / 25000 m) at D = 2500 m and a 25 km maximum distance. False = the fixed values below.
    /// </summary>
    [Export] public bool BlendFollowsLod { get; set; } = true;
    public const float BlendStartShare = 0.36f, BlendEndShare = 0.8f, PoolStartShare = 0.48f, PoolEndShare = 1.0f, LightFadeShare = 0.84f;

    // ---- debug --------------------------------------------------------------------------------
    [ExportGroup("Debug")]
    /// <summary>Force every light sprite's weight (ignores Rust's alpha). Negative = off.</summary>
    [Export] public float DebugLightAlpha { get; set; } = -1f;
    /// <summary>Pretend poll_fades lists every R1 tile with this fade (checks the dither path without Rust). Negative = off.</summary>
    [Export] public float DebugFakeFade { get; set; } = -1f;

    // ---- building material (shaders/building_windows.gdshader) -------------------------------------
    [ExportGroup("Buildings")]
    [Export(PropertyHint.Range, "0,1,0.01")] public float RoofDarken { get; set; } = 0.62f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float FloorLineStrength { get; set; } = 0.22f;
    [Export] public float FloorLineWidthM { get; set; } = 0.35f;
    [Export] public float FloorLineFadeStart { get; set; } = 150f;
    [Export] public float FloorLineFadeEnd { get; set; } = 450f;
    [Export(PropertyHint.Range, "0,0.5,0.01")] public float SeedVariation { get; set; } = 0.12f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float Roughness { get; set; } = 0.85f;
    /// <summary>Window emission relative to <see cref="NightLook.EmissionLevel"/>; keep below the glow threshold (0.82) so windows do not bloom.</summary>
    [Export(PropertyHint.Range, "0,2,0.01")] public float WindowEmission { get; set; } = 0.65f;
    /// <summary>Strength of the averaged window glow that replaces the grid where a cell is about one pixel (share x this; the full window area is 0.33).</summary>
    [Export(PropertyHint.Range, "0,0.5,0.01")] public float WindowAvgGain { get; set; } = 0.16f;
    /// <summary>0 = the profile colour, 1 = grey: the average reads as a wash of light, not as lit walls.</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float WindowAvgDesaturate { get; set; } = 0.55f;
    /// <summary>Multiplies the desaturated average (cooler than the window colour).</summary>
    [Export] public Color WindowAvgTint { get; set; } = new(0.88f, 0.96f, 1.08f);
    /// <summary>Window / sprite cross-fade range in metres of view depth: the averaged window glow fades 1 -> 0 and the light sprites 0 -> 1
    /// over the same range (one pair, written to the building and the sprite material). Keep the end below the R1/R2 border (2500 m).</summary>
    [Export] public float WindowSpriteBlendStartM { get; set; } = 900f;
    [Export] public float WindowSpriteBlendEndM { get; set; } = 2000f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float WindowDayAlbedo { get; set; } = 0.78f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float WindowDayRoughness { get; set; } = 0.45f;

    // ---- ground material (shaders/ground.gdshader) --------------------------------------------
    [ExportGroup("Ground")]
    [Export(PropertyHint.Range, "0,1,0.01")] public float GroundDarken { get; set; } = 0.88f;
    [Export] public float GroundDepthBiasPerM { get; set; } = 0.00002f;
    [Export] public float GroundDepthBiasMaxM { get; set; } = 3f;

    // ---- terrain material (shaders/terrain.gdshader, api 1.5) ---------------------------------
    [ExportGroup("Terrain")]
    /// <summary>The ground shader's grass / park colour (sRGB), the land colour the terrain's low colour defaults to, so the edge of the vector
    /// ground (R2 -> terrain) reads as a gradual loss of landuse detail instead of a colour step. Keep it equal to palette(0) in ground.gdshader.</summary>
    public static readonly Color GroundLandColor = new(0.46f, 0.63f, 0.34f);
    /// <summary>Palette (sRGB): low land (green) -> dry grass with altitude -> rock on steep faces -> snow above the snow line.</summary>
    [Export] public Color TerrainLowColor { get; set; } = GroundLandColor;
    [Export] public Color TerrainHighColor { get; set; } = new(0.64f, 0.60f, 0.38f);
    [Export] public Color TerrainRockColor { get; set; } = new(0.56f, 0.51f, 0.47f);
    [Export] public Color TerrainSnowColor { get; set; } = new(0.93f, 0.95f, 0.98f);
    /// <summary>Slope (1 - normal.y of the face; 0.22 = about 39 degrees) above which a face turns to rock; blended over <see cref="TerrainRockBlend"/>.</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float TerrainRockSlope { get; set; } = 0.22f;
    [Export(PropertyHint.Range, "0.01,0.5,0.01")] public float TerrainRockBlend { get; set; } = 0.10f;
    /// <summary>Altitude above sea level (m) where land is fully snow-covered from, with a blend of <see cref="TerrainSnowBlendM"/> below it.</summary>
    [Export] public float TerrainSnowLineM { get; set; } = 1400f;
    [Export] public float TerrainSnowBlendM { get; set; } = 200f;
    /// <summary>Altitude range (m above sea level) over which the low colour turns into the dry-grass colour.</summary>
    [Export] public float TerrainHighStartM { get; set; } = 40f;
    [Export] public float TerrainHighEndM { get; set; } = 320f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float TerrainRoughness { get; set; } = 0.95f;
    /// <summary>Overall darkening like <see cref="GroundDarken"/>; defaults to the same so land under the city and beside it match.</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float TerrainDarken { get; set; } = 0.88f;

    // ---- world seed (api 1.7; shaders/seed_palette.gdshaderinc, airport_marking.gdshader) ---------
    [ExportGroup("World seed")]
    /// <summary>Class colours of a seed world (sRGB), written to the ground AND the terrain material: the vector ground (R0..R3) and the terrain
    /// beyond it must show the same colour for the same class. Grass equals <see cref="GroundLandColor"/> (palette(0) of ground.gdshader).</summary>
    [Export] public Color SeedGrassColor { get; set; } = GroundLandColor;
    [Export] public Color SeedForestColor { get; set; } = new(0.27f, 0.43f, 0.25f);
    [Export] public Color SeedWaterColor { get; set; } = new(0.16f, 0.31f, 0.47f);
    /// <summary>Airport surfaces: the mown grass inside the fence, a paved runway (dark asphalt), a grass runway, taxiways, aprons and car parks.</summary>
    [Export] public Color SeedAirfieldColor { get; set; } = new(0.50f, 0.67f, 0.36f);
    [Export] public Color SeedRunwayColor { get; set; } = new(0.19f, 0.19f, 0.20f);
    [Export] public Color SeedRunwayGrassColor { get; set; } = new(0.56f, 0.71f, 0.38f);
    [Export] public Color SeedTaxiwayColor { get; set; } = new(0.30f, 0.30f, 0.32f);
    [Export] public Color SeedApronColor { get; set; } = new(0.56f, 0.56f, 0.57f);
    /// <summary>Countryside tint (COLOR g, level 0..7): levels 1..7 mix grass towards a meadow-to-crops ramp by <see cref="SeedTintStrength"/> and vary
    /// the brightness by +-<see cref="SeedTintValueVar"/>; forest goes towards the light forest green.</summary>
    [Export] public Color SeedMeadowColor { get; set; } = new(0.56f, 0.74f, 0.34f);
    [Export] public Color SeedFieldColor { get; set; } = new(0.80f, 0.72f, 0.36f);
    [Export] public Color SeedForestLightColor { get; set; } = new(0.36f, 0.50f, 0.24f);
    [Export(PropertyHint.Range, "0,1,0.01")] public float SeedTintStrength { get; set; } = 0.75f;
    [Export(PropertyHint.Range, "0,0.5,0.01")] public float SeedTintValueVar { get; set; } = 0.16f;

    /// <summary>Airport light sprites (the sprite shader, own material): brightness and size in pixels. They show at every distance with no
    /// window cross-fade and no grazing-angle or altitude thinning (an approach at 300 m AGL looks along the lights at a low angle).</summary>
    [Export] public float AirportLightIntensity { get; set; } = 2.2f;
    [Export] public float AirportLightPxMin { get; set; } = 3f;
    [Export] public float AirportLightPxMax { get; set; } = 4f;
    /// <summary>The intensity weight Rust sends in the colour alpha for airport lights (<c>seed.airport.lights.intensity</c>); the shader brightness is
    /// scaled by alpha / this value, so changing the profile value brightens or dims the lights relative to the default.</summary>
    [Export] public float AirportLightReferenceWeight { get; set; } = 0.35f;

    /// <summary>Runway markings (host decals from <c>airports()</c>, shaders/airport_marking.gdshader): colour, the margin above the drawn runway
    /// (the host adds the drawn runway's height above ground_height_at, level x lift_step_m per ring since generator 2; the shader also pulls the marking towards the camera by <see cref="MarkingPullPerM"/> x distance),
    /// and the view distance over which they dither out (beyond it the ground classes carry the runway).</summary>
    [Export] public Color MarkingColor { get; set; } = new(0.92f, 0.92f, 0.90f);
    [Export] public float MarkingLiftM { get; set; } = 0.12f;
    /// <summary>Stacking level of a runway inside an airport fence (contract 9 "Airports", generator 2: fence 1, taxiway / apron 2, runway 3, no drape lift): the drawn
    /// runway is <c>ground_height_at</c> + level x <c>ground.rN.lift_step_m</c>. Used for generator &gt;= 2; a generator-1 library (runway level 10 plus the drape
    /// lift) is replayed by <c>WorldView.ComputeRunwayLifts</c> with its own constant.</summary>
    [Export] public int MarkingRunwayLevel { get; set; } = 3;
    [Export] public float MarkingPullPerM { get; set; } = 0.0006f;
    [Export] public float MarkingFadeStartM { get; set; } = 2200f;
    [Export] public float MarkingFadeEndM { get; set; } = 3200f;
    /// <summary>Show the markings at all. Draw them only for airports within <see cref="MarkingRangeM"/> of the camera.</summary>
    [Export] public bool MarkingsEnabled { get; set; } = true;
    [Export] public float MarkingRangeM { get; set; } = 4500f;

    public void ApplySeedPalette(ShaderMaterial m)
    {
        static Vector3 V(Color c) => new(c.R, c.G, c.B);
        m.SetShaderParameter("seed_grass_color", V(SeedGrassColor));
        m.SetShaderParameter("seed_forest_color", V(SeedForestColor));
        m.SetShaderParameter("seed_water_color", V(SeedWaterColor));
        m.SetShaderParameter("seed_airfield_color", V(SeedAirfieldColor));
        m.SetShaderParameter("seed_runway_color", V(SeedRunwayColor));
        m.SetShaderParameter("seed_runway_grass_color", V(SeedRunwayGrassColor));
        m.SetShaderParameter("seed_taxiway_color", V(SeedTaxiwayColor));
        m.SetShaderParameter("seed_apron_color", V(SeedApronColor));
        m.SetShaderParameter("seed_meadow_color", V(SeedMeadowColor));
        m.SetShaderParameter("seed_field_color", V(SeedFieldColor));
        m.SetShaderParameter("seed_forest_light_color", V(SeedForestLightColor));
        m.SetShaderParameter("seed_tint_strength", SeedTintStrength);
        m.SetShaderParameter("seed_tint_value_var", SeedTintValueVar);
    }

    public void ApplyToMarkings(ShaderMaterial m)
    {
        m.SetShaderParameter("marking_color", new Vector3(MarkingColor.R, MarkingColor.G, MarkingColor.B));
        m.SetShaderParameter("lift_m", MarkingLiftM);
        m.SetShaderParameter("pull_per_m", MarkingPullPerM);
        m.SetShaderParameter("fade_start_m", MarkingFadeStartM);
        m.SetShaderParameter("fade_end_m", MarkingFadeEndM);
    }

    // ---- light sprites (shaders/light_sprite.gdshader) ----------------------------------------
    [ExportGroup("Light sprites")]
    [Export] public float LightIntensity { get; set; } = 2.4f;
    /// <summary>Sprite size in pixels: <see cref="LightPxMin"/> close, <see cref="LightPxMax"/> from <see cref="LightPxEndM"/> on. Never larger far away.</summary>
    [Export] public float LightPxMin { get; set; } = 2f;
    [Export] public float LightPxMax { get; set; } = 3f;
    [Export] public float LightPxEndM { get; set; } = 7000f;
    [Export] public float LightFarEndM { get; set; } = 25000f;
    [Export] public float LightFadeStartM { get; set; } = 21000f;
    [Export] public float LightClusterBrightGain { get; set; } = 0.8f;
    [Export] public float LightClusterWorldM { get; set; } = 40f;
    /// <summary>Distance falloff ref² / (ref² + d²) of a sprite's brightness: the horizon band, where kilometres of lights share a pixel, stays below white.</summary>
    [Export] public float LightFalloffRefM { get; set; } = 6000f;
    /// <summary>Share of a sprite's brightness left in full depth fog (the fog curve itself comes from the host, <see cref="WorldView.SetLightFog"/>).</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float LightFogKeep { get; set; } = 0.6f;
    /// <summary>Grazing-angle thinning: below elevation (camera height - light height) / distance <see cref="LightGrazeMax"/> lights drop out one by one,
    /// below <see cref="LightGrazeMin"/> all are gone. 0.02 / 0.08 = from 400 m the carpet thins from 5 km and ends at 20 km.</summary>
    [Export] public float LightGrazeMin { get; set; } = 0.02f;
    [Export] public float LightGrazeMax { get; set; } = 0.08f;
    /// <summary>Distance blur: sprites widen to <see cref="LightBlurPx"/> between <see cref="LightBlurStartM"/> and <see cref="LightBlurEndM"/> with
    /// a softer profile, at constant energy per light (brightness / area gained).</summary>
    [Export] public float LightBlurStartM { get; set; } = 3000f;
    [Export] public float LightBlurEndM { get; set; } = 15000f;
    [Export] public float LightBlurPx { get; set; } = 7f;
    /// <summary>Altitude thinning: the share of lights kept falls to <see cref="LightAltKeepMin"/> between these camera heights above the
    /// lights; kept lights brighten by 1 / sqrt(share).</summary>
    [Export] public float LightAltThinStartM { get; set; } = 800f;
    [Export] public float LightAltThinEndM { get; set; } = 3300f;
    [Export(PropertyHint.Range, "0.05,1,0.01")] public float LightAltKeepMin { get; set; } = 1f; // 1 = no altitude thinning (dense carpet, like aerial photos)
    /// <summary>Per-light colour variety of building lights (0 = profile colours only): cool LED / TV tint on <see cref="LightCoolShare"/>,
    /// amber on <see cref="LightAmberShare"/> of the lights (fixed per light), plus a brightness jitter.</summary>
    [Export(PropertyHint.Range, "0,1,0.01")] public float LightColorVariation { get; set; } = 1f;
    [Export] public Color LightCoolTint { get; set; } = new(0.62f, 0.78f, 1.0f);
    [Export] public Color LightAmberTint { get; set; } = new(1.0f, 0.5f, 0.16f);
    [Export(PropertyHint.Range, "0,1,0.01")] public float LightCoolShare { get; set; } = 0.22f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float LightAmberShare { get; set; } = 0.14f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float LightBrightnessJitter { get; set; } = 0.3f;
    /// <summary>Fixed pull towards the camera in metres; keep it small so buildings in front still hide the light.</summary>
    [Export] public float LightLiftM { get; set; } = 2f;

    // ---- street lamps (api 1.1; the same sprite shader, its own material) ----------------------
    [ExportGroup("Street lamps")]
    /// <summary>Brightness of a street lamp sprite (building lights: <see cref="LightIntensity"/>).</summary>
    [Export] public float StreetLightIntensity { get; set; } = 2.6f;

    /// <summary>Light pools under the lamps (shaders/street_light_pool.gdshader): disc radius, peak brightness (keep under the glow
    /// threshold so roads glow softly without bloom) and the view distance over which they fade out (the sprites carry the street beyond).</summary>
    [Export] public float StreetPoolRadiusM { get; set; } = 9f;
    [Export] public float StreetPoolIntensity { get; set; } = 0.22f;
    [Export] public float StreetPoolFadeStartM { get; set; } = 1200f;
    [Export] public float StreetPoolFadeEndM { get; set; } = 2500f;
    /// <summary>
    /// Pools exist (a MultiMesh filled, a draw call) only on tiles of this ring or finer: they fade out by about the R2 / R1 edge, so a coarser tile never shows
    /// one and would pay a draw call and its vertex work for nothing; and they are drawn only while the tile is within the fade end of the camera
    /// (<c>min(D, D / hysteresis)</c> with the live R2 / R1 edge D when <see cref="BlendFollowsLod"/>). 1 = R0 and R1; -1 = no pools.
    /// </summary>
    [Export(PropertyHint.Range, "-1,3,1")] public int StreetPoolMaxRing { get; set; } = 1;

    // ---- aircraft warning beacons (api 1.2; kind 3 of shaders/light_sprite.gdshader) -------------------
    [ExportGroup("Beacons")]
    /// <summary>Beacon brightness (blooms: above the glow threshold), size in pixels near / far, blink period, share of the period on,
    /// and the brightness left while off (0 = flashing, 1 = steady).</summary>
    [Export] public float BeaconIntensity { get; set; } = 4f;
    [Export] public float BeaconPxNear { get; set; } = 4f;
    [Export] public float BeaconPxFar { get; set; } = 5f;
    [Export] public float BeaconBlinkPeriodS { get; set; } = 1.5f;
    [Export(PropertyHint.Range, "0.05,1,0.01")] public float BeaconOnShare { get; set; } = 0.45f;
    [Export(PropertyHint.Range, "0,1,0.01")] public float BeaconOffLevel { get; set; } = 0.15f;

    public void ApplyToBuilding(ShaderMaterial m)
    {
        m.SetShaderParameter("roof_darken", RoofDarken);
        m.SetShaderParameter("floor_line_strength", FloorLineStrength);
        m.SetShaderParameter("floor_line_width_m", FloorLineWidthM);
        m.SetShaderParameter("floor_line_fade_start", FloorLineFadeStart);
        m.SetShaderParameter("floor_line_fade_end", FloorLineFadeEnd);
        m.SetShaderParameter("seed_variation", SeedVariation);
        m.SetShaderParameter("roughness_value", Roughness);
        m.SetShaderParameter("window_emission", WindowEmission);
        m.SetShaderParameter("avg_gain", WindowAvgGain);
        m.SetShaderParameter("avg_desaturate", WindowAvgDesaturate);
        m.SetShaderParameter("avg_tint", new Vector3(WindowAvgTint.R, WindowAvgTint.G, WindowAvgTint.B));
        m.SetShaderParameter("avg_fade_start_m", WindowSpriteBlendStartM);
        m.SetShaderParameter("avg_fade_end_m", WindowSpriteBlendEndM);
        m.SetShaderParameter("window_day_albedo", WindowDayAlbedo);
        m.SetShaderParameter("window_day_roughness", WindowDayRoughness);
    }

    public void ApplyToGround(ShaderMaterial m)
    {
        m.SetShaderParameter("darken", GroundDarken);
        m.SetShaderParameter("depth_bias_per_m", GroundDepthBiasPerM);
        m.SetShaderParameter("depth_bias_max_m", GroundDepthBiasMaxM);
        ApplySeedPalette(m);
    }

    public void ApplyToTerrain(ShaderMaterial m)
    {
        m.SetShaderParameter("low_color", TerrainLowColor);
        m.SetShaderParameter("high_color", TerrainHighColor);
        m.SetShaderParameter("rock_color", TerrainRockColor);
        m.SetShaderParameter("snow_color", TerrainSnowColor);
        m.SetShaderParameter("rock_slope", TerrainRockSlope);
        m.SetShaderParameter("rock_blend", TerrainRockBlend);
        m.SetShaderParameter("snow_line_m", TerrainSnowLineM);
        m.SetShaderParameter("snow_blend_m", TerrainSnowBlendM);
        m.SetShaderParameter("high_start_m", TerrainHighStartM);
        m.SetShaderParameter("high_end_m", TerrainHighEndM);
        m.SetShaderParameter("roughness_value", TerrainRoughness);
        m.SetShaderParameter("darken", TerrainDarken);
        ApplySeedPalette(m);
    }
}
