shader_type spatial;
// Scene fallback ground under the streamed tiles (the big plane in CitySpike.tscn). Plain colour, bent by the same earth
// curvature as the tiles so it stays below them at range. The plane mesh must be subdivided for the bend to show.
render_mode diffuse_burley, specular_schlick_ggx;

#include "res://shaders/earth_curve.gdshaderinc"

uniform vec4 albedo_color : source_color = vec4(0.18, 0.22, 0.17, 1.0);
uniform float roughness_value : hint_range(0.0, 1.0) = 1.0;

void vertex() {
	VERTEX.y -= earth_curve_drop((MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz);
}

void fragment() {
	ALBEDO = albedo_color.rgb;
	ROUGHNESS = roughness_value;
}
