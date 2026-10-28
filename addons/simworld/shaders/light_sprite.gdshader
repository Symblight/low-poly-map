shader_type spatial;
// Point sprites of a tile: building lights, street lamps, airport lights and aircraft warning beacons. The tile's lights are ONE mesh of PRIMITIVE_POINTS, one
// vertex per light (20 bytes: position, COLOR, CUSTOM0), no billboard quad: the size comes from POINT_SIZE in pixels and the round falloff from POINT_COORD.
// Per vertex: COLOR = rgb + weight (below); CUSTOM0 (RGBA8) = r: brightness scale x 32 / 255 (1 except for airport lights, whose Rust intensity weight scales it),
// g: kind (KIND_*; above KIND_BEACON = padding vertex, not drawn), b / a: low / high byte of the light's index within its kind in the ready-tile arrays (seeds the
// hashes below, so the light pattern does not depend on how the kinds are packed).
// Mobile-renderer compatible (Metal and Vulkan; D3D12 needs Godot's point size emulation). Additive, unshaded, no depth write; fog is applied by hand
// (fog_disabled) because additive blending of a fogged colour would add the fog colour instead of dimming the light.
// Energy model (like GTA V's distant light bulbs): a sprite stays a few pixels at every distance and its brightness falls with
// distance and fog, so the light carpet towards the horizon, where kilometres of city share one pixel row, sums to a soft band
// instead of saturating to a white wall. Sprites are depth tested at their light position, so nearer buildings hide them.
//
// COLOR (8 bits per channel): rgb = light colour, a = intensity weight from Rust for the three light kinds, clamp(sqrt(count) / 8, 0.125, 1)
// (a single light is 0.125, clusters, 100 m cells above ~3 km AGL, are higher); for a beacon a = blink phase 0..1.
render_mode unshaded, blend_add, depth_draw_never, cull_disabled, fog_disabled, skip_vertex_transform;

#include "earth_curve.gdshaderinc"

const int KIND_LIGHT = 0;
const int KIND_STREET = 1;
const int KIND_AIRPORT = 2;
const int KIND_BEACON = 3;

uniform sampler2D dot_tex : source_color, filter_linear, repeat_disable;

// Per kind (index = KIND_*), written by the host (WorldRenderSettings / SpriteMaterials):
//   kind_look: x = px_min (size in pixels close), y = px_max (size at px_end_m, then constant), z = intensity, w = color_variation (0 = profile colours only)
//   kind_gate: x = blend_start_m, y = blend_end_m (window / sprite cross-fade over view depth, docs/spec-night-windows.md W4: closer than blend_start_m the
//              point collapses because the windows carry the building; below zero = no cross-fade), z = graze_min, w = graze_max (grazing-angle thinning,
//              see below; below zero = none). Beacons use only kind_look: no cross-fade, no thinning.
uniform vec4 kind_look[4];
uniform vec4 kind_gate[4];

// Size in pixels by slant distance: px_min close, growing to px_max at px_end_m, then constant (never bigger far away). max_point_px caps a close cluster.
uniform float px_end_m = 7000.0;
uniform float max_point_px = 64.0;
uniform float far_end_m = 25000.0;
// Lights fade out completely at far_end_m so nothing pops at the LOD edge.
uniform float fade_start_m = 21000.0;

// Cluster weight (alpha) scales brightness, not the pixel size: weight 0.125 = single light, 1.0 = >= 64 lights.
uniform float cluster_bright_gain = 0.8;
// World-space radius per unit of cluster weight (100 m cells): keeps a cluster from being a pinpoint when close.
uniform float cluster_world_m = 40.0;
// Clusters sit at the mean of their 100 m cell, which reads as a regular lattice from cruise altitude: shift each one by a fixed
// per-light offset of up to this many metres on x / z (single lights, weight 0, stay where they are).
uniform float cluster_jitter_m = 35.0;

// Footprint of one screen pixel per metre of view depth: 2 * tan(fov / 2) / viewport_height_px. Set from C# (WorldView) whenever the camera fov or
// the window size changes; the built-in PROJECTION_MATRIX / VIEWPORT_SIZE did not give a usable value in this vertex stage.
uniform float px_per_depth = 0.0014;

