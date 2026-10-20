# Parity spec: Epic (Legendary) and GOG (gogdl / comet) store backends

Reference: Heroic Games Launcher **v2.22.4** (TypeScript). This document describes the observable
behaviour of the Epic and GOG store backends so that a from-scratch Rust implementation can be
behaviour-compatible: same helper-binary invocations, same files on disk, same HTTP calls, same
GameInfo objects sent to the frontend.

All `file:line` references are relative to `src/` in the Heroic source tree
(`backend/...`, `common/...`, `frontend/...`).

Bundled helper versions observed in the installed Heroic (`/usr/lib64/heroic/resources/app.asar.unpacked/build/bin/x64/linux/`):

| Binary    | Version                         | Notes |
|-----------|---------------------------------|-------|
| legendary | 0.21.1 "Lowlife" (Heroic fork)  | Python zipapp |
| gogdl     | 1.3.0                           | Python zipapp; source can be read with `unzip` |
| comet     | (Rust)                          | GOG Galaxy communication-service emulator |
| nile      | (out of scope)                  | Amazon |

Scope: host is Linux. Windows/macOS-only code paths are documented briefly so that engineers know
what they can skip; they are flagged **[non-Linux]**.

---

## 0. Conventions, directories, global settings

### 0.1 Path variables used in this document

| Name | Value on Linux (native package) | Source |
|------|----------------------------------|--------|
| `$APPDATA` | `$XDG_CONFIG_HOME` or `~/.config` (Electron `app.getPath('appData')`) | `backend/constants/paths.ts:8` |
| `$HEROIC` | `$APPDATA/heroic` (`appFolder`). Equals Electron `userData` (`userDataPath`) because the app name is `heroic`. | `paths.ts:23-24` |
| `$TOOLS` | `$HEROIC/tools` | `paths.ts:25` |
| `$GAMESCFG` | `$HEROIC/GamesConfig` (per-game settings `<appName>.json`, game logs) | `paths.ts:29` |
| `$LEGCFG` | `$HEROIC/legendaryConfig/legendary` (passed as `LEGENDARY_CONFIG_PATH`) | `storeManagers/legendary/constants.ts:4-8` |
| `$GOGDLCFG` | `$HEROIC/gogdlConfig/heroic_gogdl` (`gogdlConfigPath`) | `storeManagers/gog/constants.ts:4` |
| `$GOGSUPPORT` | `$GOGDLCFG/gog-support` | `gog/constants.ts:5` |
| `$GOGREDIST` | `$TOOLS/redist/gog` | `gog/constants.ts:6` |
| `$EPICREDIST` | `$TOOLS/redist/legendary` | `legendary/constants.ts:16` |
| `$GOGAUTH` | `$HEROIC/gog_store/auth.json` | `gog/constants.ts:7` |
| `$CACHE` | `$HEROIC/store_cache` (all `CacheStore` files) | `backend/cache.ts:17-26` |
| `$BIN` | `<resources>/app.asar.unpacked/build/bin/<arch>/linux/` | `backend/utils.ts:437-452` |

Flatpak: `$APPDATA` becomes `~/.var/app/com.heroicgameslauncher.hgl/config`.

### 0.2 Helper binary resolution (`backend/utils.ts:437-500`)

* `getLegendaryBin()`: if global setting `altLegendaryBin` is non-empty, use it; else
  `$BIN/<process.arch>/linux/legendary`, falling back to `$BIN/x64/linux/legendary` if the
  arch-specific one does not exist. Any `app.asar` in the path is rewritten to
  `app.asar.unpacked`.
* `getGOGdlBin()`: same with `altGogdlBin` / `gogdl`.
* `getCometBin()`: same with `altCometBin` / `comet`.
* Each returns `{dir, bin}` (dirname/basename). The process is spawned as `./<bin>` with
  `cwd = dir` unless `bin` is absolute (`backend/launcher.ts:1673-1678`).

### 0.3 Global settings consumed by these backends

From `GlobalConfig.get().getSettings()` (`$HEROIC/config.json`, key `defaultSettings`):

| Setting | Used by |
|---------|---------|
| `maxWorkers` (number, 0 = unset) | Legendary `--max-workers N`; gogdl `--max-workers N` |
| `downloadNoHttps` (bool) | Legendary `--no-https` |
| `altLegendaryBin`, `altGogdlBin`, `altCometBin` | binary override |
| `disablePlaytimeSync` | GOG playtime posting and GOG presence (both disabled) |
| `disableGOGPresence` | GOG presence only |
| `experimentalFeatures.cometSupport` | default **enabled** (`!== false`): spawn comet + install GalaxyCommunication dummy service |
| `autoUpdateGames` | auto-queue updates after `checkGameUpdates` |
| `language` (UI language, e.g. `en`, `pt`, `zh_Hans`) | stored in `configStore` key `language`; Legendary `--language` fallback and Epic store-content locale |
| `wineVersion`, `defaultInstallPath`, ... | generic |

Per-game settings (`GameConfig`, `$GAMESCFG/<appName>.json`) used here: `wineVersion`,
`winePrefix`, `wineCrossoverBottle`, `language`, `launcherArgs`, `targetExe`, `offlineMode`,
`verboseLogs`, `autoSyncSaves`, `savesPath` (Epic), `gogSaves` (GOG array of `{name, location}`),
`enviromentOptions` (sic), `wrapperOptions`, `steamRuntime`, `ignoreGameUpdates`,
`lastUsedLaunchOption`, `disableUMU`, MangoHud/GameMode/Gamescope toggles, etc.

---

## 1. Shared runner-invocation layer (`callRunner`) — applies to both legendary and gogdl

`backend/launcher.ts:1658-1830`. Every legendary/gogdl invocation goes through this. Reproduce
exactly; several store behaviours depend on its quirks.

### 1.1 Spawn

1. `commandParts = commandParts.filter(Boolean)` — **empty strings are dropped**
   (relevant: GOG `save-sync` with `arg = ''`) (`launcher.ts:1671`).
