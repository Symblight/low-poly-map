shader_type spatial;
// Building shader with synthetic night windows (docs/spec-night-windows.md, phases W1 + W3). Replaces the daytime placeholder building_day.
// Mobile-renderer compatible: no Forward+ only features. No texture fetch: a procedural window grid with an integer hash.
//
// Vertex COLOR encoding (from Rust):
//   r = profile id / 8 (0 residential, 1 office, 2 retail, 3 industrial, 4 civic, 5 unknown, 6 landmark, 7 dark)
//   g = seed 0..1, b = floor height / 10 m, a = building height / 400 m.
// Godot stores vertex colours as 8 bit, so the seed is mixed with the b and a bytes before it is hashed.
//
// Wall UV (api 1.0, W3):
//   windowed walls: u = window units (every run of the wall starts and ends on an integer, so windows never wrap a corner),
//                   v = floors from the extrusion bottom (every floor whole). floor(UV) is the window id, fract(UV) the position in it.
//   windowless walls (windowless profile, windows=no, wall part < 2 m, every R2 wall): u = -1 on all vertices, v = meters from the extrusion bottom.
//   roofs and bottom caps (|normal.y| > 0.7): tile-local meters in both channels; no windows, no floor lines.
render_mode cull_back, diffuse_burley, specular_schlick_ggx;

#include "lod_dither.gdshaderinc"
#include "earth_curve.gdshaderinc"

global uniform float simworld_night_factor;

// ---- day look ----------------------------------------------------------------------------------------------------------
uniform float roof_darken : hint_range(0.0, 1.0) = 0.62;
uniform float floor_line_strength : hint_range(0.0, 1.0) = 0.22;
uniform float floor_line_width_m = 0.35;
uniform float floor_line_fade_start = 150.0;
uniform float floor_line_fade_end = 450.0;
uniform float seed_variation : hint_range(0.0, 0.5) = 0.12;
uniform float roughness_value : hint_range(0.0, 1.0) = 0.85;
// Albedo factor inside a window rect by day (glass reads darker than the wall) and its roughness (smoother).
uniform float window_day_albedo : hint_range(0.0, 1.0) = 0.78;
uniform float window_day_roughness : hint_range(0.0, 1.0) = 0.45;

// ---- night windows -----------------------------------------------------------------------------------------------------
// Emission of a lit window relative to emission_level. 0.65 (x 0.85..1.15 jitter) stays under the glow threshold of
// 0.82 x EmissionLevel (NightLook): windows do not bloom, light sprites and floodlights do.
uniform float window_emission : hint_range(0.0, 2.0) = 0.65;
// HDR level a lit sprite reaches; the host copies NightLook.EmissionLevel here (TilePool.SetEmissionLevel).
uniform float emission_level = 1.0;
// Fraction of windows lit per profile id at the current hour. Godot does not allow default values on uniform arrays, so TilePool
// sets them: SetWindowShares / SetProfileColors (fallbacks in the TilePool constructor; WorldView fills them from the world profile's
// lit_share tables and colors, W4, and re-interpolates the shares when the hour moves by more than 0.01 h).
// Ids 3 (industrial), 5 (unknown), 6 (landmark) and 7 (dark) have no windows (share 0). An unset array is all zeros = no windows.
uniform float window_share[8];
// Window colour per profile id (linear emission colour).
uniform vec3 profile_color[8];

// ---- window average (distance) -----------------------------------------------------------------------------------------
// Where a cell is about one pixel the grid is replaced by its average (w below). That average is NOT the colour of a lit wall: it is
// a faint wash, weaker (avg_gain x share instead of the full window area 0.33 x share), desaturated and cooler (avg_desaturate, avg_tint),
// and it fades to zero between avg_fade_start_m and avg_fade_end_m of view depth. The light sprites of the same buildings fade IN over
// the same range (light_sprite.gdshader blend_start_m / blend_end_m; the host writes one pair to both, WorldRenderSettings), so the
// distant glow comes from the sprites and the two never show at full strength together.
uniform float avg_gain : hint_range(0.0, 0.5) = 0.16;
uniform float avg_desaturate : hint_range(0.0, 1.0) = 0.55;
uniform vec3 avg_tint = vec3(0.88, 0.96, 1.08);
uniform float avg_fade_start_m = 900.0;
uniform float avg_fade_end_m = 2000.0;

