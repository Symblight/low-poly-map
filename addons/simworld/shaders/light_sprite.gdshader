shader_type spatial;
// Distant light sprites (R2/R3): one billboard quad per light, one MultiMesh per tile.
// Mobile-renderer compatible. Additive, unshaded, no depth write; fog is applied by hand (fog_disabled)
// because additive blending of a fogged colour would add the fog colour instead of dimming the light.
//
// Per-instance COLOR (MultiMesh colours): rgb = light colour, a = intensity weight from Rust,
// clamp(sqrt(count) / 8, 0.125, 1). A single light is 0.125, clusters (100 m cells above ~3 km AGL) are higher.
render_mode unshaded, blend_add, depth_draw_never, cull_disabled, fog_disabled, skip_vertex_transform;

#include "earth_curve.gdshaderinc"

uniform sampler2D dot_tex : source_color, filter_linear, repeat_disable;

// Size in pixels by slant distance: sharp 2..4 px up to near_end_m, soft 6..12 px out to far_end_m.
uniform float near_px_min = 2.0;
uniform float near_px_max = 4.0;
uniform float far_px_min = 6.0;
uniform float far_px_max = 12.0;
uniform float soft_start_m = 6000.0;
uniform float near_end_m = 7000.0;
uniform float soft_end_m = 8000.0;
uniform float far_end_m = 25000.0;
// Lights fade out completely at far_end_m so nothing pops at the LOD edge.
uniform float fade_start_m = 21000.0;

// Cluster weight (alpha) scales brightness and size: weight 0.125 = single light, 1.0 = >= 64 lights.
uniform float cluster_size_gain = 1.2;
uniform float cluster_bright_gain = 0.8;
// World-space radius per unit of cluster weight (100 m cells): keeps a cluster from being a pinpoint when close.
uniform float cluster_world_m = 40.0;

// Footprint of one screen pixel per metre of view depth: 2 * tan(fov / 2) / viewport_height_px. Set from C#
// (WorldView) whenever the camera fov or the window size changes; the built-in PROJECTION_MATRIX /
// VIEWPORT_SIZE did not give a usable value in this vertex stage.
uniform float px_per_depth = 0.0014;

uniform float intensity = 1.6;
// 0 = off (day), 1 = full (night). Global, set through WorldView.SetTime (the host owns the clock).
global uniform float simworld_night_factor;
// Per-tile LOD transition weight from poll_fades (1 = steady). Sprites never dither: both sides of a swap are additive,
// so scaling brightness by the complementary fades keeps the sum constant instead of flashing double-bright.
instance uniform float fade_mul : hint_range(0.0, 1.0) = 1.0;

varying vec4 v_col;
varying float v_soft;
varying float v_fade;

void vertex() {
	// MODELVIEW_MATRIX includes the MultiMesh instance transform.
	vec3 c = (MODELVIEW_MATRIX * vec4(0.0, 0.0, 0.0, 1.0)).xyz;
	// Earth curvature: drop the sprite centre in world space (via the view matrices, which hold no instance transform).
	c -= (VIEW_MATRIX * vec4(0.0, earth_curve_drop((INV_VIEW_MATRIX * vec4(c, 1.0)).xyz), 0.0, 0.0)).xyz;
	float d = length(c);

	float k_near = clamp(d / near_end_m, 0.0, 1.0);
	float px_near = mix(near_px_min, near_px_max, k_near);
	float k_far = clamp((d - near_end_m) / max(far_end_m - near_end_m, 1.0), 0.0, 1.0);
	float px_far = mix(far_px_min, far_px_max, k_far);
	float soft = smoothstep(soft_start_m, soft_end_m, d);
	float px = mix(px_near, px_far, soft);

	float w = clamp((COLOR.a - 0.125) / 0.875, 0.0, 1.0); // 0 single light .. 1 big cluster
	px *= 1.0 + cluster_size_gain * w;

	// World size of one pixel at this depth (perspective projection, vertical fov).
	float depth = max(-c.z, 0.1);
	float world_per_px = depth * px_per_depth;
	float size_m = max(px * world_per_px, cluster_world_m * w);

	// Pull the sprite slightly towards the camera so a roof-top light is not cut in half by its own roof.
	float lift = min(0.5 * size_m + 2.0, d * 0.5);
	vec3 centre = c * (1.0 - lift / d);

	// View-space billboard: quad axes are the camera axes.
	VERTEX = centre + vec3(VERTEX.xy * size_m, 0.0);

	v_col = COLOR;
	v_soft = soft;
	v_fade = 1.0 - smoothstep(fade_start_m, far_end_m, d);
}

void fragment() {
	float a = texture(dot_tex, UV).a;
	// Sharp core up close, softer gentle falloff when far.
	a = pow(a, mix(1.35, 0.8, v_soft));
	float w = clamp((v_col.a - 0.125) / 0.875, 0.0, 1.0);
	float bright = intensity * simworld_night_factor * v_fade * fade_mul * (1.0 + cluster_bright_gain * w);
	// Bigger far sprites spread the same energy over more pixels.
	bright *= mix(1.0, 0.45, v_soft);
	ALBEDO = v_col.rgb * bright;
	ALPHA = a;
}
