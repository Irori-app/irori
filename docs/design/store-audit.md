# Store audit: Irori against Lutris

Irori's Epic, GOG, Amazon and ZOOM stores were built to match Heroic Games Launcher
2.22.4, and on those four stores they already go further than Lutris. Lutris installs
Epic games through the official launcher in Wine, has no Amazon updates or repair, and
syncs no playtime with any store except Steam. What Lutris adds beyond Heroic is mostly
breadth: itch.io collections, bonus files for ZOOM and itch.io, the owned-but-not-installed
libraries of Ubisoft Connect, EA app and Battle.net, community install scripts matched to
store games, and local sources (desktop entries, Dolphin, ScummVM). The proposals below
close the useful gaps and pick a few things neither launcher does well: save snapshots
before a cloud download, following games whose first process exits early, one library
entry for a game owned in several stores, and smaller updates.

| Store | Biggest gap against Lutris | Proposal | Size |
| --- | --- | --- | --- |
| Epic | None that matters; Lutris's asset filter is a little wider | Snapshot saves before a cloud download | S |
| GOG | Lutris offers lutris.net community scripts for GOG games | Apply a community script's fixes on top of the native install | M |
| Amazon | None; Lutris cannot update or repair | Delta updates through Amazon's `FUEL_PATCH` encoding | L |
| ZOOM | Bonus files (manuals, soundtracks) | Implement `Store::extras` for ZOOM | S |
| itch.io | Collections, and bonus files | Read collections; bonus files; later wharf patches | M, S, L |
| Steam | In this branch; Snap Steam and Windows Steam in Wine remain | Look in the Snap folder and every Steam folder found | S |
| Third-party launchers | Games owned but not installed; install from Lutris | “Install in Battle.net/Ubisoft Connect”; watch the launcher's files | M, S |
| ROM library | Dolphin and ScummVM libraries | Offer the emulators' own game folders as ROM folders | S |
| Sideloaded apps | Desktop entries (XDG) and Flatpak games | “Games on this computer” import on Linux | S |
| All stores | Lutris links store games across stores through lutris.net | One library entry per game owned in several stores | L |

## How to read this

- Irori paths are relative to the repository root. Lutris paths are relative to the
  `lutris/` package at commit 2ee6b2f. Line numbers are as of 2026-10-29; files the
  Steam rework is editing (`crates/services/src/games.rs`, `follow.rs`, the stores'
  `launch.rs`) may shift.
- “Heroic baseline” is what Irori matches from `docs/parity/stores-epic-gog.md` and
  `docs/parity/stores-amazon-zoom-sideload-downloads.md`. Behaviour notes on Lutris
  are in `docs/parity/lutris.md`; this file does not repeat them.
- Sizes: **S** is a few days, **M** one to two weeks, **L** more, or needs research
  first.

## Cross-store findings

These apply to several stores, so they are listed once.

1. **Community scripts for store games.** Lutris asks lutris.net which games a store's
   app ids belong to (`services/base.py:260-274`) and offers the community installers
   whose version names the store, appending a generated one marked “auto-generated”
   (`services/base.py:311-335`). Those scripts often carry fixes: winetricks verbs, DLL
   overrides, environment variables, arguments. Irori refuses GOG, Amazon and ZOOM
   autosetups with `InstallFromStore` and offers the native store instead (AGENTS.md
   4.8), so it installs better but drops the fixes. Its only per-game fixes are
   Heroic's known fixes (`crates/extras/src/known_fixes.rs:1-6`).
2. **Running state.** Lutris's `lutris-wrapper` becomes a child subreaper and watches
   every descendant (`docs/parity/lutris.md` 7.2). Irori waits for the first process
   (`crates/services/src/games.rs:257-268`), follows the install folder only for
   `Runner::Launchers` (`games.rs:257-260`, `crates/services/src/follow.rs:1-5`), and
   waits for the prefix only when the plan asks (`games.rs:279-281`). GOG never asks
   (`crates/store-gog/src/launch.rs:438-442`). A GOG or itch.io game whose first program
   hands over to another and exits ends its session early. Lutris gets this right for
   the wrong reason (it scans all of `/proc` every 2 s) and wrong for launchers.
3. **Cloud-save safety.** Both launchers detect conflicts (Lutris
   `services/gog_cloud_hooks.py:147-178`; Irori `SaveConflict`, AGENTS.md 4.1). Neither
   keeps a copy of the local saves before a download overwrites them; Irori's backups
   cover settings and library data only (`crates/services/src/backup.rs:1-6`).