// Distance falloff ref^2 / (ref^2 + d^2): about 1 up close, inverse square far away (constant pixel size, so the per-pixel
// brightness of one light falls the way a real point light's does).
uniform float falloff_ref_m = 6000.0;
// Depth fog, the same curve as the environment's (NightLook FogBeginM / FogEndM / FogCurve, written through
// WorldView.SetLightFog): fog = pow(smoothstep(begin, end, d), curve). Lights keep fog_light_keep of their brightness in full
// fog, because a light punches through haze much further than an unlit wall stays visible.
uniform float fog_begin_m = 1500.0;
uniform float fog_end_m = 9000.0;
uniform float fog_curve = 1.0;
uniform float fog_light_keep = 0.6;
// Distance blur (haze): beyond blur_start_m a sprite widens towards blur_px at blur_end_m with a softer profile, and its
// brightness drops by the area it gained (px / px_blurred)^2, so the energy per light stays the same: far lights soften into a
// glow instead of stacking into a brighter band.
uniform float blur_start_m = 3000.0;
uniform float blur_end_m = 15000.0;
uniform float blur_px = 7.0;
// Altitude thinning: from alt_thin_start_m to alt_thin_end_m of camera height above the lights the share of lights kept falls to
// alt_keep_min (same per-light threshold as the grazing thinning, so lights drop out one by one); the kept ones get brighter by
// 1 / sqrt(share), so the carpet thins out and dims a little instead of turning into a dense dot screen. It ends where Rust's 100 m
// clusters take over (lights.cluster_on_agl_m, 3300 m): clusters stand for many lights and are thinned only as far as they are small.
uniform float alt_thin_start_m = 800.0;
uniform float alt_thin_end_m = 3300.0;
uniform float alt_keep_min = 1.0; // 1 = off (dense far carpet, like aerial photos); e.g. 0.55 thins it
// Grazing-angle thinning: elevation = (camera height - light height) / distance. Below graze_max (kind_gate.w) lights start to drop out,
// each at its own fixed random threshold (with a thin_soft cross-fade, no popping), and below graze_min (kind_gate.z) none are left. From
// low altitude this ends the carpet kilometres before the 25 km band edge, where it would only stack into the horizon line;
// from cruise altitude the angles stay steep and every light is kept (Rust's 100 m clusters already cut the count there).
uniform float thin_soft = 0.15;
// Per-light colour variety (building lights only: the other kinds have color_variation 0; street lamps vary per road in Rust).
// A fixed hash of the light's index picks: cool_share of the lights take cool_tint (LED / TV light), amber_share take amber_tint
// (old bulbs, curtains), the rest keep the profile colour; every light also gets +-brightness_jitter. color_variation scales the
// tint strength (0 = profile colours only).
uniform vec3 cool_tint = vec3(0.62, 0.78, 1.0);
uniform vec3 amber_tint = vec3(1.0, 0.5, 0.16);
uniform float cool_share = 0.22;
uniform float amber_share = 0.14;
uniform float brightness_jitter = 0.3;
// Fixed pull towards the camera against z-fighting with the own roof; small, so buildings in front still hide the light.
uniform float lift_m = 2.0;

// Beacons: the size grows from kind_look.x to kind_look.y pixels at beacon_px_end_m; blink period in seconds, share of the period the beacon is on and
// the brightness left while off (0 = flashing, 1 = steady); brightness falloff reference distance.
uniform float beacon_px_end_m = 10000.0;
uniform float beacon_falloff_ref_m = 12000.0;
uniform float blink_period_s = 1.5;
uniform float blink_on_share = 0.45;
uniform float off_level = 0.15;

// 0 = off (day), 1 = full (night). Global, set through WorldView.SetTime (the host owns the clock).
global uniform float simworld_night_factor;
// Per-tile LOD transition weight from poll_fades (1 = steady). Sprites never dither: both sides of a swap are additive,
// so scaling brightness by the complementary fades keeps the sum constant instead of flashing double-bright.
#ifdef LOD_DITHER
uniform float fade_mul : hint_range(0.0, 1.0) = 1.0; // per fading tile (one material each, see FadeMaterials); never an instance uniform
#else
const float fade_mul = 1.0;
#endif

varying vec3 v_col;
varying float v_bright; // the whole brightness but fade_mul
varying float v_pow;    // exponent of the dot texture's alpha: sharp core close, soft haze far

