shader_type spatial;
// Placeholder daytime building shader (M3). Replaced by building_windows.gdshader in M6.
// Mobile-renderer compatible: no Forward+ only features.
//
// Vertex COLOR encoding (from Rust):
//   r = profile id / 8 (0 residential, 1 office, 2 retail, 3 industrial, 4 civic, 5 unknown)
//   g = seed 0..1, b = floor height / 10 m, a = building height / 400 m.   UV = meters (u along wall, v up).
render_mode cull_back, diffuse_burley, specular_schlick_ggx;

#include "lod_dither.gdshaderinc"
#include "earth_curve.gdshaderinc"

uniform float roof_darken : hint_range(0.0, 1.0) = 0.62;
uniform float floor_line_strength : hint_range(0.0, 1.0) = 0.22;
uniform float floor_line_width_m = 0.35;
uniform float floor_line_fade_start = 150.0;
uniform float floor_line_fade_end = 450.0;
uniform float seed_variation : hint_range(0.0, 0.5) = 0.12;
uniform float roughness_value : hint_range(0.0, 1.0) = 0.85;

varying float v_roof;
varying float v_depth;

// Palette in sRGB, indexed by profile id 0..5.
vec3 palette(int p) {
	if (p == 0) return vec3(0.86, 0.74, 0.60); // residential: warm plaster
	if (p == 1) return vec3(0.52, 0.60, 0.70); // office: blue-grey glass
	if (p == 2) return vec3(0.90, 0.88, 0.82); // retail: light
	if (p == 3) return vec3(0.52, 0.53, 0.55); // industrial: grey
	if (p == 4) return vec3(0.76, 0.64, 0.46); // civic: sandstone
	return vec3(0.65, 0.65, 0.63);             // unknown: neutral
}

vec3 srgb_to_linear(vec3 c) {
	return mix(c / 12.92, pow((c + vec3(0.055)) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}

void vertex() {
	// Tile roots are only translated, so model-space normal.y is world normal.y.
	v_roof = step(0.7, NORMAL.y);
	VERTEX.y -= earth_curve_drop((MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz);
	v_depth = -(MODELVIEW_MATRIX * vec4(VERTEX, 1.0)).z;
}

void fragment() {
#ifdef LOD_DITHER
	if (!lod_dither_keep(FRAGCOORD.xy)) discard;
#endif
	int profile = int(floor(COLOR.r * 8.0 + 0.5));
	float seed = COLOR.g;
	float floor_h = max(COLOR.b * 10.0, 2.0);

	vec3 base = palette(profile);
	// Small per-building variation: brightness and slight warm/cool shift.
	float k = (seed - 0.5) * 2.0 * seed_variation;
	base = clamp(base * (1.0 + k) + vec3(k * 0.05, 0.0, -k * 0.05), 0.0, 1.0);
	vec3 albedo = srgb_to_linear(base);

	// Roofs darker.
	albedo *= mix(1.0, roof_darken, v_roof);

	// Faint floor lines on walls: dark band at every floor boundary, faded with distance (anti-shimmer).
	float floor_pos = UV.y / floor_h;
	float dist_to_line = abs(fract(floor_pos + 0.5) - 0.5) * floor_h; // meters to nearest floor boundary
	float line = 1.0 - smoothstep(floor_line_width_m * 0.5, floor_line_width_m, dist_to_line);
	float line_fade = 1.0 - smoothstep(floor_line_fade_start, floor_line_fade_end, v_depth);
	albedo *= 1.0 - line * floor_line_strength * line_fade * (1.0 - v_roof);

	ALBEDO = albedo;
	ROUGHNESS = roughness_value;
	SPECULAR = 0.3;
}
