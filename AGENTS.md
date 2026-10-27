# AGENTS.md

Instructions for anyone, human or coding agent, changing Ember. Read this file
before your first change. Where it disagrees with `docs/AGENT_BRIEF.md`,
`docs/UI_BRIEF.md` or `docs/I18N_BRIEF.md`, this file wins. Those briefs were written
for the first, crate-by-crate build of Ember and point at scratch paths that no
longer exist.

Contents:

1. [What Ember is](#1-what-ember-is)
2. [Commands](#2-commands)
3. [Repository map](#3-repository-map)
4. [Architecture](#4-architecture)
5. [Rust conventions](#5-rust-conventions)
6. [UI conventions](#6-ui-conventions)
7. [Translations](#7-translations)
8. [Tests](#8-tests)
9. [Platforms and packaging](#9-platforms-and-packaging)
10. [User data, secrets and safety](#10-user-data-secrets-and-safety)
11. [Branches, commits and pull requests](#11-branches-commits-and-pull-requests)
12. [Definition of done](#12-definition-of-done)
13. [Pitfalls](#13-pitfalls)
14. [Notes for coding agents](#14-notes-for-coding-agents)

---

## 1. What Ember is

Ember is a native game launcher for Linux, macOS and Windows. It covers Epic Games,
GOG, Amazon Games and ZOOM Platform libraries, plus games the user adds by hand
("sideloaded" apps). Windows games run natively on Windows. On Linux they run through
Wine, Proton or umu, and on macOS through Wine, CrossOver or Game Porting Toolkit.

- Written in Rust (edition 2024, Rust 1.95+), with a [GPUI](https://www.gpui.rs) UI
  through [GPUI Kit](https://gpui-kit.com) (`gpui-kit = "0.8.0"`).
- It aims to match the features of Heroic Games Launcher 2.22.4 without Electron. It
  is **not a fork or port of Heroic's code**. Heroic is a reference for behaviour only
  (`docs/parity/*.md`). Its bugs, marked `[QUIRK]` in the parity docs, are fixed, not
  copied.
- Store protocols are Rust ports of legendary (Epic), gogdl (GOG) and nile (Amazon);
  itch.io uses its public API directly.
  GOG Galaxy's comet runs as a library. Ember never shells out to those tools.
- Product goals, in order: correct, fast, calm, one click to play. It runs as one
  process, renders on the GPU, uses no CPU when idle, keeps memory bounded, and
  virtualizes every long list.
- License: GPL-3.0-or-later. Bundled themes are from GPUI Kit (Apache-2.0). The
  vendored `crates/webview` is Apache-2.0 and keeps its own `LICENSE-APACHE`.

## 2. Commands

Run everything from the repository root.

| Task | Command |
| --- | --- |
| Run Ember (release, as users get it) | `cargo run --release -p ember` |
| Run Ember with the Linux sign-in window | `cargo build --release -p ember-sign-in && cargo run --release -p ember` |
| Build both shipped binaries | `cargo build --locked --release -p ember -p ember-sign-in` |
| Format | `cargo fmt --all` (CI: `cargo fmt --all --check`) |
| Lint, as CI does | `cargo clippy --locked --workspace --all-targets --profile ci -- -D warnings` |
| Test everything | `cargo test --locked --workspace --no-fail-fast` (Linux CI wraps it in `xvfb-run -a`) |
| Test one crate | `cargo test -p <package>`, for example `cargo test -p ember-manage` |
| Packaging tests | `python3 -m unittest discover -s packaging -p "test_*.py"` |
| PR-rule tests | `python3 -m unittest discover -s .github/scripts -p "test_*.py"` |
| Check a PR name locally | `python3 .github/scripts/pr_rules.py --title "fix(epic): …" --head fix/epic-login` |
| Package | `python3 packaging/package.py --platform linux\|macos\|windows --binary-dir target/release --output dist` |
| Install for this user (Linux) | `packaging/install.sh` |
| Profile with symbols | `cargo build --profile profiling -p ember` |

Commands Ember offers without opening a window (`crates/ember/src/headless.rs`):

```text
ember accounts                                   which stores are logged in
ember import-heroic                              import Heroic accounts, installs and settings
ember import-lutris                              import Lutris games, categories and playtime (read-only)
ember refresh                                    refresh every library
ember list [--installed]
ember info <runner>/<app> [--platform P]
ember playtime <runner>/<app>
ember install <runner>/<app> [--path DIR] [--platform windows|linux]   Ctrl+C stops; rerun resumes
ember uninstall <runner>/<app> [--keep-files]
```

Ember's other arguments (`crates/ember/src/cli.rs`) are:

- `-l/--launch <runner>/<app>`, `--big-picture`/`--console`, `--minimized`/`--tray`,
  `-V/--version` and `-h/--help`.
- Links: `ember://launch|game|open/<runner>/<app>`, `ember://library|downloads|accounts|stores|stats|wine|emulators|bigpicture`,
  `ember://settings/<section>`, and Heroic's `heroic://launch/...`.
- `--sign-in-window` is internal. On macOS and Windows it makes Ember run the store
  sign-in window instead of itself (see [4.6](#46-store-sign-in)).

Environment variables:

| Variable | Effect |
| --- | --- |
| `EMBER_HOME` | Puts all config, data and cache under one root (`<root>/{config,data,cache}`). Use it for experiments so your real data stays untouched. |
| `EMBER_LOG` | `tracing` filter, e.g. `EMBER_LOG=ember_services=debug,info`. The default is `info` in the app and `warn` for the commands without a window. |
| `EMBER_SIGN_IDENTITY` | macOS signing identity for `package.py` (default `-`, ad-hoc). |
| `EMBER_WINDOWS_CRT_DIR` | Windows: explicit `Microsoft.VC*.CRT` directory for packaging. |
| `EMBER_RELEASE_TEST_URL`, `EMBER_RUNTIME_SMOKE` | Opt-in, `#[ignore]`d runtime tests (see [8](#8-tests)). |

Build profiles (root `Cargo.toml`):

- `release` is fat LTO, one codegen unit, `panic = "abort"`, stripped.
- `dev` builds Ember at `opt-level = 1` and its dependencies at 2, so debug builds stay
  usable.
- `ci` is unoptimized and keeps no debug info, so builds fit hosted runners.
- `profiling` is release plus line tables, for `perf` and `heaptrack`.

## 3. Repository map

Directory names and package names differ. Use the package name with `-p`.

| Directory | Package | Role |
| --- | --- | --- |
| `crates/core` | `ember-core` | Domain types, settings schema, the `Store` trait, events, paths, process/HTTP/JSON utilities |
| `crates/runtime` | `ember-runtime` | Wine/Proton/CrossOver/GPTK discovery and downloads, prefixes, DXVK/VKD3D, umu, launch environment |
| `crates/engines` | `ember-engines` | Emulators ("engines"): manifests, discovery, managed versions, RetroArch cores, BIOS index, launch commands |
| `crates/depot` | `ember-depot` | Download engine: parallel chunked downloads, resume, verification, delta updates; a `test-server` feature |
| `crates/epic` | `ember-epic-api` | Epic protocol (port of legendary): auth, library, manifests, cloud saves, launch parameters |
| `crates/gog` | `ember-gog-api` | GOG protocol (port of gogdl): auth, content system, depots, Linux installers, saves |
| `crates/amazon` | `ember-amazon-api` | Amazon Games protocol (port of nile): device sign-in, library, signed manifests |
| `crates/itch` | `ember-itch-api` | itch.io protocol (its public API): API-key login, owned keys, uploads, download links |
| `crates/gog-galaxy` | `ember-gog-galaxy` | Hosts comet (Galaxy service for games: overlay, achievements) on `127.0.0.1:9977` |
| `crates/store-epic` | `ember-epic` | Epic `Store` (`Runner::Legendary`) |
| `crates/store-gog` | `ember-gog` | GOG `Store` |
| `crates/store-amazon` | `ember-amazon` | Amazon `Store` (`Runner::Nile`) |
| `crates/store-zoom` | `ember-zoom` | ZOOM Platform `Store`, opt-in through a setting |
| `crates/store-itch` | `ember-itch` | itch.io `Store` (`Runner::Itch`), opt-in through `emberItchIo` |
| `crates/store-steam` | `ember-steam` | Read-only Steam library from local Steam files (`Runner::Steam`) |
| `crates/store-sideload` | `ember-sideload` | Apps the user adds (native, Windows, browser) |
| `crates/store-emulation` | `ember-emulation` | The ROM library (`Runner::Emulation`): scanned folders, played with emulators |
| `crates/extras` | `ember-extras` | Shortcuts, PCGamingWiki/HLTB/ProtonDB/anticheat info, known fixes, SteamGridDB, deals, Heroic import, Lutris import planning |
| `crates/services` | `ember-services` | The `Backend` facade: library, settings, user data, download queue, launching, playtime |
| `crates/ui` | `ember-ui` | Shared UI: backend bridge, models, navigation, art, theme and translucency, components, i18n |
| `crates/feature-library` | `ember-library` | Library grid/list, search, filters, categories, stats page, sideload dialogs |
| `crates/feature-game` | `ember-game` | Game page and its sheets: install, uninstall, logs, cloud saves, import, modify |
| `crates/feature-manage` | `ember-manage` | Downloads, Wine manager, accounts and login, stores, onboarding |
| `crates/feature-settings` | `ember-settings` | App settings, game defaults, per-game overrides, logs, system info |
| `crates/feature-bigpicture` | `ember-bigpicture` | Big Picture mode and controller input for the whole app |
| `crates/ember` | `ember` | The application shell and binary: window, sidebar, routing, palette, tray, single instance, CLI |
| `crates/sign-in` | `ember-sign-in` | Store sign-in window (Wry/Tao): library and the `ember-sign-in` program |
| `crates/webview` | `gpui-webview` | Vendored GPUI WebView, **excluded from the workspace and unused** |

Other top-level paths:

- `assets/`: bundled theme JSON files, the Manrope font, and `engines/` (emulator
  manifests and the systems catalog, see [4.8](#48-engines-emulators)).
- `packaging/`: `package.py`, its tests, the Linux `install.sh`, the Windows
  `install-windows.ps1`, the desktop entry, icons and entitlements.
- `docs/`:
  - `PLATFORMS.md`: paths, desktop behaviour, translucency, packaging, release checklist.
  - `parity/`: the Heroic behaviour Ember matches, and the Lutris behaviour
    (`parity/lutris.md`).
  - `design/`: designs of larger features (`design/engines.md`: emulators, ROM
    libraries, Lutris import and scripts, third-party launchers).
  - The three briefs (see the top of this file).
- `postman/`: contract, mock and simulation used by an ignored runtime test.
- `.github/`: CI, the PR-rule check and the PR template.

### Layering

The crates form a strict hierarchy. Never add a dependency that points upward.

```text
ember-core                        (depends on nothing internal)
  ├─ ember-runtime, ember-depot, ember-extras
  ├─ ember-engines   (core, runtime, depot)
  ├─ protocol crates: ember-epic-api, ember-gog-api, ember-amazon-api, ember-itch-api
  │                   (depot only, not core)
  ├─ store crates:    ember-epic, ember-gog, ember-amazon, ember-zoom, ember-itch,
  │                   ember-steam, ember-sideload, ember-emulation (+ engines)
  └─ ember-services   (core, depot, runtime, extras; stores reach it as `DynStore`)
ember-ui          (services, core)
feature crates    (ui, services, core; concrete store crates only where a screen needs a store's own API)
ember (binary)    (everything; the only crate that names every store)
ember-sign-in     (standalone; Ember links it only on macOS/Windows)
```

- Protocol crates know nothing about Ember: no `Paths`, no settings, no events.
- Store crates turn a protocol into a `Store`. Services drive stores only through
  `DynStore = Arc<dyn Store>`.
- Feature crates never depend on each other. The shell routes between them
  (`crates/ember/src/routing.rs`, `pages.rs`) through events such as `LibraryEvent`
  and `GamePageEvent`.
- When a feature needs an operation that requires a concrete store crate (Heroic
  import, the GOG branch password), the shell installs a hook (`ember_ui::hooks`,
  `ember_game::install_hooks`) instead of the feature depending on the store.

## 4. Architecture

### 4.1 Core contracts (`ember-core`)

Read `crates/core/src/store.rs`, `events.rs`, `paths.rs`, `settings.rs`, `process.rs`
and `launch.rs` before changing anything below the UI.

**`Store` trait** (`store.rs`, `#[async_trait]`, `Send + Sync + 'static`):

- Accounts:
  - `init`: disk only, no network, and it gates the first render.
  - `user`, `login_flow`, `login(input)`, `logout`.
- Library: `refresh(full)`, `games()`, `game`, `extra_info`, `install_info`,
  `updatable_games`. `games()` returns `Vec<Arc<GameInfo>>`, including DLC flagged
  `is_dlc`.
- Operations: `install`, `update` and `repair` take an `OpContext` and return an
  `Outcome`. Also `uninstall(RemoveArgs)`, `import`, `set_install_path`, and
  `launch` → `LaunchPlan`.
- Optional, with defaults: cloud saves, achievements, `remote_playtime` (minutes by
  app name), `after_exit`, extras, `forget_install`, `set_version_pinned`.
- Accounts, with defaults for stores with one login: `accounts`, `can_add_account`,
  `account_login_flow`/`account_login`/`account_logout`/`refresh_account` by slot,
  and `game_account`.

**Other types:**

- `LoginFlow::Browser { url, capture, instructions }`. `capture` is a regex whose
  first group is what `login` needs. The sign-in window and the clipboard watcher both
  use it (see [4.6](#46-store-sign-in)). `LoginFlow::None` is for stores without
  accounts.
- `OpContext` carries a progress sink, a `CancellationToken`, a step reporter, an
  optional operation log file and the game's effective settings.
  `OpContext::detached()` is the no-op version.
- Two typed errors carry meaning across the `Store` boundary; return them anywhere in
  the `anyhow` chain:
  - `SessionExpired`: the store's token service refused the saved login (a 400 or 401,
    not a network failure or a busy server). Services remember it, tell the user once
    and offer to log in again.
  - `SaveConflict`: saves changed both locally and in the cloud since the last sync.
    Return it for every direction except the forced ones; the launch stops and the UI
    asks which saves to keep (`ember_ui::save_conflict`).
- `Runner` values are Legendary, Gog, Nile, Zoom, Itch, Steam, Sideload and Emulation. Their
  serialized ids are Heroic's (`legendary`, `gog`, `nile`, …); `from_id` also accepts
  `epic` and `amazon`. Ember-only runners use their store's plain name (`itch`,
  `steam`).
- `Runner::manages_installs()` is false for Steam: its games are listed and launched,
  never installed, moved, repaired or removed. UI code hides those commands and
  services refuse them before touching files.
- `GameId { runner, app_name: Arc<str> }` has `key() == "runner:app"`, which keys
  every user-data map.

**Events** (`events.rs`, a Tokio broadcast channel with capacity 1024):

- Variants: `LibraryChanged(Runner)`, `LibraryUpdated(rev)`, `GameChanged`, `Status`,
  `StatusCleared`, `AccountChanged`, `DownloadQueueChanged`, `WineVersionsChanged`,
  `SettingsChanged`, `GameSettingsChanged`, `Connectivity`, `Notice`, `GameExited`.
- Events carry *identities, not data*: receivers re-read state from the service that
  owns it.
- Emit `LibraryChanged(runner)` after a store's catalogue or install state changes,
  and `AccountChanged(runner)` after login or logout.
- `LibraryChanged` is a raw store signal. UI code waits for `LibraryUpdated`, which
  services emit after rebuilding the snapshot.

**Paths** (`paths.rs`):

- `Paths::get()` is global. `EMBER_HOME` relocates everything; otherwise the OS
  config, data and cache directories plus `ember`.
- Store credentials and config go under `store_config_dir(runner)`, store caches under
  `store_cache_dir(runner)`, and Ember state (`library.json`, `downloads.json`,
  `sessions.jsonl`) under `state_dir()`.
- Per-game settings are `config/games/<runner>/<app>.json`. Logs go under
  `logs_dir()`; the app log is `data/logs/ember.log`, and a panic writes
  `crash-<time>.txt` beside it (`crates/ember/src/crash.rs`).
- Tests isolate themselves with `Paths::init(Paths::under(tempdir))`.

**Persistence** (`json_store.rs`):

- Always write through `json_store::write`/`write_bytes`. They write a `.tmp`
  sibling and rename it, so a crash never leaves a half-written file.
- `read` returns `Ok(None)` for a missing file; `read_or_default` logs and falls back
  for a corrupt one.

**Settings** (`settings.rs`):

- JSON keys are Heroic's (camelCase, including Heroic's `enviromentOptions` typo)
  so Heroic configs import unchanged. **Do not rename keys.** Add new ones with
  `#[serde(default)]`.
- `AppSettings` flattens a `GameSettings` that holds the global game defaults.
- Per-game overrides are sparse (`GameSettingsOverrides`, a JSON map): the effective
  settings are the defaults with the overridden keys applied. Reset a field with
  `SettingsService::set_game_value(id, key, None)`.

**Processes** (`process.rs`):

- Spawn through `CommandSpec` + `process::run`/`spawn`, never `std::process` directly
  for game or tool processes.
- It handles line callbacks (split on `\n` and `\r`), capped logs, cancellation and
  process-group termination: SIGTERM then SIGKILL on Unix, `taskkill /T /F` on
  Windows.

**HTTP** (`http.rs`):

- Use `ember_core::http::client()`: shared, rustls, `Ember/<version>` user agent,
  10 s connect and 60 s total timeout. It must run inside the Tokio runtime.
- Large downloads use `download_client()` (no total timeout) or `ember-depot`.

**Deleting files:** call `fs_guard::ensure_deletable(dir, app)` before any recursive
delete of a game or prefix directory.

### 4.2 Backend (`ember-services`)

`Backend` (`crates/services/src/lib.rs`) is the single entry point for the UI and the
commands without a window.

- `Backend::start(make_stores)` is for the app; `start_headless` is for CLI commands
  (no periodic tasks).
- It owns a Tokio runtime named `ember-io`: 3 workers, 16 blocking threads. Use
  `backend.spawn` and `backend.spawn_blocking`.
- Public parts: `bus`, `settings` (`SettingsService`), `userdata`, `statuses`, plus
  `store(runner)`, `stores()`, `refresh_*`, downloads, launch/stop/uninstall/move/repair/import,
  cloud saves and Wine operations.
- **Library:** store games without DLC, plus user overrides (title, art), become a
  sorted `Arc<LibrarySnapshot>` with a revision number. Rebuilds are debounced (30 ms)
  after `LibraryChanged`/`GameChanged`.
- **Downloads:** one worker, one download at a time, persisted in
  `state/downloads.json`. Running items resume as Queued after a restart. Pause and
  cancel go through a `CancellationToken`.
- **User data:** favourites, hidden games, categories, recents, playtime and art
  overrides live in `state/library.json` behind `RwLock<Arc<UserData>>`, updated copy
  on write. Store playtime merges by keeping the larger total and counts games only,
  never their DLC.
- **Launching** (`games.rs`):
  1. Busy and availability checks.
  2. Optional cloud-save sync.
  3. `ember_runtime::prepare_launch`.
  4. Before-launch script.
  5. `store.launch` builds the `LaunchPlan`.
  6. `process::spawn`, with a capped game log.
  7. Sleep inhibition, then session tracking, ending in `GameExited`.

### 4.3 Stores

Every store crate follows the same shape:

- `pub struct <Name>Store` with `pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self>`,
  usually built with `Arc::new_cyclic` around a weak self.
- `pub fn import_from_heroic(heroic_config_dir: &Path) -> anyhow::Result<()>`. It copies
  the user's Heroic credentials and installs into Ember's layout. Existing Ember data
  wins over Heroic's.
- Test constructors take explicit paths or endpoints (`with_paths`, `with_endpoints`,
  `with_layout`), so tests run against temp dirs and in-process servers.

**Several accounts.** A store crate never handles more than one login. The shell
wraps Epic, GOG, Amazon, ZOOM and itch.io in `ember_services::MultiAccountStore`,
which runs one instance per account ("slot") and merges them into one `Store`:

- The main slot uses the store's usual directories, so single-account data needs no
  migration. Extra slots use `Paths::account_config_dir`/`account_cache_dir`, built
  through each store's `for_account(bus, settings, config, cache)` constructor.
- Games stay keyed by app name. A game comes from the slot it is installed with, else
  from the first slot owning it, and every per-game call goes to that slot.
- Code that needs a concrete store for one game (GOG branch passwords, REDmod) asks
  the wrapper with `store_for(app)`; store-wide tools (EOS overlay, EGL sync) use
  `main()`.

The shell registers the stores in `crates/ember/src/stores.rs`, in this order: Epic,
GOG, Amazon, ZOOM (only when `settings.zoom_platform` is on), itch.io (always
registered; empty and signed out while `settings.itch_io` is off, so its account card
can turn it on), Steam, Emulation (the ROM library), then Sideload. Adding a
store means:

1. A protocol crate, if the store has a protocol.
2. A store crate, with `for_account` if the store has accounts.
3. Registration in `stores.rs` (wrapped with `with_accounts` if it has accounts), plus
   its Heroic import in `HEROIC_IMPORTS`.
4. Its login flow.
5. Its strings in every locale.

### 4.4 UI (`ember-ui` and the feature crates)

`ember_ui::init(backend, cx)` runs after `gpui_kit::init`. It installs i18n,
`AppState`, theme, art, navigation and models.

**Calling the backend from a view.** Never block the UI thread, and never await a
backend future outside `ember_ui::run`, because it needs the Tokio runtime:

```rust
let task = ember_ui::run(cx, async move { backend.launch(&id, None, vec![]).await });
cx.spawn_in(window, async move |this, cx| {
    let result = task.await;
    let _ = this.update_in(cx, |this, window, cx| {
        // Update state; report errors with a notification or an inline Alert.
        cx.notify();
    });
})
.detach();
```

`run_infallible` returns `Task<Option<T>>`. `backend(cx)` clones the `Backend`.

**Models** (`models.rs`):

- `LibraryModel`, `StatusModel`, `DownloadsModel`, `AccountsModel`, `SettingsModel`,
  `NoticeModel` and `SessionModel`, all reached through `models(cx)`.
- One event pump reads the bus in batches and notifies each affected model once.
- Views observe only the models they render, and keep the returned `Subscription`s in
  a `_subscriptions: Vec<Subscription>` field.

**Navigation** (`nav.rs`):

- `Route` variants: Library, Game, Downloads, Settings(section), GameSettings,
  WineManager, Emulators, Accounts, Stores, Stats, BigPicture, Onboarding.
- Use `navigate(route, cx)`, `go_back`, `go_forward`. The shell decides which pages it
  caches.

**Art** (`art.rs`):

- Originals are downloaded once. Thumbnails are cut per width bucket and stored on
  disk; decodes are limited to 2 at a time.
- `ArtStore::get(url, width, keep_alpha)` returns `Ready`, `Loading` or `Missing`.
- Wrap art-heavy regions in `div().image_cache(LruImageCache::new(n, cx))`. Use the
  components (`GameCover`, `art_img`) instead of loading images yourself.

**Theme and translucency:**

- Themes live in `assets/themes`; Ember Night and Ember Day are the defaults. Apply
  them with `theme::apply`.
- Surfaces take their colours from `glass::chrome(..)`, `glass::page(..)` and
  `solid_pages()`. Never add alpha yourself.
- The available backdrops differ per platform; see the table in `docs/PLATFORMS.md`.

**Components** (`components/`): `GameCover`, `ArtImage`, `EmptyState`, `PageHeader`,
`SectionTitle`, `ProgressSummary`, `StoreBadge`. Style helpers are in `style.rs`
(`legible` contrast, `reduce_motion(cx)`, `keyboard_focusable`); motion constants are
in `motion.rs`.

**Feature crates:**

- Each exposes `pub fn init(cx)`, its `*View` entities (`impl Render`, with
  `pub fn new(.., window, cx)`), `open_*_dialog` / `open_*_sheet` functions, `*Event`
  enums and `pub use i18n::CATALOGS`.
- Key contexts are `*_CONTEXT` constants. Actions are declared per crate and bound
  once.

### 4.5 Shell (`crates/ember`)

`main.rs` does these steps in order. Keep them in this order:

1. `--sign-in-window` (macOS and Windows): run the sign-in window and exit.
2. Parse arguments. A command such as `list` runs without a window and exits; an
   unknown bare word exits 2.
3. `instance::acquire`. A second instance forwards its arguments over authenticated
   loopback TCP (`instance.json` holds the port and token, mode 0600) and exits.
4. Logging, `Backend::start(stores::build)`, the GPUI application, URL and reopen
   handlers.
5. Inside `run`: `gpui_kit::init`, `ember_ui::init`, every feature `init`, hooks,
   key bindings, theme, tray, main window, then the `ShellCommand` loop.

Other parts:

- `shell.rs` (`AppShell`) owns the window content: title bar, sidebar, pages,
  notices, language/theme/translucency changes, and the "playing" state.
- `actions.rs` binds global shortcuts. `ctrl-` becomes `cmd-` on macOS, which also
  gets a native menu.
- `palette.rs` is the Ctrl/Cmd+K command palette.
- Tray: `tray.rs` on Linux (ksni) and `tray_native.rs` on macOS and Windows
  (`tray-icon`, kept on the main thread).

### 4.6 Store sign-in

**Sign-in window:**

- `LoginDialog` (`crates/feature-manage/src/login/`) opens the store's page in a
  window of its own.
- That window runs in a **separate process**, so Ember's window never embeds a web
  engine.
- It watches every address and page text for the store's `capture` pattern, prints
  `{"text": "..."}` and exits 0. Closing it exits 2; bad arguments exit 1.

**Which program shows it** (`login/window.rs::helper`), in this order:

1. The `ember-sign-in` program next to Ember's executable, or on `PATH`.
2. On macOS and Windows only, Ember itself, started as
   `ember --sign-in-window --url … --capture … --title …`. Their web engines (WKWebView,
   WebView2) ship with the system, so this adds no runtime dependency.
3. Otherwise the browser. The dialog says why, opens the page in the default browser,
   watches the clipboard for the capture pattern and accepts a pasted code.

On Linux the window needs WebKitGTK 4.1, which stays optional: Ember runs without it.
So never make `ember` link WebKitGTK on Linux.

`ember-sign-in` stays a standalone program with a stable command line. Packages ship
it on every platform, and CI's Windows install test runs it.

**A new store's login flow:**

- Prefer a capture pattern the sign-in window can find without user help: a redirect
  URL parameter, or the JSON a page shows.
- Write `instructions` for the browser fallback.
- Make `login(input)` accept the full page text, the address or the bare code.

### 4.7 Big Picture and controllers

- `feature-bigpicture/src/gamepad/reader.rs` reads controllers on a dedicated gilrs
  thread that blocks until an event arrives, so it uses no CPU when idle.
- `input.rs` normalizes input (repeat, dedupe) and is unit-tested.
- `mod.rs` dispatches `ember_gamepad` actions to the focused element, or synthesizes
  keystrokes (arrows, Enter, Escape, Ctrl+Enter, Ctrl+K) when the element doesn't
  handle them.
- The Guide button toggles Big Picture. Every screen must stay usable with arrows,
  Enter and Escape, because that is how controllers reach it.

### 4.8 Engines (emulators)

Design: `docs/design/engines.md`; Lutris behaviour: `docs/parity/lutris.md`.

- **Words.** `Runner` is a store. An emulator, RetroArch, ScummVM or DOSBox is an
  **engine** in code (`ember_engines::EngineManifest`) and an "emulator" in the UI. A
  console is a **system** (`assets/engines/systems.json`, ids like `psx`, `snes`, `dos`).
  An engine choice is an `EngineRef`, written `duckstation` or `retroarch:<core>`.
- **Manifests are data.** Each engine is `assets/engines/<id>.json`: sources per OS
  (GitHub release regexes, the libretro buildbot, a download page, Flatpak id, `PATH`
  names, app bundle, Windows paths), the launch template, options with **one** visibility
  rule (`visibleWhen`), BIOS folder, exit hotkey and licence. Never pin a download URL;
  describe where versions are listed. The only per-engine code is the `kind` match in
  `crates/engines/src/launch.rs` (RetroArch cores, DOSBox, ScummVM, MAME). Users may
  override a manifest with `<config>/engines/<id>.json` (merge patch).
- **Option texts** are `manage.engines.option.<id>.label` / `.help` (and
  `.<choice>` for choice values) in `ember-manage`; a test fails when a built-in option
  has no English text.
- **Installs.** Ember downloads engines from the projects' own servers on request and
  never redistributes them. Managed versions sit side by side under
  `<data>/engines/<id>/<version>/` and are recorded in `<state>/engines.json` with the
  default installation per engine. Installing never changes what games use; Update does,
  and a game pinned with `emberEngineVersion` keeps its version. Flatpak and system
  installs are detected and labelled, never installed or removed.
- **BIOS.** Never download or link to BIOS files or ROMs. The BIOS index hashes only
  plausible files and caches digests by path, size and modification time; nothing is
  hashed at launch. Files are linked under the expected names before a launch.
- **Settings keys** are Ember-only: `emberEngine`, `emberEngineVersion`,
  `emberEngineOptions` (merged per engine and option over the defaults),
  `emberEngineOwnSettings`, `emberSystemEngines`, `emberRomFolders`, `emberBiosFolder`.
- **The ROM library** (`ember-emulation`, `Runner::Emulation`, serialized id
  `emulation`, app names are a hash of the game's first file) scans `emberRomFolders`
  incrementally (detections cached by path, size and mtime) and watches them with
  `notify`. It never deletes ROMs: removing a game only hides it from the library.
  Launch order: BIOS check (typed `BiosMissing`), engine choice (game, then system
  default, then first installed recommended), missing RetroArch cores installed on the
  way, version pin (typed `EngineMissing`), then the command. `GameInfo.system` and
  `GameInfo.tags` (region, revision) drive the system filter, badges, the generated
  cover and Big Picture's per-system shelves.
- **Lutris import** (`ember-extras::lutris_import` reads; `crates/ember/src/lutris.rs`
  applies). It only reads Lutris: `pga.db` is opened read-only and immutable, YAML is
  parsed, nothing is written there. Windows/Linux games become added apps with their
  prefix, Wine build (matched by name, else reported), arguments and variables;
  emulated games join the ROM library as added games with their emulator; Steam and
  store games are matched by id. Every game not imported comes back with a reason id
  that `ember_ui::lutris_summary` translates. Tests build the SQLite database in the
  test; `rusqlite` is built with `bundled` on every OS.
- **Fix dialogs.** `ember_ui::save_conflict::resolve_launch_error` also turns
  `EngineMissing` and `BiosMissing` into dialogs with a fix (install and play, use the
  default version, choose the BIOS folder). Every place that starts a game already
  calls it.
- **UI.** The Emulators page (`Route::Emulators`, `ember-manage::emulators`) installs,
  updates and removes versions and cores, and picks each system's default emulator.
  Installs run through `ember_manage::start_install` so any screen can start one and the
  page shows its progress. Changes emit `Event::EnginesChanged`.

## 5. Rust conventions

Match the code around you. In particular:

**Errors**

- Use `anyhow::Result` with `.context("…")` / `.with_context(|| …)` worded for the
  user: these messages appear in the UI. Format chains with `{e:#}`.
- `thiserror` is only for typed errors that callers match on (see `ember-depot`,
  `ember-zoom`).
- No `unwrap()`/`expect()` on fallible runtime paths. They are fine in tests, and on
  invariants with an `expect("why this holds")`.

**Async and threads**

- Async code runs on the backend Tokio runtime.
- Never block it: use `tokio::fs`, `ember_core::process`, or `spawn_blocking` for
  heavy filesystem or CPU work.
- Never hold a `parking_lot` guard across `.await`. Use `tokio::sync::Mutex` when a
  lock must span awaits.
- Shared state is copy on write: `RwLock<Arc<T>>`, with a new `Arc` published on
  change. Share `Arc<GameInfo>` and don't clone game data per frame.

**Performance and memory**

- Precompile regexes with `once_cell::sync::Lazy`.
- Keep caches on disk. Don't hold whole API responses longer than needed.
- Derive filtered or sorted lists when their inputs change (by revision), not on
  every render.

**Dependencies**

- Declare dependencies in `[workspace.dependencies]` and use `dep.workspace = true` in
  crates. A dependency only one crate needs may stay local; give it an explicit
  version and name it in the PR.
- Keep `Cargo.lock` committed; CI builds with `--locked`.
- Prefer small crates with few default features (`default-features = false` is
  common here).

**`unsafe`**

- `ember-depot`, the protocol crates and `ember-gog-galaxy` are
  `#![forbid(unsafe_code)]`. Keep it that way.
- Elsewhere `unsafe` is for OS APIs only, with a `// SAFETY:` comment.

**Platform code**

- Gate it with `#[cfg(target_os = "…")]` (or `cfg!(..)` when both branches compile
  everywhere).
- Keep platform-specific pieces in their own module or file (`tray_native.rs`,
  `desktop_native`, `macos.rs`).
- Every change must build on Linux, macOS and Windows; CI checks all three. A
  `#[cfg]` without an `else` usually needs a fallback for the other platforms.

**Comments and docs**

- Every module starts with a `//!` header saying what it owns. Public items get `///`
  docs that describe behaviour, not implementation.
- Comments explain *why* (a store quirk, an OS limitation, a workaround with its
  reason). No narration ("now we loop over…"), no commented-out code, no TODOs
  without an issue.
- Keep comment density similar to the file you're in. Keep modules focused: split
  before a file passes ~1,000 lines unless the code is one cohesive view.

**Formatting and lints**

- Default `rustfmt` and default `clippy`, with warnings as errors (CI uses
  `-D warnings`).
- Don't silence a lint with `#[allow]` without a comment saying why.

**Naming**

- Crates are `ember-<area>`. Views are `*View`, events `*Event`, entry points
  `open_*_dialog` / `open_*_sheet`, and every crate has an `init(cx)`.
- Settings keys follow Heroic's spelling (camelCase); Ember-only keys start with
  `ember` or describe themselves (`uiLanguage`, `zoomPlatform`).

## 6. UI conventions

**APIs**

- Use only `gpui-kit`: `use gpui_kit::*` for GPUI, `gpui_kit::component::*` for
  components, and `gpui_kit::assets::IconName` for icons (`Icon::new(IconName::…)`).
- **Never invent an API.** Grep the exact sources we compile against:
  `~/.cargo/registry/src/index.crates.io-*/{gpui-kit-0.8.0,gpui-component-0.8.0,gpui-base-0.8.0,gpui-pre-0.3.9}`.

**Visual language**

- Use theme tokens only (`cx.theme().*`) and rem helpers (`p_4()`, `gap_3()`, `w_64()`).
  No raw colours, and no `px(…)` except for hairlines and art.
- Cover art carries the colour; chrome stays neutral. The primary colour is reserved
  for the main action of a region.
- Text must stay legible on every bundled theme in light and dark (`style::legible`,
  WCAG 4.5:1 for text, 3:1 for icons).

**Layout**

- Give every flex child that holds text `min_w_0()` (`min_h_0()` vertically) and
  `truncate()` where text may overflow. Have one scroll owner per region.
- Leave room for German and Russian, which run 30–40% longer than English. Never put
  text in a fixed-width box.

**Feedback**

- Every async action shows loading, success and failure.
- Report errors to the user with `window.push_notification(Notification::error(format!("{e:#}")).title(t(..)))`
  or an inline `Alert`, never only in the log. Explain offline state; don't fail
  silently.

**Destructive actions**

- Confirm only destructive actions, with `window.open_alert_dialog` and a danger
  button.
- Name the object in the copy: Uninstall “Hades”? / Uninstall. Never write "Are you
  sure?".

**Reachability**

- Every action is reachable from a visible button, the context menu, the command
  palette and the keyboard, all calling the same action or method.
- No hover-only paths. Focus rings stay visible (`keyboard_focusable`).

**Motion:** use `motion::{QUICK, STANDARD}` and the `fade_in`/`rise_in` helpers.
Respect `style::reduce_motion(cx)`.

**Performance**

- Virtualize any list that can exceed ~50 items (`v_virtual_list`, `List`,
  `DataTable`).
- Never clone the library per frame. Re-render only from the models you observe.

**Copy**

- Sentence case, verbs on buttons ("Install", "Verify files"), curly quotes, no
  trailing period on buttons or titles.
- Store and tool names are never translated or changed (Epic Games, GOG, Steam,
  Proton, Wine, DXVK).

You can't see the screen, so when a change affects layout, run Ember and look. If you
can't, say so in the PR.

## 7. Translations

Ember ships 19 languages, in the order of `ember_ui::i18n::LANGUAGES`: `en, cs, de, es,
fr, it, nl, pl, pt, pt_BR, ro, sv, tr, ru, uk, ja, ko, zh_Hans, zh_Hant`.

**Catalogs**

- Each crate owns a key namespace and flat JSON catalogs in
  `crates/<crate>/locales/<code>.json`.
- Namespaces: `ui.`, `library.`, `game.`, `manage.`, `settings.`, `bigpicture.`,
  `shell.` (the `ember` crate, in `strings.rs`).
- `en.json` is the source of truth.

**In code**

- Use `t("ns.area.intent")` and `t_args("ns.area.intent", &[("name", value)])`.
- Keys describe intent, never the English text. Each full sentence gets its own key
  with `{placeholders}`; never assemble sentences from fragments.
- Plurals use `…_one` / `…_other` keys, picked in code.
- Don't translate log messages, identifiers, file paths, key names or brand names.

**Every new or changed string goes into all 19 catalogs in the same change**

- Translate it yourself, idiomatically, following each language's UI conventions.
  Keep the `{placeholder}` names identical.
- If you change an English string's meaning, update every translation too.
- Never leave English in another catalog: the tests treat a missing key as a failure.

**The tests enforce this.** They live in each crate's `i18n.rs`, Big Picture's
`catalog_tests` and `crates/ember/src/strings.rs`. They check:

- Every catalog parses and has every English key.
- No catalog has extra keys.
- Placeholder sets match.
- Every key literal in `src/**/*.rs` exists in `en.json`.
- Big Picture also fails on unused keys.

**Registration:** each crate registers its `CATALOGS` once (`std::sync::Once`) from
`init`. The language switches live; `shell.rs` calls `set_language` and refreshes the
window.

## 8. Tests

**Where tests live:** inline `#[cfg(test)] mod tests` or a `src/tests.rs` module. There
are no `tests/` directories or fixture folders. Fixtures are small inline JSON strings.

**No network, no real user data**

- Tests never reach the network or read the user's real config, Heroic data or
  stores.
- Use `tempfile` directories and `Paths::init(Paths::under(tmp))`.
- For HTTP, use `ember_depot::test_server::TestServer` (dev-dependency feature
  `test-server`) or a store's own test server. Store crates take endpoints or paths in
  test constructors for this.
- Amazon manifests have `ember-amazon-api`'s `test-support` feature.

**What to test**

- Parsers (store JSON → `GameInfo`, manifests, progress lines, login input
  extraction), settings mapping, Heroic import, and view-state logic (filters,
  sorting, model transitions).
- A bug fix comes with a test that fails without it.

**UI tests**

- `#[gpui_kit::test]` with `TestAppContext`. See `crates/feature-library/src/ui_tests.rs`
  for the harness: isolated `Paths`, `Backend::start` over an in-memory `Store`,
  `run_until_parked`, then open and draw a window.
- They need a display on Linux; CI uses `xvfb-run -a`.

**Never enable `gpui-kit`'s `test-support` feature.** It pulls the GPUI Fast crates
into the lockfile, where they conflict with `gpui-pre`. Crates use
`gpui = { package = "gpui-pre", version = "=0.3.9", features = ["test-support"] }` as a
dev-dependency instead.

**Ignored tests** (`#[ignore]`) need explicit setup:

- `ember-runtime`'s release test needs `EMBER_RELEASE_TEST_URL` pointing at the local
  Postman mock in `postman/`.
- The macOS runtime smoke test needs `EMBER_HOME` and `EMBER_RUNTIME_SMOKE=1`, and
  downloads a real runtime.

Run ignored tests only when you're changing that code.

**Packaging** changes need `packaging/test_package.py`. PR-rule changes need
`.github/scripts/test_pr_rules.py`.

## 9. Platforms and packaging

`docs/PLATFORMS.md` is the reference for paths, desktop integration, translucency,
building and the release checklist.

**Building**

- Build on the target OS; there is no cross-compiling.
- Linux needs the GPUI libraries (Vulkan, Wayland, X11/xcb, xkbcommon, fontconfig,
  freetype) and, for the sign-in window, GTK 3 and WebKitGTK 4.1. The exact apt list
  is in `.github/workflows/ci.yml`.
- macOS needs the Xcode Command Line Tools.
- Windows needs the MSVC toolchain, the Visual Studio C++ Build Tools and the Windows
  SDK, plus the WebView2 Runtime for sign-in.

**Packaging** (`packaging/package.py`, Python 3.11+, standard library only)

- Requires both `ember` and `ember-sign-in`, so a package never silently loses the
  sign-in window.
- Linux: a tarball with the desktop entry and icon.
- macOS: `Ember.app` with the `ember://` scheme.
  - Rejects any dylib outside the system paths.
  - Signs the helper before the bundle (`EMBER_SIGN_IDENTITY`, ad-hoc by default).
  - Notarization is a separate, manual step.
- Windows: a zip with `install.ps1`.
  - Checks that every binary is x64.
  - Bundles `vcruntime140.dll` and `vcruntime140_1.dll`.
  - The installer installs per user under `%LOCALAPPDATA%\Programs\Ember` and
    registers the URL handler and a Start-menu shortcut.

**Desktop behaviour to keep working**

- One instance per config root, with argument forwarding.
- `ember://` links.
- The tray.
- Sleep inhibition while playing.
- Shortcuts: `.desktop` files, app bundles or `.lnk` files.
- Translucency per platform.

## 10. User data, secrets and safety

**Secrets**

- Never print, log, commit or put in fixtures any token, password, cookie, session or
  account id.
- Redact real shapes before using them as fixtures. The login code paths handle
  secrets: keep them out of `tracing` output and error messages.

**User data**

- Work with `EMBER_HOME=<scratch dir>` when experimenting, so the user's real
  library, settings and store sessions stay untouched.
- Heroic data (`~/.config/heroic` and the Flatpak path) is read-only reference
  material. Ember's import copies from it and never writes there.
- Store accounts are real accounts. Don't sign in, install, uninstall or sync cloud
  saves against real accounts unless the person you're working for asked you to.

**Deleting files**

- Recursive deletes go through `fs_guard::ensure_deletable`.
- Uninstall and "delete prefix" are confirmed in the UI.
- Never widen what a delete can reach.

**Compatibility**

- Settings, state files and Heroic-compatible keys are user data. A change that
  breaks reading existing files needs a migration and a breaking-change marker on the
  PR (see below).

## 11. Branches, commits and pull requests

These rules are enforced by the **PR rules** check (`.github/workflows/pr-rules.yml`,
logic in `.github/scripts/pr_rules.py`). It fails a pull request that breaks them.

### Types

| Type | Use for | Branch | Title |
| --- | --- | --- | --- |
| `feat` | a new user-facing capability | `feat/cli-login` | `feat(manage): sign in from the command line` |
| `fix` | a bug fix | `fix/epic-dlc-playtime` | `fix(stats): count store playtime for games, not their DLC` |
| `perf` | faster or leaner, no behaviour change | `perf/library-filter` | `perf(library): filter once per revision` |
| `refactor` | restructuring, no behaviour change | `refactor/split-install-sheet` | `refactor(game): split the install sheet` |
| `docs` | documentation only | `docs/agents-md` | `docs: add AGENTS.md` |
| `test` | tests only | `test/gog-manifests` | `test(gog): cover v1 manifests` |
| `i18n` | translations and catalogs only | `i18n/polish-settings` | `i18n(settings): fix Polish plural forms` |
| `build` | Cargo manifests, dependencies, `packaging/` | `build/wry-0.54` | `build: update wry to 0.54` |
| `ci` | `.github/` workflows and scripts | `ci/pr-rules` | `ci: enforce pull request naming` |
| `chore` | maintenance that fits nothing else | `chore/cleanup-assets` | `chore: remove unused theme files` |
| `revert` | reverting an earlier change | `revert/sidebar-blur` | `revert: "feat(ui): blur the sidebar"` |

### Branch names

- `<type>/<short-kebab-description>`: lower case, digits, `-` and `.` only, with no
  further slashes. For example `fix/epic-login`, `feat/big-picture-search`,
  `build/wry-0.54`.
- One branch per topic. Branch from an up-to-date `main`.
- Never commit to `main` directly.
- Dependabot and Renovate branches are exempt.
- Coding agents: branch names such as `claude/…` or `codex/…` fail the check. Create
  or rename the branch with a type prefix before opening the PR.

### Pull request titles

- `<type>(<scope>): <summary>` or `<type>: <summary>`, with **the same type as the
  branch**.
- `scope` is optional, lower-case kebab: the area or crate without the `ember-` prefix
  (`epic`, `gog`, `store-epic`, `sign-in`, `library`, `game`, `manage`, `settings`,
  `bigpicture`, `ui`, `shell`, `runtime`, `depot`, `services`, `packaging`, `i18n`).
- The summary is imperative and lower case ("keep", not "Keeps" or "Kept"). An
  acronym in capitals may lead ("fix(gog): GOG Galaxy …" is fine). No trailing period.
- At most 72 characters for the whole title.
- A breaking change puts `!` before the colon (`feat(settings)!: move overrides to …`)
  and describes the break and the migration in the PR body. Breaking means changing
  settings keys, on-disk state, the CLI, the `ember-sign-in` command line or the
  `ember://` links.

### Base branch

- Pull requests target `main`.
- For stacked work, a PR may target another convention-named branch; retarget it to
  `main` when its parent merges.
- Never target a branch that has already merged: commits merged into it never reach
  `main`. That is how the translucency change in PR #2 missed `main`.

### Commits

- Same format as titles: `fix(epic): treat Mac ports released separately as native`.
- Add a body that explains *why*, wrapped at ~72 columns. Write the subject line in
  the imperative.
- Keep commits focused: unrelated changes go in separate commits, ideally separate
  PRs. Don't mix formatting-only churn with logic changes.
- Never commit secrets, `target/`, `dist/`, editor files or local paths.

### What a pull request contains

- One topic. Large features land in reviewable steps, stacked if necessary.
- The template (`.github/pull_request_template.md`), filled in:
  - What changed and why.
  - How it was checked, with the commands and the platforms tried. Say which
    platforms you didn't try.
  - Translations, docs, secrets and breaking-change checklist.
  - Screenshots for UI changes, in light and dark.
- Docs updated when behaviour changes: README, `docs/PLATFORMS.md`, this file.
- Green CI: the platform matrix, fmt, clippy, tests and packaging, plus PR rules.
- Keep the PR current with `main` by merging `main` in, or by rebasing your own
  unshared branch. Don't force-push a branch someone else is reviewing without saying
  so.

### Merging

- **Squash and merge**, with the PR title as the commit subject. That keeps `main`
  readable as one Conventional Commit per change.
- Delete the branch after merging.

## 12. Definition of done

A change is done when all of these hold:

- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo clippy --locked --workspace --all-targets -- -D warnings` passes, at
      minimum for every crate you touched and the crates that depend on them.
- [ ] `cargo test --locked` passes for those crates; the whole workspace before
      opening a PR.
- [ ] New behaviour has tests; a bug fix has a test that fails without the fix.
- [ ] User-facing text is translated into all 19 languages.
- [ ] It builds on Linux, macOS and Windows: no `#[cfg]` that leaves a platform
      without code.
- [ ] You ran Ember and exercised the change (UI, login, install, launch, whatever it
      touches), or the PR says plainly that you couldn't.
- [ ] Docs match the new behaviour.
- [ ] No secrets, personal data or absolute local paths in the diff.
- [ ] Branch, commits and PR title follow [section 11](#11-branches-commits-and-pull-requests).

## 13. Pitfalls

These are things that have bitten this codebase before:

- **`cargo run -p ember` builds only `ember`.** On Linux, sign-in falls back to the
  browser until you `cargo build -p ember-sign-in` for the same profile. macOS and
  Windows run the window from Ember itself.
- **Package names are not directory names.** `crates/store-epic` is `ember-epic`, and
  `crates/epic` is `ember-epic-api`.
- **Don't wait for `LibraryChanged` in UI code.** Wait for `LibraryUpdated`; the
  snapshot isn't rebuilt yet when `LibraryChanged` fires.
- **DLC are in `store.games()`.** Anything that counts or lists games must filter
  `is_dlc`. Epic even reports playtime per DLC, repeating the base game's time.
- **An Epic game can have several releases.** A Mac port can arrive as a release of
  its own; read every `releaseInfo` entry, not just the first.
- **Settings keys are Heroic's**, typos included. Renaming one silently drops the
  user's setting.
- **`Paths::init` only works once per process.** In tests, initialize once (for
  example in a `Once`) and give each test its own subdirectory.
- **Never enable `gpui-kit/test-support`** (see [8](#8-tests)).
- **Never add a web engine to Ember's window.** `crates/webview` is vendored but
  excluded and unused. Sign-in runs in its own process.
- **WebKitGTK must stay optional on Linux.** Linking it into `ember` would stop Ember
  from starting on systems without it.
- **macOS packages must not link Homebrew libraries.** `package.py` rejects them. Link
  statically (as with `xz2`'s `static` feature) or use system frameworks.
- **The Windows app is a GUI-subsystem executable** (`windows_subsystem = "windows"`).
  It has no console, so anything a parent must read goes through pipes the parent
  provides, as the sign-in mode does.
- **The UI thread must never wait on I/O.** Even "quick" file reads go through
  `ember_ui::run` or `backend.spawn_blocking`.

## 14. Notes for coding agents

- Read the files you're changing and their callers before editing. Read the relevant
  `docs/parity/*.md` before changing store, launch or download behaviour.
- When several agents build in one checkout, give each its own target directory
  (`CARGO_TARGET_DIR=target/agents/<name>`) to avoid lock contention. It's
  unnecessary on your own.
- A full workspace build is heavy. While iterating, use `cargo check -p <package>` and
  `cargo test -p <package>`. Run the full clippy and test commands before you push.
- Don't change public contracts (`Store`, `Backend`, events, settings keys,
  `ember-sign-in` arguments, CLI) unless the task needs it. If it does, update every
  implementation and caller in the same change and call it out in the PR.
- Don't touch unrelated files, reformat code you didn't change, or "tidy" other
  crates in passing.
- You can't see the UI. Describe what you verified and how; never claim you checked
  a screen you didn't render.
- Report honestly in the PR: what is done, what isn't, what you couldn't test, and on
  which platforms.
