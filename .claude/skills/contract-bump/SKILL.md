---
name: contract-bump
description: Checklist for a WorldStreamer contract change (api_version bump) or a world-profile change in SimWorld. Walks every file that must change in the same commit (docs/contract.md, API_MAJOR/API_MINOR, CHANGELOG.md, the C# facade, default.json + schema + PROFILE_VERSION, CLAUDE.md status) and runs the tests that pin them. Use when a #[func], signal, ready-tile key, encoding, guarantee or profile key/default is added, changed or removed.
disable-model-invocation: true
argument-hint: "<minor|major|profile> <one-line summary of the change>"
---

# Contract / profile bump

Arguments: `$ARGUMENTS` (kind: `minor` = additive contract change, `major` = breaking, `profile` = profile key or default only; then a one-line summary). If the kind is missing, decide it from the diff (`git diff`) and say which you chose and why. Rules: `docs/contract.md` section 1 and `CLAUDE.md` "Architecture" (a minor bump is additive only; a breaking change is a major bump; `api_version` is independent of the crate version).

Paths are relative to the repo root (`/Users/aleksejtkacenko/Documents/gamedev/lowpolymap`). Read the current values first; never guess a number:

```bash
grep -n "^api_version:" docs/contract.md
grep -n "pub const API_MAJOR\|pub const API_MINOR" rust/simworld-core/src/lib.rs
grep -n "pub const PROFILE_VERSION" rust/simworld-core/src/profile.rs
grep -n "ExpectedApiMajor =\|LatestKnownMinor =" addons/simworld/runtime/WorldStreamerFacade.cs
grep -n "^## " CHANGELOG.md | head -3
```

## 1. Contract change (`minor` / `major`)

1. **`docs/contract.md`**
   - the `api_version: X.Y` line at the top (new value);
   - the section or table row of the change, tagged `**[X.Y]**` like the others;
   - section 1 "Version plan" table and section 12 "Planned API": if the change was planned there, move it out of 12; if another planned item held this minor, renumber it (the doc keeps a "was ..." trail, e.g. `### 1.8 — Daylight (M6; was 0.8, then ...)`);
   - every new `#[func]` / `#[signal]` of `WorldStreamer` must be named in this file (`cargo test` fails otherwise).
2. **`rust/simworld-core/src/lib.rs`**: `API_MAJOR` / `API_MINOR` equal to the contract line (a major bump resets the minor to 0).
3. **`CHANGELOG.md`**: a bullet under `## Unreleased` in the house style: bold title with `(api X.Y; contract sections ...)`, what changed, what the host must do, measured numbers if any, what is not built.
4. **C# facade** (`addons/simworld/runtime/WorldStreamerFacade*.cs`):
   - `major`: `ExpectedApiMajor` must change, or every host refuses the library; check every `HasX` gate that compares versions.
   - `minor`: raise `LatestKnownMinor` only when the host side of the change is built; gate the new call on `api >= X.Y` (pattern: `HasSetView`, `HasEngineSurfaces`) so an older library still works. A Rust-only change leaves it (the facade then prints one info line, which is fine) and the CHANGELOG bullet says so.
5. **`docs/status.md`**: the "Contract version: `api_version` X.Y (...; X.Y = <summary>)" list; **`CLAUDE.md`** (repo root): the one `api_version` / profile_version line in "Project status"; `docs/addon-notes.md` if the host side changed; `low-poly-map/CLAUDE.md`: the `LatestKnownMinor` value quoted in "Run it" if it moved.
6. **`docs/architecture.md`** only if the mechanism changed (it is an introduction; the contract wins).

## 2. Profile change (`profile`, or a contract change that adds profile keys)

1. `rust/simworld-core/src/profile.rs` (+ `profile/ranges.rs` for ranges, `profile/seed_*schema.rs` for seed sections): the key, its default, its range.
2. `addons/simworld/profiles/default.json`: the same default.
3. `addons/simworld/profiles/world-profile.schema.json`: the key with a description that names the profile_version it arrived in.
4. `PROFILE_VERSION`: bump only for a removed / renamed key or a changed meaning (a file of the old version must migrate or be refused, see the module doc of `profile.rs`); an additive key keeps it. If the current version is still unreleased (CHANGELOG `## Unreleased` already bumped it), do not bump again.
5. A changed default changes host-visible behaviour: `CHANGELOG.md` says so, and if output changes, regenerate the goldens only for an intended change: `SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture`, and list which goldens moved.

## 3. Verify (from `rust/`)

```bash
cargo test -p simworld-core api_version 2>&1 | tail -5    # api_version vs docs/contract.md
cargo test -p simworld-godot 2>&1 | tail -5               # #[func] / #[signal] lists vs docs/contract.md
cargo test -p simworld-core profile 2>&1 | tail -5        # default.json and schema vs code
cargo test 2>&1 | grep -E '^test result|FAILED|panicked'
cargo clippy --workspace --all-targets 2>&1 | grep -c '^error'
```

Compare the totals with the baseline in `docs/status.md` ("The Rust side has a `cargo test` baseline of ..."); update that sentence if the counts changed. If the C# side changed: `tools/sync_addon.sh`, `cd low-poly-map && dotnet build`, then the headless smoke test (or the `selftest-runner` agent).

## 4. Report

End with a table: file, what changed, done / not needed (with the reason). Do not commit unless the user asks.