// ---- landmark floodlight (profile 6; lights.profiles.landmark.floodlight, set by TilePool.SetFloodlight) ---------------------
// Windowless walls get colour x strength x emission_level x night, brightest at the base: x (1 - smoothstep(0, fade_height_m, v)),
// v = meters up on these walls. strength x emission_level stays above the glow threshold, so the base blooms (windows do not).
uniform vec3 floodlight_color = vec3(1.0, 0.75, 0.45);
uniform float floodlight_strength = 1.2;
uniform float floodlight_fade_height_m = 25.0;

varying float v_roof;
varying float v_wall;
varying float v_depth;
// The wall / roof colour is a property of the building (profile id + seed in COLOR, equal on all its vertices), so it is computed per vertex, not per pixel
// (palette lookup, the seed variation and the sRGB -> linear conversion with three pow() were a fixed cost of every pixel, up to a 7 km R2 wall).
varying vec3 v_albedo;

// Palette in sRGB, indexed by profile id 0..5.
vec3 palette(int p) {
	if (p == 0) return vec3(0.86, 0.74, 0.60); // residential: warm plaster
	if (p == 1) return vec3(0.52, 0.60, 0.70); // office: blue-grey glass
	if (p == 2) return vec3(0.90, 0.88, 0.82); // retail: light
	if (p == 3) return vec3(0.52, 0.53, 0.55); // industrial: grey
	if (p == 4) return vec3(0.76, 0.64, 0.46); // civic: sandstone
	return vec3(0.65, 0.65, 0.63);             // unknown, landmark, dark: neutral
}

