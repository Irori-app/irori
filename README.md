<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/irori-lockup-dark.svg">
    <img alt="Irori" src="assets/brand/irori-lockup-light.svg" height="96">
  </picture>
</p>

<p align="center"><b>Every game you own, around one fire.</b><br><a href="https://irori.app">irori.app</a></p>

An *irori* (囲炉裏) is the sunken hearth of a traditional Japanese house, the place
everyone gathers. Irori is a fast, native game launcher for Linux, macOS and Windows
that gathers your **Epic Games**, **GOG**, **Amazon Games**, **ZOOM Platform** and
**itch.io** libraries, your **Steam** games, your emulated games and anything
you add yourself into one calm library. Windows games run natively on Windows, through
Wine, Proton or umu on Linux, and through Wine, CrossOver or Game Porting Toolkit on
macOS.

## Install

Every release is on the [releases page](https://github.com/irori-app/irori/releases),
and [irori.app](https://irori.app/#download) has the commands for each system.

| Platform | Packages |
| --- | --- |
| Linux | apt, dnf and pacman repositories at `packages.irori.app`, a Flatpak bundle, tarball |
| macOS | Signed and notarized `.dmg`, Homebrew cask (`irori-app/tap/irori`) |
| Windows | Installer, Scoop bucket (`irori-app/scoop-bucket`) |

Windows builds aren't signed yet, so SmartScreen may warn the first time you run the
installer. Flathub, winget and the AUR are planned. To build Irori yourself, see
[Building](#building).

## About

Irori is written in Rust with [GPUI](https://www.gpui.rs) and
[GPUI Kit](https://gpui-kit.com). It is a new application, built from scratch to
match the feature set of Heroic Games Launcher without Electron: one native process,
GPU-rendered UI, a small Tokio runtime for I/O, and art that is thumbnailed once to disk
and kept in a bounded GPU cache.

## Highlights

- One library across stores: fuzzy search, filters, sorting, categories, favourites,
  hidden games, "Continue playing", grid and list views — virtualized for large libraries.
  Hide the stores you don't use, see installed games by drive, or let Irori pick a
  random game (Ctrl+Shift+G).
- One-click play from anywhere: library tiles, the game page, the Ctrl+K command palette,
  the tray menu, desktop/Steam shortcuts and `irori://launch/<runner>/<app>` links
  (Heroic's `heroic://` links work too).
- Download queue with pause/resume, reordering, speed and ETA, history and "Update all".
- One Updates page for games, Wine and Proton builds, DXVK, VKD3D-Proton, umu and the
  anticheat runtimes, emulators, RetroArch cores and Irori itself: check now, update
  everything, skip a version, stop updating a game, or go back to the Wine build an update
  replaced. Automatic updates can keep to set hours and stay off metered connections.
- Per-game settings that inherit your global defaults and show what you overrode.
- Added games keep their own description, developer, release date and genres.
- Run any game in gamescope with one switch, upscaling optional. Pick the DXVK and
  VKD3D-Proton versions for Wine builds, and turn on descriptor heap and Proton's DLSS,
  FSR and XeSS upgrades without looking up variables. The game's log can open by itself
  when it starts.
- Wine/Proton manager (GE-Proton, Proton-CachyOS, Wine-GE, Kron4ek) and discovery of
  system, Steam, Lutris and Bottles builds; DXVK, VKD3D-Proton, DXVK-NVAPI, gamescope,
  MangoHud, GameMode, umu and the Steam runtime.
- Cloud saves (Epic, GOG), GOG achievements and playtime, EOS overlay, ProtonDB, Steam
  Deck, anticheat and HowLongToBeat info.
- Big Picture mode and full controller navigation (Xbox, PlayStation incl. DualSense,
  Switch Pro, Steam Deck), with hotplug and battery display. It opens by itself in the
  Steam Deck's Game Mode, reads each press once alongside Steam Input, and scales up to
  200 % for TVs.
- "Add to Steam" for any game, installed or not, filed under an "Irori" collection;
  Steam starts Irori, which offers to install a game that isn't installed yet.
- itch.io, opt-in: log in with an API key, then install, update and play the games you
  own there. Irori downloads the best upload for your system (resumable), unpacks zip and
  tar archives and finds the program to start.
- Your installed Steam games in the same library, read from Steam's own files with no
  login, with Steam's own art. Log in to Steam (QR code or password with Steam Guard)
  to add every game you own and your Steam Family's games. Play hands games to Steam
  with your launch options and follows them until they end, so playtime counts;
  Install, Verify files and Uninstall open Steam's own dialogs. On Linux, Protontricks
  opens on a Steam game's Proton prefix from its menu.
- Several accounts per store (Epic Games, GOG, Amazon Games, ZOOM Platform, itch.io) in
  one library; each installed game launches with the account it was installed with.
- One-click import of your Heroic accounts, installs, settings, favourites and categories.
- Translated into 19 languages, switchable live; searchable settings.
- Tray menu with recent games, desktop and Steam shortcuts, Discord Rich Presence,
  EOS overlay, GOG REDmod and private branches, winetricks browser, log upload.
- Downloads pause automatically when you go offline and resume when you're back.

## Performance

Measured on the release build with a 16-game library and the window open: about
124 MiB resident (≈39 MiB of it heap; the rest is mapped GPU driver and libraries),
zero CPU while idle, a 49 MB binary. Cover art is decoded once into on-disk
thumbnails and held in an LRU GPU cache; every long list is virtualized.

## Building

Requirements: Rust 1.95+. See [platform support and packaging](docs/PLATFORMS.md) for macOS and Windows. Linux needs the usual GPUI system libraries (Vulkan, Wayland, X11/xcb,
xkbcommon, fontconfig, freetype). The sign-in window (`cargo build -p irori-sign-in`)
also needs WebKitGTK 4.1 (`webkit2gtk4.1-devel` on Fedora, `libwebkit2gtk-4.1-dev` on
Debian/Ubuntu); Irori runs without it. Store downloads, updates and cloud saves are
built in: Irori needs no external store tools.

```bash
cargo run --release -p irori
```

Install for your user (binary, desktop entry, icon and `irori://` handler):

```bash
packaging/install.sh
```

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/core` | Domain types, settings schema, the `Store` trait, process/HTTP/JSON utilities |
| `crates/runtime` | Wine/Proton discovery and downloads, prefixes, DXVK/VKD3D, launch environment |
| `crates/store-*` | One store backend each: Epic, GOG, Amazon, ZOOM, itch.io, Steam, sideloaded apps |
| `crates/depot` | Shared download engine: parallel chunked downloads, resume, verify/repair, delta updates |
| `crates/epic`, `crates/gog`, `crates/amazon` | Store protocols (Rust ports of legendary, gogdl and nile) |
| `crates/gog-galaxy`, `crates/comet` | GOG Galaxy communication service for games (a small fork of comet, as a library) |
| `crates/extras` | Shortcuts, wiki/ProtonDB/anticheat info, SteamGridDB, Heroic import |
| `crates/services` | Backend facade: library, settings, download queue, launching |
| `crates/ui` | Shared UI layer: models, navigation, art pipeline, theme, components |
| `crates/feature-*` | Screens: library, game page, settings, downloads/Wine/accounts, Big Picture |
| `crates/*/locales` | Translation catalogs per crate (`docs/I18N_BRIEF.md`) |
| `crates/irori` | The application shell and binary |

`docs/parity/` documents the behaviour Irori matches, `docs/AGENT_BRIEF.md`,
`docs/UI_BRIEF.md` and `docs/I18N_BRIEF.md` the engineering rules.

## Notes

- Store logins open in Irori's sign-in window (`irori-sign-in`, a small helper process
  using WebKitGTK on Linux, WKWebView on macOS, and WebView2 on Windows) and finish on their own: Irori never needs anything copied. On macOS
  and Windows, Irori shows the window itself when the helper isn't built (as after
  `cargo run -p irori`). On Linux without the helper, or with "Use the browser instead",
  the page opens in your browser and Irori watches the clipboard for the code or redirect URL.
- Discord Rich Presence needs a Discord application ID (Settings → General → Integrations).
- Data uses each operating system’s application-data and cache directories;
  `IRORI_HOME` relocates all of it. See [platform paths](docs/PLATFORMS.md#paths-and-desktop-behavior).

## License

MIT, see [LICENSE](LICENSE). Bundled community themes are from GPUI Kit (Apache-2.0),
and the Galaxy service uses a fork of comet (Apache-2.0, `crates/comet`). The store protocol crates are Rust ports
of the parts of legendary, gogdl and nile that Irori needs.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Everyone taking
part in the project follows the [code of conduct](CODE_OF_CONDUCT.md).
