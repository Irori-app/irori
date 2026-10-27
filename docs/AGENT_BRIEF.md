# Engineering brief for Irori crates

Irori is a new, from-scratch Linux game launcher in Rust with a GPUI (gpui-kit 0.8)
UI. It reaches feature parity with Heroic Games Launcher 2.22.4 but is **not a fork
or a port of its code**: write idiomatic Rust designed for this architecture. The
Heroic TypeScript source is reference material for behaviour only.

- Repo root: the directory containing this `docs/` folder.
- Heroic reference source (read-only):
  `/tmp/claude-1000/-home-radud--config-Claude-scratch-workspaces-be9c0f01-1ca9-426c-9323-c4f5fd7497b3-bb73d751-aec5-4f35-9234-f6306f8360c4-scratch-2026-10-22-dd28a5/7cad61c2-7751-448b-add9-7e546ed39814/scratchpad/ref/heroic/src`
- Parity specs: `docs/parity/*.md` (read the one for your area first).
- User's real Heroic data (read-only, for real JSON shapes): `~/.config/heroic`. Never print,
  log or copy secrets/tokens into the repo or test fixtures; redact them.
- Irori does not run legendary, gogdl, nile or comet: their protocols are ported
  to `crates/epic`, `crates/gog`, `crates/amazon` (downloads through `crates/depot`)
  and comet is used as a library by `crates/gog-galaxy`.

## Architecture

```
irori-core      domain types, settings schema, Store trait, process/http/json utils
irori-runtime   Wine/Proton, prefixes, DXVK/VKD3D, umu, launch environment
irori-epic | irori-gog | irori-amazon | irori-zoom | irori-sideload   (one Store each)
irori-extras    shortcuts, wiki info, anticheat, known fixes, SteamGridDB, Heroic import helpers
irori-services  Backend facade: library, settings, download queue, launching, ops
irori-ui + feature crates + irori (bin)   GPUI application
```

Read `crates/core/src/*.rs` fully before writing code: `store.rs` (the `Store` trait
and `OpContext`), `launch.rs` (`LaunchRequest`, `LaunchEnvironment`, `WineLaunch`,
`LaunchPlan`), `process.rs` (`CommandSpec`, `run`, `RunOptions`, cancellation,
`split_args`), `game.rs`, `install.rs`, `settings.rs`, `paths.rs`,
`json_store.rs`, `http.rs`, `util.rs`, `events.rs`. Also read
`crates/services/src/{downloads,games}.rs` to see how stores are driven.

## Rules

- **Only edit your own crate** (plus adding it to nothing else — the workspace
  already globs `crates/*`). Do not edit `irori-core` or other crates. If you need a
  core change, implement a local helper and list the suggested core change in your
  final report.
- Add dependencies only from `[workspace.dependencies]` in the root `Cargo.toml`
  where possible (`dep.workspace = true`). If you need a new crate, add it to your
  crate's `Cargo.toml` with an explicit version and mention it in your report.
- Use your own build directory to avoid lock contention with parallel agents:
  `CARGO_TARGET_DIR=target/agents/<your-crate> cargo ...`.
- Before finishing, all of these must pass with zero warnings:
  `cargo fmt -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`,
  `cargo test -p <crate>`.
- Async runs on Tokio (the backend runtime). Never hold a `parking_lot` guard across
  an `.await`. Never block the runtime: use `tokio::fs`, `tokio::process` (through
  `irori_core::process`) or `spawn_blocking` for heavy filesystem work.
- Performance and memory matter: share `Arc<GameInfo>`; publish a new `Arc` on change
  rather than mutating in place; keep caches on disk; don't hold full API responses
  in memory longer than needed; parse progress lines with precompiled regexes
  (`once_cell::sync::Lazy`).
- Errors: `anyhow::Result` with context that a user can understand (they are shown
  in the UI). No `unwrap()`/`expect()` on fallible runtime paths outside tests.
- Emit `Event::LibraryChanged(runner)` after the catalogue or install state changes,
  `Event::AccountChanged(runner)` after login/logout.
- Store files: credentials and tool config under
  `Paths::get().store_config_dir(Runner::X)`; caches under
  `Paths::get().store_cache_dir(Runner::X)`; use `json_store` for atomic writes.
- Each store crate exposes `pub struct <Name>Store` with
  `pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self>` implementing
  `irori_core::Store`, and `pub fn import_from_heroic(heroic_config_dir: &Path) -> anyhow::Result<()>`
  that copies the user's existing credentials and installed-games state from a Heroic
  config dir into Irori's layout (so their logins and installs carry over).
- Tests: unit-test parsers (progress lines, API JSON → `GameInfo`, install-info
  mapping, login-input extraction) with small inline fixtures. Tests must not hit the
  network or the user's real config.
- Write doc comments for public items explaining behaviour and ownership; no
  narration comments.
- Fix the Heroic bugs/quirks the spec marks `[QUIRK]` instead of copying them.
- Finish with a concise report: what is implemented, what is not (be honest), any
  suggested core changes, and new dependencies.