void vertex() {
	int kind = int(CUSTOM0.g * 255.0 + 0.5);
	float live = kind <= KIND_BEACON ? 1.0 : 0.0; // padding vertices of the tile's array are not drawn
	kind = min(kind, KIND_BEACON);
	vec4 look = kind_look[kind];
	// Earth curvature: drop the point in world space (via the view matrices, which hold no instance transform).
	vec3 c = (MODELVIEW_MATRIX * vec4(VERTEX, 1.0)).xyz;
	c -= (VIEW_MATRIX * vec4(0.0, earth_curve_drop((INV_VIEW_MATRIX * vec4(c, 1.0)).xyz), 0.0, 0.0)).xyz;
	float d = length(c);
	float depth = max(-c.z, 0.1);
	float world_per_px = depth * px_per_depth; // world size of one pixel at this depth (perspective projection, vertical fov)
	float fog = pow(smoothstep(fog_begin_m, max(fog_end_m, fog_begin_m + 1.0), d), fog_curve);
	float size_px;
	vec2 jit = vec2(0.0);

	if (kind == KIND_BEACON) {
		size_px = mix(look.x, look.y, clamp(d / max(beacon_px_end_m, 1.0), 0.0, 1.0));
		// Blink: smooth-edged on phase per building.
		float t = fract(TIME / max(blink_period_s, 0.05) + COLOR.a);
		float on = smoothstep(0.0, 0.04, t) * (1.0 - smoothstep(blink_on_share, blink_on_share + 0.04, t));
		float falloff = beacon_falloff_ref_m * beacon_falloff_ref_m / (beacon_falloff_ref_m * beacon_falloff_ref_m + d * d);
		v_bright = look.z * simworld_night_factor * mix(off_level, 1.0, on) * falloff * mix(1.0, fog_light_keep, fog)
			* (1.0 - smoothstep(fade_start_m, far_end_m, d));
		v_col = COLOR.rgb;
		v_pow = 1.2;
		size_px *= step(0.0005, v_bright); // collapse while dark (no fill cost)
	} else {
		vec4 gate = kind_gate[kind];
		float px = mix(look.x, look.y, clamp(d / max(px_end_m, 1.0), 0.0, 1.0));
		float w = clamp((COLOR.a - 0.125) / 0.875, 0.0, 1.0); // 0 single light .. 1 big cluster
		float blur = smoothstep(blur_start_m, max(blur_end_m, blur_start_m + 1.0), d);
		float px_blurred = max(mix(px, blur_px, blur), px);
		float blur_energy = (px * px) / (px_blurred * px_blurred);
		px = px_blurred;
		size_px = min(max(px, cluster_world_m * w / world_per_px), max_point_px);
		// Thinning: a per-light threshold in [0, 1) (integer hash of the light's index, stable across origin shifts) against the
		// share of lights kept at this viewing angle.
		vec3 light_world = (INV_VIEW_MATRIX * vec4(c, 1.0)).xyz;
		float elev = (CAMERA_POSITION_WORLD.y - light_world.y) / max(d, 1.0);
		float alt_share = mix(1.0, mix(alt_keep_min, 1.0, w), smoothstep(alt_thin_start_m, max(alt_thin_end_m, alt_thin_start_m + 1.0), CAMERA_POSITION_WORLD.y - light_world.y));
		float keep_share = smoothstep(gate.z, max(gate.w, gate.z + 1e-4), elev) * alt_share;
		uint index = uint(CUSTOM0.b * 255.0 + 0.5) | (uint(CUSTOM0.a * 255.0 + 0.5) << 8u);
		uint h = index * 747796405u + 2891336453u;
		h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
		float threshold = float((h >> 22u) ^ h) / 4294967295.0;
		// Two more independent draws from the same hash: tint pick and brightness.
		uint h2 = h * 1664525u + 1013904223u;
		h2 = ((h2 >> ((h2 >> 28u) + 4u)) ^ h2) * 277803737u;
		float pick = float((h2 >> 22u) ^ h2) / 4294967295.0;
		uint h3 = h2 * 1664525u + 1013904223u;
		h3 = ((h3 >> ((h3 >> 28u) + 4u)) ^ h3) * 277803737u;
		float jitter = float((h3 >> 22u) ^ h3) / 4294967295.0 * 2.0 - 1.0;
		float keep = clamp((keep_share * (1.0 + thin_soft) - threshold) / thin_soft, 0.0, 1.0);
		size_px *= step(0.001, keep); // dropped lights collapse (no fill cost)
		// Window cross-fade: invisible (collapsed, no fill cost) while the windows carry the building.
		float blend = smoothstep(gate.x, max(gate.y, gate.x + 1.0), depth);
		size_px *= step(0.002, blend);
		jit = vec2(pick * 2.0 - 1.0, jitter) * cluster_jitter_m * step(0.001, w);

		float falloff = falloff_ref_m * falloff_ref_m / (falloff_ref_m * falloff_ref_m + d * d);
		vec3 tint = pick < cool_share ? cool_tint : (pick < cool_share + amber_share ? amber_tint : COLOR.rgb);
		v_col = mix(COLOR.rgb, tint, look.w) * (1.0 + brightness_jitter * look.w * jitter);
		float fade = (1.0 - smoothstep(fade_start_m, far_end_m, d)) * blend * keep * falloff * blur_energy * inversesqrt(max(alt_share, 0.05)) * mix(1.0, fog_light_keep, fog);
		v_bright = look.z * simworld_night_factor * fade * (1.0 + cluster_bright_gain * w) * (CUSTOM0.r * 255.0 / 32.0);
		v_pow = mix(1.35, 0.75, blur);
	}

	// Stand the point on the roof (raise it by half its size along world up) so the own flat roof, which projects below it,
	// does not cut it in half, and pull it a fixed lift_m towards the camera. The raise is about half a sprite on screen and
	// the depth stays at the light, so buildings in front still occlude it (a size-scaled pull of up to ~130 m did not).
	vec3 up_view = (VIEW_MATRIX * vec4(0.0, 1.0, 0.0, 0.0)).xyz;
	vec3 centre = c + up_view * (0.5 * size_px * world_per_px);
	centre += (VIEW_MATRIX * vec4(jit.x, 0.0, jit.y, 0.0)).xyz;
	centre *= 1.0 - min(lift_m, d * 0.5) / max(length(centre), 0.1);

	VERTEX = mix(vec3(0.0, 0.0, 1.0), centre, live); // a padding vertex goes behind the camera
	POINT_SIZE = size_px * live;
}

void fragment() {
	float a = pow(texture(dot_tex, POINT_COORD).a, v_pow);
	ALBEDO = v_col * (v_bright * fade_mul);
	ALPHA = a;
}
