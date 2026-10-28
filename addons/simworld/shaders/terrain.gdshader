shader_type spatial;
// Procedural terrain tiles (api 1.5, layer "terrain"): one shared material for every terrain mesh. Mobile-renderer compatible,
// opaque. The mesh carries VERTEX + INDEX only (NORMAL with terrain.shading = "smooth"), no COLOR / UV: the palette is computed here
// from the altitude above sea level and the slope of the face.
//
// Seed world (api 1.7): the mesh also carries COLOR, r = ground class / 8 (0 grass, 1 forest, 2 water incl. rivers, 7 inside an airport fence),
// g = countryside tint / 8, b = 0, a = 1 (the same classes and tint as the vector ground, seed_palette.gdshaderinc). A terrain without COLOR
// reads as white (r * 8 = 8: no class), which keeps the altitude / slope / snow palette below. Class codes are interpolated as one-hot weights
// (a linear blend of the codes themselves would turn the middle of a grass-water triangle into forest) and sharpened with a smoothstep.
//
// Winding is clockwise seen from above (Godot's front face), skirts seen from outside, so the default back-face culling is right:
// the skirts (which hide cracks between tiles of different bands) face outwards and never show from above.
render_mode diffuse_burley, specular_schlick_ggx;

#include "lod_dither.gdshaderinc"
#include "earth_curve.gdshaderinc"
#include "seed_palette.gdshaderinc"

global uniform float simworld_night_factor;

// Palette in sRGB (WorldRenderSettings.Terrain*). low = the ground shader's grass colour so the vector ground blends into it.
uniform vec4 low_color : source_color = vec4(0.46, 0.63, 0.34, 1.0);
uniform vec4 high_color : source_color = vec4(0.64, 0.60, 0.38, 1.0);
uniform vec4 rock_color : source_color = vec4(0.56, 0.51, 0.47, 1.0);
uniform vec4 snow_color : source_color = vec4(0.93, 0.95, 0.98, 1.0);
uniform float rock_slope : hint_range(0.0, 1.0) = 0.22;
uniform float rock_blend : hint_range(0.01, 0.5) = 0.10;
uniform float snow_line_m = 1400.0;
uniform float snow_blend_m = 200.0;
uniform float high_start_m = 40.0;
uniform float high_end_m = 320.0;
uniform float roughness_value : hint_range(0.0, 1.0) = 0.95;
uniform float darken : hint_range(0.0, 1.0) = 0.88;
// false = flat shading from screen derivatives (no normal array), true = the mesh NORMAL (terrain.shading = "smooth").
uniform bool use_vertex_normal = false;

varying vec4 v_class_w; // forest, water, airfield weights and "has class data" (1 for a seed world mesh)
varying float v_tint;   // COLOR g: countryside tint level / 8
varying vec3 v_model_pos; // tile-local position before the curvature drop; y = metres above sea level (tile roots sit at y = 0)

void vertex() {
	int k = int(floor(COLOR.r * 8.0 + 0.5));
	v_class_w = vec4(k == 1 ? 1.0 : 0.0, k == 2 ? 1.0 : 0.0, k == 7 ? 1.0 : 0.0, k < 8 ? 1.0 : 0.0);
	v_tint = COLOR.g;
	v_model_pos = VERTEX;
	vec3 world = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
	VERTEX.y -= earth_curve_drop(world); // tile roots are only translated, so model y == world y
}

void fragment() {
#ifdef LOD_DITHER
	if (!lod_dither_keep(FRAGCOORD.xy)) discard;
#endif
	// Face normal in tile space from the unbent position: slope is a property of the terrain, not of the visual curvature.
	// A zero cross product (edge pixels whose 2x2 quad straddles two surfaces, or a sliver triangle) must not become NaN: a NaN albedo
	// renders as a black pixel along tile seams.
	vec3 fc = cross(dFdx(v_model_pos), dFdy(v_model_pos));
	float fl = length(fc);
	vec3 fn = fl > 1e-12 ? fc / fl : vec3(0.0, 1.0, 0.0);
	if (fn.y < 0.0) fn = -fn;
	// Skirt faces (vertical walls that hide the cracks between tiles) show through 1 px seam gaps; terrain is never this steep, so they
	// take the colour and the light of the surface around them instead of the rock colour / a sideways normal.
	bool skirt = fn.y < 0.15;
	float slope = skirt ? 0.0 : 1.0 - fn.y;
	float alt = v_model_pos.y;

	bool classed = v_class_w.w > 0.5;
	float w_forest = smoothstep(0.35, 0.65, v_class_w.x);
	float w_water = smoothstep(0.4, 0.6, v_class_w.y);
	float w_fence = smoothstep(0.4, 0.6, v_class_w.z);
	vec3 col;
	if (classed) {
		// Seed world: class colours and tint like the vector ground; steep land faces still turn to rock, water and airfields stay flat.
		int level = int(floor(v_tint * 8.0 + 0.5));
		col = mix(seed_tint(seed_grass_color, 0, level), seed_tint(seed_forest_color, 1, level), w_forest);
		float land = (1.0 - w_water) * (1.0 - w_fence);
		col = mix(col, rock_color.rgb, land * smoothstep(rock_slope - rock_blend, rock_slope + rock_blend, slope));
		col = mix(col, seed_airfield_color, w_fence);
		col = mix(col, seed_water_color, w_water);
	} else {
		col = mix(low_color.rgb, high_color.rgb, smoothstep(high_start_m, high_end_m, alt));
		col = mix(col, rock_color.rgb, smoothstep(rock_slope - rock_blend, rock_slope + rock_blend, slope));
		// Snow settles on gentle faces above the line; steep faces above it stay rock.
		float snow = smoothstep(snow_line_m - snow_blend_m, snow_line_m, alt)
			* (1.0 - smoothstep(rock_slope + rock_blend, rock_slope + 3.0 * rock_blend, slope));
		col = mix(col, snow_color.rgb, snow);
	}
	vec3 albedo = mix(col / 12.92, pow((col + vec3(0.055)) / 1.055, vec3(2.4)), step(vec3(0.04045), col)) * darken;
	// Night: cooler and darker albedo (the sun / moon light and the ambient already dim it); no emission, so terrain never glows.
	albedo *= mix(vec3(1.0), vec3(0.55, 0.68, 1.0) * 0.85, simworld_night_factor);
	ALBEDO = albedo;

	if (use_vertex_normal) {
		NORMAL = normalize(NORMAL); // interpolated view-space vertex normal
	} else {
		// Flat shading: view-space face normal from derivatives of the bent view-space position, turned towards the viewer.
		vec3 nc = cross(dFdx(VERTEX), dFdy(VERTEX));
		float nl = length(nc);
		vec3 n = nl > 1e-12 ? nc / nl : -normalize(VERTEX);
		if (dot(n, VERTEX) > 0.0) n = -n;
		if (skirt) n = normalize((VIEW_MATRIX * vec4(0.0, 1.0, 0.0, 0.0)).xyz);
		NORMAL = n;
	}
	ROUGHNESS = classed ? mix(roughness_value, 0.18, w_water) : roughness_value;
	SPECULAR = classed ? mix(0.15, mix(0.8, 0.12, simworld_night_factor), w_water) : 0.15; // no moon glare on the sea (flat facets mirror it as one huge blob)
}