2. Spawn `bin` (`./legendary` or `./gogdl`) with `args = commandParts`,
   `cwd = options.cwd || runner.dir` (the binary's directory!),
   `env = {...process.env, ...options.env}`, and an `AbortSignal` from the abort controller
   registered under `abortId` (`launcher.ts:1734-1738`).
3. `abortId = options.abortId || appNameFromCommandParts(...) || Math.random()`.
4. [non-Linux] On Windows the command is wrapped in `powershell -NoProfile Start-Process ... -Wait -NoNewWindow -ArgumentList ...`.

`appNameFromCommandParts` (`launcher.ts:1615-1656`): for gogdl, if a part equals `launch` the
appName is at `idx+2`; else for `download|repair|update` at `idx+1`. For legendary, the part after
`launch|install|repair|update`. Used only for the default abortId, the "Game Output:" log header and
the error handler's force-uninstall prompt.

### 1.2 De-duplication

`key = [runner.name, commandParts].join(' ')` (note: array stringified with commas). If a command
with the same key is already running, the **same promise is returned** and no new process is
spawned (`launcher.ts:1721-1728`). Removed from the map in `finally`.

### 1.3 Output capture

* stdout and stderr are decoded as UTF-8; each `data` chunk is:
  * passed through `options.logSanitizer` (if any) and written to every log writer
    (the runner log writer for `legendary`/`gog` is always appended, plus any passed ones),
  * passed **unsanitized** to `options.onOutput(chunk, child)` (both stdout and stderr chunks go to
    the same callback),
  * `chunk.trim()` pushed into a bounded ring buffer (`memoryLog`, **last 50 chunks only**)
    (`backend/utils.ts` `memoryLog`).
* Result on close: `stdout = stdoutChunks.join('')` (trimmed chunks, **no separator**),
  `stderr = stderrChunks.join('\n')`.
  * Consequence: JSON outputs (`legendary info --json`, `gogdl info`, `gogdl auth`, `gogdl import`)
    are parsed from this concatenation. Very large outputs (> 50 chunks) would be truncated; a
    faithful reimplementation should simply capture the full stdout.
* **Exit code is ignored.** The promise resolves `{stdout, stderr}` regardless of exit status.
  `error` is only set when the child emits `'error'` (spawn failure / abort) or was terminated by a
  signal it did not receive from us (`launcher.ts:1773-1793`).
* On close, `errorHandler(stdout+stderr, appName, runner)` runs (`backend/utils.ts:300-357`):
  * ignores `IndexError: list index out of range`, `RequestsDependencyWarning`;
  * text containing `appears to be deleted` → "folder deleted, remove from installed list?" dialog →
    `game.forceUninstall()` on Yes;
  * text matching `/legendary.*\.py/` (a Python traceback) and not `MemoryError: ` → error dialog
    "Try to Logout and Login on your Epic account";
  * text containing `No saved credentials` → "credentials expired" dialog.
* Abort: if the abort controller fired, the result is
  `{stdout:'', stderr:'', fullCommand, abort:true}` (`launcher.ts:1799-1809`). Other spawn errors →
  `{stdout:'', stderr:String(error), fullCommand, error}`.

### 1.4 Logging / redaction

`getRunnerCallWithoutCredentials` (`launcher.ts:1841-1889`): the logged command is
`ENV=VAL ... <fullRunnerPath> <args...>` where only env vars that differ from `process.env` are
printed, values with spaces are double-quoted, and the argument after `--code` or `--token` is
replaced by `<redacted>`. (gogdl `--password` is **not** redacted.) Log line:
`<logMessagePrefix or "Running command">: <safeCommand>`.

### 1.5 Abort / kill / pause

* Abort controllers are keyed by `abortId` (usually the appName). `callAbortController(id)` sends
  SIGTERM via Node's AbortSignal.
* Download pause/cancel (`downloadmanager/downloadqueue.ts:231-281`): `callAbortController(appName)`
  then `game.stop(false)`. Resuming re-runs the identical install/update command; legendary resumes
  from its own resume file, gogdl from `<install>/.gogdl-resume`.
* IPC `kill` (`main.ts:1090`): `callAbortController(appName)` then `game.stop()` (see per-store stop).

### 1.6 Wine/Proton flag helpers used by both stores (`backend/utils/compatibility_layers.ts:507-594`)

`getWineFlags(gameSettings, wrapper)` where `wrapper = shlex.join(wrappers)` (may be `''`):

| `wineVersion.type` | flags |
|--------------------|-------|
| `wine`, `toolkit`, `crossover` | `--wine <bin>` and, if `wrapper` non-empty, `--wrapper <wrapper>` |
| `proton`, umu **not** used | `--no-wine --wrapper '<wrapper> "<proton bin>" waitforexitandrun'` (note leading space when wrapper empty) |
| `proton`, umu used | `--no-wine --wrapper '<wrapper>'` (wrapper already ends with the umu-run path) |

`bin` has surrounding single quotes stripped (old configs). `getWineFlagsArray` turns that into
`['--wine', bin, '--wrapper', w]` / `['--no-wine', '--wrapper', w]` (insertion order kept).

`isUmuSupported(settings, checkInstalled=true)` (`compatibility_layers.ts:581-594`): Linux AND
`wineVersion.type === 'proton'` AND `disableUMU !== true` AND (if checking) the umu-run path exists.
`getUmuPath()` = `umu-run` on `$PATH`, else `$TOOLS/runtimes/umu/umu_run.py`.

### 1.7 Wrapper list (`launcher.ts:1321-1350`)

Order: gamescope command (incl. trailing `--`), each `wrapperOptions` entry (`exe` + shlex-split
`args`), MangoHud (`mangohud --dlsym`, only if gamescope list is empty), `gamemoderun`, then the
Steam runtime / umu path. Empty strings removed.

`prepareLaunch` (`launcher.ts:484-736`) produces `offlineMode`, `mangoHudCommand`, `gameModeBin`,
`gameScopeCommand`, `steamRuntime` (`[umuPath]` when
`(!isNative && isUmuSupported(settings, false)) || (isNative && settings.steamRuntime)`; downloads umu
if missing). `offlineMode = settings.offlineMode || !isOnline()`; **for legendary only**, if still
false, `offlineMode = await isEpicServiceOffline()` (`launcher.ts:492-496`).

### 1.8 Environment helpers

* `setupWrapperEnvVars({appName, appRunner})` (`launcher.ts:1072-1102`):
  `HEROIC_APP_NAME=<appName>`, `HEROIC_APP_RUNNER=<runner>`, `GAMEID=umu-0`, plus
  legendary → `HEROIC_APP_SOURCE=epic`, `STORE=egs`; gog → `HEROIC_APP_SOURCE=gog`, `STORE=gog`.
* `setupEnvVars(settings, installPath)` (`launcher.ts:1029-1065`): nvidiaPrime vars, on Linux
  `STEAM_COMPAT_INSTALL_PATH=<installPath>`, `PROTONPATH=<settings.steamRuntime>` if set, user env
  options, `LD_PRELOAD=''` if not set.
* `setupWineEnvVars(settings, gameId)` (`launcher.ts:1110-1319`): `WINEPREFIX`,
  `WINEDLLOVERRIDES=...;winemenubuilder.exe=d`, proton vars
  (`STEAM_COMPAT_CLIENT_INSTALL_PATH`, `STEAM_COMPAT_DATA_PATH`, `PROTONPATH`, `STEAM_COMPAT_APP_ID`,
  `SteamAppId`, `SteamGameId=heroic-<gameId>`, ...), esync/fsync/FSR/wayland/NVAPI etc.
  Store code passes `gameInfo.folder_name` as `gameId` (via `prepareWineLaunch`,
  `launcher.ts:975`). Full details belong to the Wine spec.
* `getKnownFixesEnvVariables(appName, runner)`: env vars from the known-fixes DB.

### 1.9 `getWinePath({path, gameSettings, variant})` (`launcher.ts:1898-1934`)

Runs `cmd /c winepath -u|-w <path>` in the prefix (`protonVerb: 'runinprefix'`) and returns trimmed
stdout. Used by both stores to translate install paths.

---

## 2. Shared persistence primitives

### 2.1 `CacheStore` (`backend/cache.ts`)

An `electron-store` JSON file at `$CACHE/<name>.json` (`clearInvalidConfig: true`).

* `set(key, value)`: writes `key` and `__timestamp.<key>` = `Date()` string, e.g.
  `"Thu Oct 22 2026 15:26:59 GMT+0300 (Eastern European Summer Time)"` (`cache.ts:80-83`).
  electron-store treats dots as paths, so on disk this is
  `{"<key>": value, "__timestamp": {"<key>": "<date string>"}}`.
  **Keys containing dots are therefore nested** (e.g. an appName or password with `.`).
* `get(key, fallback)` (`cache.ts:49-78`): if a lifespan (minutes) is configured and the timestamp is
  missing or older than the lifespan, the entry is deleted and `fallback` returned.
* Default lifespan: **360 minutes (6 h)**; `null` = never expires.
* `use_in_memory()` / `commit()` (`cache.ts:42-47`, `96-102`): load a `Map` from the top-level entries,
  operate in memory, then write `store.store = Object.fromEntries(map)`. In memory, `set()` stores the
  literal key `"__timestamp.<key>"`, so after commit the file contains **flat literal keys**
  `"__timestamp.gog_123"` alongside the nested `"__timestamp"` object. Subsequent `get()` reads the
  nested path, i.e. the flat keys are ignored and the old nested timestamp remains authoritative.
  Observed in the real `gog_api_info.json`. A reimplementation must at least read both forms.

### 2.2 `TypeCheckedStoreBackend` (electron-store under `$HEROIC/<cwd>/<name>.json`)

Same dot-path semantics; no expiry.

### 2.3 Files owned by the Epic/GOG backends

| File | Store | Key(s) / shape | Lifespan |
|------|-------|----------------|----------|
| `$CACHE/legendary_install_info.json` | `installStore` | `{<appName>: LegendaryInstallInfo}` | 6 h |
| `$CACHE/legendary_library.json` | `libraryStore` | `{"library": GameInfo[]}` | never |
| `$CACHE/legendary_games_override.json` | `gamesOverrideStore` | `{"gamesOverride": GameOverride}` | 7 days (10080 min) |
| `$CACHE/legendary_gameinfo.json` | `gameInfoStore` | `{<namespace>: ExtraInfo}` | 6 h |
| `$CACHE/gog_api_info.json` | `apiInfoCache` | `{"gog_<id>": GamesDBData (+etag)}` | 6 h |
| `$CACHE/gog_library.json` | `libraryStore` | `{"games": GameInfo[]}` | never |
| `$CACHE/gog_achievements.json` | `achievementStore` | `{<appName>: GOGAchievement[]}` | never |
| `$CACHE/gog_install_info.json` | `installInfoStore` | `{"<app>_<platform>_<branch>_<build>_<password>": GogInstallInfo}` | 6 h |
| `$CACHE/gog_playtime_sync_queue.json` | `playtimeSyncQueue` | `{<user_id>: [{appName, session_date, time}], "lock": []}` | 6 h (!) |
| `$CACHE/umu.json` | `umuStore` | `{"<runner>_<appName>": umuId or null}` | generic |
| `$HEROIC/gog_store/config.json` | GOG `configStore` | `{isLoggedIn: bool, userData: UserData}` | — |
| `$HEROIC/gog_store/installed.json` | `installedGamesStore` | `{"installed": InstalledInfo[]}` | — |
| `$HEROIC/gog_store/saveTimestamps.json` | `syncStore` | `{<appName>: {<locationName>: "<ts>"}}` | — |
| `$HEROIC/gog_store/privateBranches.json` | `privateBranchesStore` | `{<appName>: "<password>"}` | — |
| `$HEROIC/gog_store/auth.json` | written by **gogdl** | `{"46899977096215655": {access_token, expires_in, token_type, scope, session_id, refresh_token, user_id, loginTime}}` | — |
| `$HEROIC/store/config.json` | global `configStore` | `userInfo` (Epic `UserInfo`), `language` | — |
| `$HEROIC/store/timestamp.json` | `tsStore` | `{<appName>: {firstPlayed, lastPlayed, totalPlayed}}` | — |
| `$LEGCFG/*` | written by **legendary** | see §4.1 | — |
| `$GOGDLCFG/manifests/<appName>` | written by **gogdl** and Heroic | gogdl manifest JSON (+`HGL*` keys) | — |
| `$GOGREDIST/.gogdl-redist-manifest` | written by **gogdl** | `{depots[], build_id, HGLInstalled[]}` | — |

Store definitions: `storeManagers/legendary/electronStores.ts:5-27`,
`storeManagers/gog/electronStores.ts:10-45`, `backend/constants/key_value_stores.ts`.

`clearCache(library)` (`backend/utils.ts:369-399`): always clears `wikigameinfo`; `gog` → clears
`gog_api_info`, `gog_library`, `gog_install_info`, `gog_achievements`; `legendary` → clears
`legendary_install_info`, `legendary_library`, `legendary_gameinfo` **and runs `legendary cleanup`**
(abortId `legandary-cleanup`); unless `fromVersionChange`, also clears the PCI-id caches.
`legendary_games_override` and `gog_playtime_sync_queue` are never cleared by this.

---

## 3. Shared launch pipeline (`launchEventCallback`, `backend/launcher.ts:102-328`)

Same for both stores; per-store `game.launch()` is called in the middle.

1. If `install.install_path` set but missing on disk → `askForceUninstall(game)` dialog, status
   `done`, return `{status:'abort'}`.
2. Load game settings. If no `launchArguments` passed and `lastUsedLaunchOption` set, use it.
3. `tsStore`: set `<app>.firstPlayed` (ISO) if app not yet present.
4. If `autoSyncSaves && isOnline()`: status `syncing-saves`;
   `game.syncSaves('--skip-upload', savesPath, gogSaves)` (download).
5. Status `launching`; hide window if `minimizeOnLaunch && !noTrayIcon`; power-save blocker.
6. Create game log writer. If not native: `checkWineBeforeLaunch` (abort with `error` if invalid).
7. Run the user "before launch" script.
8. Start `game.launch(logWriter, launchArguments, args, skipVersionCheck)`.
9. **GOG only**: `gogPresence.setCurrentGame(appName); await gogPresence.setPresence()` (after
   starting launch, concurrently with the game).
10. Await launch result; run the "after launch" script; close the log writer.
11. **GOG only**: `setCurrentGame(''); setPresence()`.
12. Update `tsStore` `lastPlayed`, `totalPlayed += floor(session minutes)`.
13. **GOG only**: unless `disablePlaytimeSync`, `updateGOGPlaytime(start, end)` (§5.21).
14. Add to recent games.
15. If `autoSyncSaves && isOnline()`: status `done`, then `syncing-saves`;
    `game.syncSaves('--skip-download', savesPath, gogSaves)` (upload).
16. Status `done`; `{status: launchResult ? 'done' : 'error'}`.

`prepareWineLaunch(game, logWriter)` (`launcher.ts:764-978`), store-relevant parts:

* For legendary on Linux, logs EOS overlay enabled state (§4.17).
* Maintains `<winePrefix>/installed_games` (JSON array of appNames). If the file is created or the
  appName is appended (= first launch of this game in this prefix), runs the store setup:
  * GOG → `gogSetup(appName)` (§5.18), then sends status `launching`;
  * legendary → `legendarySetup(appName, logWriter)` (§4.16);
  * then known-fixes installation (winetricks, `runInPrefix` executables).
* **GOG + comet support** (`launcher.ts:895-944`): ensure
  `C:\ProgramData\GOG.com\Galaxy\redists\GalaxyCommunication.exe` exists in the prefix (copy bundled
  `<resources>/app.asar.unpacked/build/bin/x64/win32/GalaxyCommunication.exe` (`galaxyCommunicationExePath`, `paths.ts:58-60`); then `wine sc create GalaxyCommunication binpath=<winpath>`),
  and always run
  `wine reg add HKLM\SOFTWARE\WOW6432Node\GOG.com\GalaxyClient\paths /v client /t REG_SZ /d "C:\Program Files\GOG Galaxy" /f`.
* DXVK/VKD3D/EAC/BattlEye runtime installation; returns
  `envVars = setupWineEnvVars(settings, gameInfo.folder_name)`.

---

## 4. Epic Games Store (runner id `legendary`)

Source: `backend/storeManagers/legendary/{library.ts,games.ts,user.ts,setup.ts,thirdParty.ts,constants.ts,electronStores.ts,eos_overlay/*,commands/*}`.

### 4.1 Legendary config directory (`$LEGCFG`)

Every legendary invocation sets `LEGENDARY_CONFIG_PATH=$LEGCFG` (`legendary/library.ts:694-698`) so
the user's own `~/.config/legendary` is never touched. Files legendary maintains there and which
Heroic reads directly:

| Path | Written by | Read by Heroic for |
|------|-----------|--------------------|
| `user.json` | `legendary auth` | logged-in check (exists?), `account_id`, `displayName` |
| `installed.json` | install/import/uninstall/move/sync-saves | installed games (`{<app_name>: InstalledJsonMetadata}`) |
| `assets.json` | `legendary list` | update check (`{<platform>: [{app_name, build_version, ...}]}`) |
| `metadata/<app_name>.json` | `legendary list` | library (`GameMetadata`, see §4.5) |
| `third-party-installed.json` | **Heroic** | EA/Ubisoft "installed" list: `[[appName, platform], ...]` |
| `overlay_install.json` | `legendary eos-overlay install` | EOS overlay installed? `{version, install_path}` |
| `overlay_version.json` | `legendary status` | latest overlay version `.data.buildVersion` |
| `config.ini` | legendary | not read (default: `[Legendary] disable_update_check=false, disable_update_notice=true`) |
| `manifests/`, `tmp/` | legendary | not read |

`InstalledJsonMetadata` (`common/types/legendary.ts:14-33`): `app_name, base_urls[], can_run_offline,
egl_guid, executable, install_path, install_size (bytes, number), install_tags[], is_dlc,
launch_parameters, manifest_path?, needs_verification, platform ('Windows'|'Win32'|'Mac'),
prereq_info, requires_ot, save_path?, title, version`.

`user.json` keys (legendary-owned OAuth session; **never log values**): `access_token`,
`expires_in`, `expires_at`, `token_type`, `refresh_token`, `refresh_expires`, `refresh_expires_at`,
`account_id`, `client_id`, `internal_client`, `client_service`, `displayName`, `app`, `in_app_id`,
`device_id`, ... Heroic reads only `account_id` and `displayName`.

### 4.2 Command → argv mapping (`legendary/library.ts:823-876`)

Heroic models commands as an object; translation rules (must be reproduced to get identical argv):

1. `argv = [subcommand]`.
2. Positional handling by subcommand:
   * `install`: push `appName`; if `sdlList` is present (non-empty array was set) push
     `--install-tag=` (literal, empty tag = "untagged base files"), then for each tag
     `--install-tag`, `<tag>`.
   * `launch`: push `appName`, then `shlex.split(extraArguments)` if non-empty.
   * `update`, `info`, `sync-saves`, `uninstall`, `repair`: push `appName`.
   * `move`: push `appName`, `newBasePath`.
   * `eos-overlay`: push `action` (`install|update|remove|enable|disable|info`).
   * `import`: push `appName`, `installationDirectory`.
3. Then, **in object insertion order**, every key starting with `-` whose value is truthy:
   `true` → push key only; otherwise push key and `String(value)`.

Global flags (`-y`, `-v`, ...) therefore appear *after* the subcommand; Heroic's legendary fork
strips `-y`/`--yes` from the unknown extra args and honours them (`legendary/cli.py:3161-3163`).

`runRunnerCommand(command, options)` (`library.ts:684-707`): sets
`options.env.LEGENDARY_CONFIG_PATH`, converts to argv and calls `callRunner` with runner name
`legendary`.

### 4.3 Authentication

#### Login (`legendary/user.ts:15-53`; IPC `login`, `main.ts:789`)

Two ways the frontend obtains an **authorization code**:

* Embedded webview (`frontend/screens/WebView/index.tsx:78,185-243`): load
  `https://www.epicgames.com/id/login?responseType=code` with user agent
  `Mozilla/5.0 (Windows NT 10.0; Win64; x64) EpicGamesLauncher`, partition `persist:epic`. On every
  `dom-ready`, if the current URL's hostname is `localhost`, read query param `code` and call
  `login(code)`. (Epic redirects to `https://localhost/launcher/authorized?code=...` for the EGL UA.)
* Manual ("SID login", `frontend/screens/Login/components/SIDLogin/index.tsx`): user opens
  `https://legendary.gl/epiclogin` (also `backend/constants/urls.ts:1`, IPC `openLoginPage`), copies
  the `authorizationCode` value from the JSON shown and pastes it.

Backend:

```
legendary auth --code <authorizationCode>          (abortId 'legendary-login', prefix 'Logging in')
env LEGENDARY_CONFIG_PATH=$LEGCFG
```

* legendary exchanges the code at
  `https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/token`
  (basic auth client `34a02cf8f4414e29b15921876da36f9a`, UA `UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit`)
  and writes `user.json`.
* Failure if `stderr` contains `ERROR: Logging in ` (legendary prints
  `[cli] ERROR: Logging in failed with ..., please try again.`), or `res.error`/`res.abort`.
  Returns `{status:'failed', data: undefined}`.
* Success → `getUserInfo()` → `{status:'done', data: UserInfo}`.

#### User info (`user.ts:85-107`; IPC `getUserInfo`)

* `isLoggedIn()` = `existsSync($LEGCFG/user.json)` (`user.ts:81-83`). IPC `isLoggedIn`.
* If not logged in: delete `configStore.userInfo`, return undefined.
* Else parse `user.json` → `UserInfo = {account_id, displayName, user: os.userInfo().username}`;
  store it in global `configStore` key `userInfo`; return it. Parse error → log
  `User info file corrupted, check <path>` and return undefined.
* Startup (`main.ts:345-354`): once online, if not logged in, delete `configStore.userInfo`.

#### Logout (`user.ts:55-79`; IPC `logoutLegendary`)

```
legendary auth --delete            (abortId 'legendary-logout', prefix 'Logging out')
```

On error/abort: log and stop. Otherwise clear Electron session partition `persist:epic` (storage,
cache, auth cache, host resolver cache, data), delete `configStore.userInfo`,
`clearCache('legendary')` (which also runs `legendary cleanup`).

### 4.4 Library manager state

In-memory module state (`library.ts:57-59`):

* `allGames: Set<string>` — appNames from `$LEGCFG/metadata/*.json` filenames (strip last `.json`
  extension only: `filename.split('.').slice(0,-1).join('.')`) (`library.ts:83-95`).
* `installedGames: Map<appName, InstalledJsonMetadata>` — from `installed.json` plus third-party
  entries (`library.ts:131-156`).
* `library: Map<appName, GameInfo>`.

`init()` (`library.ts:64-67`): `loadGamesInAccount()` + `refreshInstalled()` (no network).

`refreshInstalled()`: parse `installed.json` (on parse error log
`Corrupted installed.json file, cannot load installed games` and treat as empty); append
`thirdParty.getInstalledGames()` (each `[app_name, platform]` becomes
`{app_name, platform}` with no other fields); rebuild the map.

### 4.5 Library refresh (`library.ts:100-190`; IPC `refreshLibrary` with `legendary` or `all`)

```
refresh():
  if !isLoggedIn(): return {stdout:'', stderr:''}
  refreshLegendary():
     if isEpicServiceOffline(): warn, return (skip CLI)
     legendary list --third-party        (abortId 'legendary-refresh')
     refreshInstalled()
  loadGamesInAccount(); refreshInstalled()
  loadAll(): for each appName in allGames: loadFile(appName)   (exceptions caught & logged once)
  libraryStore.set('library', [...library.values()])
```

`legendary list --third-party` downloads/refreshes `metadata/<app>.json` and `assets.json`
(including non-installable third-party EA/Ubisoft titles).

#### `isEpicServiceOffline(type='Epic Games Store')` (`backend/utils.ts:186-228`)

* If Heroic is offline → `true`.
* `GET https://status.epicgames.com/api/v2/components.json` (axios, 10 s timeout); find the component
  whose `name === type`; offline iff `status === 'major'` (then shows an OS notification
  "<type> offline"). If no component matches, shows the notification anyway and returns `false`.
  Request errors → `false`.

#### Metadata file shape (`common/types/legendary.ts:36-133`)

`GameMetadata = {app_name, app_title, asset_infos: {<platform>: {app_name, asset_id, build_version, catalog_item_id, label_name, metadata, namespace}}, base_urls[], metadata: GameMetadataInner}`

`GameMetadataInner` fields used: `namespace`, `categories[{path}]`, `description`,
`shortDescription?`, `keyImages[{type,url,...}]`, `title`, `developer`, `dlcItemList?`
(array of `GameMetadataInner`), `releaseInfo[{appId, id, platform?[]}]`, `customAttributes?`
(`{<name>: {type:'STRING', value}}`), `mainGameItem?`.

#### GameInfo construction — `loadFile(app_name)` (`library.ts:485-656`)

Return `false` (game omitted) when:

* metadata file cannot be parsed;
* UE asset: `namespace === 'ue'` or any `categories[].path` in
  `['assets','asset-format','plugins','projects']`;
* any category path `=== 'mods'`;
* every `releaseInfo[i].platform` consists only of `Android`/`iOS`
  (note `[].every` is true, so an empty `releaseInfo` is also skipped; `platform` undefined → that
  entry counts as "not mobile").

Derived values:

| Variable | Rule |
|----------|------|
| `canRunOffline` | `customAttributes.CanRunOffline.value === 'true'` |
| `thirdPartyManagedApp` | `customAttributes.ThirdPartyManagedApp.value` \|\| `ThirdPartyManagedProvider.value` \|\| undefined |
| `info` | `installedGames.get(app_name)` |
| `saveFolder` | if installed platform `=== 'Mac'`: `CloudSaveFolder_MAC.value`, else `CloudSaveFolder.value`; default `''` |
| `installFolder` | `customAttributes.FolderName.value` or `app_name` |
| `art_cover` raw | first keyImage with type `DieselGameBox` or `OfferImageWide` |
| `art_square` raw | first keyImage with type `DieselGameBoxTall` or `OfferImageTall` |
| `art_square_front` | first `DieselStoreFrontTall` |
| `art_logo` | first `DieselGameBoxLogo` |
| `is_dlc` | `Boolean(metadata.mainGameItem)` |
| `install_size` | `getFileSize(Number(info.install_size))` (filesize v10, base 2: e.g. `"75.85 GiB"`), `'0'` if not installed |
| `metadataPlatform` | `releaseInfo[0].platform`, else `mainGameItem.releaseInfo[0].platform`, else `[]` |

Resulting `GameInfo` (exact key set):

```jsonc
{
  "app_name": app_name,
  "art_cover":  art_cover || art_square || "fallback",
  "art_logo":   art_logo,                              // may be undefined
  "art_square": art_square || art_square_front || art_cover || "fallback",
  "cloud_save_enabled": Boolean(saveFolder),
  "developer": metadata.developer,
  "extra": { "about": { "description": metadata.description, "shortDescription": metadata.shortDescription ?? "" },
             "reqs": [], "storeUrl": formatEpicStoreUrl(title) },
  "dlcList": metadata.dlcItemList,                    // raw metadata objects
  "folder_name": installFolder,
  "install": { "executable", "install_path", "install_size": <string>, "is_dlc", "version", "platform" },  // from installed.json
  "is_installed": info !== undefined,
  "namespace": metadata.namespace,
  "is_mac_native": info ? info.platform === "Mac" : metadataPlatform.includes("Mac"),
  "save_folder": saveFolder,
  "save_path": info.save_path,
  "title": metadata.title,
  "canRunOffline": canRunOffline,
  "thirdPartyManagedApp": thirdPartyManagedApp,
  "isEAManaged": !!tpma && ["origin","the ea app"].includes(tpma.toLowerCase()),
  "isUbisoftManaged": !!tpma && tpma.toLowerCase() == "ubisoftconnect",
  "is_linux_native": false,
  "runner": "legendary",
  "store_url": formatEpicStoreUrl(title)
}
```

The literal string `"fallback"` is used as image placeholder (frontend maps it to a default image).

`formatEpicStoreUrl(title)` (`backend/utils.ts:593-606`):
`"https://www.epicgames.com/store/product/" + cleanTitle(title)`, where
`cleanTitle = title.replaceAll(/('\w)|(\\(\w|\d){5})|(\\"(\\.|[^"])*")|[^((0-9)|(a-z)|(A-Z)|\s)]/g, '').replaceAll(' ', '-').replaceAll('®','').toLowerCase().split('--definitive')[0]`.

Accessors:

* `getGameInfo(appName, forceReload=false)` (`library.ts:203-216`): if `!allGames.has(appName)` →
  warn, undefined. If not yet in `library` or `forceReload` → `loadFile`. The IPC `getGameInfo` /
  `getExtraInfo` returns `null` early for legendary if `!hasGame(appName)` (`main.ts:707-741`).
* `LegendaryGame.getGameInfo()` (`games.ts:91-114`): returns an "empty" GameInfo
  `{app_name:'', runner:'legendary', art_cover:'', art_square:'', install:{}, is_installed:false, title:'', canRunOffline:false}`
  if missing.
* `getListOfGames()` = `libraryStore.get('library', [])`.
* `installState(appName, true)` → `loadFile`; `false` → set `is_installed=false`, `install={}`,
  remove from `installedGames` (`library.ts:460-472`).

### 4.6 Install info (`library.ts:221-277`; IPC `getInstallInfo`)

```
getInstallInfo(appName, installPlatform, {retries}):
  cached = installStore.get(appName); if cached?.manifest: return cached   // key is appName only! (platform ignored)
  cmd: legendary info <appName> --json --platform <Windows|Win32|Mac> [--offline]   (abortId appName)
       --offline added iff isEpicServiceOffline()
  info = JSON.parse(stdout)
  if info.manifest: installStore.set(appName, info); return info
  else retry up to 3 times total (retries undefined → 3, then 2, 1) ; then throw
  JSON parse failure → throw "Failed to parse install info for <app> with: <err>"
```

`installPlatform` must parse as `'Win32'|'Windows'|'Mac'` (zod) — other values throw. IPC handler
catches errors and returns `null`.

`LegendaryInstallInfo` (`common/types/legendary.ts:136-195`):
`game: {app_name, title, version, cloud_save_folder?, cloud_save_folder_mac?, cloud_saves_supported, external_activation, is_dlc, launch_options: LaunchOption[], owned_dlc: [{app_name, title, is_installed?}], platform_versions}`,
`manifest: {app_name, build_id, build_version, disk_size, download_size, feature_level, install_tags[], launch_command, launch_exe, num_chunks, num_files, prerequisites?: {args, ids[], name, path}, size, tag_disk_size[{tag,count,size}], tag_download_size[...], type, version}`.

Download-queue usage: `size = getFileSize(manifest.download_size)` or `'?? MB'`; EA/Ubisoft managed
games skip `getInstallInfo` entirely and use `'?? MB'` (`downloadmanager/downloadqueue.ts:144-181`).

#### Selective downloads (SDL) (`library.ts:731-766`; IPC `getGameSdl`)

```
GET https://heroic.legendary.gl/v1/sdl/<appName>.json
if response header content-type !== 'application/json' (exact string compare): return []
For each key in response (object of {name, description, tags[]}):
   key '__required' → unshift {name, description, tags, required:true}
   else push {name, description, tags}
errors → []
```

Frontend computes `sdlList` = concatenation of `tags` of every SDL entry that is `required` or selected
(`frontend/screens/Library/components/InstallModal/DownloadDialog/index.tsx:161-173`); empty strings
are filtered out by the backend (`downloadmanager/utils.ts:84`).

#### Game overrides (`library.ts:709-729`; IPC `getGameOverride`)

`GET https://heroic.legendary.gl/v1/version.json` → `response.data.game_overrides`
(`{executable_override, reorder_optimization, sdl_config}`) cached 7 days under `gamesOverride`.
Errors are rethrown.

### 4.7 Install (`games.ts:574-660`)

Called from the download queue (`downloadmanager/utils.ts:12-106`). Queue pre-checks: online, and
for legendary `isEpicServiceOffline()` false (else dialog "Epic Servers are having major outage
right now, the game cannot be installed!" and status error). Status `installing` with `folder: path`
is sent before, `done` after.

Third-party managed titles are diverted (§4.8). Otherwise:

```
info = getInstallInfo(appName, platformToInstall)          // for totalDownloadSize
argv: legendary install <appName>
        [--install-tag= --install-tag T1 --install-tag T2 ...]     (only if sdlList non-empty)
        --platform <Windows|Win32|Mac>
        --base-path <path>                                          (install parent dir; legendary appends folder)
        --skip-dlcs
        -y
        [--max-workers <maxWorkers>]                                (if setting truthy)
        [--no-https]                                                (if downloadNoHttps)
        [--skip-sdl]                                                (only if sdlList empty/absent)
options: abortId=appName, logWriters=[install log], onOutput=progress parser ('installing', info.manifest.download_size),
         logMessagePrefix "Installing <appName>"
```

* If `stderr` contains `MemoryError:` → rerun the same command with `--max-shared-memory 5000`
  appended (same onOutput, no prefix).
* `abort` → `{status:'abort'}`; `error` → `{status:'error', error}` (logs unless error contains
  `signal`).
* Success → `addShortcuts()` (desktop/menu shortcuts; generic), `refreshInstalled()`,
  `{status:'done'}`. (The frontend refreshes the library afterwards.)

Epic DLCs are separate library entries; the base install always passes `--skip-dlcs`. DLCs are
installed by queueing an install for the DLC's own appName.

### 4.8 EA / Ubisoft (third-party managed) titles

`install` (`games.ts:662-739`):

* EA (`isEAManaged`): ensure `$EPICREDIST/EAappInstaller.exe` exists, else download from
  `https://origin-a.akamaihd.net/EA-Desktop-Client-Download/installer-releases/EAappInstaller.exe`.
  [non-Linux] On Windows run it with `EAX_LAUNCH_CLIENT=0 IGNORE_INSTALLED=1` (exit code 3 → abort).
* Ubisoft (`isUbisoftManaged`): ensure `$EPICREDIST/UbisoftConnectInstaller.exe`, download from
  `https://static3.cdn.ubi.com/orbit/launcher_installer/UbisoftConnectInstaller.exe`.
  [non-Linux] run with `/S`.
* Any other provider → log "Third party app <x> not supported", `{status:'error'}`.
* Then `thirdParty.addInstalledGame(app_name, platformToInstall)` appends `[appName, platform]` to
  `third-party-installed.json` (creating it) → `{status:'done'}`.
* On Linux the actual installer runs at first launch in the prefix (§4.16).

`uninstall` for third-party: `thirdParty.removeInstalledGame(appName)` — reads file, finds index,
`splice(index, 1)` (**quirk:** if not found, index −1 removes the last element), writes back;
returns `{stdout:'', stderr:''}`.

Launch adds `--origin` (EA) / `--ubisoft` (Ubisoft); `getLaunchOptions` returns `[]` for them.

### 4.9 Download progress parsing (`games.ts:399-486`)

State per game: `currentDownloadSize` (MiB, float) and `tmpProgress =
{bytes:'', eta:'', percent:undefined, diskSpeed:undefined, downSpeed:undefined}`.

Each output chunk (stdout or stderr) is processed:

| Field | Regex (JS) | Update rule |
|-------|-----------|-------------|
| download size | `/Download size: ([\d.]+) MiB/` | always: `currentDownloadSize = parseFloat($1)` |
| `eta` | `/ETA: (\d\d:\d\d:\d\d)/m` | only if `eta === ''` |
| `bytes` | `/Downloaded: (\S+.) MiB/m` | only if `bytes === ''`; value = `$1 + "MB"` |
| `downSpeed` | `/Download\t- (\S+.) MiB/m` | only if falsy; `Number($1)` or undefined if NaN/no match |
| `diskSpeed` | `/Disk\t- (\S+.) MiB/m` | only if falsy; same |

(`\t` is a literal TAB; legendary prints `" + Download\t- 25.06 MiB/s (raw) / ..."`,
`" - Downloaded: 120.25 MiB, Written: ..."`,
`"= Progress: 0.52% (99/18936), Running for 00:00:05, ETA: 00:15:30"`,
`"Download size: 1234.56 MiB (Compression savings: ...)"` — all on stderr via logging.)

Percent:

```
totalMiB = round(totalDownloadSizeBytes / 1024 / 1024 * 100) / 100
if bytes != '':
   downloaded = parseFloat(bytes)
   cache = totalMiB - (currentDownloadSize ?? 0)          // already-downloaded part when resuming
   percent = round((downloaded + cache) / totalMiB * 10000) / 100
   percent = percent >= 0 ? percent : undefined
```

When **all five** fields are non-empty/non-undefined: log
`Progress for <title>: <percent>%/<bytes>/<eta> Down: <down>MB/s / Disk: <disk>MB/s`, emit
`progressUpdate` `{appName, runner:'legendary', status: 'installing'|'updating', progress:{bytes, eta, percent, downSpeed, diskSpeed}}`
(also `backendEvents` `progressUpdate-<appName>`), then reset `tmpProgress`.

### 4.10 Update (`games.ts:492-548`) and update detection (`library.ts:284-405`)

Update (queue `updateQueueElement`, `downloadmanager/utils.ts:108-180`, same online/Epic checks):

```
status 'updating'
info = getInstallInfo(appName, gameInfo.install.platform)
legendary update <appName> -y --skip-sdl [--max-workers N] [--no-https]
   abortId appName, update log writer, onOutput → progress ('updating', info.manifest.download_size),
   prefix "Updating <appName>"
status 'done'; error → {status:'error'} else {status:'done'}   (abort counts as done)
```

`listUpdateableGames()` (IPC `checkGameUpdates`, which iterates all stores and optionally auto-queues):

1. Return `[]` if not logged in, offline, or Epic offline.
2. `legendary list --third-party` (abortId `legendary-check-updates`, prefix
   "Checking for game updates"); abort/error → `[]`.
3. Read `installed.json` → `{app: {version, platform}}`; read `assets.json`.
4. For each `[platform, assets]` in `assets.json`, for each installed game with
   `installed.platform === platform`: find asset with `app_name === app`; if missing warn
   "installed but was not found on account"; if `asset.build_version !== installed.version` → update.

Auto-update (`storeManagers/index.ts:42-60`): for each candidate, if `!settings.ignoreGameUpdates`
and `isGameAvailable()`, push a queue element `{type:'update', params:{appName, gameInfo, runner, path: install_path, platformToInstall: install.platform}}`.

### 4.11 Repair / verify (`games.ts:777-807`; IPC `repair`, `main.ts:898-932`)

IPC: skip if offline; status `repairing`; on finish notify and status `done`.

```
legendary repair <appName> -y --skip-sdl [--max-workers N] [--no-https]
   abortId appName, repair log writer, prefix "Repairing <appName>"
```

No progress parsing for repair. Returns the ExecResult.

### 4.12 Uninstall (`games.ts:741-771`; IPC `uninstall` → `utils/uninstaller.ts:82-128`)

```
legendary uninstall <appName> -y          (abortId appName, prefix "Uninstalling <appName>")
on success (no error, no abort): installState(app,false); remove shortcuts; remove Steam non-steam shortcut
always: frontend message 'refreshLibrary' with 'legendary'
```

Generic wrapper afterwards: if `shouldRemovePrefix` remove the prefix (with safety checks: never the
default install dir, shared prefix, default prefix, or a non-empty dir without `drive_c`/`pfx`); if
`shouldRemoveSetting` delete `$GAMESCFG/<app>.json`, `<app>.log`, `<app>-lastPlay.log`.

`forceUninstall()` (`games.ts:1056-1081`, used when the folder vanished):
`legendary uninstall <appName> -y --keep-files`, then frontend `refreshLibrary('legendary')`.

### 4.13 Move (`games.ts:353-377`, `library.ts:415-452`; IPC `moveInstall`, `main.ts:934-1001`)

IPC rejects a destination that starts with the current install path. Then:

1. `moveOnUnix(newParent, gameInfo)` (`backend/utils.ts:1092-1206`):
   destination = `join(newParent, basename(install_path))`.
   If `rsync` exists: `rsync --archive --compress --no-human-readable --remove-source-files --info=name,progress <install_path>/ <destination>`;
   progress lines parsed (`/^\s+(\d+)/` bytes, `/(\d+:\d{2}:\d{2})/` eta, `/(\d+)%/` percent; a
   line without `%`, `:` and not starting with space is a filename) and sent as `progressUpdate`
   status `moving` with `{percent, eta, bytes, file}`; on `code !== 1` success → `rm -rf <old>`.
   Else `mv -f <install_path> <destination>`. Exit code 1 = error.
2. `changeGameInstallPath(appName, destination)`: update in-memory `library` and `installedGames`
   install_path, then
   `legendary move <appName> <dirname(destination)> --skip-move` (abortId appName) so legendary
   rewrites `installed.json`.

IPC `changeInstallPath` (no file move) calls only step 2.

### 4.14 Import (`games.ts:809-842`; IPC `importGame`, `main.ts:1003-1088`)

IPC: legendary → refuse if Epic offline (dialog). Status `importing`.

```
legendary import <appName> <folderPath> --with-dlcs --platform <Windows|Win32|Mac>
   abortId appName, import log writer
addShortcuts()            (always, even on failure)
res.error = (res.error ?? '') + all stderr lines matching /^.*ERROR:.*$/gm joined with ''
```

IPC treats `abort || error` as failure (notification "Importing Failed", returns `{status:'done'}`).
On success optionally writes `winePrefix/wineVersion/wineCrossoverBottle` into the game config.

### 4.15 Cloud saves

#### Sync (`games.ts:848-879`; IPC `syncSaves`, `main.ts:1137-1158`)

IPC: legendary → refuse if Epic offline; refuse if Heroic offline (returns the message string).

```
if !path: return 'No path provided.'
legendary sync-saves <appName> <arg> --save-path <path> -y
   arg ∈ {'--skip-upload' (download), '--skip-download' (upload), '--force-download', '--force-upload'}
   (an empty arg is simply omitted)
   abortId appName, prefix "Syncing saves for <title>", onOutput accumulates ALL output (stdout+stderr)
return accumulated output string
```

#### Default save path (`backend/save_sync.ts:36-99`; IPC `getDefaultSavePath`)

1. If `gameInfo.save_path` (from installed.json) is set: rewrite `installed.json` setting
   `installed[appName].save_path = null` (pretty-printed with 2 spaces). On failure return the old
   save_path.
2. If not native: `verifyWinePrefix(settings)`.
3. `legendary sync-saves <appName> --skip-upload --skip-download --accept-path`
   (abortId `<appName>-savePath`, env = `setupWineEnvVars(settings)` when not native) — legendary
   resolves the `CloudSaveFolder` variables inside the Wine prefix and stores `save_path` in
   `installed.json`.
4. `getGameInfo(appName, forceReload=true).save_path` → returned (or `''` + error log).

### 4.16 First-launch prefix setup (`legendary/setup.ts:13-122`)

Runs when the game is first registered in the prefix's `installed_games` (see §3):

1. Status `redist` with context `EPIC`.
2. `wine reg add HKEY_CLASSES_ROOT\com.epicgames.launcher /f` (wait, protonVerb `run`).
3. If install platform ∈ `Windows|Win32|windows` and not EA/Ubisoft managed:
   `getInstallInfo(...)`; if `manifest.prerequisites.path` non-empty, run
   `<install_path>/<prerequisites.path> <shlex.split(prerequisites.args)>` in Wine (wait).
4. EA managed: run `$EPICREDIST/EAappInstaller.exe EAX_LAUNCH_CLIENT=0 IGNORE_INSTALLED=1` in Wine
   (wait). Ubisoft: `$EPICREDIST/UbisoftConnectInstaller.exe /S`.
5. Linux only: if EOS overlay not enabled for this game's prefix and the overlay is installed →
   enable it (§4.17), logging `EOS Overlay: Enabling` / `EOS Overlay: Not Installed`.

### 4.17 EOS overlay (`legendary/eos_overlay/eos_overlay.ts`)

Constants: overlay appName `98bc04bc842e4906993fd6d6644ffb8d` (used as abortId and as a pseudo game
for progress), install dir `$TOOLS/eos_overlay`.

| Function | Behaviour |
|----------|-----------|
| `isInstalled()` | `existsSync($LEGCFG/overlay_install.json)` |
| `getStatus()` | `{isInstalled:false}` or `{isInstalled:true, version, install_path}` from that file (warn if install_path ≠ default) |
| `updateInfo()` | if installed: `legendary status` (writes `overlay_version.json`) |
| `getLatestVersion()` | `''` if `overlay_version.json` missing and not installed; else run updateInfo if missing; return `.data.buildVersion` |
| `install()` | status `installing`/`updating`; run `legendary eos-overlay install --path $TOOLS/eos_overlay` **without -y**, parse `Download size: ([\d.]+) MiB` from output, convert to bytes, immediately abort the process; then `legendary eos-overlay install -y --path $TOOLS/eos_overlay` with progress parsing via `onInstallOrUpdateOutput('installing', out, downloadSize)` on the overlay pseudo-game; status `done`; return error |
| `remove()` | confirmation dialog; `legendary eos-overlay remove -y` |
| `enable(appName)` | if not installed: dialog offering install → `{wasEnabled:false, installNow}`; prefix = `getWinePrefixFolder(app)`; `false` → `{wasEnabled:false}`; `legendary eos-overlay enable [--prefix <prefix>]` → `{wasEnabled:true}` |
| `disable(appName)` | `legendary eos-overlay disable [--prefix <prefix>]` |
| `isEnabled(appName?)` | `legendary eos-overlay info [--prefix <prefix>]`; on output chunk containing `Overlay enabled`, enabled = chunk contains `Yes`, then abort the process |

`getWinePrefixFolder(appName)`: `null` if not Linux or no appName; else
`winePrefix` (or `winePrefix/pfx` when `wineVersion.type === 'proton'`) if it contains `user.reg`,
otherwise `false`.

IPC names: `getEosOverlayStatus`, `getLatestEosOverlayVersion`, `updateEosOverlayInfo`,
`installEosOverlay`, `removeEosOverlay`, `enableEosOverlay`, `disableEosOverlay`,
`isEosOverlayEnabled`.

### 4.18 Launch (`games.ts:881-1040`)

```
settings = game settings; gameInfo
prep = prepareLaunch(settings, logWriter, gameInfo, isNative())     // abort with dialog on failure
languageCode = settings.language || configStore.get('language','')
env = {...process.env, ...setupWrapperEnvVars, ...setupEnvVars(settings, install_path), ...knownFixesEnv}

if env.USE_FAKE_EPIC_EXE not in (undefined,'0','false') and bundled EpicGamesLauncher.exe exists:
    copy <resources>/app.asar.unpacked/build/bin/x64/win32/EpicGamesLauncher.exe (fakeEpicExePath) into the prefix at C:\windows\command\EpicGamesLauncher.exe (if missing)
    env.LEGENDARY_WRAPPER_EXE = 'C:\windows\command\EpicGamesLauncher.exe'

wrappers = setupWrappers(settings, mangohud, gamemode, gamescope, steamRuntime)
wineFlags = wrappers.length ? {'--wrapper': shlex.join(wrappers)} : {}
if !isNative():                          // always true on Linux for Epic
    wp = prepareWineLaunch(game, logWriter)   // abort with dialog on failure
    env = {...env, ...wp.envVars}
    if isUmuSupported(settings): umuId = getUmuId(app_name,'legendary'); if umuId: env.GAMEID = umuId
    wineFlags = getWineFlags(settings, shlex.join(wrappers))

appToLaunch = launchArguments.type === 'dlc' ? launchArguments.dlcAppName : appName
basicParams = (launchArguments.type undefined or 'basic') ? launchArguments.parameters : undefined
```

argv (order exactly as produced by §4.2):

```
legendary launch <appToLaunch> <shlex.split([...args, basicParams, settings.launcherArgs].filter(Boolean).join(' '))...>
    <wineFlags in order: --wine <bin> --wrapper <w>   |   --no-wine --wrapper <w>>
    [--skip-version-check]          (if skipVersionCheck param, or CLI --no-gui mode)
    [--language <languageCode>]
    [--override-exe <path>]         (launchArguments.type==='altExe' → its executable; else settings.targetExe)
    [--offline]                     (prep.offlineMode)
    [--origin]                      (isEAManaged)
    [--ubisoft]                     (isUbisoftManaged)
options: abortId appName, env, logMessagePrefix "Launching <title>",
         logWriters = settings.verboseLogs ? [gameLog] : []
```

Status `playing` is sent before spawning. After exit: `launchCleanup(rpcClient)`; return `!error`.

`getUmuId(appName, runner)` (`backend/wiki_game_info/umu/utils.ts`): cached in `umu.json` key
`<runner>_<appName>`; else
`GET https://umu.openwinecomponents.org/umu_api.php?codename=<appName lowercase>&store=egs`
(`gog` for GOG) → first item's `umu_id`, cache `null` if empty.

`isNative()` (`games.ts:1042-1054`): Windows → true; macOS with platform `Mac` → true; else false
(**always false on Linux**).

`stop(stopWine=true)` (`games.ts:1085-1097`): `pkill -f <appName>` (Linux/mac), then if not native
and stopWine → `shutdownWine` (`wineserver -k` with `WINEPREFIX`, or `wineboot -k` via Wine).

`isGameAvailable()`: installed and `install_path` exists.

### 4.19 Launch options (`library.ts:878-931`; IPC `getLaunchOptions`)

* No install platform or third-party managed → `[]`.
* `info.game.launch_options` from `getInstallInfo` (legendary returns
  `[{name, parameters}]` from `extraLaunchOption_*` attributes).
* For each `owned_dlc` that is installed (`installedGames`):
  if its installed entry has `executable` → push `{type:'dlc', dlcAppName, dlcTitle}`;
  else if its metadata has `customAttributes.AdditionalCommandLine` → push
  `{type:'basic', name: dlc.title, parameters: <value>}`.
* IPC wrapper (`main.ts:1111-1135`): if list non-empty and has no basic option named `Default` with
  empty parameters, prepend `{name: t('launch.default','Default'), parameters:'', type:'basic'}`.

### 4.20 Extra info / store page data (`games.ts:116-334`; IPC `getExtraInfo`)

```
if namespace undefined → empty ExtraInfo {about:{description:'',shortDescription:''}, reqs:[], storeUrl:''}
cached = gameInfoStore.get(namespace) → return
offline → empty
slug = getProductSlug(namespace, title):
   POST https://launcher.store.epicgames.com/graphql
     headers: Content-Type: application/json,
              User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) EpicGamesLauncher
     body: {"query": "{ Catalog { catalogNs(namespace: \"<ns>\") { mappings (pageType: \"productHome\") { pageSlug pageType } } } }"}
   → data.data.Catalog.catalogNs.mappings.find(pageType==='productHome').pageSlug
   else / on error → title.toLowerCase().replace(/[^a-z ]/g,'').replaceAll(' ','-')
lang = configStore.language; 'pt'→'pt-BR', 'zh_Hans'→'zh-CN', 'es'→'es-ES'
GET https://store-content.ak.epicgames.com/api/<lang>/content/products/<slug>
   about = data.pages.find(type==='productHome')
   → {about: about.data.about, reqs: about.data.requirements.systems[0].details,
      releaseDate: about.data.meta.releaseDate?.substring(0,19), storeUrl: https://www.epicgames.com/store/product/<slug>}
if null → POST graphql:
   { Product { sandbox(sandboxId: "<ns>") { configuration { ... on StoreConfiguration { configs {
       shortDescription technicalRequirements { macos { minimum recommended title } windows { minimum recommended title } } } } } } } }
   → {about:{description: configs.shortDescription, shortDescription:''}, reqs: technicalRequirements.windows, storeUrl}
if data: gameInfoStore.set(namespace, data)
```

### 4.21 EGS sync (`library.ts:773-817`; IPC `egsSync`)

```
arg === 'unlink' → legendary egl-sync -y --unlink
else             → legendary egl-sync -y --enable-sync --egl-wine-prefix <arg>   (arg must parse as a path; else return 'Error')
abortId 'toggle-sync'
error → 'Error'; stderr contains 'ERROR' or 'error' → 'Error'; else "<stdout> - <stderr>"
```

### 4.22 Other Epic odds and ends

* `changeVersionPinnedStatus` is a no-op (warning).
* `getSettings()` = `GameConfig.get(app).config || await GameConfig.get(app).getSettings()`.
* `clearCache('legendary')` triggers `legendary cleanup`.
* Main-process exit (`backend/utils.ts:240-280`): if operations pending and user confirms,
  `pkill -f legendary`, `pkill -f gogdl`, `pkill -f nile`, abort all controllers.

### 4.23 Epic quirks to preserve (or consciously fix)

1. `installStore` is keyed by appName only — install info for one platform is returned for another
   until the 6 h expiry.
2. Exit codes are ignored; failures are detected only through `stderr` text (`ERROR: Logging in `,
   `MemoryError:`) or spawn errors.
3. `onInstallOrUpdateOutput` only emits once all 5 fields have been seen since the last emit.
4. Repair has no progress reporting.
5. `importGame` always calls `addShortcuts()` even if the import failed.
6. `removeInstalledGame` removes the last entry when the app is not found.
7. `isEpicServiceOffline` may display an OS notification even when Epic is fine (component missing).
8. `stop()` uses `pkill -f <appName>` which can kill unrelated processes whose command line contains
   the appName.

---

## 5. GOG (runner id `gog`)

Source: `backend/storeManagers/gog/{library.ts,games.ts,user.ts,setup.ts,redist.ts,presence.ts,unified_info.ts,constants.ts,electronStores.ts}`.

### 5.1 gogdl invocation (`gog/library.ts:1471-1495`)

Every call:

```
<gogdl> --auth-config-path $HEROIC/gog_store/auth.json <commandParts...>
env: GOGDL_CONFIG_PATH = dirname($GOGDLCFG) = $HEROIC/gogdlConfig      (gogdl appends 'heroic_gogdl')
runner name 'gog' (log prefix Gog); everything else per §1
```

gogdl therefore keeps manifests in `$GOGDLCFG/manifests/<appName>` and tokens in `$GOGAUTH`.

gogdl 1.3.0 subcommands used: `auth`, `download`/`update`/`repair` (same parser), `info`, `import`,
`launch`, `save-sync`, `redist`. Options (from `gogdl <cmd> --help`):

* `download|update|repair <id> --platform/--os {windows,osx,linux} --path P --support S --lang L --build/-b B --branch BR --password PW --with-dlcs|--skip-dlcs --dlcs a,b,c --dlc-only --max-workers N`.
  For `download`, `--path` is the **parent** directory and gogdl appends the game folder name; for
  `update`/`repair`, `--path` is the actual install directory.
* `info <id> --os P [--branch] [--build] [--password] ...` → prints one JSON object (§5.6).
* `import <path>` → prints one JSON object (§5.13).
* `launch <path> <id> --platform P [--prefer-task N] [--no-wine] [--wine W] [--wine-prefix X] [--wrapper "..."] [--override-exe E] [extra game args...]`.
* `save-sync <path> <id> --os P --ts T --name N [--skip-download|--skip-upload|--force-upload|--force-download]` → writes the new sync timestamp to stdout.
* `redist --ids A,B --path P [--max-workers N] [--print-manifest]`.
* `auth [--code C] [--client-id] [--client-secret]`.

gogdl progress lines (stderr via logging, `gogdl/dl/progressbar.py:83-101`):

```
= Progress: 12.34 1234567/9876543, Running for: 00:01:02, ETA: 00:10:00
= Downloaded: 120.25 MiB, Written: 150.21 MiB
 + Download	- 25.06 MiB/s (raw) / 30.00 MiB/s (decompressed)
 + Disk	- 30.00 MiB/s (write) / 0.00 MiB/s (read)
```

### 5.2 Authentication

#### Login (`gog/user.ts:27-61`; IPC `authGOG`)

Frontend webview (`frontend/screens/WebView/index.tsx:86-88,216-229`), partition `persist:gog`, no
custom UA:

```
https://auth.gog.com/auth?client_id=46899977096215655&redirect_uri=https%3A%2F%2Fembed.gog.com%2Fon_login_success%3Forigin%3Dclient&response_type=code&layout=galaxy
```

On `dom-ready`, if the URL matches `/https://embed.gog.com/on_login_success?/`, read query `code` and
call `authGOG(code)`.

Backend:

```
gogdl --auth-config-path $GOGAUTH auth --code <code>
   abortId 'gogdl-auth', logSanitizer = authLogSanitizer
```

* gogdl GETs `https://auth.gog.com/token?client_id=46899977096215655&client_secret=<galaxy public secret>&grant_type=authorization_code&redirect_uri=https%3A%2F%2Fembed.gog.com%2Fon_login_success%3Forigin%3Dclient&code=<code>`,
  adds `loginTime` (epoch seconds float), writes `{"46899977096215655": data}` to `$GOGAUTH` and prints
  the JSON (`access_token, expires_in, token_type, scope, session_id, refresh_token, user_id, loginTime`).
  On network error / non-2xx it prints `{"error": true}`.
* Heroic: `JSON.parse(stdout.trim())`; parse failure or `data.error` → `{status:'error'}`.
* Success → GOG `configStore.set('isLoggedIn', true)` → `getUserDetails()` →
  `{status:'done', data: userData}`.

`authLogSanitizer(line)`: if the chunk parses as JSON, replace `access_token`, `session_id`,
`refresh_token`, `user_id` with `<redacted>` (adds them even if absent) and append `\n`; otherwise
return as is. Applied to log output only.

#### Credentials (`gog/user.ts:105-125`)

`getCredentials()`: offline → undefined (warn). Else

```
gogdl --auth-config-path $GOGAUTH auth          (abortId 'gogdl-get-credentials', sanitizer)
```

gogdl returns the stored credentials, refreshing them first if `time >= loginTime + expires_in`
(`GET https://auth.gog.com/token?client_id=...&client_secret=...&grant_type=refresh_token&refresh_token=<rt>`,
rewrites `$GOGAUTH`). Prints `null` if not logged in or refresh failed. Heroic returns
`JSON.parse(stdout)` (so `null`), or undefined on parse error.

**Every GOG HTTP call that needs a token spawns `gogdl auth` first** (credentials are not cached in
Heroic). Concurrent identical calls are de-duplicated by `callRunner` (§1.2).

#### User details (`gog/user.ts:63-103`)

Offline → undefined; not logged in → undefined; no credentials → undefined.
`GET https://users.gog.com/users/<user_id>` with `Authorization: Bearer <access_token>`,
`User-Agent: HeroicGamesLauncher/<app version>` → store `configStore.userData` (full `UserData`:
`id, username, created_date, avatar{...}, is_employee, tags, email, email_sha256_hash, roles, status, last_active_at, checksum, settings{...}, password_set, marketing_consent`).
Called after login and at startup when online and logged in (`main.ts:356-359`).

`isLoggedIn()` = `configStore.get('isLoggedIn') || false` (`user.ts:140-142`) — **not** based on
`auth.json`.

#### Logout (`gog/user.ts:127-138`; IPC listener `logoutGOG`)

`clearCache('gog')`; `configStore.clear()` (removes `isLoggedIn` and `userData`); delete `$GOGAUTH`;
clear session partition `persist:gog` (storage, cache, auth cache). Installed games, playtime
queue, private branch passwords and save timestamps are kept.

### 5.3 Library manager state & init

Module state (`gog/library.ts:66-67`): `library: Map<appName, GameInfo>`,
`installedGames: Map<appName, InstalledInfo>`.

`refreshInstalled()` (`library.ts:865-874`): `installedGamesStore.get('installed', [])`, skip entries
without `appName`, rebuild map.

`init()` (`library.ts:72-91`):

1. `await refresh()`.
2. Ensure `$GOGDLCFG/manifests` exists.
3. Once online: `credentials = getCredentials()`; for each installed appName
   `createMissingGogdlManifest(appName, credentials)` (§5.14).
4. Once online: `checkForRedistUpdates()` (§5.19).

### 5.4 Library refresh (`library.ts:450-604`; IPC `refreshLibrary`)

```
refresh():
  if !isLoggedIn(): return
  refreshInstalled()
  loadLocalLibrary():                                        // library.ts:364-377
     for game in libraryStore.get('games', []):
        copy = {...game}
        if installed(game): if online: checkForOfflineInstallerChanges(app)   // §5.14
                            copy.install = installedGames[app]; copy.is_installed = true
        library.set(app, copy)
     installedGamesStore.set('installed', [...installedGames.values()])
  library.set('gog-redist', REDIST_GAMEINFO)
  if offline: return
  credentials = getCredentials(); if none: return
  entries = getGalaxyLibrary()                               // all pages
  if entries empty: log error, return
  entries = entries.filter(e => e.platform_id === 'gog')
  gamesObjects = [REDIST_GAMEINFO]
  apiInfoCache.use_in_memory()
  for e in entries:
     unified = undefined; gamesdbUnavailable = false; retries = 5
     while retries > 0:
        try { data = getGamesdbData('gog', e.external_id, false, e.certificate, token).data
              gamesdbUnavailable = !data }
        catch { retries--; gamesdbUnavailable = true; log; sleep 2000 ms; continue }
        candidate = gogToUnifiedInfo(data); if candidate.app_name: unified = candidate
        break
     if !unified && gamesdbUnavailable:
        p = getProductApi(String(e.external_id), ['description'], token)
        if p: warn "Using product API metadata..."; unified = productToGameInfo(p.data, String(e.external_id))
        else: log error; continue
     if !unified: continue                                   // e.g. DLC/spam types or not visible_in_library
     if library has old entry: unified.folder_name = old.folder_name
     gamesObjects.push(unified)
     inst = installedGames.get(String(e.external_id))
     if inst && inst.platform !== 'linux':
        locs = getSaveSyncLocation(unified.app_name, inst)
        if locs: unified.cloud_save_enabled = true; unified.gog_save_location = locs
     copy = {...unified}; if inst: copy.is_installed = true; copy.install = inst
     library.set(copy.app_name, copy)
  apiInfoCache.commit()
  libraryStore.set('games', gamesObjects)                    // stored WITHOUT install data
  write "Games List:\n* <title> (App name: <id>)[ - DLC]...\n\nTotal: N\n" to the gog runner log (sorted, case-insensitive)
```

`REDIST_GAMEINFO` (`library.ts:456-467`):

```json
{ "app_name": "gog-redist", "runner": "gog", "title": "Galaxy Common Redistributables",
  "canRunOffline": true, "install": { "is_dlc": true }, "is_installed": true,
  "art_cover": "https://images.gog-statics.com/516af877f6a03199526d1ce5a76358b8f85f6b828764cf46c820f77ae8832fc5.jpg",
  "art_square": "https://cdn2.steamgriddb.com/file/sgdb-cdn/grid/5fa80a0fb5ff0b2aaca6730ba213219b.png" }
```

#### Galaxy library (`library.ts:319-362`)

```
GET https://galaxy-library.gog.com/users/<user_id>/releases[?page_token=<t>]
    Authorization: Bearer <access_token>
→ {total_count, next_page_token?, page_token?, limit, items: [{platform_id, external_id, origin, owned, date_created, owned_since, certificate}]}
```

Recurses while `next_page_token` is present. **Quirk:** if a subsequent page yields zero items, the
whole function returns `[]` (and refresh aborts with "There was an error Loading games library").
Request errors → that page contributes nothing.

#### GamesDB (`library.ts:1337-1385`)

```
getGamesdbData(store='gog', game_id, forceUpdate?, certificate?, accessToken?):
  pieceId = 'gog_<id>'
  cached = forceUpdate ? null : apiInfoCache.get(pieceId)
  if cached?.id and !forceUpdate: return {isUpdated:false, data: cached}
  GET https://gamesdb.gog.com/platforms/<store>/external_releases/<id>
      headers: If-None-Match: <cached.etag> (if any), X-GOG-Library-Cert: <certificate> (if any),
               Authorization: Bearer <token> (if any)
  404 → {isUpdated:false}   (data undefined)
  other errors → throw (caller retries)
  data.etag = response.headers.etag; apiInfoCache.set(pieceId, data)
  return {isUpdated: cached?.etag === etag, data}
```

#### `gogToUnifiedInfo(info)` → GameInfo (`library.ts:1074-1133`)

Returns `{}` (no app_name → ignored) unless `info.type ∈ {'game','mod'}` and
`info.game.visible_in_library`. Image URLs come from `url_format` templates with `{formatter}` → `''`
and `{ext}` → given extension:

| Field | Value |
|-------|-------|
| `runner` | `'gog'` |
| `developer` | `info.game.developers.map(d => d.name).join(', ')` |
| `app_name` | `String(info.external_id)` |
| `art_background` | `game.background.url_format` with `{ext}=webp` |
| `art_cover` | `game.logo.url_format` with `{ext}=jpg`, else `art_background` |
| `art_square` | `game.vertical_cover.url_format` with `{ext}=jpg`, else `art_cover` |
| `art_icon` | `(game.square_icon ?? game.icon).url_format` with `{ext}=jpg` |
| `cloud_save_enabled` | `false` (set later) |
| `extra` | `{about:{description: info.summary['*'], shortDescription:''}, reqs:[], genres: game.genres.map(g => g.name['*'])}` |
| `folder_name` | `''` (filled from `gogdl info` later / preserved from old library) |
| `install` | `{is_dlc:false}` |
| `is_installed` | `false` |
| `save_folder` | `''` |
| `title` | `(info.title['*'] \|\| info.game.title['*'] ?? '').trim()` |
| `canRunOffline` | `true` |
| `is_mac_native` / `is_linux_native` | `supported_operating_systems` contains slug `osx` / `linux` |
| `thirdPartyManagedApp` | `undefined` |

No `art_logo` on this path.

#### Product API fallback (`gog/unified_info.ts:9-47`)

`getProductApi(appName, expand?, token?)` (`library.ts:1393-1417`):
`GET https://api.gog.com/products/<id>?locale=<i18next.language>[&expand=a,b]` (Bearer if token) →
axios response or `null`.

`productToGameInfo(info, appName=String(info.id))`; `normalize(url)` prefixes `https:` to
protocol-relative `//` URLs, `''` for missing:

```
art_cover  = normalize(images.logo || images.icon)
art_square = normalize(images.icon || images.sidebarIcon || images.logo) || art_cover
art_logo = normalize(images.logo); art_background = normalize(images.background); art_icon = normalize(images.icon)
cloud_save_enabled=false; extra={about:{description: description.full || description.lead || '', shortDescription: description.lead || ''}, reqs:[]}
folder_name=''; install={is_dlc: game_type !== 'game'}; installable=is_installable; is_installed=false; save_folder=''
title=title.trim(); canRunOffline=true; is_mac_native=content_system_compatibility.osx; is_linux_native=content_system_compatibility.linux
```

#### GameInfo accessors

* `getGameInfo(slug)` (`library.ts:606-624`): `library.get(slug)` or, failing that, the entry from
  `libraryStore 'games'` merged with installed info (mutates the object).
* `GOGGame.getGameInfo()` returns the "empty" object
  `{app_name:'', runner:'gog', art_cover:'', art_square:'', install:{}, is_installed:false, title:'', canRunOffline:false}` when missing.
* `installState()` is a no-op (warning).

Real example of a stored GOG library entry (from `gog_library.json`):

```json
{ "runner": "gog", "developer": "CD Projekt RED", "app_name": "1423049311",
  "art_cover": "https://images.gog.com/<hash>.jpg?namespace=gamesdb",
  "art_square": "https://images.gog.com/<hash>.jpg?namespace=gamesdb",
  "art_background": "https://images.gog.com/<hash>.webp?namespace=gamesdb",
  "cloud_save_enabled": true, "art_icon": "https://images.gog.com/<hash>.jpg?namespace=gamesdb",
  "extra": { "about": { "description": "...", "shortDescription": "" }, "reqs": [],
             "genres": ["Role-playing (RPG)", "Adventure", "Shooter"] },
  "folder_name": "Cyberpunk 2077", "install": { "is_dlc": false }, "is_installed": false,
  "save_folder": "", "title": "Cyberpunk 2077", "canRunOffline": true,
  "is_mac_native": true, "is_linux_native": false,
  "gog_save_location": [ { "name": "saves", "location": "<?SAVED_GAMES?>/CD Projekt Red/Cyberpunk 2077" } ] }
```

### 5.5 Installed-game record (`$HEROIC/gog_store/installed.json`)

`InstalledInfo` (`common/types.ts:315-355`). Real example:

```json
{ "installed": [ {
  "platform": "windows", "executable": "", "install_path": "/home/u/Games/Heroic/Cyberpunk 2077",
  "install_size": "75.85 GiB", "is_dlc": false, "version": "2.5", "appName": "1423049311",
  "installedDLCs": ["1256837418"], "language": "en-US",
  "versionEtag": "\"4e4d1ffdf6d9f0db3bff8644e753c479\"", "buildId": "60148683633405433",
  "pinnedVersion": false } ] }
```

Optional fields: `branch`, `installedWithDLCs` (legacy bool), `cyberpunk: {modsEnabled, modsToLoad[]}`.
`platform` ∈ `windows|osx|linux`.

### 5.6 Install info (`library.ts:636-860`; IPC `getInstallInfo`)

```
getInstallInfo(appName, installPlatform='windows', {branch, build}):
  branch = options.branch || 'null'                       // literal string 'null'
  platform = lower(installPlatform); 'mac' → 'osx'
  password = privateBranchesStore.get(appName, '')
  key = `${appName}_${platform}_${branch}_${build}_${password}`   // build may be 'undefined'
  if installInfoStore.has(key) and get(key): return cached      // 6 h
  (online check is buggy: `if (!isOnline)` tests the function itself → never returns here)
  credentials = getCredentials(); none → undefined
  gameData = getGameInfo(appName); none → undefined
  argv: gogdl ... info <appName> --os <platform> [--branch <branch> if != 'null'] [--build <build>] [--password <pw>]
        abortId appName, prefix 'Getting game metadata'
  if !stdout or abort:
      if stderr contains "Game doesn't support content system api": return STUB (below)
      return undefined
  gogInfo = JSON.parse(stdout)    (failure → undefined)
  libraryArray = libraryStore 'games'; idx = index of appName; if -1: await refresh() and retry; still -1 → undefined
  if !libraryArray[idx].gog_save_location and installed and installed.platform != 'linux':
      gameData.gog_save_location = getSaveSyncLocation(appName, installed)
  libraryArray[idx].folder_name = gogInfo.folder_name; libraryArray[idx].gog_save_location = gameData.gog_save_location
  gameData.folder_name = gogInfo.folder_name; libraryStore.set('games', libraryArray); library.set(appName, gameData)
  language = first of gogInfo.languages; if some i18next.languages entry `p` has an available lang starting with p → that one
  download_size = gogInfo.download_size || size['*'].download_size + size[language].download_size + Σ dlc (size['*'] + size[language])
  disk_size     = gogInfo.disk_size     || (same with disk_size)
  info = { game: { app_name, title: gameData.title,
                   owned_dlc: dlcs.map(d => ({app_name: d.id, title: d.title, perLangSize: d.size})),
                   version: versionName, launch_options: [], branches: available_branches, buildId },
           manifest: { download_size, disk_size, perLangSize: size, app_name, languages, versionEtag,
                       builds: builds?.items, dependencies } }
  installInfoStore.set(key, info); return info
```

STUB for non-content-system games:

```json
{ "game": { "app_name": "<app>", "title": "<title>", "launch_options": [], "owned_dlc": [], "version": "", "branches": [], "buildId": "" },
  "manifest": { "app_name": "<app>", "disk_size": 0, "download_size": 0, "languages": [], "versionEtag": "",
                "dependencies": [], "perLangSize": { "*": { "download_size": 0, "disk_size": 0 } } } }
```

Raw `gogdl info` output (`GOGDLInstallInfo`, `common/types/gog.ts:33-53`):
`{size: {"*": {download_size, disk_size}, "<lang>": {...}}, download_size? (linux only), disk_size?, languages[], dlcs: [{title, id, size}], buildId, os, branch, dependencies[], versionName, versionEtag, folder_name, available_branches[], builds: {items: BuildItem[], total_count, count, has_private_branches}}`.

`BuildItem`: `{build_id, product_id, os, branch (null = default), version_name, tags[], public, date_published, generation, link?, urls?[{endpoint_name, url, url_format, parameters, fallback_only, max_fails, priority}]}`.

Download queue (`downloadqueue.ts:149-176`): for GOG Windows installs, if
`installInfo.manifest.dependencies` is non-empty **or** `$GOGREDIST` does not exist, a
`gog-redist` update element is pushed **before** the game, with
`params.dependencies = manifest.dependencies`.

### 5.7 Install (`games.ts:324-496`)

Queue pre-step (`downloadmanager/utils.ts:52-57`): delete `$GOGDLCFG/manifests/<appName>` if it
exists (prevents gogdl thinking it is already installed).

```
password = privateBranchesStore.get(id,'')
credentials = getCredentials(); none → {status:'error'}
platform = platformToInstall === 'Mac' ? 'osx' : lower(platformToInstall)
argv: gogdl ... download <id>
        --platform <platform>
        --path <path>                               (parent dir)
        --support $GOGSUPPORT/<id>
        (--with-dlcs --dlcs <a,b,c>  if installDlcs non-empty   |   --skip-dlcs)
        --lang <installLanguage>                    (String(undefined) = 'undefined' if missing!)
        [--build <build>] [--branch <branch>]
        [--max-workers <N>]
        [--password <pw>]
   abortId id, install log writer, onOutput → progress('installing'), prefix "Installing <id>"
abort → {status:'abort'}; error → {status:'error', error}
installInfo = getInstallInfo(id, platform, {branch, build}); undefined → error
if platform == 'linux': additional = getLinuxInstallerInfo(id)   // product API version
if !gameInfo.folder_name: error ("game info folder is undefined")
install_path = join(path, folder_name); size = getFileSize(getPathDiskSize(install_path))   // recursive lstat sum, symlinks = 0
record = { platform, executable:'', install_path, install_size: size, is_dlc:false,
           version: linux ? additional.version : installInfo.game.version, appName:id,
           installedDLCs: installDlcs, language: installLanguage,
           versionEtag: linux ? '' : installInfo.manifest.versionEtag,
           buildId: linux ? '' : installInfo.game.buildId, pinnedVersion: !!build }
installed.json 'installed'.push(record); gameInfo.is_installed = true; gameInfo.install = record; refreshInstalled()
[non-Linux Windows host] setup(id, record)
Linux-native (platform 'linux'): GameConfig.setSetting('steamRuntime', 'umu-scout');
     if <install_path>/support/postinst.sh exists: spawn it (no args), log errors
addShortcuts(); {status:'done'}
```

Note: for Windows games on Linux, the setup (§5.18) is **not** run at install time; it runs on first
launch when the prefix is first registered (§3), and after updates/moves if the prefix exists.

### 5.8 Progress parsing (`games.ts:248-322`)

Same structure as Epic (§4.9) but **percent comes from gogdl** and no total-size math:

| Field | Regex | Rule |
|-------|-------|------|
| `percent` | `/Progress: (\d+\.\d+) /m` | only if falsy; `Number($1)` |
| `eta` | `/ETA: (\d\d:\d\d:\d\d)/m` | only if `''` |
| `bytes` | `/Downloaded: (\S+) MiB/m` | only if `''`; `$1 + 'MB'` |
| `downSpeed` | `/Download\t- (\S+.) MiB/m` | only if falsy |
| `diskSpeed` | `/Disk\t- (\S+.) MiB/m` | only if falsy |

Emit `progressUpdate {appName:id, runner:'gog', status, progress}` once all five are set, then
reset. Redist updates emit progress for appName `gog-redist`.

### 5.9 Update (`games.ts:1008-1249`) and update detection

#### Update

Called from the queue with `{build, branch, language: installLanguage, dlcs: installDlcs, dependencies}`.

`gog-redist` special case: `redist = getRequiredRedistList()` + any `dependencies` not already
present → `updateRedist(redist)` (§5.19).

Otherwise, `getCommandParameters()` (`games.ts:1255-1288`):

```
workers  = maxWorkers ? ['--max-workers', N] : []
withDlcs = (install.installedWithDLCs || installedDLCs.length > 0) ? '--with-dlcs' : '--skip-dlcs'
dlcs     = installedDLCs.length ? ['--dlcs', installedDLCs.join(',')] : []
branch   = install.branch ? ['--branch', install.branch] : []
installPlatform = install.platform; credentials = getCredentials()
```

If no platform or credentials → `{status:'error'}`.

DLC removal (only when `updateOverwrites.dlcs` provided): removed = installed DLCs not in the new
list. If any removed and `install.platform === 'windows'` and (Windows host or the prefix dir
exists): list `unins\d{3}.ini` files in the install dir; for each, INI-parse and read
`[InstallSettings] productID`; if it is a removed DLC run the matching `uninsNNN.exe`
(`.ini`→`.exe` via first `ini` substring replace) in Wine:
`<uninsNNN.exe> /ProductId=<id> /VERYSILENT /galaxyclient /KEEPSAVES`
(protonVerb `run`, cwd = install path, not waited on wineserver).

```
argv: gogdl ... update <id>
        --platform <installPlatform>
        --path <install_path>
        --support $GOGSUPPORT/<id>
        <overwrittenWithDlcs>            // overwrites.dlcs given ? (len ? --with-dlcs : --skip-dlcs) : withDlcs
        --lang <overwrites.language || install.language || 'en-US'>
        <overwrittenDlcs...>             // overwrites.dlcs non-empty ? ['--dlcs', joined] : dlcs
        <workers...>
        [--build <overwrites.build>]
        <overwrites.branch ? ['--branch', it] : branch>
        [--password <pw>]
   abortId id, update log writer, onOutput progress('updating'), prefix "Updating <id>"
abort → {status:'done'} (!)
error → status 'done' event, {status:'error'}
```

Post-update record changes (`games.ts:1178-1248`):

* non-Linux platform: `installInfo = getInstallInfo(id, platform, {branch: overwrites.branch, build: overwrites.build})`;
  `{etag} = getMetaResponse(id, platform, installInfo.manifest.versionEtag)`; undefined installInfo →
  error. Set `buildId`, `version`, `branch = overwrites.branch` (cleared if not given!),
  `language = overwrittenLanguage`, `versionEtag = etag`.
* Linux: `version = getLinuxInstallerInfo(id).version` (none → error).
* If `overwrites.dlcs` → `installedDLCs = overwrites.dlcs`.
* `install_size` recomputed from disk; save installed.json; `refreshInstalled()`.
* platform `windows` and (Windows or prefix exists) → `setup(id, record, installRedist=false)`;
  platform `linux` → run `support/postinst.sh` if present.
* status `done`; frontend message `pushGameToLibrary` with updated gameData.

Pinned version: install with explicit `build` sets `pinnedVersion: true`;
`changeVersionPinnedStatus(appName, status)` (`library.ts:1525-1545`, IPC listener
`changeGameVersionPinnedStatus`) updates library, installed map and installed.json and pushes
`pushGameToLibrary`.

#### Update detection (`library.ts:940-1068`)

```
listUpdateableGames():
  if offline or not logged in: []
  credentials = getCredentials()
  for game in installedGames (skip no appName):
     if game.pinnedVersion: skip (warn)
     if game.platform == 'linux':
         if !(await checkForLinuxInstallerUpdate(app, game.version)) → updateable
     else if checkForGameUpdate(app, game.platform, game.versionEtag, token) → updateable

checkForLinuxInstallerUpdate(app, version):
  r = getProductApi(app, ['downloads']); none → false
  first installer with os === 'linux': return installer.version === version
  return false                      // QUIRK: "no data" is reported as "update available"

getBuilds(app, platform, token):
  GET https://content-system.gog.com/products/<app>/os/<platform>/builds?generation=2&_version=2[&password=<pw>]
      Authorization: Bearer <token> (if given)

getMetaResponse(app, platform, etag, token):
  builds = getBuilds(...)
  urls = (builds.items.find(b => !b.branch)?.urls) || builds.items[0]?.urls || []
  for u in urls:
     try GET u.url with If-None-Match: <etag> (if etag); accept only 200/304
         return {status, etag: response.headers.etag}
     catch: continue
  return {status: 304, etag}

checkForGameUpdate = meta.status === 200 && meta.etag !== etag
```

### 5.10 Repair (`games.ts:821-863`)

```
argv: gogdl ... repair <id>
        --platform <install.platform>
        --path <install_path>
        --support $GOGSUPPORT/<id>
        <withDlcs>                       (--with-dlcs | --skip-dlcs; NO --dlcs list)
        --lang <install.language || 'en-US'>
        -b=<install.buildId>             (literally "-b=undefined" if missing)
        [--max-workers N]
        [--password <pw>]
   abortId id, repair log writer, prefix "Repairing <id>"
no credentials → {stderr:'Unable to repair game, no credentials', stdout:''}
```

No progress parsing (verification has none; the download phase is not parsed either).

### 5.11 Uninstall (`games.ts:925-1006`)

```
array = installed.json 'installed'; idx by appName; not found → throw "Game isn't installed"
object = array.splice(idx, 1)
if <install_path>/unins000.exe exists:
    Linux: if prefix dir exists and !shouldRemovePrefix:
        run in Wine (wait): <install_path>/unins000.exe /VERYSILENT /ProductId=<id> /galaxyclient /KEEPSAVES
    [non-Linux Windows: elevated powershell Start-Process ... "/verysilent","/dir=..."]
rm -rf <install_path> (if exists)
rm $GOGDLCFG/manifests/<id> (if exists)
rm -rf $GOGSUPPORT/<id> (if exists)
installed.json 'installed' = array; refreshInstalled()
gameInfo.is_installed = false; gameInfo.install = {is_dlc:false}
remove shortcuts; syncStore.delete(id); remove Steam non-steam shortcut
frontend 'pushGameToLibrary' gameInfo
return {stdout:'', stderr:''}
```

`forceUninstall()` (`games.ts:1290-1296`): drop the entry from installed.json, `refreshInstalled()`,
`pushGameToLibrary`. Files and manifest are left untouched.

### 5.12 Move (`games.ts:784-815`, `library.ts:876-904`)

1. `moveOnUnix(newParent, gameInfo)` (same as §4.13).
2. `changeGameInstallPath(app, destination)`: requires cached library entry with `install` and
   `folder_name` (else log error, return). If platform `osx`, `destination = join(destination,
   folder_name)`. Update installed.json entry and in-memory install_path.
3. If platform `windows` and (Windows host or prefix exists) → `setup(id, undefined, installRedist=false)`
   (re-registers registry keys etc. for the new path).

### 5.13 Import (`games.ts:203-237`, `library.ts:906-936`)

```
gogdl ... import <folderPath>         (abortId id, prefix "Importing <id>")
abort/error → return res
data = JSON.parse(stdout):
   {appName, buildId, title, tasks (playTasks or null), installedLanguage, dlcs[], platform, versionName}
library.importGame(data, folderPath):
   info = getInstallInfo(data.appName)            // NOTE: always platform 'windows' (default)
   none → return
   record = { appName, install_path: folderPath, executable: folderPath, install_size: getFileSize(info.manifest.disk_size),
              is_dlc:false, version: versionName, platform, buildId, language: installedLanguage,
              installedDLCs: dlcs, installedWithDLCs: dlcs.length > 0 }
   installedGames.set; library entry updated (is_installed=true); installed.json rewritten; refreshInstalled()
   createMissingGogdlManifest(appName)            // no credentials passed
   checkForRedistUpdates()
   frontend 'pushGameToLibrary'
addShortcuts(); if Linux and native (platform linux): GameConfig.setSetting('steamRuntime','umu-scout')
```

gogdl import detection (`gogdl/imports.py`): looks for `goggame-*.info`/`goggame-*.id` (or
`Contents/Resources` on mac, or `gameinfo` for Linux installer builds: lines = title, version, ?,
language, [game_id, ?, build_id]); version name resolved from
`content-system.gog.com/products/<id>/os/<platform>/builds?generation=2`.

### 5.14 Manifest restore and offline-installer change detection

`createMissingGogdlManifest(appName, credentials?)` (`library.ts:102-166`):

```
path = $GOGDLCFG/manifests/<appName>
if exists or installed.platform == 'linux': return
imp = JSON.parse((gogdl ... import <install_path>).stdout)     (abortId '<app>-manifest-restore')
builds = getBuilds(appName, installed.platform, token).data.items
build = imp.buildId ? items.find(b.build_id == imp.buildId) : items.find(!b.branch)
if !build or !build.urls: error, return
raw = GET build.urls[0].url (arraybuffer); if build.generation == 2: zlib-inflate
manifest = JSON.parse(raw)
manifest.HGLPlatform = imp.platform; manifest.HGLInstallLanguage = imp.installedLanguage
manifest.HGLdlcs = imp.dlcs.map(id => ({id}))
write JSON to path
```

`checkForOfflineInstallerChanges(appName)` (`library.ts:379-448`, called during refresh when online
for each installed game): skip Linux, missing install dir, or if `<install>/.gogdl-resume` exists.

* Installed DLCs = all `goggame-(\d+).info` in the install dir (or `Contents/Resources` for osx)
  except the base id, sorted. If different from `installedDLCs` (sorted), update `installedDLCs`
  and the manifest's `HGLdlcs`.
* `readInfoFile(appName)` buildId (falls back to `goggame-<id>.id`'s `buildId`); if it differs from
  the recorded `buildId`: delete the manifest, `createMissingGogdlManifest`, set `buildId`.

`readInfoFile(appName, installPath?)` (`library.ts:1234-1294`): parses
`<install>/goggame-<id>.info` (`Contents/Resources/` for osx) — `{gameId, rootGameId, buildId?,
clientId?, name, language, languages?, playTasks[], supportTasks?, osBitness?, ...}`.

`getExecutable(appName)`: primary play task (`isPrimary` or first); URLTask → throw; returns
`join(workingDir, path)` or `path`.

### 5.15 Cloud saves

#### Save locations (`library.ts:243-312`)

```
getSaveSyncLocation(appName, install):
  syncPlatform = install.platform == 'osx' ? 'MacOS' : 'Windows'
  clientId = manifest v2 .clientId (from $GOGDLCFG/manifests/<app>) else readInfoFile(app, install_path).clientId
  if none: warn, return undefined
  GET https://remote-config.gog.com/components/galaxy_client/clients/<clientId>?component_version=2.0.45
  cs = response.content[syncPlatform].cloudStorage
  if !cs.enabled: undefined
  return cs.locations                 // [{name, location}] with GOG variables, possibly []
```

#### Default save paths (`backend/save_sync.ts:101-214`; IPC `getDefaultSavePath` with runner gog)

```
locs = getSaveSyncLocation(app, install); if !locs or !install_path: [] (error)
if locs empty: push {name:'__default', location:
     windows: '%LocalAppData%/GOG.com/Galaxy/Applications/<clientId>/Storage/Shared/Files'
     else:    '$HOME/Library/Application Support/GOG.com/Galaxy/Applications/<clientId>/Storage'}
variable map:
  INSTALL → install_path
  SAVED_GAMES → '%USERPROFILE%/Saved Games'
  APPLICATION_DATA_LOCAL → '%LOCALAPPDATA%'
  APPLICATION_DATA_LOCAL_LOW → '%APPDATA%\..\LocalLow'
  APPLICATION_DATA_ROAMING → '%APPDATA%'
  APPLICATION_SUPPORT → '$HOME/Library/Application Support'
  DOCUMENTS → native ? Electron documents dir : '%USERPROFILE%\Documents'
for each loc:
  if alreadyDefined (from settings.gogSaves) has same name with non-empty location: keep it
  replace each /<\?(\w+)\?>/ with map value (unknown → the bare variable name, warn)
  non-native: absolute = getWinePath({path, variant:'unix'})
  native: absolute = getShellPath(path) (shell-expand), then realpath if exists
  push {name, location: absolute}
```

The frontend stores the result in game settings `gogSaves`.

#### Sync (`games.ts:865-923`; IPC `syncSaves` and `syncGOGSaves`)

```
syncSaves(arg, path(unused), gogSaves):
  !gogSaves → 'Unable to sync saves, gogSaves is undefined'
  !getCredentials() → 'Unable to sync saves, no credentials'
  !install.platform → 'Unable to sync saves, game info not found'
  for loc in gogSaves:
     gogdl ... save-sync <loc.location> <id> --os <install.platform> --ts <syncStore[id][loc.name] or '0'> --name <loc.name> <arg>
        abortId id, prefix "Syncing saves for <title>", onOutput appends to fullOutput
     error → log
     if stdout: syncStore.set(`${id}.${loc.name}`, stdout.trim())        // new timestamp (float seconds as string)
  return fullOutput
```

`arg` ∈ `--skip-upload|--skip-download|--force-download|--force-upload` (or `''` → dropped).
gogdl talks to `https://cloudstorage.gog.com` using game-scoped tokens obtained with the manifest's
`clientId/clientSecret`.

### 5.16 Launch (`games.ts:523-782`)

```
require install.install_path and install.platform, else false
if install_path missing: errorHandler('appears to be deleted', id, 'gog') → force-uninstall dialog; false
prep = prepareLaunch(...)   (failure → dialog, false)
exeOverride = altExe launch option ? ['--override-exe', exe] : settings.targetExe ? ['--override-exe', targetExe] : []
env = {...process.env, ...setupWrapperEnvVars, ...setupEnvVars(settings, install_path), ...knownFixesEnv}
wrappers = setupWrappers(settings, mangohud, gamemode, gamescope, steamRuntime)
wineFlag = wrappers.length ? ['--wrapper', shlex.join(wrappers)] : []
if !isNative():
    wp = prepareWineLaunch(game, logWriter)  (failure → dialog, false); env += wp.envVars
    if isUmuSupported(settings): umuId = getUmuId(app_name,'gog'); if umuId: env.GAMEID = umuId
    wineFlag = getWineFlagsArray(settings, shlex.join(wrappers))
basic = launchArguments (type undefined|'basic') ? parameters : ''
argv: gogdl ... launch <install_path> <exeOverride...>
         <app_name, or '1597316373' if app is Cyberpunk '1423049311' with cyberpunk.modsEnabled>
         <wineFlag...> --platform <lower(install.platform)>
         <shlex.split(basic)...> <shlex.split(settings.launcherArgs ?? '')...> <args...>
         [--prefer-task 0]          (Cyberpunk REDmod path, see below)
```

`isNative()` (`games.ts:498-513`): Linux host → true iff `install.platform === 'linux'`.

Native Linux GOG games: `steamRuntime = 'umu-scout'` was set at install/import; `prepareLaunch`
then adds the umu-run path to the wrappers and `setupEnvVars` sets `PROTONPATH=umu-scout`, so the
game runs as `gogdl launch ... --wrapper '<...> /path/umu-run' --platform linux` inside the Steam
Linux Runtime (scout). gogdl executes `<install>/game/<task path>` or `start.sh`.

**Cyberpunk 2077 REDmod** (`games.ts:647-722`): if `cyberpunk.modsEnabled` and
`<install>/tools/redmod/bin` exists: mods available = subdirs of `<install>/mods` containing
`info.json`; load the selected `modsToLoad` that exist, or all mods (alphabetical) if none selected.
Run (Linux: in Wine, wait, cwd = redmod bin dir)
`redMod.exe deploy -reportProgress -root <winpath(install)> [-mod <name>]...`; write output to the
game log. If **stderr contains `deploy has succeeded`** a "Mod deploy failed" dialog is shown and
launch returns true without starting the game (inverted condition; preserve or fix knowingly).
Otherwise append `--prefer-task 0`. IPC: `getAvailableCyberpunkMods`, `setCyberpunkModConfig`
(stores `install.cyberpunk = {modsEnabled, modsToLoad}` in installed.json).

**Comet** (`games.ts:724-770`): if `configStore.userData.username` is set and
`experimentalFeatures.cometSupport !== false`:

```
spawn <comet dir>/<comet> --from-heroic --username <username> --quit
  (inherits process env; stdout/stderr written to the 'comet' runner log)
```

started just before gogdl; killed (`SIGTERM`) after gogdl returns. comet reads Heroic's GOG tokens
itself (`--from-heroic`) and emulates the Galaxy communication service the game's Galaxy SDK talks
to (via the `GalaxyCommunication` dummy service installed in the prefix, §3).

Status `playing` sent before; after exit: `launchCleanup`; `abort` → return true; error → log;
return `!error`. `logWriters: [logWriter]` always (unlike Epic).

`stop(stopWine=true)`: only `shutdownWine` if not native (gogdl handles signals itself).

### 5.17 Launch options (`library.ts:1503-1523`)

From `readInfoFile(app).playTasks`: for each task index `i` with `type === 'FileTask'`, not
`isHidden`, `category !== 'document'` → `{name: task.name || info.name, parameters: '--prefer-task <i>'}`.
Return `[]` if fewer than 2. (IPC adds a "Default" option, §4.19.)

### 5.18 Windows-game setup in the prefix (`gog/setup.ts:76-404`)

`setup(appName, installInfo?, installRedist = true)`; returns early unless install platform is
`windows`. On Linux:

1. `checkWineBeforeLaunch(...)` (abort if invalid Wine); `verifyWinePrefix(settings)`.
2. Read `$GOGDLCFG/manifests/<appName>` (missing → warn & return; parse error → return).
3. `lang = Intl.DisplayNames(['en'], {type:'language'}).of(install.language.split('-')[0])`
   (e.g. `en-US` → `English`).
4. `dependencies` = v1: `product.depots[].redist`; v2: `manifest.dependencies`.
5. Unless a dependency equals `scummvm` (case-insensitive), translate
   `gameSupportDir = $GOGSUPPORT/<app>` and `gameDirectoryPath = install_path` to Windows paths via
   `getWinePath(variant 'win')`.
6. Status `redist` with context `GOG`.
7. Run setup executables (all with `wait:false`, `protonVerb:'run'`,
   `skipPrefixCheckIKnowWhatImDoing:true`, `startFolder = install_path`):
   * **v1 manifests**: if `$GOGSUPPORT/<app>` exists, for each `product.support_commands[]` whose
     `languages` include `install.language ?? 'English'` or `Neutral`:
     `$GOGSUPPORT/<app>/<gameID>/<executable> /VERYSILENT /DIR=<gameDir> /Language=<l> /LANG=<l> /ProductId=<l> /galaxyclient /buildId=<product.timestamp> /versionName=<install.version> /nodesktopshorctut /nodesktopshortcut`
     where `l = lower(lang || installLanguage || 'English')` (note `/ProductId` receives the language
     — upstream bug).
   * **v2 with `scriptInterpreter: true`**: `isi = $GOGREDIST/__redist/ISI/scriptinterpreter.exe`
     (missing → error log). For each `manifest.products[]` that is the base game or in
     `installedDLCs`:
     `<isi> /VERYSILENT /DIR=<gameDir> /Language=<L> /LANG=<L> /ProductId=<productId> /galaxyclient /buildId=<install.buildId> /versionName=<install.version> /lang-code=<install.language||en-US> /supportDir=<gameSupportDir> /nodesktopshorctut /nodesktopshortcut`
     with `L = lang || 'English'` (not lowercased).
   * **v2 otherwise**: for each relevant product with non-empty `temp_executable`:
     `$GOGSUPPORT/<app>/<productId>/<temp_executable> /VERYSILENT /DIR=<gameDir> /Language=<l> /LANG=<l> /lang-code=<install.language||en-US> /ProductId=<productId> /galaxyclient /buildId=<buildId> /versionName=<version> /nodesktopshorctut /nodesktopshortcut`
     (`l` lowercased).
8. If `installRedist` and `$GOGREDIST/.gogdl-redist-manifest` exists: for each dependency, find the
   depot with `dependencyId === dep` (missing → warn); skip if `executable.path` is empty; command =
   `$GOGREDIST/<executable.path> <shlex.split(executable.arguments)>`; for `PHYSXLEGACY` wrap as
   `msiexec /i <exe> ... /qb`. Status `redist` with context `<readableName>`; run in Wine with
   `startFolder = $GOGREDIST`, `wait:false`.

[non-Linux Windows] commands run via elevated `powershell Start-Process -Verb RunAs -Wait`.

`runWineCommand` details (prefix creation when files missing, umu vs wine binary, `PROTON_VERB`,
wrappers) belong to the Wine spec (`backend/launcher.ts:1449-1602`).

### 5.19 Galaxy common redistributables (`gog/redist.ts`)

Pseudo game `gog-redist`, installed into `$GOGREDIST`.

`getRequiredRedistList()` (`redist.ts:133-181`): start with `['ISI']`; for every file in
`$GOGDLCFG/manifests`: v1 → each depot's `redist`; v2 → `dependencies`; dedupe. (No manifests dir →
`['ISI']`.)

`checkForRedistUpdates()` (`redist.ts:27-97`; at init and after GOG import):

```
if not logged in or offline: return
manifest = $GOGREDIST/.gogdl-redist-manifest
if missing: shouldUpdate = true
else:
  required = getRequiredRedistList().filter(r => depot(r) exists && depot.executable.path startsWith '__redist')
  installed = manifest.HGLInstalled || []
  differ in count or sorted contents → update
  else GET https://content-system.gog.com/dependencies/repository?generation=2 ; update if .build_id != manifest.build_id
  (any exception → log, return)
if shouldUpdate and no 'gog-redist' element already queued:
  addToQueue({params:{appName:'gog-redist', runner:'gog', path:$GOGREDIST, platformToInstall:'windows', gameInfo, size:'?? MB'},
              type:'update', addToQueueTime: now, startTime:0, endTime:0})
```

`updateRedist(ids)` (`redist.ts:183-220`):

```
gogdl ... redist --ids <ids joined ','> --path $GOGREDIST [--max-workers N]
   abortId 'gog-redist', prefix 'GOG REDIST:', log file $GAMESCFG/gog-redist.log,
   onOutput → getGame('gog-redist').onInstallOrUpdateOutput('updating', chunk)
error → {status:'error'} else {status:'done'}
```

gogdl writes `.gogdl-redist-manifest` = repository JSON (`{depots:[{compressedSize, dependencyId,
executable:{arguments, path}, internal, languages, manifest, readableName, signature, size}],
build_id}`) plus `HGLInstalled` (ids installed) and files under `$GOGREDIST/__redist/<ID>/...`.
Observed `HGLInstalled`: `["MSVC2013_x64","MSVC2012_x64","MSVC2015","ISI","MSVC2015_x64"]`.

The queue never notifies for `gog-redist` (`downloadqueue.ts:283-288`).

### 5.20 GOG presence (`gog/presence.ts`)

```
setPresence():
  skip if disableGOGPresence or disablePlaytimeSync or !isLoggedIn or offline
  creds = getCredentials(); none → return
  first successful call starts setInterval(setPresence, 5 min) (never cleared except by deletePresence)
  POST https://presence.gog.com/users/<user_id>/status
       Authorization: Bearer <token>
       {"application_type":"Heroic Games Launcher","force_update":false,"presence":"online",
        "version":"<heroic version>","game_id":"<appName>"?}      // game_id omitted (undefined) when idle
  204 → log "GOG presence set"; errors logged
deletePresence(force=false):
  skip if disablePlaytimeSync or (!force and disableGOGPresence) or !isLoggedIn or offline
  creds; clearInterval; DELETE same URL with Bearer; 204 → log
setCurrentGame(appId|'')
```

Callers: startup once online (`main.ts:382`); launch start (`setCurrentGame(app); setPresence()`)
and end (`setCurrentGame(''); setPresence()`) (`launcher.ts:236-259`); app exit
(`deletePresence()`); settings change `disableGOGPresence` → true: `deletePresence(true)`, false:
`setPresence()` (`presence.ts:96-104`).

### 5.21 Playtime sync (`games.ts:1323-1403`, `library.ts:168-241`)

```
updateGOGPlaytime(start, end):                      // after each GOG game session unless disablePlaytimeSync
  session_date = floor(start_ms / 1000)             // seconds
  time = floor((end_ms - start_ms) / 60000)         // minutes
  if time < 1: return
  creds = getCredentials(); none → warn, return    // NOTE: offline ⇒ getCredentials() is undefined ⇒ session dropped
  if offline: queue[user_id].push({session_date, time, appName}); runOnceWhenOnline(syncQueuedPlaytime); return
  r = postPlaytimeSession(...)
  if !r or r.status != 201: queue[user_id].push(...)

postPlaytimeSession({session_date, time, appName}):
  POST https://gameplay.gog.com/games/<appName>/users/<user_id>/sessions
       Authorization: Bearer <token>
       {"session_date": <int seconds>, "time": <int minutes>}
  expects 201; errors → null

syncQueuedPlaytime():
  if queue.has('lock'): return
  creds; none → error, return
  q = queue.get(user_id, []); empty → return
  queue.set('lock', [])
  for s in q: if offline: failed.push(s) (and STILL posts — quirk); r = post(s); if !r or status != 201: failed.push(s)
  queue.set(user_id, failed); queue.delete('lock')

getGOGPlaytime():                                  // IPC (main.ts:1395-1408)
  offline/no creds → undefined
  GET https://gameplay.gog.com/games/<id>/users/<user_id>/sessions (Bearer) → data.time_sum (minutes)
```

Startup (`main.ts:373-381`): `playtimeSyncQueue.delete('lock')`; unless `disablePlaytimeSync`,
`runOnceWhenOnline(syncQueuedPlaytime)`. Queue entries live in a CacheStore with the default 6 h
lifespan, so an un-synced queue silently expires after 6 h.

### 5.22 Achievements (`games.ts:141-169`; IPC `getAchievements(appName, runner, lang='en-US')`)

```
cached = achievementStore.get(id) → return (never expires; language ignored)
!isLoggedIn → []; no creds → []
GET https://gameplay.gog.com/clients/<productId>/users/<user_id>/achievements
    Authorization: Bearer <token>, X-Gog-Lc: <lang>
items = response.data.items; missing → []
achievementStore.set(id, items); return items
```

Item shape: `{achievement_id, achievement_key, visible, name, description, image_url_unlocked,
image_url_locked, rarity, date_unlocked (ISO or null), rarity_level_description, rarity_level_slug}`.
IPC `clearAchievementCache(appName)` deletes the entry.

### 5.23 Extra info and system requirements (`games.ts:98-139`, `library.ts:1141-1202`)

```
getExtraInfo():
  target = (mac && is_mac_native) ? 'osx' : (linux && is_linux_native) ? 'linux' : 'windows'
  reqs = createReqsArray(id, target)
  changelog = getProductApi(id, ['changelog']).data.changelog
  gamesData = GET https://api.gog.com/v2/games/<id>?locale=en-US   (null on error)
  storeUrl = gamesData._links.store.href rewritten: hostname 'af.gog.com', query as=1838482841
  releaseDate = gamesData._embedded.product.globalReleaseDate.substring(0,19)
  return {about: gameInfo.extra.about, reqs, releaseDate, storeUrl, changelog}

createReqsArray(id, os):
  offline → []; apiData = getGamesData(id); none → []
  sys = apiData._embedded.supportedOperatingSystems.find(s.operatingSystem.name === os)?.systemRequirements; none/empty → []
  minimum = sys[0]; recommended = sys[1] ?? null
  for i in minimum.requirements: {title: name.replace(':',''), minimum: description, recommended: recommended?.requirements[i]?.description}
     (skip entries with empty minimum)
```

`getLinuxInstallersLanguages(id)` (IPC `getGOGLinuxInstallersLangs`): product API `downloads`,
languages of `os === 'linux'` installers; on failure `['en-US']`.
`getLinuxInstallerInfo(id)`: version of the first linux installer.

### 5.24 Private branches

`privateBranchesStore` (`$HEROIC/gog_store/privateBranches.json`), IPC
`getPrivateBranchPassword(appName)` / `setPrivateBranchPassword(appName, password)`. The password
is appended as `--password <pw>` to `info`, `download`, `update`, `repair`, and as
`&password=<pw>` to the builds URL; it is also part of the install-info cache key.

### 5.25 GOG quirks to preserve (or consciously fix)

1. `getInstallInfo`'s offline guard never triggers (`!isOnline` on a function).
2. `install --lang` receives the string `undefined` if no language was chosen.
3. `repair` passes `-b=<buildId>` (possibly `-b=undefined`) and never passes `--dlcs`.
4. `update` abort returns `{status:'done'}`; `update` resets `branch` to `undefined` unless
   explicitly overwritten.
5. `checkForLinuxInstallerUpdate` failure ⇒ reported as "update available".
6. Galaxy library pagination: an empty later page discards everything.
7. DLCs are not GameInfo entries (gamesdb type `dlc` is dropped) unless gamesdb 404s and the product
   API fallback is used (then `install.is_dlc = true`).
8. `importGame` fetches install info for platform `windows` regardless of the imported platform.
9. Playtime: offline sessions are effectively dropped because credentials cannot be obtained
   offline; queued sessions expire after 6 h; `syncQueuedPlaytime` double-queues when offline.
10. Achievements cache ignores the language and never expires.
11. Cyberpunk REDmod "deploy has succeeded" check is inverted.
12. `getAchievements` uses the product id in the `clients/<id>` path.
13. `changeGameInstallPath` for `osx` appends `folder_name` again.
14. Every token-requiring call spawns `gogdl auth` (process per call).

---

## 6. IPC surface touching these stores (`backend/main.ts`)

| IPC (kind) | Epic | GOG |
|------------|------|-----|
| `login(code)` (handle) | `LegendaryUser.login` → `{status:'done'|'failed', data}` | — |
| `authGOG(code)` (handle) | — | `GOGUser.login` → `{status:'done'|'error', data?}` |
| `logoutLegendary` (handle) / `logoutGOG` (listener) | §4.3 | §5.2 |
| `getUserInfo` / `isLoggedIn` (handle) | Epic user info / `user.json` exists | — (GOG user data is read by the frontend from config) |
| `refreshLibrary(library?)` | `refresh()`; `'all'`/undefined → all stores via `Promise.allSettled` | same |
| `getGameInfo(appName, runner)` | `null` if `!hasGame`; `null` if empty object; overrides attached | same (no fast path) |
| `getExtraInfo` | §4.20 | §5.23 |
| `getInstallInfo(appName, runner, platform, build, branch)` | §4.6 (errors → null) | §5.6 (undefined → null) |
| `getGameSdl(appName)` / `getGameOverride()` | §4.6 | — |
| `install` (via download queue) | §4.7 | §5.7 |
| `checkGameUpdates` | §4.10 | §5.9 |
| `repair(appName, runner)` | §4.11 | §5.10 |
| `uninstall(appName, runner, removePrefix, removeSettings)` | §4.12 | §5.11 |
| `moveInstall` / `changeInstallPath` | §4.13 | §5.12 |
| `importGame({appName, path, runner, platform, winePrefix, wineVersion, wineCrossoverBottle})` | §4.14 | §5.13 |
| `launch` | §3 + §4.18 | §3 + §5.16 |
| `kill(appName, runner)` | abort + `stop()` | abort + `stop()` |
| `syncSaves({arg, path, appName, runner})` | §4.15 | calls `syncSaves(arg, path)` without `gogSaves` → returns `'Unable to sync saves, gogSaves is undefined'`; the GOG UI uses `syncGOGSaves` |
| `syncGOGSaves(gogSaves, appName, arg)` | — | §5.15 |
| `getDefaultSavePath(appName, runner, alreadyDefinedGogSaves)` | §4.15 | §5.15 |
| `getLaunchOptions(appName, runner)` | §4.19 | §5.17 |
| `egsSync(arg)` | §4.21 | — |
| EOS overlay handlers | §4.17 | — |
| `getAchievements(appName, runner, lang)` / `clearAchievementCache(appName)` | — (`getAchievements` undefined → `[]`) | §5.22 |
| `getGOGLinuxInstallersLangs(appName)` | — | §5.23 |
| `getPlaytimeFromRunner(runner, appName)` (main.ts:1395-1408) | undefined | `getGOGPlaytime()` unless `disablePlaytimeSync` |
| `getPrivateBranchPassword` / `setPrivateBranchPassword` | — | §5.24 |
| `getAvailableCyberpunkMods` / `setCyberpunkModConfig` | — | §5.16 |
| `changeGameVersionPinnedStatus(appName, runner, status)` (listener) | no-op | §5.9 |
| `getEpicGamesStatus` | `isEpicServiceOffline()` | — |

Backend → frontend events used by these stores: `gameStatusUpdate {appName, runner, status, folder?, context?}`
(statuses used: `installing`, `updating`, `repairing`, `uninstalling`, `moving`, `importing`,
`syncing-saves`, `launching`, `playing`, `redist`, `winetricks`, `done`),
`progressUpdate {appName, runner, status, progress}`, `refreshLibrary` (Epic uninstall),
`pushGameToLibrary <GameInfo>` (GOG install/uninstall/update/import/pin/mods).

## 7. Download-queue interplay (`backend/downloadmanager/*`)

* `addToQueue(element)` computes `params.size` from `getInstallInfo(...).manifest.download_size`
  (EA/Ubisoft → `'?? MB'`), and for GOG Windows installs may insert a `gog-redist` update first
  (§5.6).
* `installQueueElement` / `updateQueueElement` (`downloadmanager/utils.ts`) perform the
  online/Epic-status checks, GOG manifest deletion, status events, and call `game.install(...)`
  (`path` with all `'` removed, `sdlList` without empty strings) or
  `game.update({build, branch, language: installLanguage, dlcs: installDlcs, dependencies})`.
* Pause = abort + `stop(false)`; cancel = abort, remove the element and its `installDlcs` from the
  queue, optionally delete `<path>/<folder_name>` after 2 s (`removeFolder`, `backend/utils.ts`).

## 8. Worked argv examples (Linux host, Windows games, `maxWorkers=8`)

Note: with a Proton `wineVersion` on Linux, umu is used by default (prepareLaunch downloads it if missing) unless the game setting `disableUMU` is true; the "no umu" form only appears with `disableUMU: true`.

Epic install with SDL:

```
LEGENDARY_CONFIG_PATH=$HEROIC/legendaryConfig/legendary \
./legendary install Fortnite --install-tag= --install-tag chunk0 --install-tag chunk10 \
   --platform Windows --base-path /home/u/Games/Heroic --skip-dlcs -y --max-workers 8
```

Epic launch (Wine-GE, MangoHud + gamemode, language `en`, offline):

```
LEGENDARY_CONFIG_PATH=... WINEPREFIX=... HEROIC_APP_NAME=Sugar HEROIC_APP_RUNNER=legendary STORE=egs ... \
./legendary launch Sugar -windowed --wine /path/wine/bin/wine \
   --wrapper "/usr/bin/mangohud --dlsym /usr/bin/gamemoderun" --language en --offline
```

Epic launch with Proton and `disableUMU: true`:

```
./legendary launch Sugar --no-wine --wrapper ' "/path/GE-Proton9-20/proton" waitforexitandrun' --language en
```

GOG install:

```
GOGDL_CONFIG_PATH=$HEROIC/gogdlConfig \
./gogdl --auth-config-path $HEROIC/gog_store/auth.json download 1423049311 --platform windows \
   --path /home/u/Games/Heroic --support $HEROIC/gogdlConfig/heroic_gogdl/gog-support/1423049311 \
   --with-dlcs --dlcs 1256837418 --lang en-US --max-workers 8
```

GOG launch (Proton via umu):

```
./gogdl --auth-config-path .../auth.json launch "/home/u/Games/Heroic/Cyberpunk 2077" 1423049311 \
   --no-wine --wrapper /usr/bin/umu-run --platform windows
# env includes WINEPREFIX, STEAM_COMPAT_DATA_PATH, PROTONPATH, GAMEID=<umu id or umu-0>, HEROIC_APP_*, STORE=gog
```

GOG save sync (upload):

```
./gogdl --auth-config-path .../auth.json save-sync "/home/u/Games/Heroic/Prefixes/default/Cyberpunk 2077/drive_c/users/steamuser/Saved Games/CD Projekt Red/Cyberpunk 2077" \
   1423049311 --os windows --ts 1729600000.123 --name saves --skip-download
# stdout: new timestamp → saveTimestamps.json {"1423049311": {"saves": "<ts>"}}
```

## 9. Out of scope / handled elsewhere

* Wine/Proton prefix creation (`verifyWinePrefix`), `runWineCommand`, DXVK/VKD3D, winetricks,
  known fixes, umu download — Wine/compat spec.
* Desktop/menu shortcuts and Steam non-steam shortcuts (`addShortcuts`, `removeNonSteamGame`).
* Discord RPC, anticheat info, wiki info, recent games, game metadata overrides
  (`attachOverrides`).
* Amazon (nile), ZOOM, sideload. `storeManagerCommon/games.ts` (`launchGame`) is used only by
  sideload/zoom-style executables: it spawns `game.install.executable` directly (native, chmod 0775
  if not executable, wrappers prepended) or through `runWineCommand` with protonVerb
  `waitforexitandrun`, `startFolder = dirname(executable)`; browser games open an Electron
  `BrowserWindow` with partition `persist:<hostname>`. Epic/GOG never use it.
* e2e test stubs (`CI=e2e` → `runLegendaryCommandStub` / `runGogdlCommandStub`).
