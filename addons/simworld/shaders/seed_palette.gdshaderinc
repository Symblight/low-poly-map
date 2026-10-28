// World seed palette (api 1.7), shared by ground.gdshader (vector ground, COLOR r = class / 16) and terrain.gdshader (terrain COLOR r = class / 8)
// so the two meet at the vector reach in one colour. All colours are plain sRGB uniforms (no source_color hint: the shaders convert once,
// themselves); WorldRenderSettings.Seed* writes them on both materials.
//
// Ground classes with a seed world: 0 grass / farmland / meadow, 1 forest, 2 water, 7 aeroway (sub-kind in COLOR b: 0 airfield grass inside the
// fence, 1 paved runway, 2 grass runway, 3 taxiway, 4 apron / car park).
// Countryside tint: COLOR g = tint level / 8 (0..7) on class 0 and 1 areas of a seed world. Level 0 is the plain class colour (so an archive
// or synthetic-city area, whose g is 0, is untouched); levels 1..7 shift towards a crop / meadow colour and vary the brightness.

// Class colours (the ground shader's palette(0..2); keep equal to it).
uniform vec3 seed_grass_color = vec3(0.46, 0.63, 0.34);
uniform vec3 seed_forest_color = vec3(0.27, 0.43, 0.25);
uniform vec3 seed_water_color = vec3(0.16, 0.31, 0.47);
// Airports.
uniform vec3 seed_airfield_color = vec3(0.50, 0.67, 0.36);
uniform vec3 seed_runway_color = vec3(0.19, 0.19, 0.20);
uniform vec3 seed_runway_grass_color = vec3(0.56, 0.71, 0.38);
uniform vec3 seed_taxiway_color = vec3(0.30, 0.30, 0.32);
uniform vec3 seed_apron_color = vec3(0.56, 0.56, 0.57);
// Countryside tint ramp: meadow (light green) to ripe crops (golden); forest goes to a lighter, yellower green.
uniform vec3 seed_meadow_color = vec3(0.56, 0.74, 0.34);
uniform vec3 seed_field_color = vec3(0.80, 0.72, 0.36);
uniform vec3 seed_forest_light_color = vec3(0.36, 0.50, 0.24);
uniform float seed_tint_strength : hint_range(0.0, 1.0) = 0.75;
uniform float seed_tint_value_var : hint_range(0.0, 0.5) = 0.16;

vec3 seed_aeroway_color(int sub) {
	if (sub == 1) return seed_runway_color;
	if (sub == 2) return seed_runway_grass_color;
	if (sub == 3) return seed_taxiway_color;
	if (sub == 4) return seed_apron_color;
	return seed_airfield_color;
}

// Tint of a countryside area (sRGB in, sRGB out); cls 0 grass / farmland / meadow, 1 forest. Level 0 returns `base` unchanged.
vec3 seed_tint(vec3 base, int cls, int level) {
	if (level <= 0) return base;
	float t = float(level) / 7.0;
	float v = fract(float(level) * 0.618034 + 0.17); // well spread 0..1 per level
	vec3 c = base;
	if (cls == 0) c = mix(base, mix(seed_meadow_color, seed_field_color, smoothstep(0.25, 1.0, t)), seed_tint_strength);
	else if (cls == 1) c = mix(base, seed_forest_light_color, seed_tint_strength * t * 0.7);
	return c * (1.0 + seed_tint_value_var * (v * 2.0 - 1.0));
}
