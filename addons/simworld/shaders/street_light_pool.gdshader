shader_type spatial;
// Light pools: a soft disc of lamp light on the ground under each street lamp (api 1.1 street_lights). Drawn from the tile's street
// lamp MultiMesh itself (a second MultiMeshInstance3D on the same MultiMesh, this material as override), so no extra upload: each
// instance sits at the lamp, the disc goes lamp_height_m lower (lights.street.height_m, from world_profile()).
// Mobile-renderer compatible. Additive, unshaded, no depth write; depth tested, so buildings hide the pools behind them. A pool lies on
// the ground surface, so pools seen at grazing angles do not stack the way billboards do (a pixel sees one ground point).
// Fog by hand (fog_disabled) for the same reason as the sprites: additive blending of a fogged colour would add the fog colour.
render_mode unshaded, blend_add, depth_draw_never, cull_disabled, fog_disabled, skip_vertex_transform;

#include "earth_curve.gdshaderinc"

// Lamp height above the ground (lights.street.height_m); the pool centre is this far below the instance.
uniform float lamp_height_m = 9.0;
// Disc radius and its lift above the ground (above the road strips' z-fight lift).
uniform float radius_m = 9.0;
uniform float lift_m = 0.4;
// Peak brightness at the centre, before the lamp colour. Keep it under the glow threshold (0.82): roads glow softly, they do not bloom.
uniform float intensity = 0.22;
// Pools fade out with distance; far away the lamp sprites alone carry the street.
uniform float fade_start_m = 1200.0;
uniform float fade_end_m = 2500.0;
// Depth fog, written with the sprites' (WorldView.SetLightFog).
uniform float fog_begin_m = 1500.0;
uniform float fog_end_m = 9000.0;
uniform float fog_curve = 1.0;

global uniform float simworld_night_factor;
instance uniform float fade_mul : hint_range(0.0, 1.0) = 1.0;

varying vec3 v_col;
varying float v_bright;

void vertex() {
	vec3 lamp = (MODEL_MATRIX * vec4(0.0, 0.0, 0.0, 1.0)).xyz;
	// Unit quad (xy in -0.5..0.5) laid flat on the ground under the lamp.
	vec3 world = vec3(lamp.x, lamp.y - lamp_height_m + lift_m, lamp.z) + vec3(VERTEX.x, 0.0, VERTEX.y) * (2.0 * radius_m);
	world.y -= earth_curve_drop(world);
	float d = length(world - CAMERA_POSITION_WORLD);
	float fade = 1.0 - smoothstep(fade_start_m, max(fade_end_m, fade_start_m + 1.0), d);
	float fog = pow(smoothstep(fog_begin_m, max(fog_end_m, fog_begin_m + 1.0), d), fog_curve);
	v_bright = intensity * simworld_night_factor * fade * (1.0 - fog);
	// Day or out of range: collapse the quad (no fill cost).
	world = mix(lamp, world, step(0.0005, v_bright));
	VERTEX = (VIEW_MATRIX * vec4(world, 1.0)).xyz;
	v_col = COLOR.rgb;
}

void fragment() {
	vec2 p = UV * 2.0 - 1.0;
	float k = clamp(1.0 - dot(p, p), 0.0, 1.0);
	ALBEDO = v_col * v_bright * fade_mul;
	ALPHA = k * k * k; // bright under the lamp, long soft edge (pools stay apart at 35 m spacing)
}