4. **Playtime.** Lutris syncs playtime with no store except reading Steam's hours
   (`docs/parity/lutris.md` 6.7). Irori uploads and reads back Epic and GOG playtime
   with an offline queue (`crates/store-epic/src/playtime.rs:1-4`,
   `crates/store-gog/src/playtime.rs:1-5`) and reads Steam's local totals
   (`crates/store-steam/src/lib.rs:11-13`).
5. **Same game, several stores.** Lutris links a store game to an existing Lutris game
   through lutris.net slugs (`services/base.py:236-258`) but still shows one row per
   store. Irori shows one row per store too and has no cross-store identity.

Cross-store proposals, by user value:

- **Follow the game, not its first process, everywhere (M).** When the launched
  process exits within the first minutes and processes still run from the install
  folder (or, for Wine, in the prefix), keep the session open with the existing
  `follow.rs` logic. Applies to GOG, itch.io, ZOOM, sideloaded apps and Epic
  third-party titles. No `/proc` scan while nothing qualifies, no fixed sleeps.
- **Save snapshot before every cloud download (S).** Before a download direction
  writes into a save folder, copy the files it will replace into
  `<state>/save-snapshots/<runner>/<app>/<time>/`, keep the last five, and offer
  “Restore previous saves” on the cloud-save sheet. Neither launcher does this, and
  it makes a wrong conflict choice recoverable.
- **Community fixes on native installs (M).** For a store game, look up its
  lutris.net scripts on request, parse them with `irori-scripts`, and offer only the
  parts that configure rather than install (winetricks verbs, DLL overrides,
  environment, arguments, registry values) through the existing consent plan. Fetch
  only when the user asks (AGENTS.md 4.8 etiquette).
- **One entry per game owned in several stores (L).** Match titles across stores
  (normalised title plus release year, with store ids where a public mapping exists),
  show one library entry with a store picker, and keep per-store installs and
  playtime separate underneath. Lutris and Heroic both show duplicates.

## Epic Games Store

**Heroic baseline in Irori.** Native legendary port: sign-in, library, selective
downloads, install, delta update, repair, move, import, cloud saves, EOS overlay,
EGL sync, EA and Ubisoft titles set up through Epic, achievements and several
accounts (`crates/store-epic/src/lib.rs:1-7`, `egl_sync.rs:1-3`, `overlay.rs:1-3`,
`achievements.rs:1-2`).

**What Lutris adds beyond Heroic.**

- A wider filter for non-games: entitlement type `AUDIENCE`, the `type/format-item`
  category, `ListingIdentifier` and releases with `compatibleApps`, which catch Fab
  listings (`services/egs.py:336-352`). Irori filters the `ue` namespace and the
  `assets`, `asset-format`, `plugins`, `projects` and `mods` categories
  (`crates/store-epic/src/metadata.rs:144-153`), which may let Fab items through.

**Where Irori is already better.** Lutris needs the Epic Games Launcher installed in
Wine for every install and launch (`services/egs.py:556-567`, `:569-579`), so it has
none of the native download, update, repair, cloud-save or overlay work. Irori also
syncs playtime both ways (`crates/store-epic/src/playtime.rs:1-4`).

**Proposals.**

1. Add Lutris's Fab checks to `build_entry` (S).
2. Follow EA and Ubisoft titles started through Epic by their folder (part of the
   cross-store “follow the game” item) (S once that exists).

## GOG

**Heroic baseline in Irori.** Native gogdl port: depots v1 and v2, Linux installers by
range request, DLC selection, delta updates from the recorded build, repair,
redistributables, cloud saves with several locations, comet through
`irori-gog-galaxy`, achievements and several accounts (`crates/gog/src/plan.rs:1-3`,
`linux.rs:1-3`, `crates/store-gog/src/build.rs:1-3`, `redist.rs:1-6`, `saves.rs:1-6`).

**What Lutris adds beyond Heroic.**

- An offline-installer fallback next to the depot install (`services/gog.py:660-684`)
  and patch-file updates for offline installs (`services/gog.py:932-951`).
- Updates that include only the DLC already installed (`services/gog.py:891-931`).
- Community scripts (cross-store finding 1).

**Where Irori is already better.** Irori has all three of the first two: offline
installers, patches, language packs and goodies are listed and downloadable, and a
folder of offline installers installs by unpacking, never running, them
(`crates/store-gog/src/offline.rs:1-8`, `:208-228`; `crates/feature-game/src/import_installers.rs`).
Build records keep the DLC selection for updates (`build.rs:1-3`). Irori also uploads
playtime with an offline queue (`playtime.rs:1-5`), sets GOG presence
(`presence.rs:1`) and runs comet; Lutris does none of these.

