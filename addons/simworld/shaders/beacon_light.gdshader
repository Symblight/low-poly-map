shader_type spatial;
// Aircraft warning beacons on tall buildings (api 1.2 beacon_lights): one billboard quad per beacon, one MultiMesh per tile.
// Per-instance COLOR: rgb = beacon colour (red / green), a = blink phase 0..1 of the building (all its beacons flash together).
// Mobile-renderer compatible. Additive, unshaded, no depth write, depth tested at the beacon (buildings in front hide it); fog by
// hand (fog_disabled), as for the light sprites. Beacons are few, so there is no thinning; they stay a constant few pixels and
// are bright enough to bloom.
render_mode unshaded, blend_add, depth_draw_never, cull_disabled, fog_disabled, skip_vertex_transform;

#include "earth_curve.gdshaderinc"

uniform sampler2D dot_tex : source_color, filter_linear, repeat_disable;

// Size in pixels: px_near up close growing to px_far at px_end_m.
uniform float px_near = 4.0;
uniform float px_far = 5.0;
uniform float px_end_m = 10000.0;
uniform float intensity = 4.0;
// Blink: period in seconds, share of the period the beacon is on, and the brightness left while off (0 = flashing, 1 = steady).
uniform float blink_period_s = 1.5;
uniform float blink_on_share = 0.45;
uniform float off_level = 0.15;
// Distance: brightness ref^2 / (ref^2 + d^2), fade-out at the far band edge, depth fog like the sprites (WorldView.SetLightFog).
uniform float falloff_ref_m = 12000.0;
uniform float fade_start_m = 21000.0;
uniform float far_end_m = 25000.0;
uniform float fog_begin_m = 1500.0;
uniform float fog_end_m = 9000.0;
uniform float fog_curve = 1.0;
uniform float fog_light_keep = 0.6;
// Pixel footprint per metre of view depth (set from C# with the sprites' value).
uniform float px_per_depth = 0.0014;

global uniform float simworld_night_factor;
#ifdef LOD_DITHER
uniform float fade_mul : hint_range(0.0, 1.0) = 1.0; // per fading tile (one material each, see FadeMaterials); never an instance uniform
#else
const float fade_mul = 1.0;
#endif

varying vec3 v_col;
varying float v_bright;

void vertex() {
	vec3 c = (MODELVIEW_MATRIX * vec4(0.0, 0.0, 0.0, 1.0)).xyz;
	c -= (VIEW_MATRIX * vec4(0.0, earth_curve_drop((INV_VIEW_MATRIX * vec4(c, 1.0)).xyz), 0.0, 0.0)).xyz;
	float d = length(c);
	float px = mix(px_near, px_far, clamp(d / max(px_end_m, 1.0), 0.0, 1.0));
	float size_m = px * max(-c.z, 0.1) * px_per_depth;

	// Blink: smooth-edged on phase per building.
	float t = fract(TIME / max(blink_period_s, 0.05) + COLOR.a);
	float on = smoothstep(0.0, 0.04, t) * (1.0 - smoothstep(blink_on_share, blink_on_share + 0.04, t));
	float fog = pow(smoothstep(fog_begin_m, max(fog_end_m, fog_begin_m + 1.0), d), fog_curve);
	float falloff = falloff_ref_m * falloff_ref_m / (falloff_ref_m * falloff_ref_m + d * d);
	v_bright = intensity * simworld_night_factor * mix(off_level, 1.0, on) * falloff * mix(1.0, fog_light_keep, fog)
		* (1.0 - smoothstep(fade_start_m, far_end_m, d));
	v_col = COLOR.rgb;

	// Collapse while dark (no fill cost), stand the quad on its light point like the sprites, small pull towards the camera.
	size_m *= step(0.0005, v_bright);
	vec3 up_view = (VIEW_MATRIX * vec4(0.0, 1.0, 0.0, 0.0)).xyz;
	vec3 centre = c + up_view * (0.5 * size_m);
	centre *= 1.0 - min(2.0, d * 0.5) / max(length(centre), 0.1);
	VERTEX = centre + vec3(VERTEX.xy * size_m, 0.0);
}

void fragment() {
	float a = pow(texture(dot_tex, UV).a, 1.2);
	ALBEDO = v_col * v_bright * fade_mul;
	ALPHA = a;
}