vec3 srgb_to_linear(vec3 c) {
	return mix(c / 12.92, pow((c + vec3(0.055)) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}

// PCG integer hash. Integer only: a sin() hash differs between GPUs and precisions.
uint pcg(uint v) {
	uint state = v * 747796405u + 2891336453u;
	uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
	return (word >> 22u) ^ word;
}

float hash01(uint seed, ivec2 cell) {
	uint h = pcg(seed ^ pcg(uint(cell.x) + pcg(uint(cell.y))));
	return float(h >> 8u) * (1.0 / 16777216.0);
}

void vertex() {
	// Tile roots are only translated, so model-space normal.y is world normal.y.
	// Roofs and the underside of raised parts (normal -Y) carry tile-local meter UVs: no windows, no floor lines there.
	v_roof = step(0.7, NORMAL.y);
	v_wall = 1.0 - step(0.7, abs(NORMAL.y));
	int vprofile = clamp(int(floor(COLOR.r * 8.0 + 0.5)), 0, 7);
	vec3 base = palette(vprofile);
	// Small per-building variation: brightness and slight warm/cool shift.
	float k = (COLOR.g - 0.5) * 2.0 * seed_variation;
	base = clamp(base * (1.0 + k) + vec3(k * 0.05, 0.0, -k * 0.05), 0.0, 1.0);
	v_albedo = srgb_to_linear(base);
	VERTEX.y -= earth_curve_drop((MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz);
	v_depth = -(MODELVIEW_MATRIX * vec4(VERTEX, 1.0)).z;
}

void fragment() {
#ifdef LOD_DITHER
	if (!lod_dither_keep(FRAGCOORD.xy)) discard;
#endif
	int profile = clamp(int(floor(COLOR.r * 8.0 + 0.5)), 0, 7);
	float floor_h = max(COLOR.b * 10.0, 2.0);

	vec3 albedo = v_albedo;

	// Roofs darker.
	albedo *= mix(1.0, roof_darken, v_roof);

	// Windowed walls carry u >= 0 (window units); u = -1 marks a windowless wall whose v is in meters. Roofs and caps carry meters in both.
	float windowed = v_wall * step(-0.5, UV.x); // -0.5: robust against interpolation noise at a run start (u = 0)
	// Landmarks (6) and dark buildings (7) are castles, towers, ruins, sheds: no floors, no floor lines.
	float has_floors = 1.0 - step(5.5, float(profile));

	// Faint floor lines on walls: dark band at every floor boundary, faded with distance (anti-shimmer).
	// Windowed walls: v is in floors, a boundary is at integer v. Windowless walls: v in meters, boundary every floor_h.
	// The mesher stretches floors to the wall height, so floor_h (nominal) is only approximately the real floor height there.
	float floor_pos = mix(UV.y / floor_h, UV.y, windowed);
	float dist_to_line = abs(fract(floor_pos + 0.5) - 0.5) * floor_h; // meters to nearest floor boundary
	float line = 1.0 - smoothstep(floor_line_width_m * 0.5, floor_line_width_m, dist_to_line);
	float line_fade = 1.0 - smoothstep(floor_line_fade_start, floor_line_fade_end, v_depth);
	albedo *= 1.0 - line * floor_line_strength * line_fade * (1.0 - v_roof) * v_wall * has_floors;

	// ---- windows: window id = floor(UV) = (column, floor), only on walls with u >= 0 ----
	ivec2 cell = ivec2(floor(UV));
	vec2 f = fract(UV);
	float in_window = float(all(greaterThan(f, vec2(0.2, 0.25))) && all(lessThan(f, vec2(0.8, 0.8)))) * windowed;

	// 0 while a cell covers several pixels, 1 when it is about one pixel: the grid fades into its average, no shimmer, no pop.
	// UV is in window units on windowed walls, so fwidth is the cell size in pixels (the 2 m wide cell of an office wall is 1 unit too).
	// (Derivatives stay outside the branch below: they need uniform control flow.)
	float w = smoothstep(0.25, 0.75, max(fwidth(UV.x), fwidth(UV.y)));

	// Day: glass cells slightly darker and smoother, also blended to their average with distance.
	float glass = mix(in_window, 0.33 * windowed, w);
	albedo *= mix(1.0, window_day_albedo, glass);

	ALBEDO = albedo;
	ROUGHNESS = mix(roughness_value, window_day_roughness, glass);
	SPECULAR = 0.3;

	// Emission of lit windows. It is zero on roofs and windowless walls (every R2 / R3 wall, where in_window and the average are multiplied by windowed = 0)
	// and by day (multiplied by the night factor below), so those pixels skip the hashing: the same image, a fraction of the ALU work. The condition is
	// constant over a primitive (wall or roof) and over the frame (the night factor is a global).
	vec3 emis = vec3(0.0);
	if (windowed > 0.5 && simworld_night_factor > 0.0) {
		// 24 bit seed: g (8 bit in the vertex) mixed with the floor height and building height bytes of the same building.
		uint s = uint(COLOR.g * 255.0 + 0.5) | (uint(COLOR.b * 255.0 + 0.5) << 8u) | (uint(COLOR.a * 255.0 + 0.5) << 16u);
		s = pcg(s);
		float building_factor = 0.6 + 0.8 * float(pcg(s ^ 0x85EBCA6Bu) >> 8u) * (1.0 / 16777216.0);
		float share = clamp(window_share[profile] * building_factor, 0.0, 1.0);
		float h1 = hash01(s, cell);
		float h2 = hash01(s ^ 0x9E3779B9u, cell);
		float lit = float(h1 < share);                      // monotone in share: windows go off in a fixed order as it falls
		float glow = lit * (0.85 + 0.3 * h2) * in_window;   // per-window brightness jitter
		// Average of the grid: the window area is 0.6 x 0.55 = 0.33 of the cell, but only avg_gain of it is shown (see the uniforms), and it
		// fades to zero with view depth while the sprites take over. No average on windowless walls.
		float avg = share * avg_gain * windowed * (1.0 - smoothstep(avg_fade_start_m, avg_fade_end_m, v_depth));
		vec3 col = profile_color[profile];
		vec3 avg_col = mix(col, vec3(dot(col, vec3(0.3333))), avg_desaturate) * avg_tint;
		emis = (col * glow * (1.0 - w) + avg_col * avg * w) * window_emission;
	}

	// Landmark: floodlit base on the windowless walls (roofs and caps excluded), fading up with the wall height.
	float flood = float(profile == 6) * v_wall * (1.0 - smoothstep(0.0, max(floodlight_fade_height_m, 0.1), UV.y));
	emis += floodlight_color * (floodlight_strength * flood);

	EMISSION = emis * emission_level * simworld_night_factor;
}
