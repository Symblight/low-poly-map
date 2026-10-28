shader_type spatial;
// Runway markings (world seed, api 1.7): white quads laid on the runway by the host from airports() (AirportMarkings.cs), one mesh for the
// airports near the camera. The ground has no UV, so markings are geometry, not a texture.
// Mobile-renderer compatible, opaque. The mesh y is ground_height_at (linear between the landing thresholds). The drawn runway is a little above it: since
// generator 2 (contract 9 "Airports") the ground inside an airport fence has no drape lift, so it is stacking level 3 x ground.rN.lift_step_m (0.06 m in
// R0, 0.18 / 0.45 / 1.5 m in R1..R3 with the default profile), nothing else; a generator-1 library drew it at the band's drape lift + level 10 x step
// (1.2 / 3.6 / 9.5 / 25 m) and WorldView.ComputeRunwayLifts still replays that for it. ring_lift is that height per ring; the ring is chosen by view
// distance with the band hysteresis taken the safe way (ring_edge = edge x (2 - hysteresis): beyond it the larger lift, so a marking may float a
// little above a finer ground tile but is never buried). lift_m is the margin over it, and the quads are also pulled
// towards the camera by pull_per_m x distance (along the view ray, invisible on screen) against depth precision. They dither out between
// fade_start_m and fade_end_m (R2 begins at 2.5 km); beyond that the runway's ground class reads on its own.
render_mode cull_disabled, diffuse_burley, specular_schlick_ggx;

#include "earth_curve.gdshaderinc"

uniform vec3 marking_color = vec3(0.92, 0.92, 0.90); // sRGB
uniform float lift_m = 0.12;
uniform vec4 ring_lift = vec4(0.06, 0.18, 0.45, 1.5); // drawn runway above ground_height_at per ring R0..R3 (level 3 x lift_step_m)
uniform vec3 ring_edge = vec3(510.0, 2125.0, 5950.0); // view distance beyond which ring 1, 2, 3 lift applies
uniform float pull_per_m = 0.0006;
uniform float fade_start_m = 2200.0;
uniform float fade_end_m = 3200.0;

varying float v_keep;

const float MARK_BAYER4[16] = float[16](
	0.0, 8.0, 2.0, 10.0,
	12.0, 4.0, 14.0, 6.0,
	3.0, 11.0, 1.0, 9.0,
	15.0, 7.0, 13.0, 5.0);

void vertex() {
	vec3 world = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
	float drop = earth_curve_drop(world);
	vec3 to_cam0 = CAMERA_POSITION_WORLD - world;
	float d0 = length(to_cam0);
	float lift = lift_m + (d0 < ring_edge.x ? ring_lift.x : d0 < ring_edge.y ? ring_lift.y : d0 < ring_edge.z ? ring_lift.z : ring_lift.w);
	VERTEX.y += lift - drop;
	world.y += lift - drop;
	vec3 to_cam = CAMERA_POSITION_WORLD - world;
	float d = length(to_cam);
	VERTEX += (to_cam / max(d, 0.001)) * min(d * pull_per_m, 6.0); // model space == world orientation (tile hosts only translate)
	v_keep = 1.0 - smoothstep(fade_start_m, max(fade_end_m, fade_start_m + 1.0), d);
}

void fragment() {
	int x = int(mod(FRAGCOORD.x, 4.0));
	int y = int(mod(FRAGCOORD.y, 4.0));
	float p = (MARK_BAYER4[y * 4 + x] + 0.5) / 16.0;
	if (p >= v_keep) discard;
	vec3 c = marking_color;
	ALBEDO = mix(c / 12.92, pow((c + vec3(0.055)) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
	ROUGHNESS = 0.8;
	SPECULAR = 0.2;
	NORMAL = normalize(NORMAL);
}
