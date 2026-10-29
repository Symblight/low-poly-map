// H0 spike (throwaway): CDLOD grid vertex-texture-fetch test. Defines are prepended by GridSpike.cs:
//   BASELINE   per-vertex CUSTOM0/CUSTOM1 instead of INSTANCE_CUSTOM/COLOR (one merged mesh)
//   TEST_EXACT writes fetch error / custom / colour comparison flags to ALBEDO
//   TEST_RAMP  writes the fetched height to ALBEDO
shader_type spatial;
render_mode unshaded, cull_back, depth_draw_opaque;

#include "res://addons/simworld/shaders/earth_curve.gdshaderinc"

uniform sampler2DArray heights : filter_nearest, repeat_disable;
uniform float morph_start = 6500.0;
uniform float morph_end = 8500.0;
uniform int check_custom = 0;
uniform int check_color = 0;
uniform float expect_offset = 0.0;

varying vec3 v_vpos;
varying float v_h;
varying vec3 v_dbg;

void vertex() {
	vec4 cust;
	vec4 icol;
#ifdef BASELINE
	cust = CUSTOM0;
	icol = CUSTOM1;
#else
	cust = INSTANCE_CUSTOM;
	icol = COLOR;
#endif
	int i = int(UV.x + 0.5);
	int j = int(UV.y + 0.5);
	int layer = int(cust.x + 0.5);
	float h0 = texelFetch(heights, ivec3(i + 1, j + 1, layer), 0).r;
#ifdef TEST_EXACT
	uint q = uint(i * 977 + j * 131 + layer * 17 + i * j * 3) & 0xFFFFu;
	float e = float(q) / 65535.0 + expect_offset;
	float cv = cust.y;
	float ce = 2000.123;
	float ct = 0.001;
	if (check_custom == 1) { cv = cust.z; ce = -1234.5678; }
	else if (check_custom == 2) { cv = cust.w; ce = 0.1234567; ct = 1e-7; }
	else if (check_custom == 3) { ce = 2000.5; }
	float kv = icol.r;
	float ke = 0.123456789;
	float kt = 1e-6;
	if (check_color == 1) { kv = icol.g; ke = 1234.5; kt = 0.01; }
	else if (check_color == 2) { kv = icol.b; ke = 3.14159274; kt = 1e-5; }
	else if (check_color == 3) { ke = 0.5; }
	v_dbg = vec3(abs(h0 - e) * 65535.0 / 255.0, abs(cv - ce) > ct ? 1.0 : 0.0, abs(kv - ke) > kt ? 1.0 : 0.0);
	v_h = h0;
	VERTEX.y = 0.0;
#else
#ifdef TEST_RAMP
	v_h = h0;
	v_dbg = vec3(0.0);
	VERTEX.y = 0.0;
#else
	int ox = i & 1;
	int oy = j & 1;
	float ha = texelFetch(heights, ivec3(i + 1 - ox, j + 1 - oy, layer), 0).r;
	float hb = texelFetch(heights, ivec3(i + 1 + ox, j + 1 + oy, layer), 0).r;
	float hc = texelFetch(heights, ivec3(i + 1 - ox, j + 1 + oy, layer), 0).r;
	float hp = 0.5 * (ha + hb);
	vec3 wp = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
	float dist = distance(wp, simworld_curve_center);
	float morph = clamp((dist - morph_start) / (morph_end - morph_start), 0.0, 1.0);
	// host stitch flags (bit 1 W, 2 E, 4 N, 8 S): edge odd vertices snap to the coarser neighbour
	int flags = int(icol.r + 0.5);
	bool snap = ((flags & 1) != 0 && i == 0 && oy == 1) || ((flags & 2) != 0 && i == 64 && oy == 1)
			|| ((flags & 4) != 0 && j == 0 && ox == 1) || ((flags & 8) != 0 && j == 64 && ox == 1);
	if (snap) morph = 1.0;
	float h = mix(h0, hp, morph);
	float hm = cust.y + h * (cust.z - cust.y);
	VERTEX.y = hm;
	VERTEX.y -= earth_curve_drop((MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz);
	v_h = hm + (hc - hc); // keep hc live without changing the value
	v_dbg = vec3(hc);
#endif
#endif
	v_vpos = (MODELVIEW_MATRIX * vec4(VERTEX, 1.0)).xyz;
}

void fragment() {
#ifdef TEST_EXACT
	ALBEDO = v_dbg;
#else
#ifdef TEST_RAMP
	ALBEDO = vec3(v_h);
#else
	vec3 n = normalize(cross(dFdx(v_vpos), dFdy(v_vpos)));
	if (n.z < 0.0) n = -n;
	vec3 l = normalize((VIEW_MATRIX * vec4(0.4, 0.8, 0.3, 0.0)).xyz);
	float d = max(dot(n, l), 0.0) * 0.8 + 0.2;
	float t = clamp(v_h / 400.0, 0.0, 1.0);
	ALBEDO = mix(vec3(0.2, 0.5, 0.2), vec3(0.7, 0.65, 0.5), t) * d;
#endif
#endif
}
