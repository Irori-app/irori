# Desktop platform support

Ember separates the operating system running the launcher from the platform of
each installed game. A Windows build needs Wine on Linux/macOS and runs directly
on Windows. Native macOS, Linux, and browser builds bypass Wine. A build for a
different native OS is rejected before launch.

| Host | Native game builds | Windows game builds | Runtime manager |
| --- | --- | --- | --- |
| Linux | Linux, browser | Wine / Proton / umu | GE-Proton, CachyOS, Wine-GE, Kron4ek |
| macOS Intel | macOS, browser | macOS Wine / CrossOver | Wine-Crossover, Wine-Staging-macOS |
| macOS Apple silicon | macOS, browser | macOS Wine / CrossOver / Whisky / GPTK | Game-Porting-Toolkit, Wine-Crossover, Wine-Staging-macOS |
| Windows x64 | Windows, browser | Native execution | No Wine download required |

The GPTK tab is shown on macOS, but its builds are compatible only with Apple
silicon on macOS 14 or later. Intel Wine on Apple silicon requires Rosetta 2;
Ember checks it before initializing a prefix and reports a missing prerequisite.
Runtime availability does not guarantee game compatibility. DRM, anticheat,
graphics APIs, CPU instructions, and the selected runtime still matter.

## macOS compatibility layers

The manager downloads the same three macOS source families used by Heroic:

- [Game Porting Toolkit](https://github.com/Gcenx/game-porting-toolkit/releases).
- [Wine-Crossover](https://github.com/Heroic-Games-Launcher/wine-crossover/releases).
- [Wine-Staging-macOS](https://github.com/Gcenx/macOS_Wine_builds/releases).

Wine-Crossover is a downloadable Wine distribution. It is distinct from the
commercial CrossOver application and does not create CrossOver bottles.

Discovery includes Ember/Heroic downloads, Wine app bundles, Homebrew paths,
CrossOver and CrossOver Preview in `/Applications` and `~/Applications`, and
Whisky's Wine installation. Custom paths may point to a runtime executable.
CrossOver uses the bottle selected in settings, located under
`~/Library/Application Support/CrossOver/Bottles`. Create the bottle in CrossOver
first. Ember uses `CX_BOTTLE`, including for setup and wineserver commands, and
does not initialize an external bottle. Uninstall refuses to delete a shared CrossOver bottle; keep the prefix and manage it in CrossOver. Whisky uses its runtime with an Ember
prefix; existing Whisky bottles are not imported automatically.

macOS settings include Msync, Metal HUD and Rosetta AVX advertisement. Linux
wrappers, Fsync, GameMode, gamescope, MangoHud, Linux DXVK/VKD3D/NVAPI installation,
umu, and Linux anticheat runtimes are not automatically applied on macOS or
Windows, even when old settings enable them. Explicit custom environment variables
and wrappers remain user overrides.

Native `.app` launches read `CFBundleExecutable` from XML or binary Info.plist
and spawn that executable with the game's arguments and environment. This keeps
the process attached for exit status, logs, cancellation, and playtime.

## Build and package

Use Rust 1.95 or newer and Python 3.11+ for packaging; CI uses current stable Rust. Build on the target operating
system. Cross-compiling the complete GPUI/WebView stack is not the supported
release workflow.

```sh
cargo build --locked --release -p ember -p ember-sign-in
python3 packaging/package.py --platform macos --binary-dir target/release --output dist
```

Choose `linux`, `macos`, or `windows` for `--platform`; use `python` on Windows.
The output directory must not already contain the staging `Ember`/`Ember.app`.
Both binaries are required, so a package never silently omits store sign-in.

- Linux needs the GPUI libraries listed in CI, plus GTK 3 and WebKitGTK 4.1 for
  sign-in. `packaging/install.sh` retains the per-user Linux installation flow.
- macOS needs Xcode Command Line Tools. The package includes an `.app` with the
  helper and `ember://` URL declaration. Move it into Applications before making
  shortcuts. `package.py` ad-hoc signs local builds. Set `EMBER_SIGN_IDENTITY` to
  a Developer ID for distribution signing, then notarize and staple with Apple's
  tools. An ad-hoc signature is not notarization.
- Windows needs the MSVC Rust toolchain, Visual Studio C++ Build Tools/Windows
  SDK, and WebView2 Runtime for sign-in. Unzip and run the included `install.ps1`
  to install under LocalAppData and register a Start-menu shortcut and URL
  handler. No administrator access is required for Ember itself. Game
  redistributables may have their own UAC requirements. Public builds need
  Authenticode signing before distribution.

CI builds and tests Linux, Windows x64, macOS Intel and Apple silicon and uploads
unsigned validation packages using the small `ci` Cargo profile. It does not publish a release.

## Paths and desktop behavior

| Data | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Settings/credentials | XDG config / `ember` | `~/Library/Application Support/ember` | RoamingAppData / `ember` |
| Runtimes/data | XDG data / `ember` | `~/Library/Application Support/ember` | LocalAppData / `ember` |
| Cache | XDG cache / `ember` | `~/Library/Caches/ember` | LocalAppData / `ember` (dirs cache location) |
| Steam default | `~/.steam/steam` | `~/Library/Application Support/Steam` | ProgramFiles(x86) / `Steam` |

`EMBER_HOME` relocates settings, data and cache under one root and is useful for
isolated testing. Configured Steam paths take precedence over defaults.

Each configuration root has one running Ember instance, protected by an OS file
lock. Subsequent invocations forward arguments over authenticated loopback IPC.
The tray uses the desktop's native integration. Games inhibit sleep through the
Linux portal, macOS caffeinate, or Windows thread execution state. Desktop/menu
shortcuts use `.desktop`, Finder app bundles, or Windows shell links respectively.
Windows cancellation uses `taskkill /T /F`; games that deliberately escape their
launcher process tree still require game-specific testing. External EA/Ubisoft
launchers may reuse an existing process, which limits playtime tracking.

## Verification and remaining machine tests

The regression suite checks the host/build matrix, unsupported runtime families,
Linux environment preservation, macOS environment isolation and CrossOver bottle
selection, release asset architecture selection, macOS bundle layouts and native
launches, argument quoting, instance forwarding, and native shortcuts. Platform
specific process tests run on their native CI hosts.

`postman/simulations/runtime-releases.context.md` explains the local HTTP test for
the three macOS release sources, including GitHub rate-limit errors. No real store
accounts or purchases are used by automated tests.

Before calling a platform release ready, run these checks on actual machines:

1. Sign in to each store using the packaged helper and browser fallback.
2. Install, update, repair, launch and uninstall a native game and a Windows game
   from each applicable store; test prerequisites needing elevation on Windows.
3. On macOS, launch one game with each installed Wine family, including a named
   CrossOver bottle; validate Metal rendering, sound, controllers, cloud saves,
   Rosetta, and Intel/Apple silicon behavior separately.
4. Verify cancellation kills child processes, tray quit/reopen, URL links while
   already running, shortcuts, and sleep inhibition on each desktop.
5. Validate signed/notarized release packages on clean machines without the
   development tools installed. Exercise anticheat/DRM only where the vendor
   supports the chosen runtime.

Automated checks do not establish that every game works. See the task's
verification report for which checks were actually run and their results.

## Reference inspection

Heroic Games Launcher was cloned and inspected at commit
[`509a34feea8541b73c928e289f2cf262d432db4a`](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/tree/509a34feea8541b73c928e289f2cf262d432db4a).
The reference covered `src/backend/launcher.ts`,
`src/backend/utils/compatibility_layers.ts`, and
`src/backend/wine/manager/downloader/{constants,main,utilities}.ts`.
Ember uses its own Rust abstractions; the clone is reference material, not a
runtime dependency or a vendored copy.

Windows argument handling follows Microsoft's
[CRT parsing rules](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments).
The parser/encoder has exhaustive short-input roundtrip tests covering quotes,
slashes, whitespace, Unicode and metacharacters.

On Windows, Epic Launcher synchronization accepts its native `C:\ProgramData\Epic\EpicGamesLauncher\Data` folder (or `Manifests` beneath it). On Wine hosts it continues to accept a Wine prefix.

macOS base-prefix creation temporarily suppresses Wine’s interactive Mono/Gecko downloader. Its built-in checksum validation is unchanged; the overrides apply only to `wineboot`, never to games or stored prefix settings. Games needing .NET/HTML dependencies must install the appropriate component through Wine tools/Winetricks. A successful prefix smoke test does not verify Mono or a .NET game.

Opt-in real archive/prefix smoke test (downloads roughly 200–240 MB per source; Rosetta required on Apple silicon): set a dedicated absolute `EMBER_HOME`, set `EMBER_RUNTIME_SMOKE` to `Wine-Crossover`, `Wine-Staging-macOS`, or `Game-Porting-Toolkit`, then run `cargo test -p ember-runtime macos_runtime_archive_smoke -- --ignored --nocapture`. Run it separately from the ordinary suite.
