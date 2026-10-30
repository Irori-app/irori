# Desktop platform support

Irori separates the operating system running the launcher from the platform of
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
Irori checks it before initializing a prefix and reports a missing prerequisite.
Runtime availability does not guarantee game compatibility. DRM, anticheat,
graphics APIs, CPU instructions, and the selected runtime still matter.

## macOS compatibility layers

The manager downloads the same three macOS source families used by Heroic:

- [Game Porting Toolkit](https://github.com/Gcenx/game-porting-toolkit/releases).
- [Wine-Crossover](https://github.com/Heroic-Games-Launcher/wine-crossover/releases).
- [Wine-Staging-macOS](https://github.com/Gcenx/macOS_Wine_builds/releases).

Wine-Crossover is a downloadable Wine distribution. It is distinct from the
commercial CrossOver application and does not create CrossOver bottles.

Discovery includes Irori/Heroic downloads, Wine app bundles, Homebrew paths,
CrossOver and CrossOver Preview in `/Applications` and `~/Applications`, and
Whisky's Wine installation. Custom paths may point to a runtime executable.
CrossOver uses the bottle named in a game's Wine settings ("CrossOver bottle"),
located under `~/Library/Application Support/CrossOver/Bottles` unless General ›
Folders › "CrossOver bottles folder" points elsewhere (for bottles moved to another
drive in CrossOver's preferences). Create the bottle in CrossOver first. Irori uses
`CX_BOTTLE` and `CX_BOTTLE_PATH`, including for setup and wineserver commands, and
does not initialize an external bottle. Uninstall refuses to delete a shared CrossOver bottle; keep the prefix and manage it in CrossOver. Whisky uses its runtime with an Irori
prefix; existing Whisky bottles are not imported automatically.

macOS settings include Msync, Metal HUD, Rosetta AVX advertisement and Retina mode.
Retina mode sets `HKCU\Software\Wine\Mac Driver\RetinaMode` in the prefix before
launch, only when it differs from what `user.reg` already holds, so Wine, CrossOver
and Game Porting Toolkit render at the display's full resolution. Linux
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
cargo build --locked --release -p irori -p irori-sign-in
python3 packaging/package.py --platform macos --binary-dir target/release --output dist
```

Choose `linux`, `macos`, or `windows` for `--platform`; use `python` on Windows.
The output directory must not already contain the staging `Irori`/`Irori.app`.
Both binaries are required, so a package never silently omits store sign-in.

- Linux needs the GPUI libraries listed in CI, plus GTK 3 and WebKitGTK 4.1 for
  sign-in. `packaging/install.sh` retains the per-user Linux installation flow.
- macOS needs Xcode Command Line Tools. The package includes an `.app` with the
  helper and `irori://` URL declaration. XZ support links statically; packaging rejects
  non-system dynamic libraries so Homebrew paths cannot leak into distributed binaries.
  Move it into Applications before making
  shortcuts. `package.py` ad-hoc signs local builds. Set `IRORI_SIGN_IDENTITY` to
  a Developer ID for distribution signing, then notarize and staple with Apple's
  tools. An ad-hoc signature is not notarization.
- Windows needs the MSVC Rust toolchain, Visual Studio C++ Build Tools/Windows
  SDK, and WebView2 Runtime for sign-in. Unzip and run the included `install.ps1`
  to install under LocalAppData and register a Start-menu shortcut and URL
  handler. Packaging includes the x64 Visual C++ runtime DLLs from the installed
  Visual Studio redistributable directory, so Irori does not depend on build tools
  or an existing VC runtime installation on the user's machine. Set
  `IRORI_WINDOWS_CRT_DIR` to an explicit x64 `Microsoft.VC*.CRT` directory if
  discovery cannot find it. Keep the build toolchain current and rebuild packages
  to ship runtime security updates; these app-local DLLs are updated with Irori.
  See Microsoft's [deployment guidance](https://learn.microsoft.com/en-us/cpp/windows/deployment-in-visual-cpp).
  No administrator access is required for Irori itself. Game
  redistributables may have their own UAC requirements. Public builds need
  Authenticode signing before distribution.

CI builds and tests Linux, Windows x64, macOS Intel and Apple silicon and uploads
unsigned validation packages using the small `ci` Cargo profile. It does not publish a release.
`release.yml` builds the release packages (tarball, `.deb`, `.rpm`, Flatpak, macOS
disk images, the Windows installer and zip), tests that they install and start, and
drafts a GitHub release for a `v*` tag; [RELEASING.md](RELEASING.md) covers signing
and the package channels.

## Saved logins

Store logins are JSON files under `<config>/stores/<store>/`, encrypted with AES-256-GCM.
The key is one random value in the system keyring: Secret Service on Linux (GNOME Keyring,
KWallet), Keychain on macOS, Credential Manager on Windows. Irori stores tokens, never
store passwords.

Without a keyring (no Secret Service running, a session without D-Bus), logins stay usable
as plain files only the user can read, and are encrypted the next time they are read with
a keyring available. If the keyring is locked or its key is gone, Irori leaves the files
alone, says so, and the stores look logged out until it is unlocked or the user logs in
again. Sandboxed packages need access to `org.freedesktop.secrets` to use the keyring.

On macOS an ad-hoc signed build asks for Keychain access after each update, because the
signature changes. A build signed with a Developer ID asks once.

## Paths and desktop behavior

| Data | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Settings/credentials | XDG config / `irori` | `~/Library/Application Support/irori` | RoamingAppData / `irori` |
| Runtimes/data | XDG data / `irori` | `~/Library/Application Support/irori` | LocalAppData / `irori` |
| Cache | XDG cache / `irori` | `~/Library/Caches/irori` | LocalAppData / `irori` (dirs cache location) |
| Steam default | `~/.steam/steam` | `~/Library/Application Support/Steam` | ProgramFiles(x86) / `Steam` |

`IRORI_HOME` relocates settings, data and cache under one root and is useful for
isolated testing. A configured Steam path takes precedence when it exists; otherwise,
on Linux, Irori uses the first of `~/.steam/steam`, `~/.local/share/Steam`,
`~/.steam/debian-installation`, Flatpak Steam (`~/.var/app/com.valvesoftware.Steam`,
`.steam/steam`, `.local/share/Steam` or `data/Steam`) and Snap Steam
(`~/snap/steam/common/.local/share/Steam`) that has a library.

The Steam library scans that Steam folder and every library in its
`steamapps/libraryfolders.vdf` for `appmanifest_*.acf` files, skipping Proton, the
Steam Linux Runtime and other tools, and watches those `steamapps` folders so installs
and removals made in Steam show at once. Art comes from Steam's own cache
(`appcache/librarycache/<id>`, where newer apps keep each image in a hashed subfolder),
else from the store (`IStoreBrowseService/GetItems`), else from the fixed CDN paths that
only older apps have.

Logging in to Steam is optional (Accounts › Steam). Irori draws Steam's own login: a QR
code for the Steam Mobile app, or the account name and password followed by Steam Guard.
The password goes to Steam only; Irori keeps the account's refresh token (about 200
days) in `<config>/stores/steam/session.json`, stored like every login (below), and renews
the access token itself. With an account, the library adds every game it owns and the games
its Steam Family shares (`library.json` in Steam's cache folder keeps them for offline
starts); Install hands those to Steam (`steam://install/<id>`), as Verify files and
Uninstall do (`steam://validate`, `steam://uninstall`).

Playing opens `steam://rungameid/<id>` through `xdg-open`, `open` or the Windows URL
handler, so Steam applies its own Proton version, launch options and overlay. A game's
launch options in Irori (its menu, `launcherArgs`) are passed with
`steam -applaunch <id> <args>` (`flatpak run com.valvesoftware.Steam` for Flatpak Steam),
or with `steam://run/<id>//<args>/` when Steam's program isn't found, where Steam asks
the user to confirm them. The opener exits at once, so the session follows the game
itself: on Linux Steam's per-game reaper (`reaper SteamLaunch AppId=<id>`), on every
system the processes running from the game's folder, and the manifest's running,
updating and validating flags while Steam prepares the launch. Found processes are
checked again directly every 3 seconds instead of scanning every process. Stop ends
the reaper and the game's processes, never the Steam client. Playtime is the larger of
the minutes Steam keeps in `userdata/<user>/config/localconfig.vdf` (the logged-in
account's own folder when there is one) and the account's own total.

itch.io (turned on in Settings › General › Integrations, or from its account card)
signs in with an API key from `https://itch.io/user/settings/api-keys`, kept owner-only
in the store's config folder. Installs download the upload that fits the platform into
`<install folder>/.itch-download/<game id>/` (resumable), then unpack zip and tar
archives (gzip, xz, bzip2) into the game folder; other single files such as AppImages
or portable `.exe` files are copied as they are, and disk images, 7-Zip and RAR files
are refused. Updates unpack next to the game and swap folders, so a failed update leaves
the old files in place. The program to start comes from the game's `.itch.toml` when it
has one, else from a search of the folder.

Epic Games, GOG, Amazon Games, ZOOM Platform and itch.io take several accounts each.
The first account keeps the store's usual folders (`stores/<store>` under the config
and cache roots), exactly as a single login always has. Each further account gets
`stores/<store>/accounts/<n>/` for its login and installs and `accounts/<store>/<n>/`
under the cache root; `stores/<store>/accounts.json` remembers which account each
folder holds. A game installed with one account belongs to it: updates, repairs, cloud
saves and launches use that account. For a game several accounts own, the install
sheet asks which account installs it (the first one listed by default), and the
library's account switcher shows one account's games while its store is shown. GOG's Galaxy service listens on one port, so while a game of one
GOG account runs, a game of another GOG account started alongside it gets no Galaxy
achievements. Galaxy's in-game overlay isn't available in Irori.

On Linux a Steam game's menu offers Protontricks once Steam has made its Proton prefix
(`steamapps/compatdata/<id>/pfx`). Irori runs `protontricks` from `PATH`, or the Flathub
app `com.github.Matoking.protontricks`, with `STEAM_DIR` set to the Steam folder it reads.
protontricks only knows Steam app IDs, so games Irori runs itself keep using winetricks.

Each configuration root has one running Irori instance, protected by an OS file
lock. Subsequent invocations forward arguments over authenticated loopback IPC.
The tray uses the desktop's native integration. macOS uses Command shortcuts and a native app menu; Windows/Linux retain Control shortcuts. Games inhibit sleep through the
Linux portal, macOS caffeinate, or Windows thread execution state. Desktop/menu
shortcuts use `.desktop`, Finder app bundles, or Windows shell links respectively.
Windows cancellation uses `taskkill /T /F`; games that deliberately escape their
launcher process tree still require game-specific testing. External EA/Ubisoft
launchers may reuse an existing process, which limits playtime tracking.

### New versions of Irori

Irori looks for its own new versions only where nothing else updates it. It works out
how it was installed when it starts:

| Install | Recognised by | Irori |
| --- | --- | --- |
| Flatpak | `FLATPAK_ID` or `/.flatpak-info` | Never checks |
| apt, dnf, pacman, AUR | the program lives under `/usr` (not `/usr/local`) | Never checks |
| Nix | the program lives under `/nix/store` | Never checks |
| Built from source | the program lives in a Cargo `target` folder | Never checks |
| Any build with `IRORI_UPDATE_EXPLANATION` | the variable, at build or run time | Never checks; Settings › About shows its text |
| Homebrew | `Caskroom/irori` under `/opt/homebrew` or `/usr/local` | Checks; offers `brew upgrade --cask irori` |
| Scoop | `scoop/apps/irori` in the program's path | Checks; offers `scoop update irori` |
| winget | `install.ini` beside the program says `Channel=winget` | Checks; offers `winget upgrade Irori.Irori` |
| Windows setup or zip, disk image, macOS zip, tarball | none of the above | Checks; links to the release page |

The check asks GitHub's `releases/latest` for `irori-app/irori` (which leaves out drafts
and pre-releases) at most once a day, only while online and only in the app, never in the
commands without a window. Only `vX.Y.Z` tags count. `state/app-update.json` keeps the
time of the last answer, the newest version, the version the user dismissed and the
version that ran last. A newer release shows a notice that stays until closed; closing
it, or using its button, dismisses that version. Settings › About shows the state and
"Check now", and Settings › General › Updates turns the check off
(`iroriCheckForAppUpdates`). The first start of a new version says so, with a link to its
release notes. Irori never downloads or installs itself.

Packagers who build Irori themselves set `IRORI_UPDATE_EXPLANATION` (for example
"Update Irori with your distribution's package manager.") when building.

### Steam Deck and Steam Input

When Steam's controller interface starts Irori (`XDG_CURRENT_DESKTOP=gamescope`
in the Deck's Game Mode, or `SteamGamepadUI=1` from Steam Big Picture), Irori
opens in Big Picture and never starts hidden in the tray; Settings → General →
"Open Big Picture in Steam's Game Mode" turns this off. Controllers listed in
`SDL_GAMECONTROLLER_IGNORE_DEVICES` (or left out of
`SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT`) are not read, because Steam Input
drives them through its virtual controller, and arrow, Enter and Escape key
presses that echo a controller press within 150 ms are dropped. Steam
shortcuts carry an "Irori" collection tag and may point at games that aren't
installed; `--launch` then shows the install options instead of failing.

## Window translucency

Settings → Appearance → Translucency lets the desktop show through the title bar
and sidebar (Subtle), or the pages too (Strong). The blur is the system's, so
Irori draws exactly what it draws when opaque; only some colours carry alpha.

| Host | Backdrop | Automatic |
| --- | --- | --- |
| macOS | Native vibrancy (blur only, untinted) | On |
| Windows 11 22H2+ | Mica | On |
| Windows 10 / 11 21H2 | Acrylic (lags while dragging) | Off |
| Linux, compositor with `ext-background-effect-v1` blur | Compositor blur | On |
| Linux, KWin before 6.7 on Wayland | KWin blur (`org_kde_kwin_blur`) | On |
| Linux, KWin on X11 | KWin blur (`_KDE_NET_WM_BLUR_BEHIND_REGION`) | On |
| Linux, Hyprland without the protocol | Hyprland's own blur | On |
| Linux, anything else | See-through, no blur | Off |
| gamescope | None | Off |

On Linux, Irori asks the compositor what it can do instead of going by the
desktop's name (`irori_ui::glass`, logged at start as `desktop blur: …`). KWin
6.7 removed `org_kde_kwin_blur`, the only blur protocol GPUI speaks, so Irori
binds `ext-background-effect-v1` itself on GPUI's Wayland connection. A
compositor that has the protocol with its blur switched off counts as having no
blur, and a change takes effect at the next start. The blur region is the visible
window, without the margin client-side decorations keep for their shadow. KWin
6.7 measures it from the window geometry rather than the surface, so Irori
offsets it there; if KWin starts following the protocol, the blur will sit 20
pixels off and `irori_ui::glass` needs to stop.

| Desktop | True blur from Irori's request |
| --- | --- |
| KDE Plasma 6.7+ | Yes, unless the Blur desktop effect is off |
| KDE Plasma before 6.7, Wayland or X11 | Yes |
| GNOME 51+ | Yes |
| GNOME before 51 | No. Blur my Shell can blur applications by window class |
| Hyprland | Yes |
| niri 26.04+ | Yes |
| COSMIC with Frosted Glass | Yes |
| Sway | No |
| SwayFX | No request: set `blur enable`, which is off by default |
| X11 without KWin | No. picom and the like can add their own by window class |

Irori has been run on KDE Plasma 6.7 (Wayland, and X11 windows under its
Xwayland). The other rows come from each project's own documentation and source.

Where nothing blurs, Automatic stays opaque. Subtle and Strong still work
because a compositor's own rule may add blur (match the app id or window class
`irori`), but the surfaces keep more body, as sharp detail behind text is hard
to read.

gpui-kit paints the dim behind dialogs and sheets across the whole surface, which
on Linux includes that margin, whatever the translucency level. So Irori keeps the
theme's `overlay` colour transparent there and the shell paints the dim inside the
visible window (`irori_ui::theme::modal_dim`). A dialog stacked on another no
longer dims the one below it on Linux.

The window is opaque in Big Picture and fullscreen, where nothing behind it is
worth showing, which also spares the compositor the blur. Views take their
surface colours from `irori_ui::glass` (`chrome`, `page`, `solid_pages`) rather
than adding alpha themselves.

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
Irori uses its own Rust abstractions; the clone is reference material, not a
runtime dependency or a vendored copy.

Windows argument handling follows Microsoft's
[CRT parsing rules](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments).
The parser/encoder has exhaustive short-input roundtrip tests covering quotes,
slashes, whitespace, Unicode and metacharacters.

On Windows, Epic Launcher synchronization accepts its native `C:\ProgramData\Epic\EpicGamesLauncher\Data` folder (or `Manifests` beneath it). On Wine hosts it continues to accept a Wine prefix.

macOS base-prefix creation temporarily suppresses Wine’s interactive Mono/Gecko downloader. Its built-in checksum validation is unchanged; the overrides apply only to `wineboot`, never to games or stored prefix settings. Games needing .NET/HTML dependencies must install the appropriate component through Wine tools/Winetricks. A successful prefix smoke test does not verify Mono or a .NET game.

Opt-in real archive/prefix smoke test (downloads roughly 200–240 MB per source; Rosetta required on Apple silicon): set a dedicated absolute `IRORI_HOME`, set `IRORI_RUNTIME_SMOKE` to `Wine-Crossover`, `Wine-Staging-macOS`, or `Game-Porting-Toolkit`, then run `cargo test -p irori-runtime macos_runtime_archive_smoke -- --ignored --nocapture`. Run it separately from the ordinary suite.
