---
name: bench-check
description: Build the SimWorld chain (cargo build, addon sync, dotnet build) and run the headless Godot self-tests of the test bench, reporting PASS / FAIL per test with only the failing log lines. Use after a change to rust/, addons/simworld/ or low-poly-map/ to verify it in the real host, or when asked to run the self-tests.
argument-hint: "[quick|all|rust|<test names: smoke cover fade window lod perf geodetic settings terrain-collision terrain seed>] [no-build]"
context: fork
agent: selftest-runner
---

Run the SimWorld build chain and self-tests as described in your instructions.

Selection: `$ARGUMENTS`
- empty or `quick`: smoke, fade, window, perf, lod, cover.
- `all`: every Godot self-test in table order.
- `rust`: `cargo test` only (counts against the baseline in `docs/status.md`), no Godot runs.
- test names: only those.
- `no-build` anywhere: skip cargo build and dotnet build, but still run `tools/sync_addon.sh`.

Return only the report in your fixed format.
