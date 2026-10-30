---
name: bench-measure
description: Run one scripted performance benchmark of the SimWorld test bench (--benchmark=low-250|climb|zoom|night-low|reconfigure|seed-low) and report frame time, CPU split, draw calls, memory and the baseline comparison in a few lines; optionally compare with an earlier result JSON. Use for any cost question (before / after a change, "is it slower", draw calls, memory) instead of a perf review agent. Needs a visible window and an idle machine.
argument-hint: "[low-250|climb|zoom|night-low|reconfigure|seed-low] [seconds=60] [release] [compare=<result.json>] [baseline]"
context: fork
agent: selftest-runner
---

You are called by the `bench-measure` skill: the one case where you run a windowed `--benchmark`. Follow these steps; never edit a project file except the temporary `.gdextension` swap in step 2, and always restore it.

Arguments: `$ARGUMENTS`. Defaults: benchmark `low-250`, `seconds=60`, debug library, no compare, no baseline update. Details of every field: `$ROOT/docs/bench/diagnostics.md` (read only if a number needs explaining).

1. **Idle check.** `sysctl -n vm.loadavg` and `sysctl -n hw.ncpu`; `pgrep -fl "Godot|cargo|rustc|dotnet" | head`. If the 1-minute load is above half the core count, or another Godot / build runs, report `MACHINE BUSY` with the numbers and stop. Do not continue on a busy machine: the numbers would be noise.
2. **Build.** Without `release`: the usual build chain (cargo build, `tools/sync_addon.sh`, dotnet build). With `release`: `cd $ROOT/rust && cargo build --release`, sync, dotnet build; then if `low-poly-map/simplane_world.gdextension` has `macos.debug` pointing at `target/debug`, copy the file to `$LOGS/gdext.bak`, point `macos.debug` at `res://../rust/target/release/libsimworld_godot.dylib` for the run, and restore the file from the backup afterwards, also on failure (`trap`). Say in the report which library ran.
3. **Tile server** (archive benchmarks only; skip for `seed-*`, which generate every tile from `res://world/two-cities/world.json` and need no server). `curl -sI http://127.0.0.1:8787/vilnius-v2.pmtiles | head -1` must say 200 or 206. If not, report `TILE SERVER DOWN: start data/serve.sh` and stop (the benchmark paths are over the real Vilnius archive).
4. **Run** from `$ROOT/low-poly-map` (timeout: seconds + 240 s for the settle phase, at most 600000 ms):
   `$G --path . -- --benchmark=<name> --seconds=<s> --quit --tile-url=http://127.0.0.1:8787/vilnius-v2.pmtiles --cache-dir=user://cache_bench --out=<name>-<UTC yyyymmdd-HHMM>.json [--update-baseline] > $LOGS/bench.log 2>&1; echo "exit $?"`
   `--tile-url` and `--cache-dir` belong to the archive benchmarks only; a `seed-*` run omits both (the benchmark sets `--world=` itself).
   `--update-baseline` only when the argument `baseline` was given AND step 1 found the machine idle. Exit 3 = the window was closed; 1 = error (collect the log lines as for a failed self-test).
5. **Read the result** (`bench_results/<out>`) with `python3 -I` and print only: `benchmark`, `build` (commit, dirty, library flavour), `machine.window`, `perf` frame_mean / p50 / p95 / p99 / max ms, avg_fps, one_percent_low_fps, hitches, frames_over_budget, draw_calls_mean / max, primitives_mean, `frame_split` means, `uploads` worst frame ms, `gc` collection count and max pause, `soak` slopes if present, `noise.before` load, and `baseline` (key, regression flag or error). With `compare=<file>`: the same fields of that file side by side and the delta in % for frame p50 / p95 / p99, draw calls and memory.

Report (your whole answer):

```
BENCH <name> <seconds>s  library <debug|release>  window <WxH>  load <before>
frame ms  mean <> p50 <> p95 <> p99 <> max <>   fps <> (1% low <>)   hitches <>
cpu split <...>   draws <mean/max>   prims <mean>   uploads worst <> ms   gc <n> (max <> ms)
baseline <key>: <ok | REGRESSION +x% p95 | none>
compare (<file>): p50 <+-%> p95 <+-%> p99 <+-%> draws <+-%>     (only with compare=)
result: <path>   log: <$LOGS path>
```

No interpretation beyond one line naming the largest change. Metal gives no GPU timestamps: `gpu_frames: 0` is normal, `other_wait` is the GPU proxy.