**Proposals.**

1. The cross-store “follow the game” and save-snapshot proposals matter most here,
   since GOG games often ship a launcher program (M, S).
2. Community fixes on native installs (cross-store finding 1) (M).

## Amazon Games

**Heroic baseline in Irori.** Native nile port: device sign-in, library, signed
manifests, install, update and repair through the depot engine, `fuel.json` launch
and post-install steps, and the Amazon Games SDK files
(`crates/amazon/src/lib.rs:1-9`, `crates/store-amazon/src/install.rs:1`,
`crates/store-amazon/src/lib.rs:632`, `:689`, `:702`).

**What Lutris adds beyond Heroic.** Nothing usable. Its install is a generated
script with `autosetup_amazon` (`services/amazon.py:676-721`); it defines no update
installers. It has a client for Amazon's `GetPatches` call asking for `FUEL_PATCH`
deltas, but nothing calls it (`services/amazon.py:509-547`).

**Where Irori is already better.** Updates, repair, post-install redistributables and
the SDK; Lutris runs none of them.

**Proposals.**

1. **Delta updates (L).** Ask `GetPatches` for `FUEL_PATCH` encodings
   when updating and apply the patches instead of downloading whole changed files.
   Needs research into the patch format first; fall back to today's whole-file
   download when a patch is missing or fails verification. Irori currently uses
   `GetGameDownload` only (`crates/amazon/src/api.rs:328`).

## ZOOM Platform

**Heroic baseline in Irori.** Library, install from ZOOM's Windows and Linux files
with native extraction (MojoSetup, makeself, tar, zip) that cannot write outside the
target, updates, repair, DOSBox games with the system DOSBox, and Heroic import
(`crates/store-zoom/src/extract.rs:1-9`, `launch.rs:1-10`, `lib.rs:538`, `:642`).

**What Lutris adds beyond Heroic.**

- Bonus files: manuals, misc and soundtracks from `/li/game/<id>/files`, each with a
  download link from `/li/download/<id>` (`services/zoom.py:179-207`). Irori parses
  the same lists (`crates/store-zoom/src/api.rs:63-77`) but only GOG implements
  `Store::extras` (`crates/core/src/store.rs:338-344`,
  `crates/store-gog/src/lib.rs:568`).

**Where Irori is already better.** Lutris has no updates or repair for ZOOM games, and
fails when a platform has more than one file (`services/zoom.py:283-307`).

**Proposals.**

1. **ZOOM bonus files (S).** Map `manual`, `misc` and `soundtrack` to `ExtraFile`
   and resolve the address with `/li/download/<id>` in `extra_url`. The game page
   already shows GOG's extras, so no new UI.

## itch.io

itch.io is Irori-only; Heroic has no itch.io store. The baseline is Irori's own.

**Irori today.** API-key sign-in, owned keys (`crates/itch/src/api.rs:100`), upload
choice per system preferring full versions, butler channels and archives
(`crates/itch/src/model.rs:280-305`), install and update that unpack into a sibling
folder and swap it in (`crates/store-itch/src/install.rs:280-306`), and update checks
by build id or upload revision (`crates/itch/src/model.rs:210-218`,
`crates/store-itch/src/lib.rs:462-489`).

**What Lutris adds.**

- **Collections.** Lutris reads the user's collections and their games
  (`services/itchio.py:232-242`, `:341-406`) and lists owned keys plus every
  collection game, or only a collection titled “lutris” when one exists
  (`services/itchio.py:419-442`). This is how free games without a download key
  reach the library. Irori lists owned keys only (`crates/store-itch/src/lib.rs:386-409`)
  and its upload calls require a key id (`crates/itch/src/api.rs:123-139`).
- **Bonus files**: soundtracks, books, documents and other non-game uploads
  (`services/itchio.py:480-502`). Irori skips them as not installable
  (`crates/itch/src/model.rs:307-311`).
- **Architecture-aware choice**: ARM builds are penalised on x86 and the reverse,
  wrong bitness and art packs are penalised (`services/itchio.py:832-861`). Irori's
  choice has no architecture rule.

**Where Irori is already better.** Lutris updates by downloading the upload again and
merging it over the old folder, which leaves deleted files behind
(`services/itchio.py:623-711`, wharf patching is a TODO at `:632`), and decides an
update is due by comparing upload time with install time (`:599-621`). Irori swaps
in a clean folder and compares build revisions.

**Proposals.**

