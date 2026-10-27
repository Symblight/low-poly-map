// Dithered LOD fade (night-rendering spec), shared by the building and ground shaders. Never used by light sprites.
//
// Compiled in only when the including shader is built with `#define LOD_DITHER` (TilePool builds that variant and assigns
// it to a tile only while it has a poll_fades entry below 1). The plain shader therefore has no discard at all and keeps
// early depth rejection; no `fade == 1` test is needed at run time.
//
// Rule: p = 4x4 Bayer value of the screen pixel in [0, 1). fade_parity = z & 1 of the tile key; the two sides of a swap are
// one z level apart so their parities differ. Parity 0 keeps the pixel when p < fade, parity 1 when p >= 1 - fade, so with
// fade_in + fade_out = 1 the two sides cover complementary pixels.
#ifdef LOD_DITHER
instance uniform float fade : hint_range(0.0, 1.0) = 1.0;
instance uniform float fade_parity : hint_range(0.0, 1.0) = 0.0;

const float LOD_BAYER4[16] = float[16](
	0.0, 8.0, 2.0, 10.0,
	12.0, 4.0, 14.0, 6.0,
	3.0, 11.0, 1.0, 9.0,
	15.0, 7.0, 13.0, 5.0);

bool lod_dither_keep(vec2 frag_xy) {
	if (fade >= 1.0) return true;
	int x = int(mod(frag_xy.x, 4.0));
	int y = int(mod(frag_xy.y, 4.0));
	float p = (LOD_BAYER4[y * 4 + x] + 0.5) / 16.0;
	return fade_parity < 0.5 ? (p < fade) : (p >= 1.0 - fade);
}
#endif
