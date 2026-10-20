# Ember

A fast, native game launcher for Linux. Ember plays your **Epic Games**, **GOG**,
**Amazon Games** and **ZOOM Platform** libraries plus any game you add yourself, running
Windows games through Wine, Proton or umu.

Ember is written in Rust with [GPUI](https://www.gpui.rs) and
[GPUI Kit](https://gpui-kit.com). It is a new application, built from scratch to
match the feature set of Heroic Games Launcher without Electron: one native process,
GPU-rendered UI, a small Tokio runtime for I/O, and art that is thumbnailed once to disk
and kept in a bounded GPU cache.

## Highlights

- One library across stores: fuzzy search, filters, sorting, categories, favourites,
  hidden games, "Continue playing", grid and list views — virtualized for large libraries.
- One-click play from anywhere: library tiles, the game page, the Ctrl+K command palette,
  the tray menu, desktop/Steam shortcuts and `ember://launch/<runner>/<app>` links
  (Heroic's `heroic://` links work too).
- Download queue with pause/resume, reordering, speed and ETA, history and "Update all".
- Per-game settings that inherit your global defaults and show what you overrode.
- Wine/Proton manager (GE-Proton, Proton-CachyOS, Wine-GE, Kron4ek) and discovery of
  system, Steam, Lutris and Bottles builds; DXVK, VKD3D-Proton, DXVK-NVAPI, gamescope,
  MangoHud, GameMode, umu and the Steam runtime.
- Cloud saves (Epic, GOG), GOG achievements and playtime, EOS overlay, ProtonDB, Steam
  Deck, anticheat and HowLongToBeat info.
- Big Picture mode and full controller navigation (Xbox, PlayStation incl. DualSense,
  Switch Pro, Steam Deck), with hotplug and battery display.
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

Requirements: Rust 1.85+, and the usual GPUI system libraries (Vulkan, Wayland, X11/xcb,
xkbcommon, fontconfig, freetype). The sign-in window (`cargo build -p ember-sign-in`)
also needs WebKitGTK 4.1 (`webkit2gtk4.1-devel` on Fedora, `libwebkit2gtk-4.1-dev` on
Debian/Ubuntu); Ember runs without it. Store downloads, updates and cloud saves are
built in: Ember needs no external store tools.

```bash
cargo run --release -p ember
```

Install for your user (binary, desktop entry, icon and `ember://` handler):

```bash
packaging/install.sh
```

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/core` | Domain types, settings schema, the `Store` trait, process/HTTP/JSON utilities |
| `crates/runtime` | Wine/Proton discovery and downloads, prefixes, DXVK/VKD3D, launch environment |
| `crates/store-*` | One store backend each: Epic, GOG, Amazon, ZOOM, sideloaded apps |
| `crates/depot` | Shared download engine: parallel chunked downloads, resume, verify/repair, delta updates |
| `crates/epic`, `crates/gog`, `crates/amazon` | Store protocols (Rust ports of legendary, gogdl and nile) |
| `crates/gog-galaxy` | GOG Galaxy communication service for games (comet, as a library) |
| `crates/extras` | Shortcuts, wiki/ProtonDB/anticheat info, SteamGridDB, Heroic import |
| `crates/services` | Backend facade: library, settings, download queue, launching |
| `crates/ui` | Shared UI layer: models, navigation, art pipeline, theme, components |
| `crates/feature-*` | Screens: library, game page, settings, downloads/Wine/accounts, Big Picture |
| `crates/*/locales` | Translation catalogs per crate (`docs/I18N_BRIEF.md`) |
| `crates/ember` | The application shell and binary |

`docs/parity/` documents the behaviour Ember matches, `docs/AGENT_BRIEF.md`,
`docs/UI_BRIEF.md` and `docs/I18N_BRIEF.md` the engineering rules.

## Notes

- Store logins open in Ember's sign-in window (`ember-sign-in`, a small helper process
  using WebKitGTK) and finish on their own: Ember never needs anything copied. Without
  the helper, or with "Use the browser instead", the page opens in your browser and Ember
  watches the clipboard for the code or redirect URL.
- Discord Rich Presence needs a Discord application ID (Settings → General → Integrations).
- Data lives in `~/.config/ember`, `~/.local/share/ember` and `~/.cache/ember`;
  `EMBER_HOME` relocates all of it.

## License

GPL-3.0-or-later. Bundled community themes are from GPUI Kit (Apache-2.0). The store
protocol crates are ports of legendary, gogdl and nile (GPL-3.0); the Galaxy service uses
comet (Apache-2.0).
