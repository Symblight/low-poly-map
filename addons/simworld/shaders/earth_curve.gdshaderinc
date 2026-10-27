// Earth curvature (visual only), shared by every world shader: buildings, ground, light sprites, fallback ground.
//
// Rust builds a flat world on the tangent plane of the frame origin. Here each vertex drops by d^2 / 2R, d = horizontal
// distance to the camera, so the surface bends away like a sphere of radius R seen from the camera. Under the camera the
// drop is 0, so altitude above ground, physics and Rust's tile cover stay flat and unchanged.
//
// Globals (registered by the addon plugin and by WorldView at runtime), set every frame by WorldView:
//   simworld_curve_center  camera position in world space (not CAMERA_POSITION_WORLD, which is the light's camera in shadow passes)
//   simworld_curve_inv_2r  1 / (2 R) in 1/m; 0 = flat. A larger value exaggerates the curvature (test aid).
// Tile roots are only translated, so model-space y is world y and a world-space drop can be subtracted from VERTEX.y.
global uniform vec3 simworld_curve_center;
global uniform float simworld_curve_inv_2r;

float earth_curve_drop(vec3 world) {
	vec2 d = world.xz - simworld_curve_center.xz;
	return dot(d, d) * simworld_curve_inv_2r;
}
