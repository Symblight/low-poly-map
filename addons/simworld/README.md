# SimWorld addon

Streamed low-poly OSM city for Godot 4.7 (.NET). The Rust `WorldStreamer` extension generates the world; this addon displays it.
Contract: `docs/contract.md` of the SimWorld repo. History: `CHANGELOG.md`.

## Install

1. Copy this folder to `res://addons/simworld/` (the path matters for the `.gdextension`; asset paths inside the addon are relative to it).
2. The project must be Godot .NET (net9.0). Run `dotnet build`, then enable **Project Settings > Plugins > SimWorld**. The plugin registers the
   global shader uniforms `simworld_*` in the project settings; without the plugin `WorldView` adds them at runtime, but scenes only open
   without shader errors in the editor with the plugin on.
3. `bin/` must hold the library for your platform (`tools/build_addon.sh` in the SimWorld repo builds it; release zips carry it).

## Use

```
Node3D
  Camera3D                      (any Camera3D, or a node implementing SimWorld.IWorldCameraSource, e.g. your aircraft)
  WorldView                     (script runtime/WorldView.cs; set OriginLat, OriginLon, Camera; TileUrl empty = synthetic city)
```

- `WorldView.SetTime(localHour, nightFactor)` from your clock; the shaders read `simworld_hour` / `simworld_night_factor`.
- Subscribe to `WorldView.OriginShifted` and subtract the vector from your own world nodes (the camera source is shifted for you).
- `WorldView.Settings` takes a `WorldRenderSettings` resource; `NightLook` (resource) applies bloom and fog to your `Environment`:
  `look.ApplyStatic(env)` once, `look.Update(env, nightFactor, altitudeAgl)` per frame. Exposure, tone map, sun and sky are yours.
- Several `WorldView`s are allowed; give each its own `CacheDir`.
- Needs the Mobile (or Forward+) renderer; nothing here is Forward+-only.
