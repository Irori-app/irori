# Parity spec: Lutris runners, options, database, installer scripts and services

Reference: Lutris at commit **`1f32fe0`** (22 October 2026), Python/GTK, licensed
GPL-3.0-or-later. This document describes the Lutris behaviour Irori matches when it
replaces Lutris. It is a **behaviour reference only**: Irori is not a fork or port of
Lutris, and every engine, importer and installer feature is fresh Rust. Nothing here
should be read as a code listing; where a Lutris file holds data (JSON runner files,
option help texts), this document summarises what the data does instead of quoting it.

All `file:line` references are relative to the root of the Lutris repository at that
commit. The companion design for Irori's side is `docs/design/engines.md`; this file
says what Lutris does, that one says what Irori builds.

Notation:

- **[QUIRK]** marks Lutris behaviour that is a bug, an inconsistency or a weak spot.
  Irori does not copy it.
- **[QUIRK]** followed by **Irori instead:** marks a dangerous behaviour (arbitrary
  code, unconfined writes, weakened TLS, silent fallbacks that change what runs) and
  says what Irori does in its place.

### Terminology

Lutris and Irori use the same words for different things. This document uses Lutris's
words when it describes Lutris and Irori's words when it describes Irori.

| Lutris | Irori | Notes |
| --- | --- | --- |
| runner | **engine** | The program that plays a game: an emulator, ScummVM, DOSBox, Wine. Irori's `Runner` type means a *store backend* (`Runner::Legendary`, `Runner::Gog`, …), so Irori calls Lutris runners "engines" (`crates/engines`, package `irori-engines`). Wine and Proton stay in `irori-runtime`. |
| platform | **system** | The machine a game was made for ("Sony PlayStation", "Nintendo DS"). Irori's systems are listed in `assets/engines/systems.json`. |
| service | store | A library source (GOG, Epic, Battle.net). Irori's stores implement the `Store` trait. |
| installer / script | (no direct equivalent) | Irori does not run Lutris scripts as Lutris does; see section 4. |
| game config (`configpath`) | per-game settings | Irori keeps sparse overrides in `config/games/<runner>/<app>.json`. |

Contents:

1. [Runners and engines](#1-runners-and-engines)
2. [Options and the config cascade](#2-options-and-the-config-cascade)
3. [Database and paths](#3-database-and-paths)
4. [Installer scripts](#4-installer-scripts)
5. [lutris.net API](#5-lutrisnet-api)
6. [Services](#6-services)
7. [Launch and process monitoring](#7-launch-and-process-monitoring)
8. [User complaints Irori addresses](#8-user-complaints-irori-addresses)

---

## 1. Runners and engines

### 1.1 Registry

- The 36 built-in runner ids are the module names in `lutris/runners/__init__.py:3-40`;
  the class is the attribute of the same name (`__init__.py:69-84`) unless it sets
  `runner_name` (xenia, `runners/xenia.py:15`).
- JSON runners are loaded at startup (`lutris/startup.py:163`,
  `runners/json.py:171-181`) into the same registry (`__init__.py:103-108`). When a
  JSON and a Python runner share an id, the Python one wins silently (`vita3k`
  relies on this, section 1.3).
- "Installed runners" are found by instantiating every runner and asking it
  (`__init__.py:93-100`). An unknown id displays as itself, so games whose runner was
  removed keep a label (`__init__.py:119-131`).
- **[QUIRK]** `runners/openmsx.py` is not in the registry list: dead code.
- **[QUIRK]** Instantiating every runner to list installed ones has side effects:
  the libretro constructor can start a synchronous network download (section 1.5).

Irori: the engine registry is a list of manifests (built in under `assets/engines/`,
overridable by the user). Listing engines never touches the network and never runs a
program. Discovery runs on the blocking pool.

### 1.2 The runner contract

Every runner, Python or JSON, provides the same declarations
(`lutris/runners/runner.py:54-69`):

| Declaration | Meaning |
| --- | --- |
| `human_name`, `description` | Shown in the UI. |
| `platform_dict` | Lutris platform name to the runner's own platform code (Dolphin uses `0`/`1`, mednafen uses module names). A plain list maps each name to itself (`runner.py:183-187`). |
| `game_options`, `runner_options` | Option lists (schema in section 2.2). |
| `system_options_override` | Partial option entries laid over the system options for this runner only, usually to turn the Lutris runtime off for AppImages (`sysoptions.py:557-574`). |
| `runner_executable` | Path relative to `RUNNER_DIR`, for example `mame/mame`. |
| `entry_point_option` | Which game option names the main file (default `main_file`). |
| `download_url` | A fixed archive or AppImage URL. When set, installation ignores the lutris.net API. |
| `flatpak_id` | Flatpak app id used as a fallback program. |
| `runnable_alone` | The runner can start without a game ("Run" in the runner list). |
| `multiple_versions` | The runner has side-by-side versions (Wine). |
| `require_libs`, `arch` | Shared libraries checked before launch. |
| `use_sniper_runtime` | Default for running inside Valve's Sniper container. |
| `context_menu_entries` | Extra game actions (Wine's winecfg, regedit, …). |

Behaviour every runner inherits:

- **Finding the program** (`runner.py:246-257`): a `runner_executable` key in the
  runner config wins if that file exists; otherwise `RUNNER_DIR/<runner_executable>`.
  A missing file is a "missing executable" error; a runner without an executable is a
  "misconfiguration" error. Every runner with an executable automatically gains two
  options: a custom executable path and "visible in side panel" (`runner.py:215-239`).
- **Building the command** (`runner.py:259-290`): the program path, or
  `flatpak run <id>` when the runner is *misconfigured* and the Flatpak is installed.
  **[QUIRK]** The Flatpak fallback triggers only on misconfiguration, not when the
  managed program is missing, so a user who deletes a managed build gets an error
  even though the Flatpak is installed. With Sniper on, the command is wrapped in the
  Sniper entry point with a small shell trampoline that exports the library path.
- **Installed?** (`runner.py:581-591`): the program exists, or the Flatpak is.
- **`play()`** returns a command plus optional environment, working folder, preload
  and library path (consumed by `runner_interpreter.py:23-87`).
- **Common environment** (`runner.py:292-361`): locale, a per-runner NVIDIA shader
  cache (`CACHE_DIR/shaders/<runner>`), SDL controller mappings and fullscreen
  display, multi-GPU selection (PRIME offload for NVIDIA, `DRI_PRIME=<pci id>`
  otherwise, plus the Vulkan ICD list), PulseAudio latency, the Lutris runtime
  library path, and last the user's `env`, which overrides everything. The
  "Ubuntu-18.04" runtime is on unless disabled by option, by `LUTRIS_RUNTIME=0` or by
  Sniper (`runner.py:533-543`).
- **Pre-launch** (`runner.py:478-492`): checks `require_libs`; runners override it to
  write configs or place BIOS files.
- **Launch configs** (`runner.py:383-469`): named alternate entry points that replace
  the command or exe, append arguments and set a working folder relative to the game.
- **Game folder** (`runner.py:131-142`): the database `directory`, else the entry
  point's folder.
- **Stop** (`runner.py:711-714`): SIGTERM to each tracked PID; `keep_game_alive`
  (`:716-727`) lets out-of-process launchers such as Steam hold the game "running".

### 1.3 JSON runners (good) and per-runner special cases (bad)

**The JSON pattern is the part worth copying.** A JSON runner
(`lutris/runners/json.py`, files in `share/lutris/json/`) is pure data: name,
platforms, executable, an optional download URL and Flatpak id, game options and
runner options where each runner option also carries the command-line flag it maps
to. User-added files in `RUNNER_DIR/json` override shipped ones by file name
(`json.py:14-17`). Required keys are checked at load (`json.py:38-44`).

Command construction (`json.py:138-168`), in order:

1. The runner's command (section 1.2).
2. For each runner option present in the effective runner config (defaults count,
   because the cascade fills them in):
   - boolean: append the flag when true;
   - choice: append the flag and the value, unless the value is `off`;
   - string: append the flag and the value;
   - command line: append the optional flag, then the value split like a shell would.
3. The entry-point option's own flag, if it has one.
4. The entry-point value, which must be an existing path.

That is enough to describe most emulators: 23 runners in `share/lutris/json/` are
nothing but data. JSON runners may also set extra environment variables and may run
from the runner's own folder (`working_dir: runner`).

Weak spots in the JSON loader that Irori's manifest format must avoid:

- **[QUIRK]** A string or choice option with no value still appends the flag followed
  by the literal text `None` (the AGS filter option and Ruffle's proxy option).
- **[QUIRK]** `86box.json` and `pcem.json` declare two runner options under the same
  key, so fullscreen and the config file follow one stored value.
- **[QUIRK]** A boolean flag that contains a space (Colem's sound rate) is passed as
  one argument, not two.
- **[QUIRK]** `require_libs` appears in `citra.json` but the loader never reads it.
- **[QUIRK]** The entry point must be an existing path, which breaks engines whose
  entry point is an identifier. `vita3k.json` takes a title id; the Python `vita3k`
  subclass was added to work around it and still fails unless a file with that name
  happens to exist in the working folder (`runners/vita3k.py:32-49`).
- **[QUIRK]** PPSSPP's fullscreen option is stored under the key `config`
  (`share/lutris/json/ppsspp.json`), so settings named by key are misleading.

**Per-runner Python code is the part not to copy.** Most of the 30-odd Python runners
re-implement the same three steps (read options, map them to flags, append the ROM)
with their own validation and their own bugs:

- **[QUIRK]** easyrpg declares its option as `patch` and reads `patches`, so it never
  applies; its save-path error names the game path instead (`runners/easyrpg.py:71`,
  `:533`, `:547`).
- **[QUIRK]** vice keys its tables `cmbii` while the platform code is `cbmii`, so every
  CBM-II launch raises (`runners/vice.py:142`, `:162`).
- **[QUIRK]** zdoom reads a `resolution` option it never declares
  (`runners/zdoom.py`).
- **[QUIRK]** redream never checks that the disc exists (`runners/redream.py`).
- **[QUIRK]** flatpak's entry point names an option (`application`) that does not
  exist (the option is `appid`), and its `env_vars` option is never used
  (`runners/flatpak.py:23`, `:30`, `:74`).
- Several runners write into the emulator's own config files in the home folder
  before launch (reicast into `~/.reicast/emu.cfg`, snes9x into
  `~/.snes9x/snes9x.xml`, yuzu and Ryujinx copy key files). These are silent edits to
  files the user owns.

Irori instead: every engine is a manifest (`assets/engines/<id>.json`) with sources
per OS, a launch template with conditional arguments, typed options with defaults and
an entry-point kind (file, folder or identifier). One generic, tested Rust function
turns manifest plus settings into a command. Engine-specific Rust exists only where
the engine needs real logic (RetroArch cores, BIOS, ScummVM game detection), and
Irori never edits an emulator's own config files without saying so in the UI.

### 1.4 Emulator runners Irori cares about

"Fixed URL" means `download_url`; "API" means the archive comes from the lutris.net
runners endpoint (section 1.6). Every row also accepts a custom executable path.

| Lutris id | Systems | How the program is found or installed | Launch shape | Notable options and behaviour |
| --- | --- | --- | --- | --- |
| `libretro` (RetroArch) | Many, derived from installed cores (`runners/libretro.py:21-141`) | `RUNNER_DIR/retroarch/retroarch`; API; Flatpak `org.libretro.RetroArch`; runnable alone | `retroarch [--fullscreen] [--verbose] --config=<cfg> --libretro=<cores>/<core>_libretro.so <rom>` (`:441-473`) | Game options: ROM and core. Runner options: config file, fullscreen (on), verbose. Cores and BIOS: section 1.5. |
| `duckstation` | Sony PlayStation | Fixed URL to the `latest` rolling AppImage on GitHub (`runners/duckstation.py:16-18`); Flatpak `org.duckstation.DuckStation`; runtime off | `<appimage> <rom>` | No options at all. **[QUIRK]** the URL is unpinned and the Flatpak id is stale (the Flathub repo was archived in 2024; terms-notes section 3). |
| `ppsspp` (JSON) | Sony PSP | `RUNNER_DIR/PPSSPPSDL`; API; Flatpak `org.ppsspp.PPSSPP` | `PPSSPPSDL [--fullscreen] [--pause-menu-exit] <iso>` | Both flags default on. Fullscreen is stored under key `config` ([QUIRK], section 1.3). |
| `dolphin` | Nintendo GameCube, Nintendo Wii (codes `0`, `1`) | Fixed URL to a pinned AppImage on GitHub (`runners/dolphin.py:22-24`); Flatpak `org.DolphinEmu.dolphin-emu`; needs `libOpenGL.so.0` | `<exe> [--batch] [-u <user dir>] -e <iso>` | Batch mode (on), user directory. Upstream publishes no AppImage; the URL points at a third-party repackaging on GitHub. |
| `scummvm` | "Linux" (sic) | `RUNNER_DIR/scummvm/bin/scummvm`; API; Flatpak `org.scummvm.ScummVM` (`runners/scummvm.py:54-59`) | `scummvm <flags> [--extrapath=<d> --themepath=<d>] --path=<game dir> <args> <game id>` (`:537-553`) | About 30 options mapped one to one to flags (`:71-104`): aspect ratio, subtitles, fullscreen or `--no-fullscreen`, scaler and scale factor, renderer, stretch mode, filtering, music driver, MIDI and MT-32, soundfont, volumes, language, debug. Empty values emit nothing. Warns about scaler and renderer combinations (`:13-50`). Lists games with `--list-games` (`:555-577`). Bundled libraries go on the library path only for the bundled build (`:483-494`). **[QUIRK]** its platform is declared as "Linux", not as the games' real systems. |
| `dosbox` | MS-DOS | `RUNNER_DIR/dosbox/dosbox`; API; Flatpak `io.github.dosbox-staging` (`runners/dosbox.py:16-21`) | `dosbox <main> \| -conf <main.conf>` then `[-conf <cfg>] [-fullscreen] [-exit] <args>` (`:121-147`) | Main file may be an exe, bat or conf; extra config file; arguments validated as shell words; working folder. Fullscreen off, exit on. Relative paths resolve against the game folder, or to an empty string when there is none (`:80-91`). Installer tasks `dosexec` and `makeconfig` (`runners/commands/dosbox.py:13`, `:43`). |
| `pcsx2` | Sony PlayStation 2 | `RUNNER_DIR/pcsx2/PCSX2`; API; Flatpak `net.pcsx2.PCSX2`; runtime off (`runners/pcsx2.py:11-54`) | `PCSX2 [-fullscreen] [-slowboot] [-nogui] <iso>` | Full boot maps to `-slowboot`; ISO must exist. |
| `melonds` (JSON) | Nintendo DS | Fixed URL to a zip holding the AppImage on melonds.kuribo64.net; Flatpak `net.kuribo64.melonDS`; runtime off | `<appimage> [--fullscreen] <rom>` | Fullscreen defaults on. |
| `mgba` (JSON) | Game Boy Advance, Game Boy (Color) | Fixed URL to the pinned 0.10.5 AppImage on GitHub; Flatpak `io.mgba.mGBA` | `<appimage> <rom>` | No options. |
| `azahar` | Nintendo 3DS | Fixed URL to a pinned GitHub tag; Flatpak `org.azahar_emu.Azahar`; runtime off (`runners/azahar.py:7-55`) | `<appimage> [-f] <rom>` | Fullscreen only; there is deliberately no windowed flag. |
| `citra` (JSON) | Nintendo 3DS | `RUNNER_DIR/citra-qt`; API; Flatpak `org.citra_emu.citra`; runtime off | `citra-qt <rom>` | Superseded by Azahar; kept so old games still launch. |
| `mame` | Arcade, plus a platform per machine | `RUNNER_DIR/mame/mame`; API; Flatpak `org.mamedev.MAME`; runnable alone (`runners/mame.py:144-160`) | `mame -skip_gameinfo -inipath ~/.mame -video <v> [-window] [-waitvsync] [-uimodekey <k>] …` then either `[-rompath <p>] <machine> <slots> -<device> <file>` or `-rompath <dir> <rom basename>`, then extra args (`:339-431`) | Machine (searchable list), device type, slots, autoboot command and delay, ROM path, fullscreen, CRT shader (three GLSL shader triples), video backend, vsync, UI mode key. Builds its machine list by running `mame -listxml` once into a cache (`:78-95`, `:293-312`). Creates `~/.mame/mame.ini` on first launch (`:325-337`). Working folder is always `RUNNER_DIR/mame` (`:278-280`). A main file without a dot is a ROM id, not a path (`game.py:685-688`). |
| `cemu` | Nintendo Wii U | `RUNNER_DIR/cemu/Cemu`; API; Flatpak `info.cemu.Cemu` (`runners/cemu.py:11-95`) | `Cemu [-f] [-m <mlc>] [-u] [--nsight] [--legacy] -g <game>` | Game is a folder or a `.wua` file; the MLC folder must exist. |
| `rpcs3` | Sony PlayStation 3 | `RUNNER_DIR/rpcs3/rpcs3`; API; Flatpak `net.rpcs3.RPCS3`; runtime off (`runners/rpcs3.py:11-40`) | `rpcs3 [--no-gui] <EBOOT.BIN>` | No-GUI only. |
| `xemu` | Microsoft Xbox | `RUNNER_DIR/xemu/xemu`; API; Flatpak `app.xemu.xemu`; runtime off (`runners/xemu.py:8-47`) | `xemu [-full-screen] -dvd_path <iso>` | Fullscreen only. |
| `shadps4` | Sony PlayStation 4 | `RUNNER_DIR/shadps4/shadPS4QtLauncher-qt.AppImage`; no Flatpak; runtime off (`runners/shadps4.py:11-97`) | `<launcher> (-e <emulator> \| -d) -g <game folder> -- --fullscreen true\|false` | Option picks the bundled SDL emulator, a custom one, or the launcher's default. |

Outside this list, `ryujinx` and `yuzu` (Switch) copy user-provided key files into
the emulator's data folder before launch (`runners/ryujinx.py:72-104`). Irori does
not fetch or place console keys or firmware (terms-notes section 4).

### 1.5 RetroArch cores and BIOS

**Core metadata** (`runners/libretro.py:148-203`):

- Core info files live in `RETROARCH_DIR/info/*_libretro.info`. When the folder is
  missing, Lutris downloads `info.zip` from the libretro buildbot over plain HTTP
  with a 5-second timeout and extracts it.
- Only info files whose categories include "Emulator" are offered. The label is the
  core's display name; the system is the core's `systemname`, mapped to a Lutris
  platform through a large table (`:21-141`); unmapped systems keep RetroArch's name.
- **[QUIRK]** HTTP, not HTTPS. Irori fetches core metadata over HTTPS only.
- **[QUIRK]** The runner's constructor lists cores (`:244-252`), so creating the
  runner object (which happens when Lutris lists installed runners) can trigger a
  synchronous download on whatever thread asked.

**Core location** (`:269-292`): read `libretro_directory` from the effective
`retroarch.cfg`. A leading `:` means "relative to the cfg file's folder"; `default`
or a missing key means `RUNNER_DIR/retroarch/cores`. The core file is
`<core>_libretro.so` (Linux only; Lutris has no `.dylib` or `.dll` handling).

**Installed?** (`:297-303`): RetroArch exists *and* the game's core file exists,
unless a custom RetroArch executable is set, in which case only RetroArch is checked.

**Installing** (`:309-332`): if RetroArch is missing, install it first (API default),
then install the core by asking the API for runner `libretro` with the core name as
the version, extracting into `RUNNER_DIR/retroarch/cores` without merging a single
top folder (`runner.py:648-650`). An installer script for libretro must name a core
(`installer/installer.py:192-194`).

**retroarch.cfg** (`:351-381`): when the config file does not exist, Lutris writes one
with absolute paths for the cores, info, databases, cursors, screenshots, remaps,
shaders, assets, thumbnails, playlists, autoconfig, overlays and system folders, all
under `RUNNER_DIR/retroarch/`. An existing file is never touched. The format is
`key = "value"` lines; comments and empty values are skipped when reading
(`util/libretro.py:29-52`).

**BIOS** (`:383-439`, `util/retroarch/firmware.py`):

1. Read the core's firmware list from its info file: count, path and "optional" flag
   per entry, plus MD5 hashes parsed out of the free-text notes field.
2. If any firmware is mandatory, the global BIOS folder (Preferences → Storage,
   stored as raw system config `bios_path`) must be set, or launch fails with a
   missing-BIOS error.
3. Hash every file in the BIOS folder (MD5) into `CACHE_DIR/bios-files.json`
   (`firmware.py:13-18`).
4. For each firmware: if already in RetroArch's system folder, check its MD5 and log
   the result; if absent and a hash is known, copy the matching file from the BIOS
   folder into the system folder under the expected name (`firmware.py:21-41`).

Quirks:

- **[QUIRK]** The whole BIOS folder is re-hashed on **every launch**, which is slow
  for users who keep BIOS files next to large ROM sets. Irori keeps a hash index keyed
  by path, size and modification time, and rehashes only what changed.
- **[QUIRK]** The variable holding RetroArch's config is reassigned to the core info
  file before the system folder is read (`libretro.py:386`, `:395`), so the system
  folder is never read from `retroarch.cfg`. The effective folder is always
  `RUNNER_DIR/retroarch/system`, even when the user configured another.
- **[QUIRK]** A checksum mismatch is only logged; the user launches with a bad BIOS
  and sees the emulator's error, not Lutris's.
- **[QUIRK]** Hashes come from parsing human-readable notes, which breaks whenever a
  core's notes change format.
- **[QUIRK]** Only RetroArch gets BIOS help. Standalone emulators that need BIOS
  (DuckStation, PCSX2) get none; atari800, hatari, jzintv and o2em each have their own
  ad hoc BIOS folder rules (`runners/atari800.py:88-103`, `runners/hatari.py:121-140`,
  `runners/o2em.py`).

Irori instead: one BIOS index for every engine, a per-system list of expected files
with hashes, a status the user can see before launch ("PlayStation: SCPH-5501 found,
hash matches"), and files linked or copied into the engine's folder only when the
engine needs them there. Irori never ships or downloads BIOS files or console keys.

### 1.6 Runner downloads and versions

- **Fixed URL runners** (`runner.py:611-627`, `:655-695`): download into
  `CACHE_DIR/<file name>`, extract into `RUNNER_DIR/<runner>`, delete the archive,
  mark the program executable. An AppImage passes through the generic extractor
  (copied as is). No version, no checksum, no signature.
  - **[QUIRK]** DuckStation, Vita3K, Basilisk II, SheepShaver and Xenia point at
    rolling `latest` or `continuous` URLs, so two users "on the same runner" may run
    different builds, and an update can arrive without the user asking.
  - **[QUIRK]** No integrity check of any kind.
- **API runners** (`api.py:179-206`, `:252-326`): look in the cached `versions.json`
  first, else `GET /api/runners/<name>`. Each version entry has a version name, an
  architecture (`x86_64`, `i386` or `all`), a URL and a "default" flag.
- **Version choice** (`api.py:241-312`): a requested name may end in `-i386` or
  `-x86_64`; without one, the host's architecture is assumed. Candidates, in order:
  exact version and architecture; the default for the architecture; the only entry
  for the architecture; any default (a 64-bit host may get a 32-bit default); the
  first entry for the architecture. From the cache, only an unambiguous match is used.
- **Install locations** (`runner.py:640-650`): Wine builds go to
  `RUNNER_DIR/wine/<version>-<arch>`, with a single top folder merged; Proton builds
  never get an architecture suffix (`api.py:232-233`). Cores go to
  `RUNNER_DIR/retroarch/cores`.
- **Runner manager** (`gui/dialogs/runner_install.py`): lists API versions merged with
  installed folders (`:127-140`), sorts by the numeric parts of the version
  (`:303-317`), shows which games use each version (`:38-49`), downloads with progress
  and cancel, and uninstalls by deleting the folder.
- **Proton** is never downloaded by Lutris: the default Wine "version" is the sentinel
  `ge-proton`, which runs umu and lets umu fetch Proton (`runners/wine.py:77-81`,
  `:907-917`). umu's own progress lines are shown as the launch status
  (`util/wine/proton.py:28-61`).
- **Runtime** (`lutris/runtime.py`): at most every 6 hours, Lutris fetches the runtime
  and runner version index, sending the machine's GPU PCI ids, and downloads
  components 8 at a time (`runtime.py:219-237`, `:405-455`). A newer client version in
  the response prompts the user to update (`:200-216`).
  - **[QUIRK]** File names from the server are joined into local paths without
    checking for `..` or absolute paths. **Irori instead:** every path that comes from
    a server or an archive is checked to stay inside its destination before anything
    is written.

Irori instead: engines come from their upstream's own release URLs (GitHub releases,
buildbot, the project's download site), always pinned to a version the user can see
and choose, several versions side by side, and a size and SHA-256 check whenever
upstream publishes one. "Update available" is a prompt, never a silent swap. Flatpak,
`PATH`, macOS app bundles and Windows install locations are discovered as equals of
Irori-managed installs, not as a fallback that only triggers on misconfiguration.

---

## 2. Options and the config cascade

### 2.1 Three levels of YAML

Lutris keeps settings in three kinds of YAML file (`lutris/config.py:164-178`):

| Level | File | Sections it may hold |
| --- | --- | --- |
| System | `CONFIG_DIR/system.yml` | `system` |
| Runner | `CONFIG_DIR/runners/<runner>.yml` | `<runner>`, `system` |
| Game | `CONFIG_DIR/games/<configpath>.yml` | `game`, `<runner>`, `system`, plus installer metadata |

- `configpath` is `<slug>-<unix time>` (`config.py:21-23`). Renaming a game's slug
  renames its file (`config.py:45-54`).
- Effective values (`config.py:194-222`), each layer overriding the one before:
  - **system**: system option defaults (with the runner's overrides applied) →
    `system.yml` → runner file `system` → game file `system`;
  - **runner**: runner option defaults → runner file `<runner>` → game file
    `<runner>`;
  - **game**: game option defaults → game file `game`.
- Callable defaults are evaluated while the cascade is built (`config.py:290-309`),
  so a default such as "gamemode if installed" is a fact about the machine at the
  moment the config was read.
- **`env` is merged key by key** across levels, and empty keys are dropped
  (`config.py:224-239`). Every other mapping, including Wine's DLL overrides, is
  replaced whole by the more specific level.
  - **[QUIRK]** Two different merge rules for mappings that look the same in the UI.
    A user who adds one DLL override to a game silently loses every override set for
    the runner.
- Writes go to the raw section of the level being edited; empty top-level sections
  are dropped and the file reloaded (`config.py:267-288`).
- A game file written by an installer also stores the installer's name, the whole
  installer script, variables, version, dependencies, slug, game slug, year, service
  and service id next to the sections (`installer/installer.py:356-368`).
- Per-folder `lutris.json` settings exist too (`settings.py:84-131`). **[QUIRK]** a
  file that is not a JSON object is logged as an error but still returned.

Irori mapping: Irori has the same three ideas with two levels of storage. Global
defaults live in app settings; per-engine defaults live with the engine's settings;
per-game overrides are sparse (`GameSettingsOverrides`). Every mapping field has one
documented merge rule, and the UI shows which level a value comes from and offers a
reset to the inherited value.

### 2.2 Option schema

Options are dictionaries shared by runners, the system option list and the config
widgets (`gui/config/widget_generator.py`):

| Key | Meaning |
| --- | --- |
| `option` | Key inside the YAML section (required). |
| `type` | `string`, `bool`, `range` (with `min`/`max`), `choice`, `choice_with_entry`, `choice_with_search`, `file`, `multiple_file`, `directory`, `mapping`, `command_line`, `label` (`widget_generator.py:68-80`). An unknown type is an error. |
| `label`, `help` | UI text. The tooltip adds "Default: x" for string defaults (`:212-217`). |
| `default` | A value or a callable. |
| `choices` | `(label, value)` pairs or plain values, or a callable. |
| `section` | Groups options in a frame (`:105-127`). |
| `advanced` | Hidden unless the "show advanced options" setting is on. |
| `scope` | Levels (`game`, `runner`, `system`) where the option is shown (`gui/config/boxes.py:178-179`). |
| `available` | False: the option is not rendered at all (`widget_generator.py:227-237`). |
| `visible` | False: the option is hidden *and has no effect* (`config.py:66-76`). |
| `condition` | False: the widget is greyed out. |
| `conditional_on` | Key of a boolean option; the widget is greyed out while that option is off (`widget_generator.py:758-773`). |
| `warning`, `error` | Text or a callable returning text, shown under the widget. |
| `validator` | Callable that checks string input (for example shell-word splitting). |
| `default_path` | System option whose value is the file chooser's starting folder. |
| `warn_if_non_writable_parent` | Folder pickers warn on read-only parents. |
| `argument` | JSON runners only: the command-line flag. |
| `active` | Used by some Wine options. |

Callables receive the option key and as many extra arguments as they accept
(`widget_generator.py:800-830`).

**[QUIRK] Four overlapping ways to hide or disable an option.** `available`,
`visible`, `condition` and `conditional_on` all control whether a user can touch an
option, with different consequences:

- `available` false removes the widget, but a value stored in YAML still applies.
- `visible` false removes the widget *and* makes the stored value inert
  (`config.py:66-76`); Wine registry options use this to ignore settings that Proton
  does not support (`runners/wine.py:1089-1133`).
- `condition` false greys the widget out, but the stored value still applies.
- `conditional_on` greys it out while a switch is off; whether the value applies
  depends on the runner reading the switch.

Add `scope` and `advanced` and there are six visibility rules. Users see options
vanish depending on the GPU count, whether `gamescope` is on `PATH`, the display
server and the level they are editing, and cannot tell whether a hidden value still
affects the game.

Irori instead: an option has one availability rule. An unavailable option is shown
disabled with the reason ("Needs gamescope", "X11 only"), and its stored value is
never applied while it is unavailable. Advanced options are collapsed, not removed.

### 2.3 System options

`lutris/sysoptions.py:105-555` declares about 40 system options in sections (Wine
adds about 34 more runner options, `runners/wine.py:275-660`). In outline:

| Section | Options |
| --- | --- |
| Lutris | default install folder (`~/Games`), disable runtime, prefer system libraries (on), use Sniper, cloud save sync (GOG only) |
| Display | GPU (with more than one), MangoHud, reset desktop (X11), disable compositor (X11), prevent sleep (on when an inhibitor exists), SDL fullscreen display (X11), turn off other monitors (X11), switch resolution (X11) |
| Gamescope | on/off, HDR, force-grab cursor, output and game resolution, window mode, FSR sharpness, FPS limit, extra flags; the whole section exists only when `gamescope` is on `PATH` |
| CPU | restrict to N cores (taskset), GameMode (default: on when available) |
| Audio | PulseAudio latency |
| Input | force US layout (X11), AntiMicroX profile, SDL controller mapping |
| Text games | run in a terminal, terminal program |
| Game execution | environment mapping, locale, command prefix, pre-launch script and "wait for it", post-exit script, include and exclude process lists, kill switch device |

- `bios_path` is not in this list but is read as raw system config
  (`gui/config/storage_box.py:20-57`).
- **[QUIRK]** `manual_command` is declared and shown but never read.
- **[QUIRK]** Most options only explain themselves in a tooltip; options that only
  work on X11 are hidden on Wayland instead of explained (section 8).

How each option applies at launch: section 7.1.

### 2.4 Entry-point keys

Lutris finds a game's main file by looking for these game-section keys, in this order:
`exe`, `main_file`, `iso`, `rom`, `disk-a`, `path`, then the first entry of `files`
(`installer/__init__.py:17-41`). By runner:

| Runner | Entry key |
| --- | --- |
| linux, wine | `exe` |
| most emulators, libretro (plus `core`) | `main_file` |
| reicast | `iso` |
| hatari | `disk-a` |
| scummvm | `path` (folder) plus `game_id` |
| easyrpg | `project_path` |
| frotz | `story` |
| steam, flatpak | `appid` |
| vita3k | `title_id` |

- **[QUIRK]** `rom` is in the search list but no runner uses it, while
  `project_path`, `story`, `appid` and `title_id` are missing from it.

Irori's Lutris import must therefore look up the key per runner, using the table
above, not the generic list.

---

## 3. Database and paths

### 3.1 `pga.db` (SQLite, `lutris/database/schema.py:9-70`)

The `games` table, which is what an importer reads:

| Column | Type | Meaning for import |
| --- | --- | --- |
| `id` | INTEGER primary key | Row id; join key for categories. |
| `name` | TEXT | Title. |
| `sortname` | TEXT | Optional sort title. |
| `slug` | TEXT | lutris.net game slug. |
| `installer_slug` | TEXT | Installer used; service imports use `<slug>-<service>`. |
| `parent_slug` | TEXT | The base game this one requires (DLC, mods). |
| `platform` | TEXT | Lutris platform name (Irori: system). |
| `runner` | TEXT | Runner id (Irori: engine). Set to an empty string on uninstall (`game.py:470`). **[QUIRK]** the uninstalled game forgets which runner it used. |
| `executable` | TEXT | Legacy, unused. |
| `directory` | TEXT | Install folder. |
| `updated` | DATETIME | Last change. |
| `lastplayed` | INTEGER | **Unix seconds** (`game.py:1097`). |
| `installed` | INTEGER | 0 or 1. |
| `installed_at` | INTEGER | Unix seconds. |
| `year` | INTEGER | Release year. |
| `configpath` | TEXT | Game YAML id (section 2.1). |
| `has_custom_banner`, `has_custom_icon`, `has_custom_coverart_big` | INTEGER | User-supplied art exists. |
| `playtime` | REAL | **Hours**, fractional (`game.py:979`). Steam playtime is minutes ÷ 60 (`services/steam.py`). |
| `service` | TEXT | Service id (`gog`, `egs`, `humblebundle`, `steam`, …). |
| `service_id` | TEXT | The game's id in that service. |
| `discord_id` | TEXT | Discord application id. |

Other tables:

- `service_games`: id, service, appid, name, slug, icon, logo, url, details (the
  store's JSON payload as text), lutris_slug.
- `categories`: id, name (unique). `games_categories`: game_id, category_id, with no
  keys or indexes.
- **Favourites** are membership in the category named `favorite`; **hidden** games
  are in the category `.hidden` (`game.py:238-263`). `all`, `favorite` and names
  starting with `.` are reserved (`database/categories.py:25-27`).
- `sources` (id, unique uri) and `saved_searches` (id, unique name, search).
- Migrations only add missing columns (`schema.py:117-141`).

Import rules for Irori:

- Open the database read only and never write to it; Lutris data is reference
  material, as Heroic's is.
- Convert `playtime` hours to Irori's minutes (rounding to the nearest minute) and
  `lastplayed` seconds to Irori's timestamps; zero means never.
- Map `favorite` and `.hidden` to Irori favourites and hidden games, and every other
  category to an Irori category of the same name.
- Rows with `service` set belong to a store Irori may already manage (GOG, Epic,
  Amazon, itch.io, Steam, ZOOM); match them by store id and merge playtime by keeping
  the larger total, never creating a duplicate entry.
- Rows with an emulator runner become engine games: runner → engine, platform →
  system, entry key per section 2.4, resolved against `directory`.
- Wine and Linux rows become sideloaded games, keeping the prefix and Wine version
  where Irori can find them.

### 3.2 Paths (`lutris/settings.py`)

| Name | Value |
| --- | --- |
| `CONFIG_DIR` | `$XDG_CONFIG_HOME/lutris` **only if that folder exists**, else `DATA_DIR` (`:22-26`). |
| `DATA_DIR` | `$XDG_DATA_HOME/lutris` (`~/.local/share/lutris`). |
| Settings | `CONFIG_DIR/lutris.conf` (INI; services are enabled in its `[services]` section). |
| `RUNNER_DIR` | Setting `runner_dir`, else `DATA_DIR/runners`; Wine builds in `RUNNER_DIR/wine`. |
| `RUNTIME_DIR` | Setting, else `DATA_DIR/runtime`. |
| `CACHE_DIR` | Setting, else `$XDG_CACHE_HOME/lutris` (`:30-32`). |
| Game and runner YAML | `CONFIG_DIR/games/`, `CONFIG_DIR/runners/`. |
| Database | Setting `pga_path`, else `DATA_DIR/pga.db` (`:46`). |
| Banners | `DATA_DIR/banners` (`<slug>.jpg`). |
| Cover art | `DATA_DIR/coverart` (`<slug>.jpg`). |
| Icons | `$XDG_DATA_HOME/icons/hicolor/128x128/apps/lutris_<slug>.png`. |
| Caches | `CACHE_DIR/shaders`, `CACHE_DIR/installer`, `CACHE_DIR/tmp`, `CACHE_DIR/versions.json`. |
| Site | Setting `website`, else `https://lutris.net` (`:56`). |

- **[QUIRK]** `CONFIG_DIR` depends on whether a folder exists, so two Lutris installs
  on one machine can read different files. Irori's importer checks both
  `~/.config/lutris` and `~/.local/share/lutris` for YAML.
- **Flatpak** (`net.lutris.Lutris`): there is no Flatpak-specific code in
  `settings.py`; the XDG folders resolve inside the sandbox, giving
  `~/.var/app/net.lutris.Lutris/{config,data,cache}/lutris`. Irori's importer checks
  the native and Flatpak roots, as it does for Heroic.
- **[QUIRK]** A Steam Web API key is embedded in the source as a default (`:67`),
  shared by every Lutris user. Irori reads Steam libraries from local files and does
  not embed third-party keys.

---

## 4. Installer scripts

Lutris installers are YAML (or JSON) programs that download files, run commands and
write a game config. They come from lutris.net (section 5) or a local file. This
section describes them so Irori can read what a script intends and help users who
come from Lutris. **Irori does not execute Lutris scripts the way Lutris does**; see
section 4.8 for why and what it does instead.

### 4.1 Format

An installer is a mapping; a local file may hold one, a list, or `{results: [...]}`
(`installer/__init__.py:51-61`). Required top-level keys: `version`, `slug`, `runner`,
`name`, `game_slug`. Optional: `year`, `script`, `installer_file` (set for local
files), `is_dlc`/`dlcid`, `steamid`, `gogid`, `humblestoreid`, `itchid`, `service_id`,
`discord_id`, `install_complete_text` (`installer/installer.py:36-61`).

The `script` mapping:

| Key | Meaning |
| --- | --- |
| `files` | List of one-key mappings: a file id to a URL, or to a mapping with `url`, `filename`, `checksum`, `referer`, `alternate_filenames`, `size`, `total_size`, `downloader`. |
| `installer` | Ordered list of commands (one-key mappings, or a bare command name). |
| `game` | Game section of the resulting config (entry point and game options). |
| `system` | System section; its `env` is also used while installing. |
| `<runner>` | Runner section, for example `wine: {version: …}`. |
| `requires` | Dependency slug(s); the script installs **into** the base game's folder. |
| `extends` | Like `requires`, but no new game entry is created. |
| `require-binaries` | Host programs that must exist; `a \| b` gives alternatives. |
| `variables` | User variables for substitution. |
| `install_complete_text` | Message shown at the end. |

Validation (`installer.py:178-204`): the script is a mapping; runner, name and slug
exist; libretro scripts name `game.core`; Steam scripts name `game.appid`; not both
`requires` and `extends`. Dependencies are comma-separated (all needed) with `|`
alternatives; the first dependency's folder becomes the target (`interpreter.py:176-204`).

Service detection when none is given (`installer.py:73-105`): a runner containing
`steam` means Steam; a `version` containing `humble`, `gog` or `itch.io` means that
service. The app id comes from `dlcid`, `steamid`/`service_id`, `gogid`, `humbleid`/
`humblestoreid` or `itchid`, at the top level or in `game`.

### 4.2 Target folder and flow (`installer/interpreter.py`)

The default target is `<install folder option>/<service id or empty>/<game slug>`
(`interpreter.py:120-128`); it is created only for non-dependency, non-Steam scripts
that have commands and need a folder (`installer.py:140-163`).

1. Check `require-binaries` and dependencies (`:158-204`).
2. Collect the runners needed: the script's runner plus any runner named by a `task`
   command, and install the missing ones one by one (`:245-293`).
3. Prepare files (`installer.py:206-274`): for service installs, the first file whose
   URL starts with `N/A` is replaced by the service's real downloads; URLs get
   variable substitution; ModDB links are resolved (falling back to a manual pick);
   extras are added and later copied into `$GAMEDIR/extras`.
4. The files page downloads or asks for each file (section 4.5).
5. Create the target folder and `CACHE_DIR/installer/<game_slug>`, then run the
   commands one by one on a worker thread with the target as the working folder
   (`:295-367`). A command may return "stop" to pause until the UI resumes it (disc
   prompts, menus). A mapping parameter may carry `description`, shown as status.
6. Finish (`:390-416`): post-install hooks (GOG config), write the game YAML, add or
   update the database row (platform from the runner), warn if the entry point is
   missing, fetch lutris.net art, show completion.
7. Clean the per-game installer cache (`:418-428`).

### 4.3 Variables

Substitution (`interpreter.py:487-541`) uses template replacement over this map:

| Variable | Value |
| --- | --- |
| `$GAMEDIR` | Target folder. |
| `$SCRIPTDIR` | Folder of a local script file. |
| `$CACHE` | Per-game installer cache folder. |
| `$HOME`, `$USER` | User's home and name. |
| `$STEAM_DATA_DIR` | Steam's data folder. |
| `$DISC` | Mounted disc found by `insert_disc`. |
| `$INPUT`, `$INPUT_<id>` | Last menu choice, and named menu choices. |
| `$VERSION` | Installer version. |
| `$RESOLUTION`, `$RESOLUTION_WIDTH`, `$RESOLUTION_HEIGHT`, `…_WIDTH_HEX`, `…_HEIGHT_HEX` | Current desktop resolution. |
| `$WINEBIN` | Wine program used for the install. |
| any `variables` entry | Its value. |
| any file id | Local path of that file. |

- Unknown variables are left in place as literal text; values that are unset are
  dropped first, so `$GAMEDIR` stays literal when there is no target
  (`interpreter.py:518-523`).
- Dashes in names become underscores on both sides (`util/system.py:368-400`). A
  string that is exactly one variable returns the raw value, so non-text values
  survive.
- **[QUIRK]** Script variables and file ids are applied after built-ins, so a script
  can redefine `$GAMEDIR` or `$HOME` and redirect every later write.
- **[QUIRK]** Leaving unknown variables as literal text turns a typo into a folder
  named `$GAMEDRI` instead of an error.

### 4.4 Commands

Any public method of the interpreter can be called as a command: the name is
lower-cased, `-` becomes `_`, outer underscores are stripped, and the method is looked
up by name (`interpreter.py:369-388`).

**[QUIRK]** This exposes internal methods (`launch_install`, `revert`, `cleanup`,
`install_runners`, `create_game_folder`, …) to scripts. **Irori instead:** any script
reader Irori has works from a fixed list of known commands and rejects everything
else.

The commands scripts actually use (`installer/commands.py`):

| Command | Parameters | What it does |
| --- | --- | --- |
| `chmodx` | path | Substitute, require the file to exist, set the execute bit (`:93-98`). |
| `execute` | a shell command string, or a mapping with `file` *or* `command`, plus `args`, `working_dir`, `terminal`, `env`, `disable_runtime`, `include_processes`, `exclude_processes`, `return_code` | `command` runs through `bash -c`. `file` is a file id or a path, made executable and looked up on `PATH`. Environment = runtime + script `system.env` + `env`. Working folder = `working_dir` if it exists, else the target. Waits for the process (polling every second) and fails on an exit code other than 0 or `return_code` (`:100-191`, `:460-468`). |
| `extract` | `file` or `src` (file id, path or glob), `dst` (default `$GAMEDIR`), `format`, `nomerge` | Extracts every match. A single top folder is merged unless `nomerge`. Runs in a process that cancel can kill (`:193-218`). |
| `input_menu` | `options` (list of value → label), `id`, `preselect`, `description` | Shows a drop-down; the choice feeds `$INPUT` and `$INPUT_<id>` (`:220-236`). |
| `insert_disc` | `requires` (relative path), `message` | Waits for a mounted disc, or a user-chosen folder, containing that file (case-insensitive); sets `$DISC` (`:238-274`). |
| `mkdir` | path | Creates folders; existing is fine (`:276-284`). |
| `merge`, `copy` | `src`, `dst`, `optional`, `remove_source` | A file is copied into `dst` and the file id now points there; a folder is merged recursively without copying permissions; `remove_source` deletes the source tree (`:286-316`). |
| `move` | `src`, `dst`, `optional` | Skips when the file is already at `dst`; copies instead of moving out of the user's custom cache (`:318-346`). |
| `rename` | `src`, `dst` | Removes an empty `dst` folder, fails if `dst` exists, moves through a temporary folder on the destination file system (`:348-369`). **[QUIRK]** the error names `src` instead of `dst`. |
| `substitute_vars` | `file` | Replaces variables in a text file line by line (`:386-399`). |
| `task` | `name` (`[runner.]task`), any parameters, `return_code` | Calls a module-level function in a runner's module (`runners/__init__.py:87-90`). For Wine-family runners, injects the Wine path, prefix (default `game.prefix` or `$GAMEDIR`), architecture, and for `wineexec` the script's environment. Waits for a returned process (`:401-458`). |
| `write_file` | `file`, `content`, `mode` (`w` or `a`) | Writes text (`:470-486`). |
| `write_json` | `file`, `data`, `merge` (default true) | Deep-merges into a JSON file (`:488-515`). |
| `write_config` | `file`, then `section`/`key`/`value` or `data`, `merge` | Writes an INI file, keeping key case (`:517-552`). |
| `autosetup_gog_game` | file id, `silent` | Lists a GOG Inno Setup installer: DOSBox games are unpacked with innoextract and given a DOSBox config; ScummVM games read `goggame-*.info`; anything else runs the installer under Wine with Inno's silent flags and then applies GOG's config (`:595-655`). |
| `autosetup_zoom_platform` | file id, `silent` | The same idea for ZOOM Platform installers; DOSBox arguments come from `Launch*.bat` (`:673-729`). |
| `autosetup_amazon` | `files` (hash → paths), `directories` | Creates folders and copies each downloaded blob to every listed path under `$GAMEDIR/drive_c/game/` (`:731-759`). |
| `gogdl_setup` | `game_id`, `platform`, `lang`, `dlcs`, `preserve_manifest`, `command` (`download`, `update`, `repair`) | Runs heroic-gogdl from the Lutris runtime with progress, then reads `goggame-*.info` to fill the game section (exe, args, working folder, DOSBox/ScummVM detection, `start.sh` on Linux); keys the script set win (`:761-991`). |
| `install_or_extract` | file id | Wine only: run an `.exe` with Wine, else extract to `$GAMEDIR/drive_c/<slug>` (`:993-1005`). |
| `extract_or_run` | `file`, `dst` (default `$CACHE`), `merge_dst` | Extracts; if an extracted `.exe` has an NSIS or Inno signature it is run with Wine, else merged into `merge_dst` (`:1017-1039`). |

Wine tasks (`runners/commands/wine.py:38-644`): `create_prefix` (prefix, Wine path,
architecture, DLL overrides, Gecko and Mono), `wineexec` (program, args, prefix,
architecture, working folder, blocking, process lists, environment, overrides),
`winetricks` (verb or GUI), `winecfg`, `set_regedit` (path, key, value, type),
`set_regedit_file`, `delete_registry_key`, `eject_disc`, `install_cab_component`,
`winekill`, `open_wine_terminal`. DOSBox tasks: `dosexec` and `makeconfig`.

Bugs in the command layer:

- **[QUIRK]** `_extract_innosetup` passes the extractor under one name while
  `extract` reads another (`commands.py:571` vs `:215`), so innoextract is picked by
  guessing the file type, not by request.
- **[QUIRK]** `autosetup_gog_game` uses an unset variable when no
  `goggame-*.info` is found (`:642-646`).
- **[QUIRK]** `autosetup_zoom_platform` calls a method that does not exist for
  ScummVM games, and uses an unset variable when there is no `.bat` file.

### 4.5 Files

- File ids have `-` replaced by `_` (`installer_file.py:21`).
- URL forms (`installer_file.py:44-55`, `:160-220`, `steam_installer.py:19-104`):
  - `http(s)://…`: downloaded.
  - `/absolute/path`: copied as a `file://` URL.
  - `N/A` or `N/A:<prompt>`: the user picks a file; the text after the colon is the
    prompt. Service installs replace the first `N/A` entry with the store's download.
  - `$STEAM:<appid>:<relative path>`: install the Steam game and use files from its
    folder (a path of `/` means the whole folder).
  - `$WINESTEAM` and `gogsetup` are not supported at this commit.
- `filename` is required for mapping entries; string entries use the URL's last part
  (`:64-76`). `alternate_filenames` are accepted when already cached (`:113-129`).
- `checksum` is `<algorithm>:<hex>`, checked after download; a mismatch is a script
  error (`:254-276`). Most scripts have no checksum.
- Providers: Steam, already cached, user-picked, download (`:191-216`).
- Downloads go to `<cache>/<game slug>/<file id>/<filename>` (GOG files under `gog`),
  written to a `.tmp` file and renamed (`cache.py:25-47`). The cache root is the
  user's installer cache setting if it (or its parent) exists, else
  `CACHE_DIR/installer`.
- The Steam provider opens `steam://install/<appid>` and polls Steam's app state every
  2 seconds until it reads "Fully Installed" (`steam_installer.py:66-103`).

### 4.6 Service autosetup and automatic executables

- **GOG** (`services/gog.py:660-748`): the preferred generated script is one
  `gogdl_setup` command (Wine prefix `$GAMEDIR/pfx`). The fallback uses the offline
  installer: `autosetup_gog_game` for Windows, or unzip `data/noarch` for Linux.
- **Humble**: section 6.4. **Steam**: one file `$STEAM:<appid>:.`
  (`services/steam.py:124-128`).
- **Automatic executable**: an `exe` of `_xXx_AUTO_ELF_xXx_` or
  `_xXx_AUTO_WIN32_xXx_` makes Lutris search the target for a Linux or Windows
  executable when saving (`installer/__init__.py:11-13`, `installer.py:345-348`).
- Entry-point paths are case-corrected against the file system
  (`installer.py:350-354`).

### 4.7 Cancellation and rollback

`revert()` (`interpreter.py:455-485`) stops the command loop, marks cached files
"failed" (kept for a retry), runs `winekill` for Wine runners, kills the current
extract/copy/move process (`commands.py:560-568`), and deletes the target folder if
asked. The cancel dialog offers "Remove game files" (pre-checked) only when Lutris
created the folder, the install is fresh (not an update or DLC), and the path passes
a "removable" check (`gui/installerwindow.py:225-273`).

- **[QUIRK]** Files written outside the target, registry edits, Steam installs and
  database rows are never rolled back. A cancelled install can leave a half-configured
  prefix and files anywhere a script wrote.

### 4.8 Dangers, and what Irori does instead

- **[QUIRK]** `execute` runs arbitrary shell commands and arbitrary programs, from a
  script downloaded from a community website, **without asking the user**. The only
  review is lutris.net moderation.
- **[QUIRK]** `write_file`, `write_json`, `write_config`, `merge`, `move`, `rename`,
  `mkdir` and `extract` accept any absolute path; nothing confines them to `$GAMEDIR`.
- **[QUIRK]** `task` can call any public module-level function in any runner module,
  and any public interpreter method is a command.
- **[QUIRK]** Extraction does not check archive entries for `..` or absolute paths.
- **[QUIRK]** Amazon manifest paths and runtime file names from servers are joined
  without checks.
- **[QUIRK]** Scripts pin Wine builds and runner versions by name; when the build is
  removed from the server, the script fails (section 8).

**Irori instead:**

- Irori never runs a shell command, a downloaded program or a script step without
  showing the user what it will run and getting a confirmation for that script.
- Every path a script names is resolved and must stay inside the game folder, the
  prefix or Irori's cache; anything else is refused with a message naming the path.
- Archive entries are checked before extraction.
- Only known commands are accepted, through a fixed table.
- Where a script names a Wine or runner version that no longer exists, Irori shows the
  substitution it proposes instead of falling back silently.
- Irori's first goal is to need fewer scripts at all: GOG, Epic, Amazon, itch.io and
  ZOOM are native stores, emulated games need only a file and an engine, and Windows
  setup programs go through the sideload flow. Scripts are a fallback for the long
  tail, fetched on request (section 5).

---

## 5. lutris.net API

### 5.1 Client behaviour (`lutris/util/http.py`)

- User agent `Lutris <version>` (`http.py:159-161`); timeout 30 s or a setting
  (`:26`); certifi CA bundle (`:29-33`); desktop proxy settings (SOCKS ignored,
  `:36-97`); relative URLs are prefixed with the site URL (`:143-157`).
- 401 becomes an "unauthorised" error; other HTTP errors carry their code
  (`:197-204`). No HTTP caching apart from the files noted below.
- Sign-in: `POST /api/accounts/token` with form fields username and password returns a
  token, stored as `username:token` in `CACHE_DIR/auth-token`; then `GET /api/users/me`
  with `Authorization: Token <token>`, cached in `CACHE_DIR/user.json`
  (`api.py:100-176`).
  - **[QUIRK]** The token is stored in plain text in a cache folder, and a `:` in the
    user name or token breaks parsing.

### 5.2 Endpoints

| Endpoint | Method and parameters | Fields Lutris uses | Source |
| --- | --- | --- | --- |
| `/api/games?search=<q>&with-installers=True` | GET | `results` (id, name, slug, year, cover art, platforms, provider games) | `api.py:453-464` |
| `/api/games/<slug>` | GET | details, `discord_id`, icon, banner and cover URLs | `api.py:423-431`, `services/lutris.py:172-190` |
| `/api/games[?page=N]` | POST JSON `{games: [slugs], page}` | paginated `results`, `next` | `api.py:342-355` |
| `/api/games/service/<service>[?page=N]` | POST JSON `{appids}` | `results[]` with `slug` and `provider_games[] {service, slug}` | `api.py:358-366`, `services/base.py:260-274` |
| `/api/installers/<game slug>` | GET | `results[]` installers | `settings.py:58`, `api.py:399-420` |
| `/api/installers/game/<slug>/revisions/<rev>` | GET | one installer | `settings.py:60` |
| `/api/runners/<name>` | GET | `versions[] {version, architecture, url, default}` | `api.py:179-206` |
| `/api/runtimes/versions?pci_ids=…` | GET | `runtimes`, `runners`, `client_version` (cached 6 h in `versions.json`) | `api.py:72-88` |
| `/api/runtimes/<name>` | GET | `[{filename, url, modified_at}]` | `runtime.py:440-455` |
| `/api/tosec/games?md5=<md5>` | GET | `results[]`; `category.name` gives the platform (ROM identification) | `scanners/tosec.py:68-80`, `:142-146` |
| `/api/accounts/token` | POST form | `token` | `api.py:118-149` |
| `/api/users/me` | GET, token | stored as is | `api.py:160-168` |
| `/api/users/library[?since=]` | GET or POST, token | per game: name, slug, runner, platform, playtime (hours), last played, service, service id, categories | `util/library_sync.py:19-97` |
| `https://lutris.net/games/cover/<slug>.jpg` | GET | Battle.net cover art | `services/battlenet.py:99` |

- Pagination follows `next` by pulling `page=N` out of the URL with a regex
  (`api.py:369-396`).
- Installer mappings are normalised so name, version, os, slug, game slug, DLC id and
  runner are strings (`api.py:434-450`).
- `lutris:` links (`api.py:467-557`): `lutris:<slug>`, `lutris:<action>/<slug>`,
  `lutris:<action>/<slug>/<launch config>`, `?revision=N`; `<service>:<appid>` may stand
  in for the slug. **[QUIRK]** `format_installer_url` raises when there is neither an
  action nor a launch config (its condition is inverted, `:535-538`).
- **[QUIRK]** The runtime index request sends the machine's GPU PCI ids on every
  refresh.

### 5.3 Terms, limits and licence (from terms-notes section 1)

- lutris.net publishes **no terms of service, privacy policy or API documentation**;
  `/api/` itself returns 404, and the site's email template links "Terms" to `#`.
- The website code is AGPL-3.0; that covers code, not data, and Irori reuses no code.
- **Installer scripts carry no licence.** They are user-submitted and moderated, so
  each contributor keeps default copyright.
- The production site throttles anonymous clients at **4 requests per second** and
  signed-in clients at 6, with a page size of 250 (`lutrisweb/settings/base.py` in
  the website repository). It sits behind Cloudflare.
- `robots.txt` disallows `/api`, `/files` and `/user` for all user agents. That
  governs crawlers rather than a client acting for a user, but it says clearly: do
  not crawl or mirror the API.
- Many small third-party launchers call `/api/installers` and `/api/games`; there is
  no sign Lutris has objected.

### 5.4 Conclusion for Irori

- **Fetch on user request only.** Irori asks lutris.net when the user searches for a
  game or opens "Find an install script" for one game; it never crawls, prefetches
  the catalogue or refreshes in the background.
- **Cache** responses per slug on disk and reuse them; scripts change rarely.
- **Honest user agent**: `Irori/<version> (+<repository URL>)`, never Lutris's.
- **Rate limit** client-side, well under 4 requests per second, and back off on 429.
- **Attribute**: show that a script comes from lutris.net, its author and revision
  where the API gives them, and link to its page.
- **No mirroring**: Irori's repository and packages contain no lutris.net scripts or
  catalogue data.
- Irori does not sign in to lutris.net, does not send GPU ids, and does not use the
  runtime or runner endpoints: engines come from upstream (section 1.6).

---

## 6. Services

### 6.1 Common model (`lutris/services/base.py`, `services/__init__.py`)

- Registered services: gog, zoomplatform, humblebundle, egs, gamejolt, itchio,
  ea_app, ubisoft, amazon, flathub, dlsite, battlenet, xdg (not in Flatpak), steam
  (when Steam is present), steamwindows, steamfamily, dolphin and scummvm (when their
  config files exist), and lutris behind an environment flag; mame is unfinished.
  Enabled by default: gog, egs, ea_app, ubisoft, steam. Enabled state lives in
  `lutris.conf`.
- `load()` fetches a library into `service_games` with the store's JSON;
  `match_games()` asks `/api/games/service/<id>` for lutris.net slugs and links
  existing Lutris games (`base.py:236-274`).
- Installing a service game looks for lutris.net installers whose version mentions the
  service, links an existing install instead of reinstalling, and appends a generated
  installer, labelled "auto-generated" when community ones exist (`base.py:288-335`).
  Local services write a config and database row directly (`base.py:379-394`).
- Online sign-in uses a WebKit window that captures redirect URLs; cookies are stored
  in Netscape `cookies.txt` format (`util/cookies.py:5-31`). "Signed in" means every
  credential file exists (`base.py:506-511`).
- **Launcher services** (Battle.net, Ubisoft Connect, EA app, Epic) depend on the
  store's official Windows client installed through Lutris (`client_installer`
  slug); signing in first installs the client (`base.py:468-482`). Their games are
  Wine games sharing the client's prefix, launched by running the client with launch
  arguments.

### 6.2 Battle.net (`services/battlenet.py`)

- **Library**: a hard-coded table of Blizzard products (id, name, product code,
  lutris.net slug, `:22-66`), all saved without any account (`:123-127`).
- **Installed detection** (`:134-151`, `:262-308`): find the Battle.net client game,
  take its prefix, and parse `drive_c/ProgramData/Battle.net/Agent/product.db`. That
  file is a protobuf: a list of product installs, each with a uid, product code,
  settings (install path, play region) and cached state (installed, playable, version
  string) (`util/battlenet/product_db.py:26-80`). The `agent` and `bna` entries are
  skipped; the region is read from them.
- For each product found, Lutris creates `<slug>-battlenet`, copying the client's
  config with arguments that ask the client to launch that product code, and records
  the client exe as `client_exe` (`:153-186`).
  - **[QUIRK]** The installed and playable flags are ignored, so products the agent
    merely knows about count as installed.
- **Install**: run the client with an install request for the product code and tell
  the user to close Battle.net when it is done (`:212-259`).
- **Launch**: Wine first starts `client_exe` and **sleeps 15 seconds**
  (`runners/wine.py:1183-1213`), then runs the client again with the launch argument.

### 6.3 Ubisoft Connect (`services/ubisoft.py`, `util/ubisoft/*`)

- **Sign-in** (`:101-132`, `:215-228`): a web view on Ubisoft's login page; injected
  script reads three login values from the page's local storage and redirects to a
  URL that carries them; Lutris parses it and stores the session as JSON in
  `CACHE_DIR/ubisoft/.token`.
- **Library** (`:180-208`): Ubisoft's GraphQL `ownedGames` query, keeping PC entries;
  the app id is the space id or install id. Plus games parsed from the client's local
  configuration cache.
- **Configuration cache** (`:139-178`, `util/ubisoft/parser.py:16-154`): the newest of
  two known locations inside the client's prefix. It is a binary record format with a
  variable-length size field; large records that mention `start_game` hold YAML
  describing the game (space id, name or a localisation key, thumbnail, the registry
  key holding the install path, the exe) (`ubisoft.py:247-305`).
- **Installed detection** (`:230-289`): for each game, read the install path and exe
  from the prefix's registry; both present → `<slug>-ubisoft` with a `uplay://launch/<id>`
  argument.
  - **[QUIRK]** Games known only from the online API have no registry path, so only
    games already in the local configuration cache can be detected. Helpers for the
    client's install-state file exist but are unused (`util/ubisoft/helpers.py:54-65`).
- **Install**: run the client with `uplay://install/<id>` (`:291-327`).

### 6.4 Humble Bundle (`services/humblebundle.py`)

- **Sign-in** (`:83-110`): a dialog says Humble restricts third-party clients and
  offers a cookie import: the user exports Humble's cookies from Firefox in
  `cookies.txt` format and pastes them; Lutris writes them to
  `CACHE_DIR/.humblebundle.auth`. Declining falls back to the web-view login.
  Requests send those cookies (`:139-151`); a 401 signs out.
- **Endpoints** (`:153-205`): the user's order list (game keys), then each order with
  its subproducts (machine name, title, icon, downloads per platform, each with
  named file variants and web URLs). Orders are cached as
  `CACHE_DIR/humblebundle/library/<gamekey>.json`, 8 fetched in parallel.
  - **[QUIRK]** Once any order file exists, the order list is never fetched again, so
    new purchases never appear until the cache is wiped (`:192-199`).
  - **[QUIRK]** A failed order request caches `null` (`:164-168`).
- **Library**: subproducts with a Linux or Windows download, de-duplicated by title;
  the app id is the machine name (`:122-137`, `:170-180`).
- **Installer choice** (`:273-331`): Linux when available, else Windows. Linux files
  are handled by type (MojoSetup `.sh` unzipped and merged, `-bin`/`mojo.run`
  unzipped, `.air` moved, anything else extracted). Windows zips are extracted into a
  new prefix; anything else runs under Wine. The exe uses the automatic markers.
- **Download link** (`:244-255`, `:352-399`): drop and re-fetch the order (links
  expire), pick the platform, and score the file variants (RPM penalised, 32-bit
  penalised on 64-bit hosts, non-`.deb` preferred).

### 6.5 Humble Bundle: terms, and why Irori has no Humble store

From terms-notes section 2 (Humble's terms dated 24 December 2024):

- Section (b) "Restrictions" forbids scraping (i), robots sending more requests than a
  human with a browser (ii), unreasonable load (v), bypassing access restrictions
  (xii), and, in **clause (xi)**, accessing any content on the service through any
  technology or means other than those Humble provides or authorises.
- Users may not share the unique URL of their download page; the service is for
  personal, non-commercial use.
- Humble has **no public API**. Every third-party client (Lutris, Playnite, the GOG
  Galaxy plugin, humble-cli) uses undocumented endpoints with a session cookie, and
  Lutris itself tells users that Humble restricts such clients. Heroic has no Humble
  support.

**Conclusion: Irori will not ship a Humble Bundle store.** A third-party client that
signs in and fetches the library is, by definition, a means Humble has not provided
or authorised, which clause (xi) forbids, and Irori does not build store integrations
that depend on breaking a store's terms. Users who buy on Humble download the files
from Humble's own site and add the game through Irori's sideload flow (Windows setup
programs, Linux installers, or a ROM for an engine). Irori can read a Humble entry
from an imported Lutris library as a sideloaded game, without contacting Humble.

### 6.6 EA app (`services/ea_app.py`)

- **Sign-in** (`:180-215`): OAuth through EA's accounts site with the Origin web
  client id; the token goes in `CACHE_DIR/ea_app/auth_token`.
  - **[QUIRK]** Requests go through a TLS adapter that allows unsafe legacy
    renegotiation. **Irori instead:** Irori uses its shared rustls client with
    default settings for every request and never weakens TLS for a store.
- **Library** (`:349-473`, `:167-178`): EA's GraphQL service: owned PC products (paged),
  base games only, then titles, slugs and art in batches of 100 offer ids; the app id
  is the content id.
- **Installed detection** (`:30-56`, `:485-529`): scan the client prefix's
  `Program Files/EA Games/*/__Installer/installerdata.xml` for content ids; the first
  must match a library game; create `<slug>-ea_app` with an `origin2://game/launch`
  argument listing the offer ids and asking for automatic download (`:577-580`).
- **Install**: run the client with that same URI; the client downloads, then launches
  (`:531-575`).

### 6.7 Other services, briefly

- **Stores Irori already has natively**: GOG (`services/gog.py`, installs through
  `gogdl_setup`, cloud saves around launch, `game.py:778-799`, `:1077-1093`), Epic
  (`services/egs.py`, runs the official launcher in Wine and reads its `.item`
  manifests, `util/egs/egs_launcher.py:10-21`), itch.io (`services/itchio.py`, pasted
  API key), Amazon (`services/amazon.py:676-716`, `autosetup_amazon`) and ZOOM
  (`services/zoom.py`). Irori's own stores need neither scripts nor official clients.
- **Steam** (`services/steam.py:90-245`): library from the Steam Web API with the
  embedded key (public profile required), installs from `appmanifest_*.acf`,
  playtime copied in hours. Irori reads Steam's local files and needs no key.
- **Flathub** (`services/flathub.py:160-200`): the generated script is an `execute`
  command running `flatpak install`.
- **Local** sources (`.desktop` files, Dolphin's game cache, ScummVM's config) are
  imported straight into the library; GameJolt and DLsite are online stores with
  manual-file fallbacks.

Irori's plan for the launcher services (Battle.net, Ubisoft Connect, EA app) is in
`docs/design/engines.md`; the Lutris behaviour above is what users will compare it to.

---

## 7. Launch and process monitoring

### 7.1 Launch sequence (`lutris/game.py`)

1. **Checks** (`:805-840`): installed, in the database, has a runner; the UI may veto
   (runner missing); wait for component updates.
2. **Prepare**: reload config, clear the log buffer, state "launching", **snapshot
   every running PID from `/proc`** (`:822-827`), run the runner's pre-launch on a
   worker thread.
3. **Configure** (`:715-802`):
   - The runner's `play()` result; the user may pick a launch config (`:653-676`).
   - Wrap the command (`runner_interpreter.py:23-147`), outermost first:
     gamescope (with its options) → `gamemoderun` → `taskset -c 0-(N-1)` → the
     command prefix → `mangohud` → the command. MangoHud is skipped for Steam and
     becomes `--mangoapp` inside gamescope. Gamescope flags cover game and output
     resolution, FPS limit, window mode, FSR (flag probed from `gamescope --help`),
     cursor grab, preferred GPU and HDR.
   - Environment: runner environment + `play()` environment + preload and library
     paths, plus `STORE`, `GAME_NAME` and `GAME_DIRECTORY`.
   - Desktop changes: US keyboard layout (X11), save the display layout, disable the
     compositor, inhibit the screensaver; turning off other monitors **sleeps 3
     seconds** (`:562-585`); switching resolution **sleeps 3 seconds** (`:760-764`).
   - Start AntiMicroX with the profile (`:587-603`); record the kill-switch device
     (`:643-651`).
   - Pre-launch script: must be an existing path (`:605-630`). **[QUIRK]** a command
     name on `PATH` is ignored with only a log warning. With "wait", it runs to
     completion; otherwise it runs in the background and is tracked.
   - GOG cloud-save download.
4. **Start** (`:842-880`): run the command through the monitor (section 7.2), start
   the playtime timer, state "running", Discord presence, heartbeat every 2 seconds
   (`:49`), write the title to `CACHE_DIR/now-playing.txt`.
5. Terminal mode writes the command to a script and runs it in the chosen terminal
   (`monitored_command.py:131-136`).
6. "Write script" exports the same command as a bash script (`runner_interpreter.py:150-173`).
   **[QUIRK]** environment values are not shell-escaped.

### 7.2 The wrapper and running-state tracking

- **Environment** (`monitored_command.py:152-192`): the full OS environment, plus the
  user's, plus `LUTRIS_GAME_UUID` (a fresh UUID) and a path for the return-code file.
  Keys containing `=` and unset values are dropped.
- The real program started is `lutris-wrapper` with the title, the include and
  exclude lists and the command (`monitored_command.py:110-136`).
  **[QUIRK]** A missing working folder is **created** (falling back to `/tmp`,
  `:329-334`), so a typo in a working folder makes a new empty folder.
- Output: stdout and stderr merged, read without blocking in 256 KiB chunks
  (`:283-299`); a few known noise lines are hidden (`:226-236`). **[QUIRK]** any chunk
  that mentions `winemenubuilder.exe` is dropped whole, taking unrelated lines with it.
- `lutris-wrapper` (`share/lutris/bin/lutris-wrapper`):
  - Becomes a **child subreaper** (`:54-85`), so orphaned grandchildren reparent to it
    instead of to init. This is how Lutris keeps track of games that fork and exit
    their launcher process.
  - Polls every 0.1 seconds (`:189-201`): first until a non-excluded descendant
    exists, then until no monitored descendant remains, reaping zombies and keeping
    the first process's exit code.
  - "Monitored" = descendants not in the exclude list or a built-in list of Wine
    service processes (wineserver, services.exe, winedevice.exe, explorer.exe,
    svchost.exe, rundll32.exe, conhost.exe and others), minus the include list. Names
    are compared truncated to 15 characters, the kernel's limit
    (`util/process_watcher.py:15-76`).
  - SIGTERM or SIGINT → SIGTERM to every descendant; a second signal → SIGKILL
    (`:136-151`). Writes the exit code to the return-code file and exits with it
    (`:207-218`).
- **Heartbeat** (`game.py:984-1025`), every 2 seconds:
  1. A wrapper error ends the session.
  2. A kill-switch device that disappears (a controller unplugged) forces a stop.
  3. Game PIDs = all current PIDs minus the pre-launch snapshot, filtered by the
     runner: by default, processes whose command line contains the game folder
     **and** whose environment has this launch's `LUTRIS_GAME_UUID`, plus any
     `gamescope` process when gamescope is on (`runner.py:545-564`). Wine also counts
     `pressure-vessel` processes (`runners/wine.py:1469-1493`).
  4. If only the background pre-launch script is left, quit.
  5. If the runner asks to keep the game alive (Steam), continue.
  6. Wrapper stopped and no game PIDs → the game has quit.
  7. Wrapper stopped but game PIDs remain (gamescope orphans) → force stop, then quit.
- **[QUIRK]** Scanning all of `/proc` and reading other processes' environment every
  2 seconds costs time proportional to every process on the machine, and a PID reused
  between the snapshot and a check is misattributed.
- **[QUIRK]** The running state is often wrong in both directions (section 8): a game
  whose launcher exits early looks stopped, and a game that leaves a helper process
  behind looks running.

### 7.3 Steam (`runners/steam.py`)

The `steam://` handler returns immediately, so Lutris tracks Steam's **reaper**
process whose command line contains `SteamLaunch AppId=<appid>` (`:58-95`). The game
is "alive" while the reaper exists; before it first appears Lutris waits up to
`launch_grace_seconds` (default 120 s, `:431-466`). The app manifest's state flags
(running, updating, validating) are a second signal (`:30-35`, `:481-492`). Stopping
signals reapers and tracked PIDs but **never the Steam client** (`:412-429`).

### 7.4 Stop and quit

- **User stop** (`game.py:1027-1042`): SIGTERM to the wrapper, which terminates its
  tree.
- **Force stop** (`:897-936`): the runner's own stop (Wine: `winekill`, then SIGTERM);
  then a 5-second watch polling every 0.5 seconds; then SIGKILL for survivors.
- **Session end** (`:964-981`): state "stopped", a warning if the game ran under 5
  seconds, the now-playing file deleted, and the timer's duration added to `playtime`
  in hours.
- **After quit** (`:1044-1141`): stop the pre-launch script, release sleep
  inhibition, run the post-exit script (**[QUIRK]** its include list uses the base
  name of the whole command string), upload GOG cloud saves, set `lastplayed`,
  restore the resolution, compositor and keyboard layout, stop AntiMicroX, clear
  Discord, and turn two exit patterns into readable errors: 127 with a shared-library
  message (missing library) and 1 with a wineserver mismatch (prefix in use by another
  Wine).

### 7.5 Launchers, sleeps and playtime

There is no general "close the launcher" step. The only launcher-specific behaviour
is Wine's `client_exe` pre-start with its **fixed 15-second sleep**
(`runners/wine.py:1183-1213`) and Steam's refusal to signal the Steam client.

For Battle.net, Epic, Ubisoft Connect and EA app games, the game's command *is* the
client with a launch argument (section 6), so the wrapper monitors the client:

- **[QUIRK]** The game counts as "running" for as long as the client's process tree
  lives. Playtime keeps counting after the game exits, until the user closes the
  client.
- **[QUIRK]** Stopping the game stops the client; Lutris never closes the client after
  the game exits on its own.
- **[QUIRK]** The fixed sleeps (15 s for clients, 3 s for each display change) are
  guesses: too short on slow machines, wasted time on fast ones.

Irori instead: playtime and running state follow the **game's** process, not the
launcher's. Irori identifies the game process by its executable path inside the
install folder (and the prefix's process list for Wine), waits for readiness by
watching for that process instead of sleeping, and offers to close a launcher it
started once the game exits. Irori uses no fixed sleeps in the launch path.

Two more weak spots: uninstall clears the game's runner (`game.py:464-485`), and
moving a game rewrites the old path in the YAML by text replacement before moving the
files, so a failed move leaves the config pointing at the new location
(`game.py:1151-1200`). **[QUIRK]** both.

---

## 8. User complaints Irori addresses

These are long-standing Lutris complaints from its issue tracker and community. Each
line gives Irori's answer; the details are in `docs/design/engines.md`.

| Complaint | Lutris issues | Irori's answer |
| --- | --- | --- |
| Dated GTK interface, poor 4K and fractional scaling | lutris/lutris#3886, #1719 | GPU-rendered GPUI interface with resolution-independent layout (rem units), tested on every bundled theme in light and dark. |
| Install scripts break because they pin a Wine build that no longer exists | #958, #4299 | Irori manages Wine and Proton versions itself and shows a proposed substitute when a requested version is gone, never a silent failure or silent swap. |
| Changing a default (for example the default Wine) breaks games that relied on it | #6426 | Each game records the engine or Wine version it was installed or last launched with; changing a default never changes an existing game without asking. |
| Store sign-ins break: EA, Ubisoft, Humble, itch.io | #4235, #4836, #4099, #6215 | Native store protocols (Epic, GOG, Amazon, itch.io, ZOOM) with a separate sign-in window and a browser fallback; no Humble store (section 6.5); clear "signed out, sign in again" state instead of an empty library. |
| Official launchers running in Wine are slow and fragile | #6259 | Native stores need no official launcher. Where one is unavoidable, Irori tracks the game process, not the launcher, and can close the launcher afterwards. |
| No unified library across stores and emulators | #4770, #2887 | One library for store games, sideloaded games and emulated games, with shared categories, favourites, playtime and search. |
| About 40 system options and about 34 Wine options, mostly unexplained | (general) | Fewer options, each with a one-line description in the UI, grouped by what they affect; unavailable options shown disabled with the reason. |
| Hard to debug a game that does not start | (general) | Per-game log with the exact command, environment changes and engine version; readable errors for missing BIOS, missing engine, missing file and bad prefix before the game starts. |
| Wrong running state: games shown as running after exit, or stopped while running | #973 | Running state and playtime follow the game's own process; no fixed sleeps. |
| Adding a local game is confusing (runner first, then options in tabs) | #4899 | "Add game" asks for the file, detects the system, and suggests the engine; Windows setup programs and folders take the sideload flow. |
| Scripts run shell commands without confirmation | (section 4.8) | Irori shows what a script will run and asks first; writes are confined to the game folder, prefix and cache. |
| Linux only | (by design) | Irori runs on Linux, macOS and Windows; engines come from each upstream's builds for that OS. |
