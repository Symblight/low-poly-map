# Spec: Night Rendering — GTA V Lessons

2026-10-26 · Aleksei

> Source: https://claude.ai/artifact/HpzXEiRh1VS71asBF2ER4d

## Purpose and scope

This spec turns the useful parts of the GTA V graphics study into concrete rendering tasks for the streamed-city spike. It covers how the city looks at night: LOD transitions, bloom, exposure, depth precision and fog. Streaming, tiles and LOD band logic stay in [Spike: Streamed OSM City with Night Lights](spike-streamed-osm-city.md) ([source](https://claude.ai/artifact/1Li6b4rWTHqGtKGDYQZE6E)).

**Constraints**

- Godot 4 Mobile renderer (forward), desktop first, iPhone 17 later.
- Everything must fit the spike budgets: ≤ 300 draw calls, ≤ 2 ms main-thread upload, 30 fps sustained on iPhone.
- Pilot view behaves like human eyes; external view may behave like a camera.

**Rule of thumb from GTA V:** spend effort on what is visible at distance (lights, bloom, fog) and fake everything else. Expensive camera effects (depth of field, shadows at night, deferred lighting) are out.

## Subagents

Work is split between the two project subagents (`.claude/agents/`). "Owner" in the tables below names the subagent.

| Subagent | Area | Owns in this spec |
|----------|------|-------------------|
| `godot-csharp-integrator` | This Godot project: scenes, C# scripts, shaders | Building shader fade, glow, tone map, exposure, camera planes, fog |
| `rust-world-streamer` | `../rust/` (the `simplane_world` crate) | Keeping outgoing tiles alive during fades, `fade` in the ready-tile dictionary |

Any change to the ready-tile dictionary (here: the new `fade` key) is a contract change: it must be written identically into both agent files before either side implements it.

## In the spike

Six techniques go into the spike; each maps to a Godot feature or a small shader change.

| Technique | What GTA V does | Our implementation | Owner |
|-----------|-----------------|--------------------|-------|
| Dithered LOD fades | Discards every 2nd pixel in a checkerboard while a mesh switches LOD, then smooths the pattern in post ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | `fade` uniform in the building shader; discard by screen-pixel pattern during R0↔R1↔R2 switches | `godot-csharp-integrator` (shader), `rust-world-streamer` (tile lifetime, `fade` value) |
| Fog hides far detail | Fog covers the low detail of distant low-poly buildings ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | Depth fog + height fog tuned so R2 blocks never read as boxes | `godot-csharp-integrator` |
| Bloom on bright pixels only | Bright-pass, blur down to 1/16, back up to 1/2, add before tone map ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | `WorldEnvironment` glow, HDR threshold just below window emission | `godot-csharp-integrator` |
| Filmic tone map | Uncharted 2 filmic curve, keeps blacks from washing out ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | Godot Filmic or AgX tone map; compare both at night | `godot-csharp-integrator` |
| Eye adaptation | Exposure drifts slowly; dark→bright adapts faster than bright→dark ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | Auto exposure, slow speeds; external view only at first | `godot-csharp-integrator` |
| Depth precision + draw order | Reversed log depth for long range; front-to-back draws for early depth rejection ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | Near plane ≥ 0.5 m outside the cockpit, far 30 km; opaque building materials, no discard outside fades | `godot-csharp-integrator` (camera, materials), `rust-world-streamer` (ground/roof offsets that avoid Z-fighting) |

**Dithered fade details**

- Rust adds no geometry for fades: the outgoing and incoming tiles both exist for ~0.5 s and swap pattern parity (even pixels vs odd pixels).
- `fade` goes 0→1 over the hysteresis gap of the band, driven by slant distance, not by time, so hovering doesn't freeze half-faded tiles.
- Light sprites never dither; they use additive alpha, which is already order-free.

**Bloom and exposure targets**

- Only lit windows, light sprites and runway lights may cross the bloom threshold. Concrete, roofs and sky must stay below it.
- At cruise (10 km), the city should read as a soft glow carpet: glow intensity rises with altitude (see the spike spec).

## Later, and what we skip

Three ideas wait until after the spike; five are out because they cost too much for a phone or don't fit a pilot's view.

**Later**

| Feature | What GTA V does | Our use |
|---------|-----------------|---------|
| Anamorphic streaks | Sprite streaks only on very bright sources, such as headlights facing the camera ([Part 3](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-3/)) | Runway and approach lights, external view |
| Low-res reflections | 128×128 cubemap per face, ~30 draw calls, scenery only ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | City reflections on the fuselage and on water, from R2 meshes |
| Vector minimap | Square tiles, one draw call each, roads as vector meshes ([Part 1](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)) | The ND draws roads, coastlines and airports from the same `.pmtiles` |

**Skip**

| Effect | Why not |
|--------|---------|
| Depth of field | 2-pass compute blur at full resolution ([Part 3](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-3/)): too heavy for iPhone, and pilots don't see DOF. Sprite softness, fog and bloom give the distance blur. |
| Lens flares in the cockpit | GTA shows almost none in first-person view, since that's eyes, not a camera ([Part 3](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-3/)). Allowed in external view only. |
| Deferred lighting | Mobile renderer is forward; real lights stay limited to a few near the airport. |
| Cascaded shadows, SSAO | Off at night; daytime shadows are a separate task. |
| Light shafts, subsurface scattering | No visible gain from the air at night. |

## Tasks and acceptance

These tasks slot into the spike's "Night" milestone (milestone 6).

**Tasks**

| Task | Owner |
|------|-------|
| Contract: add `fade` (float 0..1) to the ready-tile dictionary in both agent files | both, before implementation |
| Building shader: `fade` uniform + checkerboard discard with parity per tile | `godot-csharp-integrator` |
| Rust: keep the outgoing tile alive until its replacement's fade completes; expose `fade` per tile in the ready dictionary | `rust-world-streamer` |
| Glow: HDR threshold set from window emission; altitude-driven intensity | `godot-csharp-integrator` |
| Tone map: A/B Filmic vs AgX at night, pick one | `godot-csharp-integrator` |
| Auto exposure in external view, slow adaptation | `godot-csharp-integrator` |
| Camera: near ≥ 0.5 m external, far 30 km; check Z-fighting on R2 roofs at 10 km | `godot-csharp-integrator` |
| Fog: depth + height, tuned so R2 blocks never look like boxes | `godot-csharp-integrator` |

**Acceptance**

- [ ] No visible pop during band switches at 250 m/s, judged from a screen recording at 1× speed
- [ ] At night, only windows, sprites and runway lights bloom
- [ ] No flicker or Z-fighting on roofs or ground at any altitude up to 12 km
- [ ] At 10 km, the city reads as a soft glow carpet, not a dot grid
- [ ] Rendering changes add ≤ 1 ms GPU time on desktop vs the spike baseline

**Open points against the current code**

- The fade bullets mix two drivers: tiles overlap "for ~0.5 s" (time) but `fade` is "driven by slant distance, not by time". Decide which one ends the overlap before the contract is written.
- `fade` per tile changes over time while a tile is held, but the ready dictionary is only sent on delivery. Either Rust re-delivers (expensive: meshes) or the contract adds a separate per-frame call such as `poll_fades()` returning key → fade.
- Today the sibling-swap rule hides a parent the moment all 4 children are present, and `poll_unload` removes it in the same frame. A fade needs both visible at once, which changes contract guarantee 3 and the Godot visibility rule.
- The scene camera uses far = 32 km and the light sprites fade out at 25 km; the spec says far 30 km.

## Sources

- [GTA V Graphics Study, Part 1: Dissecting a Frame](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study/)
- [GTA V Graphics Study, Part 2: LOD and Reflections](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-2/)
- [GTA V Graphics Study, Part 3: Post-Effects](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-3/)
