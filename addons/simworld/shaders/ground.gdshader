shader_type spatial;
// Flat ground (M5): landuse / park / water areas and road strips, one mesh per tile (R3 since api 1.3), one shared material.
// Mobile-renderer compatible: no Forward+ only features. Opaque, no transparency.
//
// Vertex COLOR encoding (from Rust):
//   r = class / 16 (0 grass/park, 1 forest, 2 water, 3 residential, 4 commercial, 5 industrial, 6 other landuse, 7 aeroway (seed world, api 1.7), 8 road)
//   g = roads: lit (1 for lit=yes roads, else 0); carried for later (street lamps, cars) and not used for shading: roads do not glow.
//       areas (api 1.7): countryside tint level / 8 on class 0 and 1 (0 for archive and synthetic-city areas); see seed_palette.gdshaderinc
//   b = roads: road class / 8 (0 none, 1 motorway, 2 primary, 3 secondary, 4 tertiary, 5 residential, 6 other)
//       areas (api 1.7): class 7 aeroway sub-kind / 8 (0 airfield grass, 1 runway paved, 2 runway grass, 3 taxiway, 4 apron); 0 otherwise
//   a = 1
// Roads are told apart by r == 8/16 BEFORE g or b are read (g and b mean something else on areas).
// The mesh is flat, but the shading normal is rebuilt from screen derivatives and forced to face up, so it does not
// depend on triangle winding (cull_disabled) and keeps working when a DEM later makes the ground non-flat.
render_mode cull_disabled, diffuse_burley, specular_schlick_ggx;

#include "lod_dither.gdshaderinc"
#include "earth_curve.gdshaderinc"
#include "seed_palette.gdshaderinc"

// Overall subtle darkening so the ground sits below the buildings in brightness.
uniform float darken : hint_range(0.0, 1.0) = 0.88;
// Roads are pulled this fraction of their distance towards the camera (0.00002 = 10 cm at 5 km). It is applied to the
// whole ground mesh, so Rust's small per-class y offsets keep their order, and it only fights depth-buffer precision
// at range (floors of buildings, ground classes against each other). Kept tiny so building bases are not covered.
uniform float depth_bias_per_m : hint_range(0.0, 0.0005) = 0.00002;
uniform float depth_bias_max_m = 3.0;
// Roads no longer glow; the ground reads simworld_night_factor only to take the glare off water at night (a flat shaded sea mirrors the moon as a huge blob).
global uniform float simworld_night_factor;

varying float v_depth;

vec3 srgb_to_linear(vec3 c) {
	return mix(c / 12.92, pow((c + vec3(0.055)) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}

// Fixed low-poly palette in sRGB, indexed by class 0..8.
vec3 palette(int k) {
	if (k == 0) return vec3(0.46, 0.63, 0.34); // grass / park
	if (k == 1) return vec3(0.27, 0.43, 0.25); // forest
	if (k == 2) return vec3(0.16, 0.31, 0.47); // water
	if (k == 3) return vec3(0.73, 0.71, 0.65); // residential
	if (k == 4) return vec3(0.76, 0.68, 0.63); // commercial
	if (k == 5) return vec3(0.60, 0.59, 0.63); // industrial
	if (k == 6) return vec3(0.66, 0.69, 0.58); // other landuse
	if (k == 8) return vec3(0.17, 0.17, 0.19); // road: dark asphalt
	return vec3(0.55, 0.60, 0.50);             // unknown class
}

void vertex() {
	// Tile roots are only translated, so world = model + translation (floating origin keeps this precise).
	vec3 world = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
	float drop = earth_curve_drop(world);
	VERTEX.y -= drop;
	world.y -= drop;
	vec3 to_cam = CAMERA_POSITION_WORLD - world;
	float d = length(to_cam);
	float bias = min(d * depth_bias_per_m, depth_bias_max_m);
	VERTEX += (to_cam / max(d, 0.001)) * bias; // model space == world space orientation
	v_depth = d;
}

void fragment() {
#ifdef LOD_DITHER
	if (!lod_dither_keep(FRAGCOORD.xy)) discard;
#endif
	int cls = int(floor(COLOR.r * 16.0 + 0.5));
	int road_class = int(floor(COLOR.b * 8.0 + 0.5));
	bool is_road = cls == 8;

	vec3 base = palette(cls);
	// Seed world (api 1.7): airport surfaces by sub-kind, countryside tint on grass / forest areas. Never on roads (cls 8, g = lit).
	bool is_aero = cls == 7;
	if (is_aero) base = seed_aeroway_color(road_class);
	else if (cls <= 1) base = seed_tint(base, cls, int(floor(COLOR.g * 8.0 + 0.5)));
	// Motorways / primary roads a hair lighter than side streets (road class 1 = motorway ... 6 = other).
	if (is_road) base += vec3(0.03) * clamp(float(4 - road_class), 0.0, 3.0) * 0.5;
	vec3 albedo = srgb_to_linear(base) * darken;

	// Flat shading normal, facing up (see the header).
	vec3 n = normalize(cross(dFdx(VERTEX), dFdy(VERTEX)));
	vec3 up_view = (VIEW_MATRIX * vec4(0.0, 1.0, 0.0, 0.0)).xyz;
	if (dot(n, up_view) < 0.0) n = -n;
	NORMAL = n;

	bool is_water = cls == 2;
	ALBEDO = albedo;
	ROUGHNESS = is_water ? 0.18 : ((is_road || (is_aero && road_class != 0 && road_class != 2)) ? 0.75 : 0.92);
	SPECULAR = is_water ? mix(0.8, 0.12, simworld_night_factor) : 0.25;
}