1. **Collections (M).** Fetch `profile/collections` and their games; list games
   without a key when they are free (`min_price` 0) and make `uploads` and
   `download_location` take an optional key. Let the user choose which collections
   count in the itch.io account card instead of Lutris's magic title.
2. **Bonus files (S).** Implement `Store::extras` from non-game uploads.
3. **Architecture in upload choice (S).** Add an architecture term ahead of
   recency in `best_upload`, for ARM Linux and Apple silicon.
4. **Wharf patches (L).** For butler channels, download the patch chain between the
   installed and current build and apply it, falling back to a full download.
   Neither launcher does this; the wharf format is open source.

## Steam

**In this branch.** Sign-in with QR code or Steam Guard for owned and Family Sharing
games; `steam://install` and `steam://uninstall`; reaper and `RunningAppID` process
tracking; watching `steamapps`; launch arguments; Steam's state flags; Lutris import of
Steam arguments.

**Also in Lutris, not in the list above.**

- **More Steam folders.** Lutris looks in Debian's `~/.steam/debian-installation`,
  Snap's `~/snap/steam/common/.local/share/Steam`, several Flatpak layouts and
  `/usr/share/steam` (`util/steam/config.py:14-30`). Irori takes the setting, one
  Flatpak path, then the default (`crates/runtime/src/locations.rs:92-108`), so a
  Snap install is not found, and with both native and Flatpak Steam only the Flatpak
  library is read. Proposal: try every
  known folder and merge the libraries found, de-duplicated by app id (S).
- **Windows Steam in a prefix** (`services/steamwindows.py:52-90`): games installed by
  Steam for Windows in Wine. Low value next to Proton; not recommended now.
- **Stop never signals the Steam client** (`runners/steam.py`, `docs/parity/lutris.md`
  7.3). Keep that rule in the reaper work.

## Third-party launchers (Battle.net, EA app, Ubisoft Connect, Rockstar)

Heroic has no equivalent beyond EA and Ubisoft titles bought on Epic. Irori sets each
launcher up in its own prefix, reads installed games from local data, and starts them
through the launcher (`crates/store-launchers/src/lib.rs:1-13`).

**What Lutris adds.**

- **Games not installed yet.** Battle.net lists a fixed table of Blizzard products
  without any account (`services/battlenet.py:22-68`, `:123`). Ubisoft Connect signs
  in and reads owned games from Ubisoft's GraphQL service (`services/ubisoft.py:180`);
  EA app signs in with OAuth and reads owned products (`services/ea_app.py:349-473`).
- **Install from the library.** `Battle.net.exe --exec="install <code>"`
  (`services/battlenet.py:236`), `uplay://install/<id>` (`services/ubisoft.py:316`),
  `origin2://game/launch?…&autoDownload=1` (`services/ea_app.py:580`). Irori refuses
  install, update and repair for these games (`crates/store-launchers/src/lib.rs:484-506`)
  and can only open the launcher (`:232`).

**Where Irori is already better.** No account handling, so nothing breaks when EA or
Ubisoft change their sign-in pages (Lutris issues #4235, #4836). Battle.net counts a
product only when the agent says it is installed and playable
(`crates/launchers/src/battlenet.rs:9-10`). Rockstar is supported; Lutris has no
Rockstar service. Sessions follow the game's processes instead of the launcher's,
with no 15-second sleep (`crates/services/src/follow.rs:1-5`; Lutris
`runners/wine.py:1226-1256`).

**Proposals.**

1. **“Install in Battle.net” and “Install in Ubisoft Connect” (M).** Without
   signing in: offer Blizzard's products from a static table, and Ubisoft products
   the client's configuration cache already describes
   (`crates/launchers/src/ubisoft.rs:78`) that the registry doesn't list as
   installed (`:221-257`). The button runs the launcher with its install argument;
   the game appears when the launcher records it. Label these as “not installed” and
   leave ownership to the launcher. EA stays installed-only, since its local data
   has nothing for games that are not installed.
2. **Watch launcher data (S).** Rescans run only on refresh
   (`crates/store-launchers/src/lib.rs:286-305`, `:451-454`). Watch `product.db`, the
   EA game folders and the prefix's `system.reg` with `notify`, as the ROM library
   does (`crates/store-emulation/src/lib.rs:43`, `:83`), so a game installed inside a
   launcher shows up by itself.

## ROM library and emulators

Heroic has no equivalent. Irori scans ROM folders incrementally, detects ScummVM and
DOS folder games, and watches the folders (`crates/store-emulation/src/scan.rs:8`,
`:147`; `crates/store-emulation/src/lib.rs:43`).

**What Lutris adds.** Imports from emulators' own lists: Dolphin's game cache
(`services/dolphin.py:33-40`) and ScummVM's `scummvm.ini` (`services/scummvm.py:14`,
`:33`). MAME is a 14-line stub (`services/mame.py`).

