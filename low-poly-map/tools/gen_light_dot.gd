# Generates textures/light_dot_32.png: 32x32 white radial falloff in the alpha channel.
# Run once from the project root:
#   Godot --headless --path . -s res://tools/gen_light_dot.gd
# then let Godot import it (Godot --headless --path . --import) and commit the PNG + .import.
extends SceneTree

func _init() -> void:
	const N := 32
	var img := Image.create(N, N, false, Image.FORMAT_RGBA8)
	var c := (N - 1) * 0.5
	for y in N:
		for x in N:
			var r := Vector2(x - c, y - c).length() / (N * 0.5)
			var t := clampf(1.0 - r, 0.0, 1.0)
			# Smooth core with a long soft tail: (1-r)^2 weighted by smoothstep so the border is exactly 0.
			var a := t * t * (3.0 - 2.0 * t)
			a = pow(a, 1.35)
			img.set_pixel(x, y, Color(1, 1, 1, a))
	var err := img.save_png("res://textures/light_dot_32.png")
	print("light_dot_32.png saved, err=", err)
	quit()
