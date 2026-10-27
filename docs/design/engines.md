# Design: engines, ROM libraries and the rest of Lutris (P4)

Status: accepted for implementation, October 2026.
Behaviour reference: [`docs/parity/lutris.md`](../parity/lutris.md) (Lutris at commit 1f32fe0).

P4 makes Irori the launcher that replaces Lutris as well as Heroic: emulators and ROM
libraries, community install scripts, the official third-party launchers (Battle.net, EA
app, Ubisoft Connect, Rockstar) and importing an existing Lutris setup. The bar is that
someone who has never heard of Wine, BIOS files or libretro cores points Irori at a ROM
folder and plays, with a controller, in under two minutes, on Linux, macOS or Windows.

Contents:

1. [Terminology and ids](#1-terminology-and-ids)
2. [Crates](#2-crates)
3. [Engines](#3-engines)
4. [The ROM library](#4-the-rom-library)
5. [Settings](#5-settings)
6. [User interface](#6-user-interface)
7. [Lutris import](#7-lutris-import)
8. [Lutris install scripts](#8-lutris-install-scripts)
9. [Third-party launchers](#9-third-party-launchers)
10. [Humble Bundle: not shipped](#10-humble-bundle-not-shipped)
11. [Licences and terms](#11-licences-and-terms)
12. [Delivery](#12-delivery)

---

## 1. Terminology and ids

Irori's `Runner` already means *the store a game belongs to* and is serialized into user
data, so P4 never reuses the word.

| Concept | In code | In the interface | Notes |
| --- | --- | --- | --- |
| An emulator, RetroArch, ScummVM, DOSBox | `Engine` (`irori_engines::EngineManifest`) | "Emulator" | Wine/Proton stay in `irori-runtime` and show as "Windows (Wine/Proton)". |
| A libretro core | `Core` | the core's own name ("Snes9x") | Shown only on the engine card and in "Play with…". |
| A console or platform | `System` | "System" (PlayStation, SNES, Arcade, MS-DOS) | Ids are short, lower case: `psx`, `snes`, `dos`. |
| An engine choice | `EngineRef`, written `duckstation` or `retroarch:snes9x` | "Play with…" | Stored in settings. |
| One install of an engine | `Installation`, id `managed:<version>`, `flatpak`, `system`, `app`, `windows` or `custom` | "Version" | Stored in settings when a game pins one. |

New serialized ids (they land in user data, so they are fixed now):

| Id | Where | Meaning |
| --- | --- | --- |
| `emulation` | `Runner::Emulation` | The ROM library store (P4.2). `GameId` keys look like `emulation:<sha1-of-path>`. |
| `launchers` | `Runner::Launchers` | Games of Battle.net, EA app, Ubisoft Connect and Rockstar Games Launcher (P4.5). App names are `<launcher>:<product id>`. |
| `iroriEngine`, `iroriEngineVersion`, `iroriEngineOptions`, `iroriEngineOwnSettings` | game settings | The game's engine, pinned installation, option values by engine, and whether to use the engine's own settings. Sparse per game like every other key. |
| `iroriSystemEngines`, `iroriRomFolders`, `iroriBiosFolder`, `iroriThirdPartyLaunchers` | app settings | Default engine per system, watched ROM folders, the BIOS folder, the launchers switch. |

Both new `Runner` values have no account (`has_account()` is false). `Emulation` manages
its own entries but has nothing to download per game; `Launchers` games belong to the
official launcher, so `manages_installs()` is false for it, like Steam.

## 2. Crates

```text
irori-core
  ├─ irori-runtime                    (Wine/Proton; unchanged)
  ├─ irori-engines   (new)            core, runtime (download, extraction), depot
  ├─ irori-extras                     + lutris_import (P4.3)
  ├─ irori-scripts   (new, P4.4)      core, runtime, depot; the install-script interpreter
  ├─ store crates
  │    irori-emulation (new, P4.2)    core, engines
  │    irori-launchers (new, P4.5)    core, runtime, extras
  └─ irori-services                   + engines.rs, emulation and scripts hooks
irori-ui, feature crates              the Emulators page lives in irori-manage
```

- `irori-engines` (`crates/engines`) has no UI, no settings knowledge beyond what it is
  given, and never blocks a Tokio worker. It owns manifests, discovery, managed installs,
  cores, the BIOS index and launch commands.
- `irori-emulation` (`crates/store-emulation`) is a `Store`. It owns the ROM library file
  and the scanner. Launching asks `irori-engines` for a command.
- `irori-services` gains `Backend` methods for engines (list, releases, install, update,
  remove, choose the default, custom path, cores, BIOS status and rescan). Installs report
  through the same `OpContext` progress as Wine builds and show in the Emulators page and
  the status bar.
- Feature crates never depend on each other. The Emulators page is in `irori-manage`
  beside the Wine manager, "Play with…" is in `irori-game`, the system filter and the
  setup guide are in `irori-library`, shelves are in `irori-bigpicture`.

## 3. Engines

### 3.1 Manifests

Each engine is one JSON file in `assets/engines/<id>.json`, compiled in. The user can
drop a file of the same name into `<config>/engines/`; it is merged key by key (JSON
merge patch) over the built-in one, or, with an id Irori does not ship, adds an engine.
A broken override is logged and the built-in manifest is used. Nothing in a manifest is
executed; it is data.

| Key | Meaning |
| --- | --- |
| `id`, `name`, `license`, `homepage` | Identity. `license` is shown on the engine card. |
| `kind` | `standalone`, `retroarch`, `dosbox`, `scummvm`, `mame` or `relinker`: the only per-engine code paths (the `Engine` behaviour of the brief), each a small `match` arm in `launch.rs`; `relinker` also converts the game first (`relink.rs`). |
| `systems` | System ids it plays. |
| `sources` | In priority order: `github` (repo + per-host asset regex + optional checksum asset + optional `extraAssets`, more files per host that unpack beside the program), `buildbot` (libretro: version folders of an index), `page` (a download page or JSON with a `version` regex group), `flatpak` (id), `path` (program names), `macApp` (bundle name), `windows` (`%VAR%` paths). The first three are managed downloads; the rest are detected and labelled, never installed. |
| `binaries` | Per OS, the program inside a managed download (may contain one `*`, may be an `.app`). |
| `launch.args` | The argument template: strings with `{rom}`, `{rom_dir}`, `{engine_home}`, `{engine_dir}`, `{opt.<id>}`; `{"if": "<toggle>", "args": […]}` and `{"unless": …}`. |
| `launch.env` | Extra environment, placeholders expanded. |
| `options` | `id`, `kind` (`toggle`, `choice`, `text`), `default`, `advanced`, `choices`, and **one** visibility rule, `visibleWhen` (`hosts`, and/or `option` + `is`). Labels and help are translation keys derived from the id (`manage.engines.option.<id>.label` / `.help`), shared by every engine with that option. |
| `biosDir` | Per OS, where the engine looks for BIOS files; Irori links the user's files there before a launch. RetroArch has none because Irori sets its `system_directory`. |
| `portableMarker` | Per OS, a file that makes a managed install keep its settings beside itself. |
| `exitHotkey` | Shown once per engine, the first time a game starts with it. |
| `experimental` | Never recommended; shown with a warning. |
| `cpuFeatures` | x86-64 features the engine's games use unchecked (`avx2`, `bmi2`, …); a processor lacking one gets an explanation instead of a crash. |
| `cores` | RetroArch only: id, name, licence and systems of each core Irori offers. |

Download links are never pinned in a manifest: each source says where versions are
listed and how to recognise this computer's build. Lists are cached for a day per engine
(`<cache>/engines/releases/<id>.json`) and used offline.

### 3.2 Starter engines (P4.1)

| Engine | Systems | Managed download | Detected | Licence |
| --- | --- | --- | --- | --- |
| RetroArch + cores | NES, SNES, N64, GB/GBC/GBA, DS, Master System, Mega Drive, Game Gear, Mega-CD, PC Engine, PS1, PSP | buildbot stable (Linux x64 `.7z` with AppImage, macOS universal `.dmg`, Windows x64 `.7z`) | Flatpak `org.libretro.RetroArch`, `PATH`, `RetroArch.app` | GPL-3.0; cores vary (below) |
| DuckStation | PS1 | GitHub rolling `latest` (AppImage, macOS zip, Windows zip) | `PATH`, `DuckStation.app` | CC BY-NC-ND 4.0 |
| PPSSPP | PSP | GitHub releases (AppImage, `PPSSPPSDL-macOS` zip, Windows zip) | Flatpak, `PATH`, `PPSSPPSDL.app` | GPL-2.0-or-later |
| Dolphin | GameCube, Wii | `dolphin-emu.org/update/latest/beta` (macOS dmg, Windows 7z); Linux is Flatpak only | Flatpak, `PATH`, `Dolphin.app` | GPL-2.0-or-later |
| ScummVM | ScummVM adventures | downloads page (macOS dmg, Windows zip); Linux is Flatpak or distribution | Flatpak, `PATH`, `ScummVM.app` | GPL-3.0-or-later |
| DOSBox Staging | MS-DOS | GitHub releases (Linux tar.xz, macOS dmg, Windows zip) | Flatpak, `PATH`, `DOSBox Staging.app` | GPL-2.0-or-later |

Cores in P4.1, with the systems that recommend them first: Nestopia (NES, GPL-2.0), Mesen
(GPL-3.0), Snes9x (SNES, **non-commercial**), bsnes (GPL-3.0), Mupen64Plus-Next (N64,
GPL-2.0), Gambatte (GB/GBC, GPL-2.0), mGBA (GBA, MPL-2.0), melonDS DS (DS, GPL-3.0),
Genesis Plus GX (Sega 8/16-bit and Mega-CD, **non-commercial**), Beetle PCE Fast (GPL-2.0),
SwanStation (PS1, GPL-3.0), PPSSPP (GPL-2.0-or-later).

P4.7 adds PCSX2 (PS2), melonDS and mGBA standalone, Azahar (3DS) and MAME (arcade), then
the experimental engines Cemu (Wii U), RPCS3 (PS3), xemu (Xbox) and shadPS4 (PS4).

### 3.2.1 Engines added in P4.7

| Engine | Systems | Managed download | Detected | Licence |
| --- | --- | --- | --- | --- |
| PCSX2 | PS2 | GitHub releases (AppImage, macOS tar.xz with a versioned `PCSX2-v….app`, Windows 7z) | Flatpak, `PATH`, `PCSX2.app` | GPL-3.0-or-later |
| melonDS | DS | GitHub releases (AppImage in a zip, macOS universal zip, Windows zip) | Flatpak, `PATH`, `melonDS.app` | GPL-3.0-or-later |
| mGBA | GB, GBC, GBA | GitHub releases (AppImage, macOS dmg, Windows 7z) | Flatpak, `PATH`, `mGBA.app` | MPL-2.0 |
| Azahar | 3DS | GitHub releases (AppImage, macOS universal zip, Windows zip) | Flatpak, `PATH`, `Azahar.app` | GPL-2.0-or-later |
| MAME | Arcade | none: official builds are Windows-only self-extracting archives | Flatpak, `PATH`, Windows locations | GPL-2.0-or-later |
| Cemu *(experimental)* | Wii U | GitHub releases (AppImage, macOS x64 dmg, Windows zip) | Flatpak, `PATH`, `Cemu.app` | MPL-2.0 |
| RPCS3 *(experimental)* | PS3 | Linux x64 AppImage from `RPCS3/rpcs3-binaries-linux`; macOS and Windows detected only | Flatpak, `PATH`, `RPCS3.app` | GPL-2.0-only |
| xemu *(experimental)* | Xbox | GitHub releases (AppImage, macOS universal zip, Windows zip) | Flatpak, `PATH`, `xemu.app` | GPL-2.0-only |
| shadPS4 *(experimental)* | PS4 | GitHub releases (SDL builds) | `PATH` | GPL-2.0-or-later |

New systems: PlayStation 2 (BIOS required; its files have many names, so the catalog
matches them by a name pattern and links them under their own names), Nintendo 3DS,
Arcade, Wii U, PlayStation 3, Xbox and PlayStation 4. Arcade sets (`.zip`/`.7z`), Xbox
images and PS3/PS4 games count as games only inside a folder of their system
(`folderOnly`), so a loose zip is never taken for an arcade game. PS3 and PS4 games are
folders; the catalog names the file inside that engines open (`folderMain`).

### 3.2.2 AnyPS5

[AnyPS5](https://github.com/boykopovar/AnyPS5) (GPL-2.0-only, experimental) plays
PlayStation 5 games without emulating the console: its relinker converts a game's
decrypted `eboot.bin` and modules into a program of this computer, which runs against
AnyPS5's own system libraries. Its `kind` is `relinker`.

- **Download.** Each release has a relinker program and a library archive per OS
  (`relinker-v….exe` + `prx-windows-v….zip`, `relinker-v…` + `prx-linux-v….tar.gz`);
  the manifest's `extraAssets` names the archive, which unpacks to `libs/` beside the
  relinker. Releases exist for Linux and Windows on x86-64 only.
- **macOS.** AnyPS5 publishes no macOS build: its relinker can produce Mac programs,
  but the libraries they need are not released, and the port is still open work
  upstream. Irori lists PS5 games there but explains, on the Emulators page and when
  Play is pressed, that AnyPS5 has no version for this computer and where it runs. The
  manifest needs no change once a macOS release appears; Irori offers it then.
- **Games.** PS5 games are folders inside a `PS5` folder, holding `eboot.bin`; the
  title comes from `sce_sys/param.json` (`folderTitle`), since dumps are usually named
  by title id.
- **Converting.** On Play, Irori checks the processor (AVX2, FMA, F16C, BMI1/2, LZCNT,
  MOVBE, from `cpuFeatures`) and that `eboot.bin` is a plain ELF file, then runs the
  relinker into `<data>/engines/relinked/anyps5/<key>/`: `app.elf` or `app.exe`, with
  `--rpath` at the installed `libs/` (copied beside the program on Windows when that
  path is not ASCII), `--windows --windows-gui` on Windows, and an empty module folder
  outside the game for games without one. Everything else in the game folder is linked
  into the output's `app0/` (symbolic links; junctions and hard links on Windows), so the
  user's folder is never written. A stamp of the relinker, `eboot.bin` and the modules
  (size and modification time) skips converting again until one changes. Output goes to
  the game's operation log.
- **Linux.** Without access to `/dev/udmabuf`, AnyPS5 games run slowly on drivers other
  than NVIDIA's; Irori says so once per run and names the usual fix (the `kvm` group).

### 3.2.3 Legal review of the experimental engines

- All five are open source (MPL-2.0 or GPL-2.0), developed in public, and distributed by
  their projects from GitHub. Irori downloads the projects' own unmodified releases when
  the user asks, exactly as for the other engines; it redistributes nothing.
- None of them needs Irori to handle copyrighted material, and Irori never does:
  firmware (PS3 system software from Sony's site, PS4 modules), decrypted PS5 games
  (AnyPS5 needs no firmware or keys, but only converts dumps the user made), console keys (Wii U
  `keys.txt`) and Xbox BIOS, MCPX boot ROM and hard-disk image are set up in each
  emulator's own settings by the user, from hardware they own. Irori neither downloads,
  links to, nor names a source for them; it only explains that they are needed.
- They are marked `experimental`: shown with an "Experimental" tag, never picked as a
  system's default while another emulator plays it, and their compatibility is game by
  game. No experimental engine is offered for a system an established engine covers.
- Nothing here changes the conclusions of section 11.

### 3.3 Installations and versions

- Discovery (`discovery.rs`) only looks at the filesystem: managed versions recorded in
  `<state>/engines.json`, Flatpak `current` folders, `which` on `PATH` (skipping Flatpak
  export shims), app bundles in `/Applications` and `~/Applications`, known Windows
  paths, and the custom program the user picked. Irori never installs a Flatpak or a
  distribution package.
- Managed versions live side by side in `<data>/engines/<id>/<version>/`. Downloads go
  through `irori-depot` (resumable, SHA-256 checked when the project publishes checksums,
  CRC-32 checked for cores against the buildbot index) and are unpacked into a staging
  folder first (zip, 7-Zip, tar, macOS disk images through `hdiutil`, or the bare program).
- The first managed version becomes the engine's default. Later installs do not change
  it. **Update** installs the newest version and makes it the default; the old version
  stays until the user removes it. A game pinned to a version (`iroriEngineVersion`)
  keeps it. If its pinned version disappears, the launch fails with `EngineMissing` and the
  failed-start sheet offers to install that engine or to use the default.
- Engine data stays in Irori-owned folders: `<data>/engines/home/<id>` for settings and
  saves of engines Irori configures, a portable marker for managed Windows installs that
  support one. The per-game toggle "Use the emulator's own settings" leaves the engine's
  configuration alone (RetroArch then only gets Irori's BIOS and core folders appended).

### 3.4 BIOS

- Irori never downloads or links to BIOS files or ROMs.
- The systems catalog (`assets/engines/systems.json`) lists each system's BIOS files with
  names, regions, known-good MD5s and a rule: `optional`, `anyOf` (one region is enough)
  or `all`.
- The BIOS folder is indexed in the background (`bios.rs`): only plausible files (known
  names, or `.bin`/`.rom`/`.img` under 32 MiB) are hashed, and each digest is cached by
  path, size and modification time. Nothing is hashed at launch.
- Status per system: **Ready**, **Not recognised** (a file of the right name with an
  unknown digest; it may still work), **Missing** (with expected names and regions) or
  **Optional**. A file is recognised by its digest whatever its name.
- Before a launch, the files a system needs are linked (symbolic links; hard links or
  copies on Windows) into the engine's BIOS folder under the expected names. A real file
  the user put there is never replaced.

### 3.5 Launching

`irori-engines::launch::command` returns a `CommandSpec`. The emulation store wraps it in
a `LaunchPlan`, so wrappers, environment variables, sessions, playtime, sleep inhibition,
logs and the failed-start sheet all work as for any other game. Flatpak engines are
started with `flatpak run --filesystem=<dir>` for the ROM, BIOS and engine folders, so the
sandbox sees the game without the user granting permissions by hand.

Typed errors that the failed-start sheet turns into fix buttons:

| Error | Fix buttons |
| --- | --- |
| `EngineMissing { engine }` | Install «engine» · Play with… |
| `EngineMissing { core }` | Install the «core» core |
| `EngineMissing { pinned }` | Install «version» · Use the default version |
| `BiosMissing { system }` | Choose BIOS folder · Show what's needed |
| `EngineUnavailable { NoBuild }` | none: says where the engine runs · Open Emulators |
| `EngineUnavailable { Processor }` | none: names the missing processor features |
| unknown system or format | Choose the system · Play with… |
| a crash right after start with Vulkan in the log | Use OpenGL for this game (RetroArch `video_driver`) |

Running state follows the engine's process tree, which Irori started itself; nothing
polls `/proc` and nothing sleeps at launch.

## 4. The ROM library

- **Store** `irori-emulation`, `Runner::Emulation`, `LoginFlow::None`, no accounts. Its
  library is `<config>/stores/emulation/library.json`: one entry per game with the
  system, the main file, the other files (discs, tracks), the clean title, region and
  revision tags, and an optional DAT match.
- **Folders.** The user adds ROM folders (the setup guide, the Emulators page, the library's
  Add game flow or drag and drop). They are stored in `iroriRomFolders` and scanned in the
  background, incrementally: a file whose path, size and modification time are unchanged is
  not looked at again. A `notify` watcher picks up changes while Irori runs; a full rescan
  runs at start-up. Nothing touches the network per file.
- **Detection** (`systems.rs`): the folder name first (`SNES/`, `Mega Drive/`), then the
  extension when only one system claims it, then header magic for shared extensions
  (`.iso`, `.bin`, `.rvz`). Zip and 7z archives inside a system folder belong to it.
- **Discs.** A `.cue` owns the `.bin` tracks it lists; `.m3u` playlists own their discs;
  files named `(Disc 1)`, `(Disc 2)` become one entry with an `.m3u` written into Irori's
  cache when the engine needs one.
- **Names.** Titles are cleaned locally (No-Intro/Redump naming): `(USA)`, `(Rev 1)`,
  `[!]` become region and revision badges. DAT files (No-Intro, Redump) the user supplies
  are matched by CRC-32 in the background; Irori does not bundle them because their
  licence is unstated.
- **Art.** In order: the user's override, libretro-thumbnails fetched on demand by clean
  title (never bundled), SteamGridDB when the user has a key, else a generated cover in
  the system's colours with its short name and the title.
- **Choosing engines.** The default per system is the first installed engine from the
  system's recommendation list, changeable in the Emulators page (`iroriSystemEngines`).
  Each game can override it in "Play with…", which lists engines (and cores) with their
  installed versions.
- **Scale.** 10,000+ entries: the library keeps `Arc<GameInfo>`, the scanner runs on the
  blocking pool and publishes one `LibraryChanged` per batch, and every list is virtual.

## 5. Settings

Engine options are rendered from the manifest by the settings form, with the same rules
as every other field: plain help under each label, advanced options collapsed under
"Advanced", a "Set for this game" marker with a one-click reset on overridden values, and
"From defaults" otherwise. Option values are stored per engine
(`iroriEngineOptions: {"retroarch": {"vsync": false}}`), globally and sparsely per game.

There is one visibility rule per option (`visibleWhen`). Options that do not apply on this
OS are not shown at all.

## 6. User interface

Every flow below is reachable from a visible button, the command palette and the
keyboard, and works with arrows, Enter and Escape in Big Picture.

**Sidebar.** "Emulators" sits under "Wine manager" (on Windows, where there is no Wine
manager, in its place). Route `Route::Emulators`, link `irori://emulators`.

**Emulators page** (`irori-manage::emulators`), modelled on the Wine manager:

- A header with a search field, "Set up emulation" and "Refresh".
- **Systems**: a grid of system cards (name, maker, game count, default emulator and a
  status line: "Ready", "Needs a startup file from your console (BIOS)", "No emulator
  installed"). Selecting a card opens its detail: default emulator picker, BIOS files
  with their state and expected names, and "Scan BIOS folder".
- **Emulators**: one card per engine: name, licence, systems, the installations found
  (managed versions with size, Flatpak, system, app, custom) with the default marked, and
  the actions Install (with size), Update (when a newer version is listed), Set as
  default, Open, Configure (engine options), Remove version, and "Use a program on this
  computer…". RetroArch's card lists its cores with Install/Remove.
- Progress shows inline on the card and in the status bar; failures show an inline alert
  with Retry.

**Set up emulation guide** (a sheet; from the Library's empty state and toolbar, the
Emulators page and Big Picture; every step skippable):

1. "Where are your games?" — pick one or more folders.
2. Scan with live progress: "312 games for 9 systems".
3. "Recommended emulators": the default engine for each found system with its download
   size, and **Install all**.
4. "Startup files (BIOS)": pick the folder; per-system Ready/Missing with plain wording.
5. Done: "Play" opens the library filtered to the new games.

**Library.** A "System" filter beside the store filter, a system badge on tiles of
emulated games, and region/revision badges on the game page. The one **Add game** dialog
detects what it is given: a ROM goes to its system; a Windows installer gets a prefix and
Wine; an executable or folder is sideloaded; a lutris.net link or script opens the script
installer; a `.desktop` file is read.

**Game page.** "Play with…" (engine, core and version, with "Default for SNES"), the
system badge, and the engine's exit hotkey the first time.

**Big Picture.** One shelf per system with games; the guide is reachable from the
library screen; engines start fullscreen.

**Failed start.** The sheet in `crates/feature-game/src/launch_problem.rs` gains the
engine hints of [3.5](#35-launching), each with its fix button.

## 7. Lutris import

`irori-extras::lutris_import`, mirroring `heroic_import`. Read-only: it opens `pga.db`
with SQLite in read-only, immutable mode and reads the YAML files.

- **Locations**: `pga_path` from `lutris.conf` if set, else `~/.local/share/lutris/pga.db`;
  config in `~/.config/lutris` or, for old installs, `~/.local/share/lutris`; the Flatpak
  under `~/.var/app/net.lutris.Lutris/`.
- **Reading**: `games` (installed rows), `categories` + `games_categories` (`favorite`
  means favourite, `.hidden` means hidden), and the config cascade `system.yml` →
  `runners/<runner>.yml` → `games/<configpath>.yml` with the `game`, `<runner>` and
  `system` sections, `env` merged key by key, entry points `exe`, `main_file`, `iso`,
  `rom`, `disk-a`, `path` resolved against `directory`.
- **Mapping**: `wine` → a sideloaded Windows app with its prefix and Wine build (mapped
  to a build Irori knows by name, else flagged); `linux` → a sideloaded native app;
  emulator runners → the emulation library with the matching engine and core; `steam` →
  the Steam library (matched by app id, nothing imported); store services (`gog`, `egs`,
  `amazon`, `itchio`, `zoom`) → a note pointing at the native store; anything else →
  "not imported" with the reason.
- **Also**: categories, favourites, hidden games, playtime (hours → minutes, keeping the
  larger total), last played, and covers from `coverart/` and `banners/`.
- **Entry points**: `irori import-lutris`, an onboarding card, a Settings card, and a
  summary sheet that lists what was imported and, per game, why anything was skipped.
- **Dependency**: `rusqlite` with the `bundled` feature (SQLite compiled in on every OS).

## 8. Lutris install scripts

### 8.1 Sources and etiquette

lutris.net publishes no terms of service and no API licence; its scripts are
user-contributed without a stated licence. Irori therefore fetches scripts only when the
user asks, one at a time, as the Lutris client does, and never bundles or mirrors them:

- search: `GET https://lutris.net/api/games?search=<q>&with-installers=True`;
- scripts: `GET https://lutris.net/api/installers/<slug>`;
- `User-Agent: Irori/<version>`, at most two requests a second, responses cached for a day;
- every script shows its name, version, author, date and a link to its lutris.net page.

Local `.yml`/`.json` files and `lutris:<slug>` links (`irori://lutris/<slug>`) are also
accepted.

### 8.2 Native first

When the game is in a store Irori supports natively (GOG, Epic, Amazon, itch.io, ZOOM,
Steam), the installer offers the native install first. Scripts that install the Epic
Games Launcher, EA app or Ubisoft Connect inside Wine are refused in favour of native
stores or the third-party launcher setup.

### 8.3 Interpreter

`irori-scripts` parses a script into a typed plan before anything runs:

- Commands are an enum: file (`chmodx`, `extract`, `merge`, `copy`, `move`, `rename`,
  `mkdir`, `write_file`, `write_json`, `write_config`, `substitute_vars`), prompts
  (`input_menu`, `insert_disc`), programs (`execute`), tasks (`create_prefix`, `wineexec`,
  `winetricks`, `set_regedit`, `winekill`, `dosexec`) and store autosetup (mapped to the
  native store). An unknown command or task refuses the script before it starts, naming it.
- Variables: `$GAMEDIR`, `$CACHE`, `$HOME`, `$DISC`, `$INPUT`, `$INPUT_<id>`,
  `$RESOLUTION`, `$RESOLUTION_WIDTH`, `$RESOLUTION_HEIGHT`, `$WINEBIN`, script
  `variables` and file ids. **An unknown variable is an error.** Built-ins cannot be
  shadowed.
- Files: http(s) with checksum verification, also for files the user picks when the
  script gives a checksum; `N/A:<prompt>` asks the user to pick a file; `$STEAM:<appid>:…`
  maps to the Steam library.
- Wine versions named by the script are mapped to builds in the Wine manager; a build that
  is gone gets a clear message and the default build.

### 8.4 Consent

Before anything runs, a consent screen built from the parsed plan lists: downloads (host
and size), every write outside the game folder, programs it runs, registry edits and,
verbatim, every shell command. Shell commands need an explicit "I understand" tick. Writes
outside `$GAMEDIR`, `$CACHE` and the prefix are refused unless listed there and approved.

### 8.5 Installer window

Steps: pick a version (description and date), destination, files, consent, progress,
done. Logs are collapsed; prompts appear inline. Cancel rolls back the game folder and
keeps downloads for a retry. A missing executable at the end is a failure with a fix
("Choose the game's program"). The result is a library entry with its engine and settings,
plus a record of the script slug and revision so the game can be reinstalled or updated.

## 9. Third-party launchers

Behind `iroriThirdPartyLaunchers` (off by default), like ZOOM.

- Irori never sees these launchers' credentials.
- A card per launcher (Battle.net, EA app, Ubisoft Connect, Rockstar Games Launcher):
  "Set up" creates a dedicated prefix with a known-good Wine build and the launcher's
  known fixes, downloads the official installer from the vendor and runs it, with
  progress. On Windows the card only detects the installed launcher.
- Installed games are read from the launcher's own local data (Battle.net `product.db`,
  Ubisoft's `configurations` cache and registry, EA's `__Installer/installerdata.xml`)
  and listed under `Runner::Launchers` with `third_party_managed_app`. They launch through
  the launcher's URI or flags; the session follows the game's process, not the
  launcher's.
- Anti-cheat status (`crates/extras/src/anticheat.rs`) is shown before setup and on each
  game.

## 10. Humble Bundle: not shipped

Humble Bundle's terms of service (effective 24 December 2024) forbid "accessing any content
on the Service through any technology or means other than those provided or authorized by
the Service" (§b(xi)) as well as scraping, and Humble publishes no API. Lutris itself says
Humble restricts calls from software like Lutris. A Humble store would have to use the
undocumented API with the user's session cookie, which is exactly what that clause covers.
P4.6 is therefore **not built**. Users can still download a Humble installer in their
browser and use the Add game flow, which installs it with a prefix and Wine. If Humble
publishes an API or grants permission, the store fits the standard shape (a protocol crate
plus a store crate, sign-in window with cookie capture).

## 11. Licences and terms

- Irori downloads engines from their projects' own servers when the user asks; it does
  not redistribute them. Non-commercial cores (Snes9x, Genesis Plus GX) and DuckStation
  (CC BY-NC-ND 4.0) are never bundled or linked; they run as separate programs. Irori
  ships no DuckStation settings: it only passes command-line flags and links BIOS files.
- No BIOS files, ROMs, DAT files or thumbnails are bundled. libretro-thumbnails are
  fetched on demand.
- lutris.net: see [8.1](#81-sources-and-etiquette). Humble Bundle: see [10](#10-humble-bundle-not-shipped).
- Every manifest records its licence; the engine card shows it.

## 12. Delivery

Stacked pull requests, each retargeted to `main` once its parent merges:

| PR | Branch | Contents |
| --- | --- | --- |
| P4.0 | `docs/lutris-parity-and-engines` | This document and the Lutris parity document. |
| P4.1 | `feat/engines-core` | `irori-engines`, the services API and the Emulators page with the starter engines. |
| P4.2 | `feat/rom-library` | `irori-emulation`, scanner and watcher, BIOS page, setup guide, "Play with…", system filter, Big Picture shelves, failed-start fixes. |
| P4.3 | `feat/lutris-import` | Lutris import (CLI, onboarding, Settings) with a summary. |
| P4.4 | `feat/lutris-scripts` | `irori-scripts`, consent screen, installer window, lutris.net source. |
| P4.5 | `feat/third-party-launchers` | Battle.net, EA app, Ubisoft Connect and Rockstar setup and library import. |
| P4.6 | — | Not built; see [10](#10-humble-bundle-not-shipped). |
| P4.7 | `feat/more-engines` | PCSX2, melonDS, mGBA, Azahar, MAME, then the experimental engines. |
| P4.8 | `feat/anyps5` | PlayStation 5 games with AnyPS5 on Linux and Windows. |