**Proposal.** **Suggest the emulators' folders (S).** When an engine is installed or
detected, read the folders it already knows (Dolphin's `ISOPath` entries, ScummVM's
game `path` values, RetroArch's playlist folders) and offer them as ROM folders in one
step. This reuses Irori's scanner instead of importing each emulator's list, so names,
systems and art stay consistent.

## Sideloaded apps

**Heroic baseline in Irori.** Native, Windows and browser apps, with Heroic import
and browser app windows (`crates/store-sideload/src/lib.rs`, `launch.rs:1-18`).

**What Lutris adds.** The XDG service imports every visible desktop entry in the
`Game` category, skipping its own shortcuts and a block list
(`services/xdg.py:56-95`). That covers distribution packages and installed Flatpak
games.

**Proposal.** **“Games on this computer” (S, Linux).** In the Add game dialog, list
desktop entries in the `Game` category that are not Irori's own shortcuts and not
already in the library (Steam and Lutris shortcuts included), and add the chosen ones
as native apps that launch through the entry's `Exec` line. Never write the entries.

## Lutris services with no Irori equivalent

| Service | What it does in Lutris | Recommendation |
| --- | --- | --- |
| Humble Bundle | Cookie sign-in, undocumented order API, downloads Linux or Windows files (`services/humblebundle.py`) | **No.** Humble's terms forbid third-party clients (`docs/parity/lutris.md` 6.5). Users download from Humble and use Add game. |
| Flathub | Lists Flathub's game category and installs with `flatpak install` (`services/flathub.py:60`, `:86-200`) | **Not as a store.** Installed Flatpak games come in through “Games on this computer”. A Flathub catalogue is a software centre's job. |
| GameJolt | Cookie sign-in to the site's private `site-api`, library and build downloads (`services/gamejolt.py:21`, `:73`, `:136-245`) | **No for now.** The library API is undocumented and the audience is small; revisit if Game Jolt publishes one. |
| DLsite | Cookie sign-in, undocumented purchase and download endpoints (`services/dlsite.py:73-130`, `:184-231`) | **No.** Same undocumented-API problem, and the downloads are mostly Windows archives that Add game handles. |
| XDG desktop entries | Imports `Game` desktop entries (`services/xdg.py:56-95`) | **Yes**, as “Games on this computer” (S). |
| Dolphin | Imports Dolphin's game cache (`services/dolphin.py:33-40`) | **Yes, indirectly**: suggest Dolphin's folders as ROM folders (S). |
| ScummVM | Imports games from `scummvm.ini` (`services/scummvm.py:14`, `:33`) | **Yes, indirectly**: suggest the folders it lists (S). |
| MAME | Unfinished stub (`services/mame.py`) | **No.** The ROM library already detects MAME sets. |
| Steam for Windows | Steam games installed by Windows Steam in Wine (`services/steamwindows.py`) | **No for now** (see Steam). |
| Lutris (lutris.net account) | The user's lutris.net library (`services/lutris.py:90-104`) | **No.** Irori has no account of its own; the Lutris import covers local Lutris data. |

## All proposals, by user value

| # | Proposal | Stores | Size |
| --- | --- | --- | --- |
| 1 | Follow the game, not its first process | GOG, itch.io, ZOOM, sideload, Epic third-party | M |
| 2 | Save snapshot before every cloud download, with restore | Epic, GOG | S |
| 3 | itch.io collections, including free games without a key | itch.io | M |
| 4 | “Install in Battle.net / Ubisoft Connect” for games not installed | Launchers | M |
| 5 | “Games on this computer” from desktop entries | Sideload | S |
| 6 | Bonus files for ZOOM and itch.io | ZOOM, itch.io | S each |
| 7 | Every Steam folder, including Snap and Debian | Steam | S |
| 8 | Watch launcher data for new installs | Launchers | S |
| 9 | Suggest emulators' own game folders | ROM library | S |
| 10 | Community fixes on native installs | GOG, Epic, Amazon, ZOOM | M |
| 11 | Fab and other non-game filters | Epic | S |
| 12 | Architecture-aware itch.io upload choice | itch.io | S |
| 13 | One library entry per game owned in several stores | All | L |
| 14 | itch.io wharf patches | itch.io | L |
| 15 | Amazon `FUEL_PATCH` delta updates | Amazon | L |
