# Parity spec: game launching, Wine/Proton compatibility layer, tools, settings, and app infrastructure

Reference: Heroic Games Launcher **v2.22.4** (TypeScript/Electron). This document describes the
observable behaviour of the launch pipeline, the Wine/Proton layer, the tool installers (DXVK,
VKD3D, DXVK-NVAPI, DXMT, winetricks, runtimes, umu), the settings system, and the surrounding
infrastructure (shortcuts, protocol, logging, wiki info, discounts, tray, online monitor,
migrations, startup and IPC). The goal is a from-scratch Rust implementation that is
behaviour-compatible: the same child-process invocations, environment variables, files on disk,
HTTP calls and IPC semantics.

All `file:line` references are relative to `src/` in the Heroic source tree. Host is **Linux**.
Windows and macOS-only paths are flagged **[non-Linux]** and are only summarised.

Document layout:
- **Part 1 (sections 0 to 9)**: launch, Wine, tools and config. This is the core.
- **Part 2 (`A.*`)**: shortcuts, Steam shortcuts, protocol, save paths, anticheat, known fixes,
  game overrides, system info.
- **Part 3 (`B.*`)**: wiki info, SteamGridDB, discounts, logger, tray, online monitor,
  migrations, `main.ts` startup and the full IPC surface.

---

## 0. Conventions and paths

### 0.1 Path variables

| Name | Linux value | Source |
|---|---|---|
| `$CFG` (`appFolder`) | `~/.config/heroic` (Electron `appData` + `heroic`). Flatpak: `~/.var/app/com.heroicgameslauncher.hgl/config/heroic` | `backend/constants/paths.ts:8,23` |
| `configPath` | `$CFG/config.json` (the global settings) | `paths.ts:28` |
| `gamesConfigPath` | `$CFG/GamesConfig/` (`<appName>.json` per game) | `paths.ts:29` |
| `toolsPath` | `$CFG/tools` | `paths.ts:25` |
| `runtimePath` | `$CFG/tools/runtimes` | `paths.ts:26` |
| `defaultUmuPath` | `$CFG/tools/runtimes/umu/umu_run.py` | `paths.ts:27` |
| `heroicIconFolder` | `$CFG/icons` | `paths.ts:30` |
| `heroicInstallPath` | `~/Games/Heroic` | `paths.ts:31` |
| `defaultWinePrefixDir` | `~/Games/Heroic/Prefixes` | `paths.ts:32-37` |
| `sharedWinePrefix` | `~/Games/Heroic/Prefixes/shared` | `paths.ts:38` |
| `defaultWinePrefix` | `~/Games/Heroic/Prefixes/default` (unused constant) | `paths.ts:39` |
| `flatpakHome` | `$XDG_DATA_HOME` with the first `/data` substring removed, or `homedir()` if the variable is unset or empty. Native with `XDG_DATA_HOME` unset gives `~`. Native with `XDG_DATA_HOME=~/.local/share` keeps `~/.local/share`, because `/data` does not occur. Flatpak: `~/.var/app/<id>/data` becomes `~/.var/app/<id>`. | `paths.ts:20` |
| `fakeEpicExePath` | `<resources>/app.asar.unpacked/build/bin/x64/win32/EpicGamesLauncher.exe` | `paths.ts:54-56` |
| `galaxyCommunicationExePath` | `.../bin/x64/win32/GalaxyCommunication.exe` | `paths.ts:58-60` |
| `vulkanHelperBin` | `.../bin/<arch>/<platform>/vulkan-helper` | `backend/utils/graphics/vulkan/index.ts:6-8` |
| electron-store `store/` files | `$CFG/store/<name>.json` (e.g. `wine-downloader-info.json`, `timestamp.json`, `config.json`) | `backend/electron_store.ts` |

**Quirk.** `flatpakHome` is used for `STEAM_COMPAT_CLIENT_INSTALL_PATH` (as `<flatpakHome>/.steam/steam`)
and for `PROTON_LOG_DIR`. On a native install where `XDG_DATA_HOME` is set to `~/.local/share`,
the value becomes `~/.local/share/.steam/steam`, which is almost always wrong. Replicate it as-is
for parity, or flag it.

### 0.2 Environment flags (`backend/constants/environment.ts`)

| Flag | Definition |
|---|---|
| `isLinux` | `process.platform === 'linux'` |
| `isSteamDeckGameMode` | `XDG_CURRENT_DESKTOP === 'gamescope'` (`:27-28`) |
| `isSteamDeck` | game mode, or (`SESSION_MANAGER` contains `unix/steamdeck` and `HOME==/home/deck` and `DESKTOP_SESSION` contains `steamos`) (`:29-33`) |
| `isFlatpak` | `FLATPAK_ID` is set (`:37`) |
| `flatpakRuntimeVersion` | `runtime-version` read from `/app/manifest.json`, else `'unknown'` (`:9-21,39`) |
| `isCLINoGui` / `isCLIFullscreen` / `isCLIConsoleMode` | argv contains `--no-gui` / `--fullscreen` / `--console` |
| `isAppImage` | `APPIMAGE` is set |

### 0.3 Core types (`common/types.ts`)

```ts
WineInstallation { bin: string; name: string; type: 'wine'|'proton'|'crossover'|'toolkit';
                   lib?: string; lib32?: string; wineserver?: string }          // :367-374
EnviromentVariable { key: string; value: string }                                // :459   (sic, "Enviroment")
WrapperVariable    { exe: string; args: string }                                 // :464
LaunchOption = { type?: undefined|'basic'; name: string; parameters: string }
             | { type: 'altExe'; executable: string }
             | { type: 'dlc'; dlcAppName: string; dlcTitle: string }             // :43-66
SteamRuntimeName = 'umu-scout'|'umu-soldier'|'umu-sniper'|'umu-steamrt4'         // common/types/umu.ts
ProtonVerb = 'run'|'waitforexitandrun'|'runinprefix'|'destroyprefix'|'getcompatpath'|'getnativepath'  // :641
WineCommandArgs { commandParts; wait?; protonVerb?; gameSettings?; gameInstallPath?; installFolderName?;
                  options?: CallRunnerOptions; startFolder?; skipPrefixCheckIKnowWhatImDoing?; ignoreLogging? } // :649
CallRunnerOptions { logMessagePrefix?; logWriters?; logSanitizer?; env?; wrappers?; onOutput?; abortId?; cwd? } // :448
```

`WineInstallation` objects are persisted verbatim in settings JSON. Key order doesn't matter, but
keys must round-trip.

---

## 1. Launch pipeline

### 1.1 Entry point: IPC `launch` -> `launchEventCallback` (`backend/launcher.ts:102-328`)

The IPC handler is `addHandler('launch', (e, args) => launchEventCallback(args))` (`main.ts:877-879`).
`args: LaunchParams = { appName, runner, launchArguments?, skipVersionCheck?, args? }`.
It returns `{status: 'done'|'error'|'abort'}`.

Steps, in order:

1. `game = libraryManagerMap[runner].getGame(appName)`, then `gameInfo = game.getGameInfo()`.
2. If `install.install_path` is set **and does not exist**, call `askForceUninstall(game)`, send
   status `done` and return `{status:'abort'}` (`:112-125`).
3. Call `gameSettings = await game.getSettings()` (section 7.3).
4. If no `launchArguments` were passed and `gameSettings.lastUsedLaunchOption` is set, use it
   (`:130-132`).
5. Read the global `minimizeOnLaunch` and `noTrayIcon`, and set `startPlayingDate = now`.
6. Timestamp store `tsStore` (`$CFG/store/timestamp.json`): if `!tsStore.has(appName)`, set
   `<appName>.firstPlayed = ISO(now)` (`:140-142`).
7. If `autoSyncSaves && isOnline()`: send status `syncing-saves`, then
   `game.syncSaves('--skip-upload', savesPath, gogSaves)`. Errors are only logged (`:146-162`).
8. Send status `launching`. If `minimizeOnLaunch && !noTrayIcon`, `mainWindow.hide()`.
9. Display-sleep blocker: `if (!powerDisplayId) powerDisplayId = powerSaveBlocker.start('prevent-display-sleep')`
   (`:176-179`). **Quirk:** this is never reset to `null` after `stop`. Every later launch in the
   same process skips `start` when the id is non-zero, and stops the stale id again (`:261-264`).
   An id of 0 means the check is always true.
10. `logWriter = createGameLogWriter(appName, runner)`, which writes
    `$XDG_STATE_HOME/Heroic/logs/games/<appName>_<runner>/launch.log` (see B.4). If
    `!gameSettings.verboseLogs`, two forced warnings are written: `IMPORTANT: Logs are disabled`,
    then "Enable verbose logs in Game's settings > Advanced tab > 'Enable verbose logs' before
    reporting an issue." Note that a launch log writer is **disabled** when `verboseLogs` is
    false. Only `forceLog` lines get through.
11. If not native: `checkWineBeforeLaunch(gameInfo, gameSettings, logWriter)` (section 3.6). If
    false: log `Was not possible to launch using <name>`, send status `done`, close the log and
    return `{status:'error'}`.
12. `runBeforeLaunchScript` (section 1.8), then send status `launching` again.
13. `command = game.launch(logWriter, launchArguments, args, skipVersionCheck)`. The store-specific
    assembly is in section 1.5.
14. GOG only: `gogPresence.setCurrentGame(appName); await setPresence()`.
15. `await command`. On an exception: log it, write `An exception occurred when launching the game:`
    plus the exception to the game log, and treat the result as `false`. In `finally`:
    `runAfterLaunchScript`, then `logWriter.close()`.
16. GOG only: clear the presence. Stop the power-save blocker.
17. Playtime (`:267-274`): `lastPlayed = ISO(now)`. `session = (end - start)/60000` minutes,
    fractional. `totalPlayed = floor(session + tsStore.get('<app>.totalPlayed', 0))`. The value is
    floored on every session, so fractional minutes are lost each time.
18. GOG and `!disablePlaytimeSync`: `updateGOGPlaytime(start, end)`. Otherwise log
    `Posting playtime session to server skipped - playtime sync disabled`.
19. `addRecentGame(gameInfo)`.
20. If `autoSyncSaves && isOnline()`: send status `done`, then `syncing-saves`, then
    `syncSaves('--skip-download', ...)`.
21. Send status `done`. If `--no-gui`, call `app.exit()`. Return `{status: launchResult ? 'done' : 'error'}`.

`timestamp.json` shape: `{ "<appName>": { "firstPlayed": ISO, "lastPlayed": ISO, "totalPlayed": int_minutes } }`.

### 1.2 `prepareLaunch(gameSettings, logWriter, gameInfo, isNative)` (`launcher.ts:484-736`)

Returns `LaunchPreperationResult { success, failureReason?, rpcClient?, mangoHudCommand?, gameModeBin?, gameScopeCommand?, steamRuntime?, offlineMode? }`.

1. `offlineMode = gameSettings.offlineMode || !isOnline()`. For `legendary`, if not offline,
   `offlineMode = await isEpicServiceOffline()`. That function GETs
   `https://status.epicgames.com/api/v2/components.json` and shows a notification if the service
   is down (`utils.ts:186+`).
2. If `!gameInfo.canRunOffline && offlineMode`, warn in the game log.
3. If global `discordRPC`, call `constructAndUpdateRPC(gameInfo)`: Discord client id
   `852942976564723722`, activity name = title, `state: 'via Heroic on <OS>'`, large image =
   `art_icon || art_square` if it is an http URL, else `icon_new` (`utils.ts:555-588`).
4. Game-log header lines, in this order:
   - `Launching "<title>" (<runner>)`
   - `Native? <bool>`
   - `Installed in: <path>`. Sideload uses `folder_name`. Third-party-managed apps also log the
     prefix and `Managed by a third-party app: <name>`.
   - `System Info:` followed by the formatted system info (A.8.3).
   - `Game Settings: <filterGameSettingsForLog(...)> \n Stored at: $CFG/GamesConfig/<app>.json`
   - `Anticheat Status: ...` / `Anticheats: ...` (A.5)
   - `Wiki Link, read this: <url>` if known fixes contain one (A.6)
5. `filterGameSettingsForLog` (`:330-482`) on Linux deletes these keys before logging:
   - Always: `enableQuickSavesMenu`, `verboseLogs`.
   - Gamescope limiter sub-keys if the limiter is off, and upscaling sub-keys if upscaling is off.
   - Linux: `enableMsync`, `wineCrossoverBottle`, `advertiseAvxForRosetta`.
   - Not native: also `steamRuntime`. Proton: also `autoInstallDxvk` and `autoInstallVkd3d`.
     Steam Deck: also `autoInstallDxvkNvapi`.
   - Native: all Wine keys (`wineVersion, winePrefix, autoInstall*, enableFsync, enableEsync,
     enableFSR, enableWineWayland, enableHDR, enableWoW64, showFps, enableDXVKFpsLimit,
     eacRuntime, battlEyeRuntime, useGameMode`).
6. **Non-Linux returns here** with `{success:true, rpcClient, offlineMode}`.
7. MangoHud (`:587-602`): if `showMangohud && !isSteamDeckGameMode`, find `mangohud` on `$PATH`
   (section 1.9). If it is missing, return failure `Mangohud is enabled, but \`mangohud\` executable could not be found on $PATH`.
   Flatpak appends `. Make sure to install Mangohud's flatpak package with runtime <ver> and restart Heroic.`
   Otherwise `mangoHudCommand = [<abs mangohud>, '--dlsym']`.
8. GameMode (`:604-613`): if `useGameMode`, find `gamemoderun` on PATH. If it is missing, fail
   with `GameMode is enabled, but \`gamemoderun\` executable could not be found on $PATH`. Note that
   GameMode is **not** suppressed in Steam Deck game mode.
9. Gamescope (`:615-710`): only if `(gamescope.enableLimiter || gamescope.enableUpscaling) && !isSteamDeckGameMode`.
   - If `gamescope` is not found, log a **warning only** (launch continues, `gameScopeCommand = []`).
   - Version probe: `spawnSync(gamescope, ['--help'])`. If **stderr** contains `-F, --filter`, it
     is the new CLI (3.12 and later), otherwise the old one.
   - Arguments, in push order:

     | Condition | New CLI | Old CLI |
     |---|---|---|
     | upscaling and `gameWidth` | `-w <gameWidth>` | same |
     | upscaling and `gameHeight` | `-h <gameHeight>` | same |
     | upscaling and `upscaleWidth` | `-W <upscaleWidth>` | same |
     | upscaling and `upscaleHeight` | `-H <upscaleHeight>` | same |
     | upscaling and method `fsr` | `-F fsr` | `-U` |
     | upscaling and method `nis` | `-F nis` | `-Y` |
     | upscaling and method `integer` | `-S integer` | `-i` |
     | upscaling and method `stretch` | `-S stretch` | (nothing) |
     | upscaling and windowType `fullscreen` | `-f` | `-f` |
     | upscaling and windowType `borderless` | `-b` | `-b` |
     | limiter and `fpsLimiter` | `-r <fps>` | same |
     | limiter and `fpsLimiterNoFocus` | `-o <fps>` | same |
     | `enableForceGrabCursor` (needs limiter or upscaling enabled to reach here) | `--force-grab-cursor` | same |
     | `showMangohud` | `--mangoapp` | same |
     | always | `...shlex.split(additionalOptions ?? '')` | same |
     | always, last | `--` | same |

     The command starts with the absolute path to `gamescope`. Empty strings count as "not set".
10. umu / Steam runtime (`:712-725`):
    - `umuAvailable = (await isInstalled('umu')) || (await getUmuPath()) !== defaultUmuPath`
    - `useUmu = (!isNative && isUmuSupported(gs, checkInstalled=false)) || (isNative && gs.steamRuntime)`
      (`steamRuntime` is a non-empty runtime name).
    - If `useUmu && !umuAvailable`, call `download('umu')` (section 5.1).
    - If `useUmu`, `steamRuntime = [await getUmuPath()]`.
11. Return `{success:true, rpcClient, mangoHudCommand, gameModeBin, gameScopeCommand, steamRuntime, offlineMode}`.

### 1.3 `setupWrappers(gs, mangoHudCommand?, gameModeBin?, gameScopeCommand?, steamRuntime?)` (`launcher.ts:1321-1351`)

The wrapper array is built in this exact order:

1. `...gameScopeCommand` (which ends with `--`), if the array is given.
2. For each `gs.wrapperOptions` entry: `exe`, then `...shlex.split(args ?? '')`.
3. `...mangoHudCommand`, **only if** `gameScopeCommand?.length === 0`, i.e. gamescope is not in
   use. When gamescope is in use, MangoHud is provided through `--mangoapp` instead. If
   `gameScopeCommand` is `undefined`, MangoHud is also skipped, but every caller passes an array.
4. `gameModeBin`.
5. `...steamRuntime` (the umu path) when the caller passes it.
6. Finally, `.filter(n => n)` drops empty strings.

Callers decide whether to pass `steamRuntime`:
- Legendary, GOG, Nile and Zoom pass it whenever `steamRuntime?.length`, so umu becomes the
  **last wrapper**.
- Sideload passes it only for native games. For Wine games, `runWineCommand` makes umu the
  runner itself (`storeManagerCommon/games.ts:169-181`).

### 1.4 Environment builders

#### 1.4.1 `setupEnvVars(gs, installPath?)` (`launcher.ts:1029-1065`): generic, used for native and Wine

These are applied in this order. Later keys overwrite earlier ones inside this function.

| Var | Value | Condition |
|---|---|---|
| `DRI_PRIME` | `1` | `nvidiaPrime` |
| `__NV_PRIME_RENDER_OFFLOAD` | `1` | `nvidiaPrime` |
| `__GLX_VENDOR_LIBRARY_NAME` | `nvidia` | `nvidiaPrime` |
| `MTL_HUD_ENABLED` | `1` | [non-Linux] macOS and `showFps` |
| `STEAM_COMPAT_INSTALL_PATH` | `installPath` | Linux and `installPath` truthy |
| `PROTONPATH` | `gs.steamRuntime` (e.g. `umu-sniper`) | Linux and `steamRuntime` truthy |
| `<user key>` | `removeQuoteIfNecessary(value)` | each `enviromentOptions` entry, in array order |
| `LD_PRELOAD` | `''` | neither `process.env.LD_PRELOAD` nor a user-set `LD_PRELOAD` (the Fall Guys fix) |

`removeQuoteIfNecessary(s)` (`utils.ts:621-631`): if `s` starts **and** ends with `"`, it strips
all leading and trailing `"` characters. Otherwise it returns `String(s)`.

#### 1.4.2 `setupWrapperEnvVars({appName, appRunner})` (`launcher.ts:1072-1102`)

| Var | Value |
|---|---|
| `HEROIC_APP_NAME` | appName |
| `HEROIC_APP_RUNNER` | runner (`legendary`/`gog`/`nile`/`sideload`/`zoom`) |
| `GAMEID` | `umu-0` (this default is later overwritten by the umu id when one is found) |
| `HEROIC_APP_SOURCE` | `gog`→`gog`, `legendary`→`epic`, `nile`→`amazon`, `sideload`→`sideload`, `zoom`→`zoom` |
| `STORE` | `gog`→`gog`, `legendary`→`egs`, `nile`→`amazon`, `zoom`→`zoomplatform` (not set for sideload) |

#### 1.4.3 `setupWineEnvVars(gs, gameId = '0')` (`launcher.ts:1110-1319`): Wine and Proton specific

`gameId` is `gameInfo.folder_name` when called from `prepareWineLaunch`, and `installFolderName`
when called from `runWineCommand`. Undefined defaults to `'0'`.
`steamInstallPath = <flatpakHome>/.steam/steam`.

Rows are applied in source order:

| # | Var | Value | Condition |
|---|---|---|---|
| 1 | `WINEPREFIX` | `winePrefix` | type `wine` |
| 2 | `WINEDLLOVERRIDES` | `winemenubuilder.exe=d` | type `wine`. **If** the user has an env entry whose key lowercases to `winedlloverrides`, then `ret[<that exact key>] = <raw user value> + ';winemenubuilder.exe=d'` instead. The raw value is used without unquoting. |
| 3 | `STEAM_COMPAT_CLIENT_INSTALL_PATH` | `steamInstallPath` | type `proton` |
| 4 | `WINEPREFIX` | `winePrefix` | type `proton` |
| 5 | `STEAM_COMPAT_DATA_PATH` | `winePrefix` | type `proton` |
| 6 | `PROTONPATH` | `dirname(wineVersion.bin)` | type `proton` |
| 7 | `CX_BOTTLE` | `wineCrossoverBottle` | [non-Linux] type `crossover` |
| 8 | `WINEPREFIX` | `winePrefix` | type `toolkit` [non-Linux] |
| 9 | `DXVK_HUD` | `fps` | `showFps` (macOS: `MTL_HUD_ENABLED=1` instead) |
| 10 | `DXVK_FRAME_RATE` | `gs.DXVKFpsCap` | `enableDXVKFpsLimit`. If the cap is `undefined`, Node drops the variable; an empty string sets `DXVK_FRAME_RATE=`. |
| 11 | `WINE_FULLSCREEN_FSR` | `1`, plus `WINE_FULLSCREEN_FSR_STRENGTH = maxSharpness?.toString() \|\| '2'` | `enableFSR`. If FSR is off, `WINE_FULLSCREEN_FSR=0` is **always** set. |
| 12 | `WINEESYNC` | `1` | `enableEsync && type !== 'proton'` |
| 13 | `PROTON_NO_ESYNC` | `1` | `!enableEsync && type === 'proton'` |
| 14 | `WINEMSYNC` (+ `WINEESYNC=1` for toolkit) | `1` | [non-Linux] macOS |
| 15 | `WINEFSYNC` | `1` | Linux, `enableFsync`, type `!== proton` |
| 16 | `PROTON_NO_FSYNC` | `1` | Linux, `!enableFsync`, type `proton` |
| 17 | `PROTON_ENABLE_WAYLAND` (+ `PROTON_ENABLE_HDR=1` if `enableHDR`) | `1` | Linux, `enableWineWayland`, proton |
| 18 | `DISPLAY` (+ `DXVK_HDR=1` if `enableHDR`) | `''` (empty, which forces winewayland) | Linux, `enableWineWayland`, not proton |
| 19 | `PROTON_USE_WOW64` | `1` | Linux, `enableWoW64`, proton |
| 20 | `WINEARCH` | `wow64` | Linux, `enableWoW64`, not proton |
| 21 | `PROTON_ENABLE_NVAPI=1`, `DXVK_NVAPI_ALLOW_OTHER_DRIVERS=1` | | proton and `autoInstallDxvkNvapi` |
| 22 | `PROTON_DISABLE_NVAPI` | `1` | proton and `!autoInstallDxvkNvapi` |
| 23 | `DXVK_ENABLE_NVAPI=1`, `DXVK_NVAPI_ALLOW_OTHER_DRIVERS=1` | | Linux, `autoInstallDxvkNvapi`, type `wine` |
| 24 | `PROTON_EAC_RUNTIME` | `$CFG/tools/runtimes/eac_runtime` | Linux and `eacRuntime` (any wine type) |
| 25 | `PROTON_BATTLEYE_RUNTIME` | `$CFG/tools/runtimes/battleye_runtime` | Linux and `battlEyeRuntime` |
| 26 | `STEAM_COMPAT_APP_ID` | `process.env.STEAM_COMPAT_APP_ID \|\| '0'` | proton |
| 27 | `SteamAppId` | `process.env.SteamAppId \|\| STEAM_COMPAT_APP_ID` | proton |
| 28 | `SteamGameId` | `process.env.SteamGameId \|\| 'heroic-' + gameId` | proton |
| 29 | `PROTON_LOG_DIR` | `flatpakHome` | proton |
| 30 | `WINEDEBUG=+fixme`, `DXVK_LOG_LEVEL=info`, `VKD3D_DEBUG=fixme` | | proton and `verboseLogs`. Each is set only if the user env does not already define that exact (case-sensitive) key. |
| 31 | `ORIG_LD_LIBRARY_PATH` | `process.env.LD_LIBRARY_PATH ?? ''` | `!preferSystemLibs`, type `wine`, and `ORIG_LD_LIBRARY_PATH` not already in `process.env` |
| 32 | `LD_LIBRARY_PATH` | `[lib, lib32, process.env.LD_LIBRARY_PATH].filter(Boolean).join(':')` | `!preferSystemLibs`, type `wine`, **and both** `lib` and `lib32` truthy |
| 33 | `GST_PLUGIN_SYSTEM_PATH_1_0` | `<lib>/gstreamer-1.0:<lib32>/gstreamer-1.0` | as #32, and both dirs exist |
| 34 | `WINEDLLPATH` | `<lib>/wine:<lib32>/wine` | as #32, and both dirs exist |
| 35 | (log only) | If `lib`/`lib32` are missing: logError "Couldn't find all library folders of <name>! ... Falling back to system libraries!" | `!preferSystemLibs`, type `wine`. This **fires for system wine and custom wine**, because those carry no `lib`/`lib32` (section 3). |
| 36 | `ROSETTA_ADVERTISE_AVX=1` | | [non-Linux] |
| 37 | `SteamVirtualGamepadInfo` | `''` | Linux and `wineVersion.name` matches `/(GE\|Wine\|Proton)-(Proton[7-8]\|[4-7].*-GE\|GE-4.[0-9]*)/` (old GE builds; a warning is logged) |

Heroic **never** sets `WINEDEBUG=-all` and never sets `PROTON_LOG`. (`ignoreLogging` in
`runWineCommand` only deletes an inherited `PROTON_LOG`.)

#### 1.4.4 Known-fixes env

`getKnownFixesEnvVariables(appName, runner)` returns `knownFixes.envVariables || {}` (`launcher.ts:1018-1022`; see A.6).

### 1.5 Store-specific command assembly (Linux)

Env precedence below is listed from lowest to highest. A later layer overwrites a key from an
earlier one.

#### 1.5.1 Epic / Legendary (`storeManagers/legendary/games.ts:881-1040`)

Legendary is never native on Linux.

1. `prepareLaunch(...)`. On failure: log `Launch aborted: <reason>`, show the error dialog
   "Launch aborted", return false.
2. `languageCode = gs.language || configStore.get('language','')`. `configStore` is
   `$CFG/store/config.json`, and that key holds the **UI language**. So by default Legendary
   receives `--language <ui-lang>`.
3. `commandEnv = {process.env} < setupWrapperEnvVars(legendary) < setupEnvVars(gs, install_path) < knownFixesEnv`.
4. Fake EGL exe: if `commandEnv.USE_FAKE_EPIC_EXE ∉ {undefined,'0','false'}` and `fakeEpicExePath`
   exists, call `getWinePath('C:\\windows\\command\\EpicGamesLauncher.exe','unix')`. If that path
   is missing, copy the exe there. Then set `LEGENDARY_WRAPPER_EXE = 'C:\\windows\\command\\EpicGamesLauncher.exe'`.
5. `wrappers = setupWrappers(gs, mangoHud, gameMode, gamescope, steamRuntime?.length ? steamRuntime : undefined)`.
6. `prepareWineLaunch(game, logWriter)` (section 1.6). On failure, return false (the dialog is
   shown only if a reason exists).
7. `commandEnv = commandEnv < wineEnvVars`.
8. If `isUmuSupported(gs)` (installed check included): `umuId = getUmuId(app_name,'legendary')`
   (B.1.9). If it is truthy, `GAMEID = umuId`.
9. `wineFlags = getWineFlags(gs, shlex.join(wrappers))` (section 1.5.6).
10. Build the `LegendaryCommand` object. Its **key insertion order defines argv order**
    (`legendary/library.ts:823-874`):
    - `subcommand:'launch'`
    - `appName`: either `launchArguments.dlcAppName` (for type `dlc`) or `appName`
    - `extraArguments = [...args, launchOption.parameters (basic only), gs.launcherArgs].filter(Boolean).join(' ')`
    - `...wineFlags` (`--wine`/`--no-wine` first, then `--wrapper`)
    - `--skip-version-check` (if requested)
    - `--language <code>`
    - `--override-exe <altExe.executable | gs.targetExe>`
    - `--offline` (if `offlineMode`)
    - `--skip-version-check` (if `--no-gui`; it keeps its original position if already present)
    - `--origin` (if `isEAManaged`)
    - `--ubisoft` (if `isUbisoftManaged`)
11. Rendering: `['launch', appName, ...shlex.split(extraArguments), ...for each key starting with '-' with a truthy value: true → [key], otherwise → [key, String(value)]]`.
12. `runRunnerCommand` adds `LEGENDARY_CONFIG_PATH=$CFG/legendaryConfig/legendary`, then calls
    `callRunner(argv, {name:'legendary', bin, dir}, {env, wrappers, abortId: appName, logWriters: verboseLogs ? [logWriter] : []})`.
    `callRunner` spawns with `env = {...process.env, ...options.env}`. `options.wrappers` is
    **ignored** by `callRunner` and has effect only through `--wrapper`.
13. Send status `playing`, run `launchCleanup(rpcClient)`, return `!error`.

Example (GE-Proton, umu installed, gamemode on, one user wrapper `strace -f`):
```
legendary launch <app> --no-wine --wrapper "strace -f /usr/bin/gamemoderun /home/u/.config/heroic/tools/runtimes/umu/umu_run.py" --language en
```
Without umu, the wrapper string is `'<shlex.join(wrappers)> "<proton bin>" waitforexitandrun'`.
This is a JS template string, **not** shlex. With no wrappers it gets a leading space:
` "<proton>" waitforexitandrun`.

#### 1.5.2 GOG (`storeManagers/gog/games.ts:523-772`)

1. Return false if `install` / `install_path` / `platform` is missing. If the install path does
   not exist, call `errorHandler('appears to be deleted')` and return false.
2. `prepareLaunch` (same failure handling as Legendary).
3. `exeOverrideFlag = ['--override-exe', altExe.executable | gs.targetExe]` or `[]`.
4. `commandEnv = {process.env} < wrapperEnv(gog) < setupEnvVars(gs, install_path) < knownFixesEnv`.
5. `wrappers = setupWrappers(... steamRuntime?.length ? steamRuntime : undefined)`. Then
   `wineFlag = wrappers.length ? ['--wrapper', shlex.join(wrappers)] : []`. That is the native case.
6. If not native: `prepareWineLaunch`, `commandEnv < wineEnvVars`, `GAMEID = umuId` (as for
   Legendary), and `wineFlag = getWineFlagsArray(gs, shlex.join(wrappers))`.
7. argv (before `runRunnerCommand` prepends `--auth-config-path <userData>/gog_store/auth.json`):
   ```
   launch <install_path> [--override-exe X] <app_name | '1597316373' if app_name=='1423049311' && cyberpunk.modsEnabled>
          <wineFlag...> --platform <install.platform.toLowerCase()>
          <shlex(launchOption.parameters)...> <shlex(gs.launcherArgs)...> <args...> [--prefer-task 0]
   ```
   - `--prefer-task 0` is appended only after a REDmod deploy (Cyberpunk mods): `redMod.exe deploy
     -reportProgress -root <win path of install> -mod <m>...`, run via `runWineCommand` with
     `startFolder <install>/tools/redmod/bin`.
   - `GOGDL_CONFIG_PATH = dirname(gogdlConfigPath)` is added to the env.
8. Comet: if `configStore.userData.username` is set and `experimentalFeatures.cometSupport !== false`,
   spawn `<comet> --from-heroic --username <u> --quit`. Its output goes to the `comet` runner log,
   and it is killed when the game exits.
9. Run through `callRunner` with `logWriters: [logWriter]` (always, unlike Legendary). Abort
   counts as success.

#### 1.5.3 Amazon / Nile (`storeManagers/nile/games.ts:298-410`)

Env and wrappers are as for GOG, with runner `nile`. When not native:
`wineFlag = [...getWineFlagsArray(gs, shlex.join(wrappers)), '--wine-prefix', gs.winePrefix]`.
argv:
`launch [--override-exe X] <wineFlag...> <shlex(launchOption.parameters)...> <shlex(launcherArgs)...> <appId> <args...>`.
`NILE_CONFIG_PATH = dirname(nileConfigPath)` is set.

#### 1.5.4 Zoom (`storeManagers/zoom/games.ts:578-742`)

- `commandParts = [...shlex(launchOption.parameters), ...shlex(launcherArgs), ...args]`, plus
  `-conf <c>` for each `install.dosboxConf` when `install.isDosbox`.
- Native: `callRunner(commandParts, {name:'zoom', bin: install.executable, dir: isDosbox ? undefined : install_path}, {env: commandEnv, wrappers, cwd: install_path})`.
  **Quirk:** `callRunner` ignores `options.wrappers`, so wrappers (gamemode, mangohud, gamescope,
  user wrappers, steam runtime) are silently **not applied** to native Zoom games.
- Wine: `prepareWineLaunch`. `executable = gs.targetExe || install.executable`.
  `startFolder = dirname(abs(executable))`. `GAMEID = umuId` when umu is supported.
  Then `runWineCommand({commandParts: [basename(executable), ...commandParts], gameInstallPath, installFolderName: folder_name, protonVerb:'waitforexitandrun', startFolder, (wait unset), options:{env: {...commandEnv, ...envVars}, wrappers, ...}})`.
  It reports an error when `code !== 0 && stderr`.

#### 1.5.5 Sideload (`storeManagers/storeManagerCommon/games.ts:116-274`)

- `executable = gs.targetExe || install.executable`. If `browserUrl` is set, a BrowserWindow is
  opened instead (out of scope).
- `extraArgs = [...shlex.split(gs.launcherArgs ?? ''), ...args]`.
- `prepareLaunch`. If not native, `prepareWineLaunch` runs **before** the success check, and its
  failure is ignored.
- `wrappers = setupWrappers(gs, mh, gm, gs_cmd, isNative && steamRuntime?.length ? steamRuntime : undefined)`.
- Send status `playing`.
- **Native:**
  - If the executable is not executable (`access X_OK` fails), `chmod 0o775` it.
  - `env = setupWrapperEnvVars(sideload) < setupEnvVars(gs, install_path) < knownFixesEnv`.
    `knownFixesEnv` is always `{}` for sideload.
  - If there are wrappers: `argv = [...wrappers, executable, ...extraArgs]` and
    `executable = argv.shift()` (the first wrapper).
  - `callRunner(argv, {name: runner, bin: basename(executable), dir: dirname(executable)}, ...)`.
    `callRunner` turns a relative bin with a dir into `./<bin>` and uses cwd = dir.
  - **Quirk:** with wrappers, cwd becomes the **first wrapper's directory** (e.g. `/usr/bin`),
    not the game's.
- **Wine:** `runWineCommand({commandParts: [executable, ...extraArgs], gameSettings: gs, wait: true, protonVerb:'waitforexitandrun', startFolder: dirname(executable), options:{wrappers, logWriters:[logWriter]}})`.
  **No** `options.env` is passed, so there are no `HEROIC_APP_*` / `STORE` variables, `GAMEID` is
  unset, and `STEAM_COMPAT_INSTALL_PATH` is unset (`gameInstallPath` is not passed). `SteamGameId`
  is `heroic-0`.

#### 1.5.6 `getWineFlags(gs, wrapper)` / `getWineFlagsArray` (`utils/compatibility_layers.ts:518-576`)

`wineBin = bin` with all `'` characters removed if it starts and ends with `'`. This fixes old
configs.

| type | flags (in this order) |
|---|---|
| `wine`, `toolkit` | `--wine <wineBin>`, then `--wrapper <wrapper>` if wrapper is non-empty |
| `proton` | `--no-wine`, then `--wrapper <isUmuSupported(gs) ? wrapper : `${wrapper} "${wineBin}" waitforexitandrun`>` |
| `crossover` | `--wine <wineBin>` [+ `--wrapper`] |

The array form turns `true` into `[key]` and anything else into `[key, value]`.

### 1.6 `prepareWineLaunch(game, logWriter)` (`launcher.ts:764-978`)

1. If `!validWine(gs.wineVersion)` **and** `!validWine(global.wineVersion)`, return `{success:false}`
   with no reason. Note: if only the game's Wine is invalid, the launch continues with the
   **invalid** game Wine. `checkWineBeforeLaunch` normally fixed this earlier.
2. [non-Linux] CrossOver bottle check.
3. Async log `Winetricks packages: <comma list from <prefix>/winetricks.log>`.
4. Legendary on Linux: async log `EOS Overlay: Enabled|Not enabled`.
5. If `offlineMode && !canRunOffline`, warn.
6. `await verifyWinePrefix(gs)` (section 2.1).
7. `installed_games` bookkeeping (`:847-870`), using file `<winePrefix>/installed_games`, a JSON
   array of appNames:
   - If the file is missing: `mkdir -p prefix`, write `["<app>"]`, and set `hasUpdated`.
   - Otherwise parse it. If the app is not listed, push it, write the file back (compact JSON),
     and set `hasUpdated`.
8. If `hasUpdated`: log `Created/Updated Wineprefix at <prefix>`, then run the store setup:
   - GOG: `gogSetup(app)` (redistributables), then send status `launching`.
   - Nile: `nileSetup(app)`.
   - Legendary: `legendarySetup(app, logWriter)`. This includes `reg add HKEY_CLASSES_ROOT\com.epicgames.launcher /f`
     and the manifest prerequisites.
   - All: `installFixes(app, runner)` (known-fixes winetricks and runInPrefix, see A.6).
9. GOG and `experimentalFeatures.cometSupport !== false` (`:895-944`). Errors here are only logged:
   - `dest = getWinePath('C:\\ProgramData\\GOG.com\\Galaxy\\redists\\GalaxyCommunication.exe','unix')`.
   - If `dest` is missing: `mkdir -p`, copy the bundled `GalaxyCommunication.exe`, then run
     `wine sc create GalaxyCommunication binpath=C:\ProgramData\GOG.com\Galaxy\redists\GalaxyCommunication.exe` (verb `run`).
   - **On every launch:** `wine reg add HKLM\SOFTWARE\WOW6432Node\GOG.com\GalaxyClient\paths /v client /t REG_SZ /d "C:\Program Files\GOG Galaxy" /f`.
10. Only when type is `wine` (never Proton):
    - `autoInstallDxvk` → `DXVK.installRemove(gs,'dxvk','backup')`
    - Linux and `autoInstallDxvkNvapi` → `'dxvk-nvapi'`
    - Linux and `autoInstallVkd3d` → `'vkd3d'` (section 4.2)
11. If `eacRuntime && isOnline() && !isInstalled('eac_runtime')`, call `download('eac_runtime')`.
    The same applies to `battlEyeRuntime` and `battleye_runtime`. (This applies for any Wine type,
    even if the setting is irrelevant.)
12. Return `{success:true, envVars: setupWineEnvVars(gs, gameInfo.folder_name)}`.

### 1.7 `runWineCommand(args)` (`launcher.ts:1449-1602`): the generic Wine/Proton spawner

Used for games (sideload/zoom), prefix creation, `reg` edits, winecfg, run exe, winepath, and
the setups.

1. `settings = gameSettings ?? GlobalConfig.getSettings()`.
2. Prefix sanity check (unless `skipPrefixCheckIKnowWhatImDoing` or crossover):
   - Required for `wine`: `dosdevices, drive_c, system.reg, user.reg, userdef.reg`, plus the
     prefix dir itself.
   - Required for `proton`: `pfx.lock, tracked_files, version, config_info, pfx/dosdevices,
     pfx/drive_c, pfx/system.reg, pfx/user.reg, pfx/userdef.reg`, plus the prefix dir.
   - If any is missing: log a warning, `mkdir -p prefix`, then `verifyWinePrefix(settings)`.
3. If `!validWine(wineVersion)`, return `{stdout:'', stderr:'Invalid wine'}`.
4. Env: `{...process.env} < options.env < setupEnvVars(settings, gameInstallPath) < setupWineEnvVars(settings, installFolderName) < {PROTON_VERB: protonVerb}`.
   `protonVerb` defaults to `'run'`. If `ignoreLogging`, delete `PROTON_LOG`.
5. `wineBin = wineVersion.bin` with all `'` removed. `umuSupported = isUmuSupported(settings)`.
   `runnerBin = umuSupported ? getUmuPath() : wineBin`.
6. If proton and not umu: `commandParts.unshift(protonVerb)`. This gives
   `proton <verb> <exe> args`. With umu, the verb travels only in `PROTON_VERB` and argv is
   `umu-run <exe> args`.
7. Wrappers (`options.wrappers`; **mutated** by `shift`): if present, `bin = wrappers[0]` and
   argv is `[...wrappers[1..], runnerBin, ...commandParts]`. Otherwise `bin = runnerBin`.
8. `spawn(bin, argv, {env, cwd: startFolder})`. Not a shell. If log writers are given, write
   `Wine Command: <bin> <argv joined by ' '>\n\nGame Log:\n`, then stream stdout/stderr into
   them. `onOutput` is called for each chunk.
9. On `close`: collect `{stderr, stdout, code}`. If `wait && wineVersion.wineserver`, spawn
   `<wineserver> --wait` with the same env/cwd and wait for it. Proton has no `wineserver`
   field, so `wait` is a no-op there.
10. On `error`, the error is only `console.log`ged and the promise **never resolves**. Parity:
    resolve or reject in Rust, but note this hang.

### 1.8 Before/after scripts (`launcher.ts:1936-2036`)

- Skipped if the path is empty. Otherwise the game log gets
  `Running script before|after <title> (<path>)\n`.
- `spawn(scriptPath)` with no args, no shell, `cwd = install.install_path`.
- Env: `{HEROIC_GAME_APP_NAME, HEROIC_GAME_EXEC: install.executable, HEROIC_GAME_PREFIX: winePrefix, HEROIC_GAME_RUNNER, HEROIC_GAME_SCRIPT_STAGE: 'before'|'after', HEROIC_GAME_TITLE, HEROIC_GAME_SETTINGS: JSON(gameSettings), HEROIC_GAME_INFO: JSON(gameInfo), ...process.env}`.
  `process.env` comes **last**, so it wins.
- stdout/stderr go to the game log only if `verboseLogs`.
- The promise resolves on `exit`, which means Heroic waits for the script. It rejects on spawn
  `error`, e.g. a non-executable script. The rejection from the before-script propagates out of
  `launchEventCallback`; the IPC call rejects and the game does not start.

### 1.9 `searchForExecutableOnPath(name)` (`utils/os/path/index.ts:11-21`)

For each `$PATH` entry (split on `:`) it checks `join(dir, name)` with `stat`. The first entry
that exists is returned. There is no executable-bit check.

### 1.10 `callRunner(commandParts, runner, options)` (`launcher.ts:1658-1830`): helper-binary spawner

- Extract the appName from argv:
  - gog: after `launch` → idx+2, else after `download|repair|update` → idx+1.
  - legendary: after `launch|install|repair|update` → idx+1.
  - nile: if `launch|install|update|verify` is present → last element.
- Always append the runner log writer (`$LOGS/runners/<runner>.log`), and filter falsy argv
  entries.
- If `runner.dir` is set and the bin is relative, use bin `./<bin>`.
- Log the safe command line via `getRunnerCallWithoutCredentials` (`:1841-1889`):
  - Redact the values after `--code` and `--token`.
  - Prefix `KEY=quoteIfNecessary(value)` for each env var whose value differs from
    `process.env`.
  - `quoteIfNecessary` wraps the string in `"` if it contains a space and is not already quoted.
  - This line is written to the runner log and to the game log, followed by `Game Output:` when
    an appName was found.
- **De-duplication:** `key = [runner.name, commandParts].join(' ')`. If an identical command is
  already running, the same promise is returned.
- Abort controller id = `options.abortId || appName || random` (the IPC `kill` handler aborts it).
- `spawn(bin, argv, {cwd: options.cwd || runner.dir, env: {...process.env, ...options.env}, signal})`.
  stdout/stderr pass through the optional `logSanitizer` and go to the writers. `memoryLog`
  collects the trimmed chunks.
- On close: call `errorHandler(stdout+stderr, appName, runner)`. If a signal arrived and the
  child was not killed by us, reject. Otherwise resolve `{stdout: chunks.join(), stderr: chunks.join('\n')}`.
  Note that stdout is joined with `','` (the `Array.join` default).
- Result: `{stdout, stderr, fullCommand}`. If aborted: `{..., abort:true}`. If errored:
  `{stdout:'', stderr: String(err), error}`.

### 1.11 Stopping a game (IPC `kill`, `main.ts:1090-1093`)

`callAbortController(appName)` kills the runner child via its AbortSignal. Then
`game.stop()`. Legendary example (`legendary/games.ts:1085-1097`):
- `pkill -f <appName>`.
- If not native, `shutdownWine(gs)` (`utils.ts:816-829`): if `wineVersion.wineserver` exists,
  `spawnSync(wineserver, ['-k'], {env: {WINEPREFIX}})`. Note that this is a **minimal env**, not
  `process.env`. Otherwise run `runWineCommand(['wineboot','-k'], wait, verb run)`.

### 1.12 `getWinePath({path, gameSettings, variant})` (`launcher.ts:1898-1934`)

`runWineCommand({commandParts: ['cmd','/c','winepath', variant==='unix' ? '-u' : '-w', path], wait:false, protonVerb:'runinprefix', ignoreLogging:true})`,
then return `stdout.trim()`. If the result is empty, log `Couldn't get wine path for <path>.\n<stderr>`.

---

## 2. Prefix creation and maintenance

### 2.1 `verifyWinePrefix(settings)` (`launcher.ts:1396-1440`)

1. `winePrefix` defaults to `sharedWinePrefix` if undefined.
2. If `!validWine`, return empty. If crossover, return empty.
3. If the prefix dir is missing and **not** umu-supported, `mkdir -p prefix`. With umu, Heroic
   lets umu create it.
4. `systemRegPath` is `<prefix>/pfx/system.reg` for proton and `<prefix>/system.reg` otherwise.
   `haveToWait = !exists(systemRegPath)`.
5. Command:
   - umu-supported: `umu-run createprefix` with `PROTON_VERB=run`.
   - Otherwise: `wine wineboot --init`, or `proton run wineboot --init` for Proton without umu.
   - Options: `wait: haveToWait`, `protonVerb:'run'`, `skipPrefixCheckIKnowWhatImDoing:true`.
6. Errors are logged as `Unable to create Wineprefix: ` and rethrown.

### 2.2 When prefixes are created or chosen

- Global default prefix: `winePrefix = sharedWinePrefix` (`config.ts:356`).
- At install time (frontend `InstallModal/WineSelector/index.tsx:54-64`), the suggested per-game
  prefix is `${defaultWinePrefixDir}/${removeSpecialcharacters(title ?? appName)}`.
  - `removeSpecialcharacters` (`frontend/helpers/index.ts:119-122`) removes every character in
    the class `[:|/|*|?|<|>|\\|&|{|}|%|$|@|`|!|™|+|'|"|®]` (the class includes `|`).
  - If the "Use shared Wine prefix" toggle is on, the global `winePrefix` / `wineVersion` /
    `wineCrossoverBottle` are used instead.
- When the user confirms the install (`DownloadDialog/index.tsx:269-283`), and Wine is in use and
  the OS is not Windows, the frontend calls
  `writeConfig({appName, config: {...requestGameSettings(appName), winePrefix, wineVersion, wineCrossoverBottle}})`.
  That **materialises the full per-game file**, which is then explicit (section 7.3). The import
  flow does the same (`main.ts:1066-1074`).
- The prefix is actually created lazily at first launch (section 1.6, step 6) or on the first
  `runWineCommand` (section 1.7, step 2).
- Known-fixes and store setups run once per (prefix, appName), tracked through
  `<prefix>/installed_games`.

### 2.3 Proton prefix layout

Heroic uses `STEAM_COMPAT_DATA_PATH = WINEPREFIX = <prefix>`, so Proton puts the real Wine
prefix in `<prefix>/pfx`. Tools that need a real `wine` (winetricks, DXVK checks) map the path
to `<prefix>/pfx` through `getWineFromProton` (section 3.5), **unless** umu is used.

---

## 3. Wine/Proton discovery

### 3.1 `GlobalConfig.getAlternativeWine(scanCustom = true)` (`config.ts:191-206`)

This is the IPC `getAlternativeWine` (`main.ts:809-811`).

1. If `wine-downloader-info.json` has an empty or missing `wine-releases`, first run
   `await updateWineVersionInfos(true)`. That fetches from GitHub, **blocking** the call.
2. [non-Linux] macOS uses its own set.
3. Linux: `[...getLinuxWineSet(scanCustom)]`.

### 3.2 `getLinuxWineSet(scanCustom)` (`compatibility_layers.ts:112-203`)

Order of the resulting list (this order matters: the first entry becomes the fallback in
section 3.6):

1. **Proton set**, built from `protonPaths` in this order:
   1. `$CFG/tools/proton/`
   2. For each Steam library `L` from `getSteamLibraries()`, in order:
      - if `showValveProton`: `L/steam/steamapps/common`, then `L/steamapps/common`
      - always: `L/root/compatibilitytools.d`, then `L/compatibilitytools.d`

   For each existing path, for each entry `v` from `readdirSync`, in filesystem order (not
   sorted): skip names starting with `UMU-Latest`. If `<path>/<v>/proton` exists, add
   `{bin: <path>/<v>/proton, name: v, type:'proton'}`. No wineserver and no libs are recorded.
2. **Wine set**:
   - For each `v` in `$CFG/tools/wine/`, add
     `{bin: <tools>/wine/v/bin/wine, name: v, type:'wine', ...getWineLibs, ...getWineExecs}`.
     **No existence check** is done, so any directory entry is listed, even a broken one.
   - Lutris: for each `v` in `~/.local/share/lutris/runners/wine/` (if the dir exists), add the
     same structure. Only the native Lutris path is scanned; the Flatpak Lutris path is not.
3. **System wine**, from `getDefaultWine()`:
   - `execSync('which wine')`, first line, gives the bin.
   - `execSync('wine --version')`, first line, gives the name (e.g. `wine-11.17 (Staging)`).
   - Then `getWineExecs(bin)`.
   - On any failure the result is `{bin:'', name:'Default Wine - Not Found', type:'wine'}`. It is
     only included if the name does not contain `Not Found`. Note that **no `lib`/`lib32`** are
     recorded.
4. **Custom paths** (only when `scanCustom` and `config.json` exists), from global `customWinePaths: string[]`:
   - A path ending in `proton` gives `{bin: path, name: 'Custom Proton - <path>', type:'proton'}`.
   - Anything else gives `{bin: path, name: 'Custom Wine - <path>', type:'wine', ...getWineExecs(path)}`
     (no libs).

The list is a JS `Set` of fresh objects, so **there is no de-duplication**. The same Proton can
appear twice if two library paths resolve to the same directory.

Helpers:
- `getWineExecs(bin)`: if `dirname(bin)/wineserver` exists, return `{wineserver: that}`, else `{wineserver: ''}`.
- `getWineLibs(bin)`: `lib32 = dirname(bin)/../lib` (if it exists) and `lib = dirname(bin)/../lib64`
  (if it exists), else `''`. Mind the naming: `lib32` comes from `../lib`.

**Not scanned in v2.22.4:** Bottles runners, Flatpak Steam's `compatibilitytools.d` (unless it is
in `libraryfolders.vdf`), `~/.steam/root/compatibilitytools.d` (unless `L/root/...` matches), and
system Proton packages outside the Steam libraries.

### 3.3 Steam libraries: `getSteamLibraries()` (`utils.ts:532-552`)

- `path = defaultSteamPath` (all `'` removed). `vdf = <path>/steamapps/libraryfolders.vdf`.
- Always start with `['/usr/share/steam']`.
- If the vdf exists: parse it as text VDF (the `simple-vdf` style `parse`). If there is no
  `libraryfolders` key, return the base list. Otherwise return
  `['/usr/share/steam', ...Object.values(libraryfolders).map(f => f.path)].filter(existsSync)`.
  The order is the vdf order, keys "0", "1", and so on.
- If the vdf is missing, log a debug message and return `['/usr/share/steam']`, unfiltered.

`defaultSteamPath` default, from `getSteamCompatFolder()` (`config.ts:39-65`):
- If `~/.var/app/com.valvesoftware.Steam/.steam/steam` exists **and** `flatpak info com.valvesoftware.Steam`
  exits 0, use that path.
- Otherwise use `~/.steam/steam`.

### 3.4 `validWine(wineVersion)` (`launcher.ts:1358-1389`)

- Returns false if `wineVersion` is falsy.
- Logs `Checking if wine version exists: <name>`.
- Required files: `[bin, wineserver]` for type `wine` (so a `''` wineserver means **invalid**),
  `[bin]` for anything else. All must exist.
- [non-Linux] On macOS, toolkit requires Sonoma or later and Apple Silicon.

### 3.5 `getWineFromProton(gs)` (`utils.ts:144-183`)

If the type is not `proton` **or** umu is supported, return `{winePrefix: gs.winePrefix, wineVersion}`
unchanged. Otherwise:
- `winePrefix = <prefix>/pfx`.
- For `distPath` in `['dist','files']`: if `dirname(bin)/<distPath>/bin/wine` exists, return
  `{type:'wine', bin: that, wineserver: <.../wineserver> if it exists}`. Old Proton uses `dist`;
  GE-Proton and Experimental use `files`.
- Otherwise log `Proton <name> has an abnormal structure...` and return the input unchanged.

### 3.6 `checkWineBeforeLaunch` and the fallback chain (`utils.ts:943-1038`)

1. If the game's Wine is valid, return true.
2. Otherwise log `Wine version <name> is not valid, trying another one.` (unless `disableLogs`),
   plus a game-log warning if `verboseLogs`.
3. If the **global** Wine is valid, show a blocking dialog `Wine not found!` with the text "We
   could not find the selected wine version to launch this title (<sel>). \n We found another
   one, do you want to continue launching using <found> ?" and buttons [Yes, No].
   - Yes (index 0): set the game's `wineVersion` to the global Wine (persisted through
     `GameConfig.setSetting`) and return true.
   - No: log `User canceled the launch` and return false.
4. Otherwise `wineList = getAlternativeWine()` and `first = wineList[0]`:
   - If the list is empty, or `first` is invalid: `downloadDefaultWine()` (section 3.7). If that
     returns a Wine, set it on the game and return true.
   - If `first` is valid: show the same dialog with `first`. Yes persists it and returns true;
     No returns false.
5. Return false.

### 3.7 `downloadDefaultWine()` (`utils.ts:893-941`)

Called at startup when no Wine was found (`main.ts:166-190`, 2.5 s after the window is created),
and from section 3.6.

1. `await updateWineVersionInfos(true)`.
2. On Linux, pick the **first** entry of `wine-releases` with `type === 'Proton-CachyOS'`. The
   list is sorted by date, newest first, and the first entry is normally `Proton-CachyOS-latest`.
3. `installWineVersion(release, progress → sendFrontendMessage('progressOfWineManager', version, state))`.
4. On success: `downloaded = getAlternativeWine()[0]`. That is the first Proton found, normally
   `$CFG/tools/proton/<first readdir entry>`. **Quirk:** this is not necessarily the one just
   installed. Then `GlobalConfig.setSetting('wineVersion', downloaded)` and return it.

---

## 4. Tools: DXVK, VKD3D, DXVK-NVAPI, DXMT, winetricks, winecfg, run exe

### 4.1 Tool download: `installOrUpdateTool(tool)` (`tools/index.ts:73-161`)

`Tool = {name, url, os, strip?}`. Steps:

1. Skip if `tool.os !== process.platform`.
2. `GET tool.url` with `axiosClient` (10 s timeout, keep-alive, no auth). The URL is a GitHub
   release JSON.
3. `asset = assets[0]`, the **first asset**. The exception is `dxvk-macOS`, where a `-builtin`
   asset is skipped in favour of the one ending in `repack.tar.gz`.
4. `latestVersion = asset.name` minus `.tar.gz` / `.tar.xz`. The archive goes to
   `$CFG/tools/<tool>/<asset.name>`.
5. Marker `$CFG/tools/<tool>/latest_<tool>`, first line. If it equals `latestVersion` **and**
   `$CFG/tools/<tool>/<latestVersion>` exists, do nothing.
6. `mkdir -p`, then `downloadFile({url: browser_download_url, dest: archivePath, abortSignal})`
   (section 4.7).
   - On failure show the dialog "DXVK/VKD3D error" / "Error installing DXVK/VKD3D! Check your
     connection!" (for DXMT: "DXMT Download Error"), then return.
7. Extract with `tar -xf <archive> -C $CFG/tools/<tool>/<latestVersion> --strip-components=<tool.strip ?? 1>`
   and delete the archive.
8. If the extraction status is `done`, write `latest_<tool>` = `latestVersion`, with no newline.

`DXVK.getLatest()` (`:164-204`) does nothing on Windows or when offline. Otherwise it runs these
in parallel:

| tool | URL | os | strip |
|---|---|---|---|
| `vkd3d` | `getVkd3dUrl()` | linux | 1 |
| `dxvk` | `getDxvkUrl()` | linux | 1 |
| `dxvk-nvapi` | `https://api.github.com/repos/jp7677/dxvk-nvapi/releases/latest` | linux | **0** |
| `dxvk-macOS` | `https://api.github.com/repos/Gcenx/DXVK-macOS/releases/tags/v1.10.3-20230507-repack` | darwin | 1 |

`getDxvkUrl()` / `getVkd3dUrl()` (`:803-869`) use `vulkan-helper physical-versions`, which
returns JSON `[{name,major,minor,patch}]`:
- Any GPU with Vulkan 1.3 or later: `https://api.github.com/repos/doitsujin/dxvk/releases/latest` /
  `https://api.github.com/repos/Heroic-Games-Launcher/vkd3d-proton/releases/latest`. A warning is
  logged if `vulkan-helper instance-version` is below 1.3.
- Otherwise any GPU with 1.1 or later: `.../doitsujin/dxvk/releases/tags/v1.10.3` /
  `.../Heroic-Games-Launcher/vkd3d-proton/releases/tags/v2.6`.
- Otherwise: warn and use the latest anyway.

`DXVK.getLatest()` is called at startup (`main.ts:181`) and from `installRemove` when the marker
is missing.

Resulting layout:
```
$CFG/tools/dxvk/latest_dxvk                 -> "dxvk-2.x"
$CFG/tools/dxvk/dxvk-2.x/{x32,x64}/*.dll
$CFG/tools/vkd3d/latest_vkd3d               -> "vkd3d-proton-2.x"
$CFG/tools/vkd3d/vkd3d-proton-2.x/{x86,x64}/*.dll   (+ setup_vkd3d_proton.sh)
$CFG/tools/dxvk-nvapi/latest_dxvk-nvapi     -> "dxvk-nvapi-v0.x"
$CFG/tools/dxvk-nvapi/dxvk-nvapi-v0.x/{x32,x64,layer,LICENSE,README.md}
```

### 4.2 `DXVK.installRemove(gs, tool, action: 'backup'|'restore')` (`tools/index.ts:206-525`)

These are called from `prepareWineLaunch` (backup, Wine type only) and from the IPC handlers
`toggleDXVK` / `toggleDXVKNVAPI` / `toggleVKD3D` (`main.ts:828-850`; any type).

1. If `wineVersion.bin` contains `toolkit`, return true (skip). [non-Linux] On macOS, `-DXMT`
   Wine is skipped and only `dxvk` is handled.
2. `winePrefix = gs.winePrefix` with the first `~` replaced by home. If
   `<prefix>/.update-timestamp` does not exist, log `DXVK cannot be installed on a Proton or a invalid prefix!`
   and return true. This effectively excludes Proton prefixes, whose files live under `pfx/`.
3. `is64bitPrefix = exists(<prefix>/drive_c/windows/syswow64)`. If false, warn `32-bit prefix detected!`.
4. If `$CFG/tools/<tool>/latest_<tool>` is missing, `await DXVK.getLatest()`.
5. `globalVersion` = first line of the marker.
   - `dlls32 = readdir($CFG/tools/<tool>/<ver>/<tool==='vkd3d' ? 'x86' : 'x32'>)`
   - `dlls64 = readdir(.../x64)`
   - These are all file names in the folder, not only `.dll` files. If a folder is missing,
     `readdirSync` throws and the launch errors.
6. `currentVersionCheck = <prefix>/current_<tool>` (first line).
7. **restore**:
   1. Delete `current_<tool>` (async, errors logged).
   2. For each dll name with the first `.dll` removed:
      `wine reg delete HKEY_CURRENT_USER\Software\Wine\DllOverrides /v <dll> /f`. The 64-bit
      list is used if 64-bit; the 32-bit list is always used. These run concurrently and are
      **not awaited** (`forEach(async)`).
   3. Delete the DLL files:
      - 64-bit: `<prefix>/drive_c/windows/system32/<dll64>`. **Bug:** the `syswow64` list is
        built with `concat` and the result is discarded, so the 32-bit DLLs stay.
      - 32-bit prefix: `system32/<dll32>`.
   4. `wine wineboot -u` (wait). Return true.
8. **backup** (install):
   1. If `currentVersion === globalVersion`, log `<tool> already installed!` and return true.
   2. Copy the files. These are async copies and are **not awaited**:
      - 64-bit: `x32/* → drive_c/windows/syswow64/`, `x64/* → drive_c/windows/system32/`.
      - 32-bit: `x32/* → system32/`.
   3. Register overrides (not awaited, concurrent): for each dll (64-bit list if 64-bit, plus the
      32-bit list always), with `.dll` stripped:
      `wine reg add HKEY_CURRENT_USER\Software\Wine\DllOverrides /v <dll> /d native,builtin /f`.
      Each runs via `runWineCommand(..., wait:true, protonVerb:'run')`.
   4. `dxvk-nvapi` only: `vulkan-helper nvapi-path` gives a dir. If it is non-empty, copy
      `<dir>/nvidia/wine/{nvngx.dll,_nvngx.dll}` to `system32/`, then
      `wine reg add "HKEY_LOCAL_MACHINE\SOFTWARE\NVIDIA Corporation\Global\NGXCore" /v FullPath /d C:\windows\system32 /f`.
      Otherwise warn `Could not find nvngx.dll for DLSS!`.
   5. Write `<prefix>/current_<tool>` = `globalVersion` (async).
   6. Return true.

For example, DXVK 2.x x64 contains `d3d8 d3d9 d3d10core d3d11 dxgi`, which become the registered
override names. vkd3d-proton contains `d3d12 d3d12core`. dxvk-nvapi contains `nvapi64` (x64),
`nvapi` (x32) and `nvofapi64`. All are set to `native,builtin`.

**Parity notes:**
- Because the copies and registry calls are fire-and-forget, the game may start before the
  overrides land on a first launch. A Rust port should await them; the end state is the same.
- Upgrades: when `latest_dxvk` changes (a new download at startup), the next launch re-copies,
  because `current_dxvk` differs.
- Old DLLs from a previous version are overwritten in place, not removed.

### 4.3 DXMT [non-Linux, Apple Silicon macOS] (`tools/dxmt.ts`)

- `https://api.github.com/repos/3Shain/dxmt/releases/latest`, tool `dxmt`, os darwin.
- On `wineVersionInstalled` with type `Wine-Staging-macOS`: copy the Wine to `<ver>-DXMT` and
  copy 11 DXMT files into `Contents/Resources/wine/lib/wine/{x86_64-windows,x86_64-unix,i386-windows}`.
- `Info.plist` versions get a `-DXMT` suffix.
- On `releasesInfoReady`, if `dxmt.tag` differs from the current version, update and re-copy.

### 4.4 Winetricks (`tools/index.ts:528-797`)

- **Download** (`:530-558`): skipped on Windows or when offline.
  - `GET https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks` →
    `$CFG/tools/winetricks`.
  - Uses `utils/inet/downloader`: header `User-Agent: HeroicGamesLauncher/<version>`, response
    type text, `maxCache` 7 days. The cache check is file mtime + 7 d ≥ now, in which case the
    network is skipped.
  - Then `chmod 0755`. Errors are logged as warnings.
  - Called at startup (`main.ts:183`) and lazily when the file is missing.
- **runWithArgs(runner, appName, args, returnOutput=false)**:
  1. `gs = game.getSettings()`. Return if `!validWine(gs.wineVersion)`.
  2. `winetricks = $CFG/tools/winetricks` (downloaded if missing). `gui = args.includes('--gui')`.
  3. If umu is supported:
     - `winetricks = getUmuPath()`, and `-q` is removed from args.
     - GUI: remove `--gui` and `args.unshift('')`, giving `umu-run ''`.
     - Otherwise `args.unshift('winetricks')`, giving `umu-run winetricks <verbs>`.
  4. `{winePrefix, wineVersion: w} = getWineFromProton(gs)`. With umu this stays Proton and the
     prefix is unchanged; without umu, Proton becomes `files/bin/wine` and `<prefix>/pfx`.
  5. Linux env:
     `{...process.env, ...setupEnvVars({...gs, wineVersion: w}), ...setupWineEnvVars({...gs, wineVersion: w}, appName), WINEPREFIX: winePrefix, PATH: dirname(w.bin)+':'+$PATH, GAMEID: gui ? 'winetricks-gui' : 'umu-0', UMU_RUNTIME_UPDATE: '0'}`.
     Note that `SteamGameId` here is `heroic-<appName>`, not the folder name.
  6. `checkDependencies(env)` (not awaited): run `which 7z`, `which cabextract`, `which zenity`,
     `which unzip`, `which curl` through `/bin/bash`. For each missing one append
     `<dep> not installed! Winetricks might fail to install some packages or even open`.
  7. `spawn(winetricks, args, {env})`.
     - stdout: collected if `returnOutput`, else appended to the message list (max 100 kept) and
       logged.
     - stderr: logged as an error and appended.
     - Every 1 s, if anything changed, send `sendFrontendMessage('progressOfWinetricks', {messages, installingComponent})`.
     - On exit, send `{messages:['Done']}`.
     - On error, show the dialog "Winetricks error".
  8. Resolve with the output array, or null.
- `run` = `runWithArgs(['-q','--gui'])`. This is the IPC `callTool` with `tool:'winetricks'`.
- `listAvailable` (IPC `winetricksAvailable`): run `dlls list` and `fonts list` with
  `returnOutput`. Each **stdout chunk** (not line) gives `chunk.split(' ',1)[0]`. **Quirk:**
  because this is chunk-based, the result is only the first token of each pipe read. A Rust
  port should split by lines; the parity difference is benign.
- `listInstalled(game)` (IPC `winetricksInstalled`, and logged at launch): read
  `<getWineFromProton(gs).winePrefix>/winetricks.log` and return the non-empty lines.
- `install(runner, appName, component)` (IPC listener `winetricksInstall`, and known fixes):
  send `installing-winetricks-component` with the component, run `runWithArgs(['-q', component])`,
  then send `installing-winetricks-component` with `''`.

### 4.5 winecfg, run exe, arbitrary commands (`tools/ipc_handler.ts`)

- `callTool {tool, exe?, appName, runner}`:
  - `winetricks` → `Winetricks.run`.
  - `winecfg` → `runWineCommandOnGame(runner, app, {commandParts:['winecfg'], wait:false})`, verb
    `run` (the default).
  - `runExe` → `{commandParts:[exe], wait:false, startFolder: dirname(exe)}`.
  - Afterwards, GOG only: `checkForOfflineInstallerChanges(app)` and push the possibly updated
    GameInfo with `pushGameToLibrary`.
  - Finally send status `done`.
- `runWineCommandForGame {appName, commandParts, runner}` → `runWineCommandOnGame(..., {wait:false, protonVerb:'runinprefix'})`.
- `runWineCommandOnGame(runner, app, args)` (`tools/index.ts:871-893`): if the game is native,
  log an error and return empty. Otherwise
  `runWineCommand({gameSettings, installFolderName: folder_name, gameInstallPath: install_path, commandParts, wait, protonVerb, startFolder})`.

### 4.6 Runtime-dependent tool notes

- `UMU_RUNTIME_UPDATE=0` is only set for winetricks. Game launches let umu self-update its
  Steam runtime (`~/.local/share/umu`).
- Heroic never passes `GAMEID` other than `umu-0`, a umu-database id, or the winetricks values.

### 4.7 `downloadFile({url, dest, abortSignal, progressCallback, ignoreFailure})` (`utils.ts:1364-1460`)

- First `HEAD url` with `axiosClient` to read `content-length`. On failure throw
  `Failed to get headers`.
- Then `EasyDl(url, dest, {existBehavior:'overwrite', connections: 5})`: a multi-connection
  ranged download. `dest` may be a directory, in which case the file name comes from the URL.
- Progress is throttled to 1 s. It logs `Downloaded: X / Y @Z/s (p%)` and calls
  `progressCallback(bytes, speed, percentage, writingSpeed)`.
- `dl.wait()` returning false throws `Download stopped or paused`. Abort calls `dl.destroy()`.

`axiosClient` = `axios.create({timeout: 10000, httpsAgent: keepAlive})` (`utils.ts:1578-1581`).
It sets no custom User-Agent, so axios uses its default.

---

## 5. Runtimes (umu, EAC, BattlEye) (`backend/wine/runtimes/*`)

### 5.1 `download(name)` (`runtimes.ts:25-69`)

`name ∈ {'umu','eac_runtime','battleye_runtime'}`.

1. `mkdir -p $CFG/tools/runtimes`, then `GET https://lutris.net/api/runtimes` (axiosClient). The
   response is an array of `Runtime {id, name, created_at, architecture, url}`. Find the entry
   whose `name === name`; if none, throw.
2. Log `Downloading runtime <name> with download link <url>`.
3. Download the whole file into memory (`axios GET responseType arraybuffer`; non-200 throws) and
   write it to `$CFG/tools/runtimes/<basename(url)>`.
4. `rm -rf $CFG/tools/runtimes/<name>`, `mkdir -p`, then
   `tar -xf <tar> -C <dir> --strip-components=1`.
5. Write `<dir>/current_version` = `runtime.created_at`, e.g. `2024-01-14T19:38:26.671443Z`,
   with no newline.
6. Delete the tarball. Return true. Any error logs `Failed to download runtime <name>:` and
   returns false.

### 5.2 `isInstalled(name)` (`runtimes.ts:71-90`)

- False if `$CFG/tools/runtimes/<name>` is missing.
- Otherwise fetch the list again (on failure use `[]`). If the name is not found, log an error
  and return **true**, faking installed so that it is not re-downloaded offline.
- False if `current_version` is missing. Otherwise return `created_at === current_version`.
- **Every call does a network request.**

IPC: `downloadRuntime(name)` and `isRuntimeInstalled(name)` (`runtimes/ipc_handler.ts:4-8`).

### 5.3 umu resolution (`compatibility_layers.ts:578-594`)

- `getUmuPath()`: `umu-run` on `$PATH` if found, else `$CFG/tools/runtimes/umu/umu_run.py`.
- `isUmuSupported(gs, checkUmuInstalled = true)`:
  - `isLinux`
  - `&& gs.wineVersion.type === 'proton'`
  - `&& gs.disableUMU !== true`
  - `&& (!checkUmuInstalled || exists(getUmuPath()))`
- `experimentalFeatures.umuSupport` exists in the type but is **unused**.
- umu receives: `WINEPREFIX`, `PROTONPATH` (the Proton dir, or for native the runtime name such
  as `umu-sniper`), `GAMEID`, `STORE`, `PROTON_VERB` (only through `runWineCommand`; store
  launches leave it unset, so umu defaults to `waitforexitandrun`), plus all the Proton variables
  from section 1.4.3.

### 5.4 Native games with the Steam runtime

If `gs.steamRuntime` is set (a `SteamRuntimeName`), and the game is native on Linux:
- `PROTONPATH=<runtime name>`
- `STEAM_COMPAT_INSTALL_PATH=<install path>`
- umu is appended as the last wrapper, so the final argv is `[...wrappers, umu-run, <native exe>, ...args]`.

---

## 6. Wine downloader (Wine Manager) (`backend/wine/manager/**`)

### 6.1 Sources (`downloader/constants.ts`)

| Repositorys enum | type string | URL (`?per_page=<count>` appended) | Linux default fetch |
|---|---|---|---|
| `PROTONGE` (0) | `GE-Proton` | `https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases` | yes |
| `PROTON` (1) | `Proton` | `https://api.github.com/repos/ValveSoftware/Proton/releases` | no |
| `WINELUTRIS` (2) | `Wine-Lutris` | `https://api.github.com/repos/lutris/wine/releases` | no |
| `WINECROSSOVER` (3) | `Wine-Crossover` | `https://api.github.com/repos/Heroic-Games-Launcher/wine-crossover/releases` | macOS |
| `WINESTAGINGMACOS` (4) | `Wine-Staging-macOS` | `https://api.github.com/repos/Gcenx/macOS_Wine_builds/releases` | macOS |
| `GPTK` (5) | `Game-Porting-Toolkit` | `https://api.github.com/repos/Gcenx/game-porting-toolkit/releases` | macOS |
| `PROTONCACHYOS` (6) | `Proton-CachyOS` | `https://api.github.com/repos/CachyOS/proton-cachyos/releases` | yes |

The `Type` union also contains `Wine-GE` and `Wine-Kron4ek`, but no repository is wired to them.

### 6.2 `fetchReleases({url, type, count})` (`downloader/utilities.ts:66-158`)

`GET url?per_page=<count>` (`count` = 50 from `updateWineVersionInfos`). For each release:
- `version = type.includes('Wine') ? 'Wine-' + tag_name : tag_name`
- `date = published_at.split('T')[0]`, `disksize = 0`, `release_notes_link = html_url`
- Asset selection:
  - `Wine-Staging-macOS`: the first asset whose name contains `staging`.
  - `GE-Proton` / `Proton-CachyOS`: the first asset where `isCompatibleDownload(type, url)`.
    `checksum` is the asset named `<asset name with .tar.gz|.tar.xz replaced by .sha512sum>`.
  - Others: loop over all assets. Names ending in `sha512sum` set the checksum; names ending in
    `tar.gz` or `tar.xz` set `download`/`downsize`. The **last** match wins.
- Releases without `download` are dropped.
- If any release is left, insert a synthetic entry at index 0:
  `{version: '<type>-latest', type, date, download, downsize, disksize, checksum, release_notes_link}`
  copied from the **first release whose version matches `/\d+-\d+$/`**, or else from `releases[0]`.
  (The regex skips specials like "LoL" builds.)

`isCompatibleDownload(type, download)` (`:35-55`):
- Types other than `GE-Proton` / `Proton-CachyOS`: true.
- `Proton-CachyOS`: the arch is `x86_64` (x64) or `arm64`. The URL must end with `-<arch>.tar.xz`.
- `GE-Proton`: the URL must end in `.tar.gz` or `.tar.xz`. `isArm64` means it matches
  `-(aarch64|arm64).tar.(gz|xz)$`. On arm64 the asset must be ARM; on x64 it must be non-ARM.

### 6.3 Store: `$CFG/store/wine-downloader-info.json` (`manager/utils.ts:29-35`)

```json
{ "wine-releases": [
  { "version": "Proton-CachyOS-latest", "type": "Proton-CachyOS", "date": "2026-10-20",
    "download": "https://github.com/CachyOS/proton-cachyos/releases/download/<tag>/proton-cachyos-<...>-x86_64.tar.xz",
    "downsize": 347335620, "disksize": 0,
    "checksum": "https://github.com/.../<...>-x86_64.sha512sum",
    "release_notes_link": "https://github.com/CachyOS/proton-cachyos/releases/tag/<tag>",
    // only after install:
    "installDir": "/home/u/.config/heroic/tools/proton/Proton-CachyOS-latest", "isInstalled": true, "hasUpdate": false },
  { "version": "GE-Proton11-11", "type": "GE-Proton", ... } ] }
```

The array is sorted by `Date.parse(date)` descending. The user's real file has 102 entries:
51 `Proton-CachyOS` (50 releases plus latest) and 51 `GE-Proton`.

### 6.4 `updateWineVersionInfos(fetch=false, repositorys=null, count=50)` (`manager/utils.ts:143-253`)

This is the IPC `refreshWineVersionInfo(fetch?)`.

`keepCachedRelease(r) = isCompatibleDownload(r.type, r.download) || (r.installDir && exists(r.installDir))`.

- **fetch = true:**
  1. Repos default to `[PROTONGE, PROTONCACHYOS]` on Linux. They are fetched sequentially;
     per-repo errors are logged and skipped.
  2. If the store already has `wine-releases`, take `old = cached.filter(keepCachedRelease)` and
     push each `old` entry into `releasesToStore`. Then find the matching new release (same type
     and same version; for GE-Proton, `'Proton-'+new.version === old.version`, or new
     `GE-Proton-latest` matching old `Proton-GE-latest`).
     - If `old.installDir` exists on disk and a match was found: copy `installDir`,
       `isInstalled` and `disksize` into the new entry, and set `hasUpdate = true` if
       `download` or `checksum` differ, or if `old.hasUpdate` was set.
     - If `old.installDir` exists and there is no match: `releases.push(old)`.
  3. Then each new release replaces the stored entry of the same type and version (or
     `Proton-<version>`), or is appended.
  4. Store the result sorted by date, newest first.
  5. If the store had no entry, store the fetched list as-is.
- **fetch = false:** read the cache, filter with `keepCachedRelease`, and rewrite it if anything
  was dropped.
- Always send `sendFrontendMessage('wineVersionsUpdated')`. Return the releases.

`updateWineListsIfOutdated(releasesInfo)` (`:87-141`) runs on `releasesInfoReady`, which is
emitted after `GET https://raw.githubusercontent.com/Heroic-Games-Launcher/releases-info/refs/heads/main/release-data.json`
(`utils/releases.ts:11-28`, called from `main.ts:457` via `runOnceWhenOnline`).
- `ReleasesInfo` (`common/types.ts:883-904`) maps keys
  `ge-proton|wine-ge|game-porting-toolkit|proton-cachyos|wine-staging|wine-crossover|dxvk|dxvk-mac|dxmt|vkd3d`
  to `{tag, published_at}`, plus `anticheatFiles {shaMac, shaLinux}` and `knownFixesSha`.
- On Linux, a repo is refetched (`updateWineVersionInfos(true, [repo])`) when the cached
  `GE-Proton-latest` (or `Proton-CachyOS-latest`) entry **exists** and either its download is
  incompatible, or `Date.parse(local.date) < Date.parse(published_at without the time part)`.
  If there is no cached latest entry, nothing is fetched here.

### 6.5 `installWineVersion(release, onProgress)` (`manager/utils.ts:284-373`)

This is the IPC `installWineVersion(release)`. The handler (`ipc_handler.ts:15-45`):
- Notifies `{title: version, body: t('notify.install.startInstall')}`.
- Sends progress `{status:'downloading', percentage:0, avgSpeed:0, eta:'00:00:00'}`.
- Notifies the result (error / canceled / finished), then sends `{status:'idle'}`.

Steps:

1. `mkdir -p tools/wine`, `tools/proton` (and `tools/game-porting-toolkit` on macOS).
2. `installDir = getInstallDir(release)` (`:255-282`):
   - type contains `Wine` → `$CFG/tools/wine`.
   - type contains `Toolkit` → `$CFG/tools/game-porting-toolkit`.
   - Otherwise (Proton types): if `downloadProtonToSteam && defaultSteamPath` and
     `<steam>/steam.sh` exists, use `<steam>/compatibilitytools.d` (created). Otherwise warn and
     use `$CFG/tools/proton`.
3. Abort controller id = `release.version`. The IPC `abort(version)` cancels it.
4. `installVersion({versionInfo: release, installDir, overwrite: release.hasUpdate, onProgress, abortSignal})`
   (`downloader/main.ts:188-367`):
   1. Throw if there is no `download`, or if `!isCompatibleDownload`: "Download for <v> is not
      compatible with <arch>. Refresh the compatibility-tool list."
   2. `tarFile = <installDir>/<basename(download)>`.
      `installSubDir = versionInfo.installDir || <installDir>/<version>`. The folder name is the
      **version string**, e.g. `GE-Proton11-11`, `GE-Proton-latest`, `cachyos-11.0-20261015-slr`,
      `Proton-CachyOS-latest`. This folder name later becomes the `WineInstallation.name`.
   3. If `checksum` is set, `GET checksum` as text to get `sourceChecksum`.
   4. If `installDir` exists and is not a directory, throw. If `installSubDir` exists and
      `!overwrite`, warn and return early with `disksize = du`.
   5. Remove any stale `tarFile`. `downloadFile({url: download, dest: installDir, progressCallback, abortSignal})`.
      Progress:
      `{status:'downloading', percentage, avgSpeed: bytes/s, eta: calculateEta(bytes, speed, downsize)}`.
      ETA format is `HH:MM:SS`.
   6. Compute the SHA-512 hex of the tarball. If `sourceChecksum` exists and does **not contain**
      the hex digest (substring check), delete the tarball and throw `Checksum verification failed`.
      If there is no checksum, warn.
   7. If `overwrite`, `rename(installSubDir, installSubDir + '_backup')`.
   8. `mkdir installSubDir`. Send `{status:'unzipping'}`, then
      `tar -xf <tarFile> -C <installSubDir> --strip-components=1`, then `{status:'idle'}`.
      **Quirk:** `extractFiles` returns `{status:'error'}` instead of rejecting when `tar` fails,
      so an extraction failure still counts as success. A Rust port should fail, which is a
      deliberate divergence.
   9. On a rejected unzip: `rm -rf` the subdir, delete the tar, and restore the backup.
   10. Clean up: remove `_backup` and the tarball. `disksize = du -sb installSubDir`, as an int.
       Emit `wineVersionInstalled(versionInfo, installDir)` (DXMT hook). Return
       `{versionInfo, installDir: installSubDir}`.
5. On error: if aborted, return `'abort'`, else `'error'`.
6. Update the store entry matched **by version only**:
   `{...response.versionInfo, installDir, isInstalled:true, hasUpdate:false, type}`. If no entry
   matches, return `'error'`.
7. Send `wineVersionsUpdated` and return `'success'`.

### 6.6 `removeWineVersion(release)` (`manager/utils.ts:375-427`)

This is the IPC `removeWineVersion`, which notifies `uninstalled` on success.
1. `rm -rf installDir` if it exists, then emit `wineVersionUninstalled`.
2. Update the store entry: `isInstalled=false, installDir='', disksize=0, hasUpdate=false`.
3. Send `wineVersionsUpdated`.

---

## 7. Settings

### 7.1 Files

**Global, `$CFG/config.json`:**
```json
{ "defaultSettings": { ...AppSettings... }, "version": "v0" }
```
Written with `JSON.stringify(cfg, null, 2)` (`config.ts:241-243`).

**Per game, `$CFG/GamesConfig/<appName>.json`:**
```json
{ "<appName>": { ...GameSettings subset... }, "version": "v0", "explicit": true }
```
Version handling:
- `currentGameConfigVersion = 'v0'` (`constants/others.ts:3`). v0 configs are **explicit**: the
  whole in-memory config is written.
- Version `v0.1` is non-explicit. On flush it drops the keys whose value `===` the global value.
  Only primitives compare equal.

**Mirror, `$CFG/store/config.json`:** electron-store `configStore`, keys
`userHome, language, settings, games, userData, ...`. Its `settings` key mirrors global settings
on `setSetting` / `writeConfig`.

### 7.2 Global config load (`config.ts:74-321`)

- `GlobalConfig.get()`:
  - Read `version`. If the JSON is corrupt, log an error and treat the version as `v0`. A
    missing version also means `v0`.
  - Instantiate it once (singleton). `upgrade()` is a no-op.
- `load()`: if `config.json` is missing, `resetToDefaults()`, which writes the factory defaults.
- `getSettings()`:
  - Return the cached `this.config` if set.
  - `mkdir -p GamesConfig`. If `config.json` is missing, return the factory defaults.
  - Otherwise parse it and take `defaultSettings`.
  - Back-compat: if `defaultWinePrefixDir` is unset, copy it from `defaultWinePrefix`.
  - `winePrefix = defaultSettings.winePrefix?.replace('~', home)`. Only the first `~` is
    replaced. Windows uses `''`.
  - Return `{...factoryDefaults, ...defaultSettings, winePrefix}`. Missing keys fall back to the
    defaults in memory.
- `setSetting(key, value)`:
  - Also write `configStore.settings = {...(configStore.settings || config), [key]: value}`.
  - Update memory and emit `backendEvents 'settingChanged' {key, oldValue, newValue}`. That is
    logged as `Settings key <k> changed from <old> to <new>`, **including secrets**.
  - Then `flush()`, which writes the full `{defaultSettings: config, version:'v0'}`.
- `resetToDefaults()`: set config to the factory defaults and flush.
- IPC: `requestAppSettings` → `getSettings()`.
  - `setSetting {appName:'default', key, value}` → global.
  - `writeConfig {appName:'default', config}`: logs each changed key (`Changed config: <k> from <old> to <new>`),
    calls `set(config)`, flushes, and merges into `configStore.settings` if it exists
    (`utils.ts:1583-1622`).

### 7.3 Per-game config (`game_config.ts`)

- `GameConfig.get(appName)` returns one instance per app (a Map). The version detection is the
  same as for the global config.
- The constructor calls `load()` asynchronously (not awaited), which sets `this.config = await getSettings()`.
  If `config.json` (global) is missing, `resetToDefaults()` writes `{}`.
- `getSettings()` (`:203-318`) builds the defaults from the **global** settings (copied keys):
  - `autoInstallDxvk, autoInstallDxvkNvapi, autoInstallVkd3d, DXVKFpsCap, preferSystemLibs`
  - `autoSyncSaves, enableEsync, enableMsync, enableFSR, enableFsync, enableWineWayland, enableHDR, enableWoW64, enableDXVKFpsLimit, maxSharpness`
  - `launcherArgs, nvidiaPrime, offlineMode, enviromentOptions` (a shallow copy of the array),
    `wrapperOptions, savesPath, showFps, showMangohud, targetExe, useGameMode`
  - `battlEyeRuntime, eacRuntime, beforeLaunchScriptPath, afterLaunchScriptPath, gamescope, verboseLogs, advertiseAvxForRosetta`
  - Forced: `language: ''`, `enableQuickSavesMenu: false`, `steamRuntime: false`.
  - `disableUMU: false` **only if the game file does not exist yet**. For existing files it stays
    `undefined`, which umu treats as enabled because the check is `!== true`.
  - Non-Windows: `wineVersion = global.wineVersion` and
    `winePrefix = global.winePrefix || sharedWinePrefix` (`wineCrossoverBottle` is macOS only).
  - Then `gameSettings = file[appName] || {}`. Replace the first `~` in `gameSettings.winePrefix`.
  - Return `{...defaults, ...gameSettings}`.
  - **Not inherited from global:** `ignoreGameUpdates, lastUsedLaunchOption, gogSaves, otherOptions, disableUMU` (when the file exists).
- `setSetting(key, value)`: requires `this.config` to be loaded. It sets the key, logs
  `<app>: Setting <key> to <JSON>`, and flushes.
- `flush()` (v0, explicit): writes `{[appName]: this.config, version:'v0', explicit:true}`.
  Because `this.config` came from `getSettings()` (defaults merged with the file), the **first
  setSetting freezes every inherited default into the file**. After that, global changes no
  longer propagate for those keys. This is the existing behaviour; replicate it.
- `resetToDefaults()`: config `{}` and flush.
- Store managers' `game.getSettings()` returns `GameConfig.get(id).config || await GameConfig.get(id).getSettings()`.
  This returns a cached object, so changes made through `writeConfig` are visible because
  `writeConfig` replaces `.config`.
- IPC: `requestGameSettings(appName)` → `getSettings()`.
  - `setSetting {appName, key, value}`.
  - `writeConfig {appName, config}` sets `GameConfig.config = config` and flushes.

### 7.4 Full settings schema

Legend:
- **Scope** **G** means the key is in `AppSettings` only (`common/types.ts:91-142`).
- **G→P** means it is a `GameSettings` key (`:228-272`) whose per-game default is inherited from
  global.
- **P** means per-game only.
- **Factory** is `getFactoryDefaults()` in `config.ts:323-383`; "—" means the key is absent, so it
  is `undefined` in the backend.
- **UI fallback** is the frontend `useSetting(key, fallback)` value shown when the key is
  undefined.

| Key | Type | Scope | Factory default (Linux) | UI fallback | Meaning / effect on launch |
|---|---|---|---|---|---|
| `wineVersion` | WineInstallation | G→P | `getDefaultWine()` (system wine or `{bin:'',name:'Default Wine - Not Found',type:'wine'}`) | default wine | Wine/Proton used: sections 1.4.3, 1.5.6, 1.7 |
| `winePrefix` | string | G→P | `~/Games/Heroic/Prefixes/shared` | sharedWinePrefix | `WINEPREFIX`/`STEAM_COMPAT_DATA_PATH`. `~` is expanded (first occurrence only) |
| `wineCrossoverBottle` | string | G→P (macOS) | `Heroic` | `Heroic` | [non-Linux] `CX_BOTTLE` |
| `defaultWinePrefixDir` | string | G | `~/Games/Heroic/Prefixes` | `''` | Base for suggested per-game prefixes |
| `defaultWinePrefix` | string | G | same as above | — | Legacy alias of `defaultWinePrefixDir` |
| `sharedWinePrefix` | string | G | — | `''` | Declared; the UI reads it, the backend uses the constant instead (and `uninstaller.ts:25` reads the setting) |
| `customWinePaths` | string[] | G | `[]` | `[]` | Extra Wine/Proton binaries (section 3.2) |
| `showValveProton` | bool | G | false | false | Also scan `steamapps/common` for Valve Proton |
| `downloadProtonToSteam` | bool | G | false | false | Install Proton downloads into `<steam>/compatibilitytools.d` |
| `defaultSteamPath` | string | G | section 3.3 | `''` | Steam libraries, compat dirs, Steam shortcuts |
| `autoInstallDxvk` | bool | G→P | `true` | false | Install DXVK into Wine (not Proton) prefixes at launch |
| `autoInstallVkd3d` | bool | G→P | `true` | false | Same for VKD3D-Proton |
| `autoInstallDxvkNvapi` | bool | G→P | `true` | false | Wine: install dxvk-nvapi, `DXVK_ENABLE_NVAPI=1`. Proton: `PROTON_ENABLE_NVAPI=1`, else `PROTON_DISABLE_NVAPI=1` |
| `preferSystemLibs` | bool | G→P | false | false | When false and type is `wine`: set `LD_LIBRARY_PATH`/`WINEDLLPATH`/`GST_PLUGIN_SYSTEM_PATH_1_0` from the Wine build |
| `enableEsync` | bool | G→P | true | false | `WINEESYNC=1`, or with Proton `PROTON_NO_ESYNC=1` when false |
| `enableFsync` | bool | G→P | true (Linux) | false | `WINEFSYNC=1`, or with Proton `PROTON_NO_FSYNC=1` when false |
| `enableMsync` | bool | G→P | false (Linux) | false | [non-Linux] |
| `enableFSR` | bool | G→P | — | false | `WINE_FULLSCREEN_FSR=1/0`, plus strength |
| `maxSharpness` | number | G→P | — | 5 | `WINE_FULLSCREEN_FSR_STRENGTH` (backend fallback `'2'`) |
| `enableDXVKFpsLimit` | bool | G→P | — | false | Enables `DXVK_FRAME_RATE` |
| `DXVKFpsCap` | string | G→P | — | `''` | Value of `DXVK_FRAME_RATE` |
| `showFps` | bool | G→P | false | false | `DXVK_HUD=fps` |
| `showMangohud` | bool | G→P | — | false | `mangohud --dlsym` wrapper, or `--mangoapp` with gamescope. Fails the launch if mangohud is missing |
| `useGameMode` | bool | G→P | `isFlatpak` | false | `gamemoderun` wrapper. Fails the launch if missing |
| `nvidiaPrime` | bool | G→P | false | false | `DRI_PRIME=1`, `__NV_PRIME_RENDER_OFFLOAD=1`, `__GLX_VENDOR_LIBRARY_NAME=nvidia` |
| `enableWineWayland` | bool | G→P | false | false | Proton: `PROTON_ENABLE_WAYLAND=1`. Wine: `DISPLAY=` |
| `enableHDR` | bool | G→P | false | false | With Wayland: `PROTON_ENABLE_HDR=1` / `DXVK_HDR=1` |
| `enableWoW64` | bool | G→P | false | false | `PROTON_USE_WOW64=1` / `WINEARCH=wow64` |
| `eacRuntime` | bool | G→P | true (Linux) | false | Download the EAC runtime and set `PROTON_EAC_RUNTIME` |
| `battlEyeRuntime` | bool | G→P | true (Linux) | false | Download the BattlEye runtime and set `PROTON_BATTLEYE_RUNTIME` |
| `disableUMU` | bool | G / P | false | false | Disable umu for Proton, which then runs `proton` directly |
| `steamRuntime` | `false \| SteamRuntimeName` | P (forced `false` default) | — | false | Native games: run through umu with `PROTONPATH=<runtime>` |
| `gamescope` | GameScopeSettings | G→P | — (undefined) | `{enableUpscaling:false, enableLimiter:false, enableForceGrabCursor:false, windowType:'fullscreen', gameWidth:'', gameHeight:'', upscaleHeight:'', upscaleWidth:'', upscaleMethod:'fsr', fpsLimiter:'', fpsLimiterNoFocus:'', additionalOptions:''}` | Section 1.2, step 9 |
| `enviromentOptions` | `{key,value}[]` | G→P | `[]` | `[]` | User env vars (section 1.4.1). `WINEDLLOVERRIDES` is merged |
| `wrapperOptions` | `{exe,args}[]` | G→P | `[]` | `[]` | User wrappers, after gamescope (section 1.3) |
| `launcherArgs` | string | G→P | — | `''` | Extra game args (shlex-split) |
| `targetExe` | string | G→P | — | `''` | `--override-exe` / alternative exe |
| `offlineMode` | bool | G→P | — | false | `--offline` (Legendary). Warnings |
| `language` | string | P (forced `''`) | `en` (global = UI language) | `''` | Game language. Legendary falls back to the UI language |
| `lastUsedLaunchOption` | LaunchOption | P | — | default option | Used when the launch passes none |
| `ignoreGameUpdates` | bool | P | — | false | Skip the update check; `skipVersionCheck` on protocol launch |
| `beforeLaunchScriptPath` / `afterLaunchScriptPath` | string | G→P | `''` | `''` | Section 1.8 |
| `verboseLogs` | bool | G→P | true | true | The game launch log is only written if true. Proton debug variables (section 1.4.3, #30). Legendary output goes to the game log |
| `autoSyncSaves` | bool | G→P | — | false | Download saves before launch and upload after |
| `savesPath` | string | G→P | — | `''` | Epic save path |
| `gogSaves` | GOGCloudSavesLocation[] | P | — | `[]` | GOG save locations |
| `enableQuickSavesMenu` | bool | P (forced false) | — | false | UI only |
| `advertiseAvxForRosetta` | bool | G→P | false | false | [non-Linux] |
| `otherOptions` | string | P | — | — | Deprecated, unused |
| `analyticsOptIn` | bool | G | false | false | Plausible analytics (`utils/plausible.ts`) |
| `checkUpdatesInterval` | number | G | 10 | — | Unused in the backend beyond the defaults |
| `enableUpdates` | bool | G | false | — | Unused in the backend beyond the defaults |
| `checkForUpdatesOnStartup` | bool | G | `!isFlatpak` | true | Heroic self-update check at startup |
| `autoUpdateGames` | bool | G | false | true | Auto-queue game updates (`main.ts:628-640`) |
| `addDesktopShortcuts` / `addStartMenuShortcuts` | bool | G | false | false | A.1 |
| `addSteamShortcuts` | bool | G | false | false | A.2 |
| `defaultInstallPath` | string | G | `~/Games/Heroic` | `''` | Install dialog default |
| `libraryTopSection` | enum | G | `disabled` | `disabled` | UI |
| `hideChangelogsOnStartup` | bool | G | false | false | UI |
| `maxWorkers` | number | G | 0 | 0 | `--max-workers N` for legendary/gogdl/nile installs (0 means omitted) |
| `downloadNoHttps` | bool | G | — | false | Legendary `--no-https` |
| `minimizeOnLaunch` | bool | G | false | false | Hide the window at launch unless `noTrayIcon` |
| `noTrayIcon` | bool | G | false | false | B.5 |
| `exitToTray` / `startInTray` / `darkTrayIcon` | bool | G | — | false | B.5, `main.ts:217,420` |
| `framelessWindow` | bool | G | false | false | `main_window.ts` |
| `gamepadRepeatDelay` / `gamepadInitialRepeatDelay` | number | G | 50 / 300 | 50 / 250 | UI |
| `hideWindowOnProtocolLaunch` | bool | G | false | false | A.3 |
| `disableLogs` | bool | G | — | false | Disables all log files (B.4) |
| `disablePlaytimeSync` | bool | G | — | false | GOG playtime posting |
| `disableGOGPresence` | bool | G | false | false | GOG presence |
| `discordRPC` | bool | G | — | false | Discord Rich Presence |
| `steamGridDbApiKey` | string (secret) | G | `''` | — | B.2. Never print it |
| `experimentalFeatures` | `{enableHelp, cometSupport, umuSupport?, zoomPlatform?}` | G | — | `{enableHelp:false, cometSupport:true, zoomPlatform:false}` | `cometSupport !== false` enables Comet and the GalaxyCommunication setup |
| `altLegendaryBin` / `altGogdlBin` / `altNileBin` / `altCometBin` | string | G | — | `''` | Override helper binaries (`utils.ts:457-497`) |
| `customCSS` / `customThemesPath` | string | G | — | `''` | UI themes (`main.ts:1304-1328`) |
| `disableSmoothScrolling` | bool | G | — | false | Chromium switch at startup (`main.ts:369`) |
| `disableAnimations` / `disableController` | bool | G | — | false | Frontend only |
| `startInConsoleMode` | bool | G | — | false | Opens `/console` (`main.ts:230`) |
| `maxRecentGames` | number | G | — | 5 | Recent games list and tray |
| `egsLinkedPath` | string | G | — | `''` | Epic Games Store sync path (Windows) |
| `allowInstallationBrokenAnticheat` | bool | G | — | false | Frontend install guard |

The user's real `config.json` matches the factory set above with these differences:
`useGameMode:false`, `wineVersion` = system `wine-11.17 (Staging)` at `/usr/bin/wine`, and
`steamGridDbApiKey` present.

### 7.5 Per-game file example (user data, `GamesConfig/1423049311.json`)

The file holds the full set of inherited keys, plus `wineVersion` (a Proton build in
`~/.local/share/Steam/compatibilitytools.d/...`), `winePrefix` (`~/Games/Heroic/Prefixes/Cyberpunk 2077`),
`wineCrossoverBottle: ""`, `version: "v0"` and `explicit: true`. This confirms the
"frozen defaults" behaviour from section 7.3.

---

## 8. Parity checklist and known quirks (Part 1)

1. The env precedence order is load-bearing. User `enviromentOptions` are overridden by Heroic's
   Wine variables and by known-fixes variables. `WINEDLLOVERRIDES` is appended to, not replaced.
2. `LD_PRELOAD=''` is injected unless it is set anywhere.
3. `WINE_FULLSCREEN_FSR=0` is always present for Wine launches with FSR off.
4. umu is a wrapper (store launches) or the runner (`runWineCommand`). Never both.
5. Without umu, Proton store launches go through `--wrapper '... "<proton>" waitforexitandrun'`.
6. `GAMEID=umu-0` is the default for store launches (from the wrapper env), even without umu.
   Sideload Wine launches have no `GAMEID`.
7. `steamRuntime` only affects native games.
8. DXVK, VKD3D and NVAPI installs only apply to type `wine`, and only when the prefix has
   `.update-timestamp`.
9. A missing `wineserver` makes a `wine` type invalid.
10. The display-sleep blocker id is never reset (section 1.1, step 9).
11. The `extractFiles` failure is swallowed (section 6.5).
12. A `runWineCommand` spawn error hangs (section 1.7, step 10).
13. Zoom native games ignore wrappers (section 1.5.4).
14. The sideload native cwd becomes the wrapper's directory (section 1.5.5).
15. `downloadDefaultWine` picks `getAlternativeWine()[0]`, not the installed version (section 3.7).
16. Settings-change logging prints secrets. Redact `steamGridDbApiKey` in Rust.
17. Playtime is floored on each session (section 1.1, step 17).
18. `isInstalled(runtime)` does a network call on every launch when the runtime toggles are on.

---

## 9. Where Part 2 and Part 3 connect to the launch flow

| Launch step | See |
|---|---|
| `createGameLogWriter`, log paths and format | B.4 |
| `isOnline()`, `runOnceWhenOnline` | B.6 |
| `getUmuId` (`GAMEID`) | B.1.9 |
| `gameAnticheatInfo` | A.5 |
| known fixes (`envVariables`, `winetricks`, `runInPrefix`, `wikiLink`) | A.6 |
| `getSystemInfo` / `formatSystemInfo` | A.8 |
| `syncSaves` paths | A.4 |
| protocol launches (`heroic://launch`) | A.3 |
| startup tasks (DXVK/winetricks/default Wine at +2.5 s, release-info fetch) | B.8 |
| IPC channel names | B.9 |

---

# Part 2: Shortcuts, protocol, save paths, anticheat, known fixes, overrides, system info


## A.0 Conventions for this section

- All refs are relative to `src/` of Heroic v2.22.4.
- `$CFG` = Heroic's app folder = `app.getPath('appData')/heroic` = `~/.config/heroic` on native Linux, `~/.var/app/com.heroicgameslauncher.hgl/config/heroic` under Flatpak (`backend/constants/paths.ts:8,23`). With `CI=e2e` it is a temp dir instead.
- `$USERDATA` = `app.getPath('userData')`. Because package.json `name` is `heroic`, this is the same directory as `$CFG` on Linux. electron-store files whose `cwd` is relative (`'store'`, `'store_cache'`) resolve under `$USERDATA`.
- electron-store semantics: the file is pretty-printed JSON (`<cwd>/<name>.json`). Keys use **dot notation**, so `a.b` makes a nested object. With `clearInvalidConfig: true`, a corrupt file is silently reset to `{}`.
- IPC: `addHandler(name, fn)` is `ipcMain.handle` (request/response), and `addListener(name, fn)` is `ipcMain.on` (fire-and-forget). `sendFrontendMessage(name, ...args)` pushes from the backend to the renderer.
- "Notify" means `notify({title, body})`, a desktop notification. "Error dialog" means `showDialogBoxModalAuto({title, message, type:'ERROR'})`.
- Windows/macOS-only branches are only mentioned. The Linux behaviour is specified in full.

---

## A.1 Desktop / Start-menu shortcuts (`backend/shortcuts/**`)

### A.1.1 Files

| File | Purpose |
|---|---|
| `backend/shortcuts/ipc_handler.ts` | IPC for shortcuts and Steam |
| `backend/shortcuts/shortcuts/shortcuts.ts` | `.desktop` / `.lnk` / macOS `.app` creation and removal |
| `backend/shortcuts/shortcuts/macos_shortcut.ts` | macOS `run.sh` command builder (macOS only) |
| `backend/shortcuts/utils.ts` | Icon resolution and download, image write/remove helpers |
| `backend/shortcuts/types.ts` | `ShortcutsResult { success: boolean; errors: string[] }` |
| `backend/shortcuts/nonesteamgame/*` | Steam non-Steam-game shortcuts (see A.2) |

### A.1.2 IPC (`backend/shortcuts/ipc_handler.ts`)

| Channel | Kind | Args | Returns / effect |
|---|---|---|---|
| `addShortcut` | listener | `(appName, runner, fromMenu: boolean)` | `:14`. Calls `getGame(appName, runner).addShortcuts(fromMenu)` **without awaiting**, then **immediately** notifies: title `t('box.shortcuts.title','Shortcuts')`, body `t('box.shortcuts.message','Shortcuts were created on Desktop and Start Menu')` (mac: `box.shortcuts.message-mac` "Shortcuts were created on the Applications folder"). The notification fires even if creation fails. |
| `shortcutsExists` | handler (sync) | `(appName, runner)` | `:33`. Takes `title` from the game info, computes `shortcutFiles(title)`, and returns `existsSync(desktop) \|\| existsSync(menu)`. |
| `removeShortcut` | listener | `(appName, runner)` | `:41`. Calls `removeShortcuts()` (not awaited), then notifies title `box.shortcuts.title` (default "Shortcuts Removed") and body `box.shortcuts.message-remove` "Shortcuts were removed from Desktop and Start Menu" (mac: `-remove-mac`). |
| `addToSteam` | handler | `(appName, runner)` | `:60`. Returns `Promise<boolean>` from `addNonSteamGame` (A.2). |
| `removeFromSteam` | handler | `(appName, runner)` | `:65`. Returns `Promise<boolean>` from `removeNonSteamGame`. |
| `isAddedToSteam` | handler | `(appName, runner)` | `:70`. Returns `Promise<boolean>` from `isAddedToSteam`. |

Each store's `Game.addShortcuts(fromMenu?)` and `removeShortcuts()` delegates to `shortcuts.ts`. These are also called automatically:
- after install/import: legendary `games.ts:655,832`, gog `games.ts:226,494`, nile `games.ts:142,263`
- on a new sideload app: `sideload/library.ts:72`, which passes no `fromMenu`

Uninstall calls `removeShortcuts` and `removeNonSteamGame` (e.g. legendary `games.ts:766-767`, gog `:1001-1003`, nile `:510-511`, sideload `:130-132`, zoom `:785-786`).

### A.1.3 `shortcutFiles(gameTitle)` (`shortcuts.ts:149-175`)

The title is first passed through npm `sanitize-filename`, with `''` as the replacement:
- It removes `/ ? < > \ : * | "`, C0/C1 control characters, and the names `.` and `..`.
- It removes Windows-reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`, with or without an extension) and trailing dots or spaces.
- It truncates the result to 255 UTF-8 bytes.

| Platform | desktopFile | menuFile |
|---|---|---|
| linux | `${app.getPath('desktop')}/${title}.desktop` (XDG desktop dir, usually `~/Desktop`) | `${HOME}/.local/share/applications/${title}.desktop` |
| win32 | `${desktop}\\${title}.lnk` | `${appData}\\Microsoft\\Windows\\Start Menu\\Programs\\${title}.lnk` |
| darwin | `~/Applications/${title}.app` (same path used for both) | same |

### A.1.4 `addShortcuts(game, fromMenu?)` (`shortcuts.ts:34-117`)

1. If `gameInfo.install.is_dlc`, return immediately.
2. Read the global settings `addDesktopShortcuts` (default `false`), `addStartMenuShortcuts` (default `false`) and `addSteamShortcuts` (default `false`) (`backend/config.ts:331-336`).
3. If `addSteamShortcuts`, call `addNonSteamGame(game)` **without awaiting**. This happens even when `fromMenu` is set.
4. Build `launchWithProtocol = heroic://launch?appName=${app_name}&runner=${runner}`. Nothing is URL-encoded.
5. On Linux, `icon = await getIcon(app_name, gameInfo)` (A.1.6). If `getIcon` throws (for example when `art_square` is undefined), the promise rejects and no file is written.
6. The file content is exact, with LF line endings and a trailing newline:
   ```
   [Desktop Entry]
   Name=${gameInfo.title}
   Exec=xdg-open ${launchWithProtocol}
   Terminal=false
   Type=Application
   Icon=${icon}
   Categories=Game;
   ```
   - `Name` is the **unsanitized** title.
   - `Exec` is unquoted, and the `&` is not escaped.
7. If `addDesktopShortcuts || fromMenu`, write the desktop file asynchronously with `fs.writeFile` mode `0o777` (umask then applies, typically giving 0755). The callback ignores the error argument and always logs `Shortcut saved on <path>`.
8. If `addStartMenuShortcuts || fromMenu`, write the menu file with the default mode `0o666` (minus umask) and the same log.
9. No parent directories are created: neither `~/Desktop` nor `~/.local/share/applications`.
- Windows (`:79-109`): `shell.writeShortcutLink` with target = protocol URL; icon = install_path + executable, `iconIndex` 0.
- macOS (`:110-115`, `generateMacOsApp` `:177-251`) writes the following inside the `.app` bundle:
  - `Contents/Info.plist`
  - `Contents/Resources/shortcut.icns`, a 512×512 `ic11` icon converted from the icon
  - `Contents/MacOS/run.sh`, mode 755, which runs `<exe> --no-gui "heroic://launch?appName=..&runner=.."` (`macos_shortcut.ts:1-8`)

### A.1.5 `removeShortcuts(game)` (`shortcuts.ts:124-147`)

- DLCs are skipped.
- On Linux, `fs.unlink(desktopFile)` and `fs.unlink(menuFile)` run asynchronously. Errors are ignored, and "Desktop shortcut removed" / "Applications shortcut removed" are logged regardless.
- On macOS, the `.app` is removed with `rm -rf`.
- Files are located by the *current* sanitized title. If the title changed, the old files are not found.

### A.1.6 `getIcon(appName, gameInfo)` (`backend/shortcuts/utils.ts:61-96`)

- `iconsFolder = $CFG/icons` (`paths.ts:30`). It is created with a non-recursive `mkdirSync` if missing.
- Default:
  - `image = art_square.replaceAll(' ', '%20').replace('{ext}', 'jpg')` (only the first `{ext}` is replaced)
  - `icon = $CFG/icons/${appName}.jpg`
- GOG only:
  1. If `<install_path>/goggame-${appName}.ico` exists, **return it** and download nothing.
  2. Otherwise, if `<install_path>/support/icon.png` exists, return it. This is the Linux-native GOG installer layout.
  3. Otherwise call `GET https://api.gog.com/products/${appName}?locale=${i18next.language}` (`gog/library.ts:1393-1417`, axios timeout 10 s, errors give `null`). If `data.images.icon` is present:
     - `image = 'https:' + icon` (the value is protocol-relative)
     - `icon = $CFG/icons/${appName}.png`
  - `install_path!` is assumed. A missing install path throws inside `path.join`.
- `checkImageExistsAlready(icon)` (`utils.ts:48-59`) returns true if `<dir>/<basename-without-ext>.png` **or** `.jpg` exists.
  - If false, `downloadImage(image, icon)` is called. It runs `downloadFile` from `backend/utils.ts:1364`: an axios `HEAD`, then EasyDl with 5 connections and `existBehavior: 'overwrite'`.
  - That call is **not awaited**. The icon path is returned before the file exists, and download errors become unhandled rejections or log lines. The sync `try/catch` only catches synchronous throws.
  - Quirk: if `<appName>.jpg` exists but the GOG branch chose `.png`, nothing is downloaded and the returned `.png` path does not exist.
- Other helpers in `utils.ts`:
  - `createImage(buffer, path)` (`:9`) uses `writeFileSync(..., {encoding:'ascii'})`. The buffer is written raw. It returns an error string or `undefined`.
  - `removeImage(path)` (`:39`) uses `unlinkSync` and returns an error string.

---

## A.2 Steam non-Steam-game shortcuts (`backend/shortcuts/nonesteamgame/**`)

### A.2.1 Constants (`nonesteamgame/constants.ts`)

| Name | Value |
|---|---|
| `transparentSteamLogoHex` | `89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000b494441541895636000020000050001d69f16cd0000000049454e44ae426082` (a 1×1 transparent PNG) |
| `pictureExt` | `.jpg` |
| `coverArtSufix` | `p.jpg` |
| `backGroundArtSufix` | `_hero.jpg` |
| `logoArtSufix` | `_logo.jpg` |
| `steamDBBaseURL` | `https://cdn.cloudflare.steamstatic.com/steam/apps` |

### A.2.2 Steam path and user discovery

- `getSteamPath()` (`nonesteamgame.ts:59-62`) is `GlobalConfig.defaultSteamPath.replaceAll("'", '')`.
- Default (`backend/config.ts:39-65`, Linux):
  - If `~/.var/app/com.valvesoftware.Steam/.steam/steam` exists **and** `flatpak info com.valvesoftware.Steam` exits 0, use that path.
  - Otherwise use `~/.steam/steam`.
  - Windows: `%PROGRAMFILES(X86)%\Steam`. macOS: `~/Library/Application Support/Steam`.
- `steamUserdataDir = <steamPath>/userdata`.
- `checkSteamUserDataDir(dir)` (`:121-147`):
  - If the directory is missing, return `{folders:[], error:"<dir> does not exist. Can't add/remove game to/from Steam!"}`.
  - Otherwise list subdirectories (directories only), excluding names `0` and `ac`.
  - If none remain, return error `"<dir> does not contain a valid user directory!"`.
  - Every remaining folder is treated as a Steam user, and is processed for **all** users.
- Per-user paths:
  - `cfg = userdata/<uid>/config`
  - `cfg/shortcuts.vdf`
  - `cfg/grid/`

### A.2.3 Is Steam running? (`nonesteamgame/steamProcess.ts:39-65`)

- If `isSteamDeckGameMode` (env `XDG_CURRENT_DESKTOP === 'gamescope'`, `constants/environment.ts:27`), return `true` always.
- Windows: `reg query HKCU\Software\Valve\Steam\ActiveProcess /v pid`, parse `REG_DWORD 0x..`, then check the pid is alive.
- Linux/mac: read pid files `<steamPath>/../steam.pid`, then `<steamPath>/steam.pid`. For each, `parseInt(trim)`.
- Alive check is `kill(pid, 0)`. It counts as alive if the call succeeds **or** fails with `EPERM`.
- If no live pid is found, return `false`.

### A.2.4 SteamClient API over CEF remote debugging (`nonesteamgame/steamClient.ts`)

| Constant | Value |
|---|---|
| `CEF_DEBUGGER_URL` | `http://127.0.0.1:8080` |
| `EVAL_TIMEOUT_MS` | 10000 |
| `SOCKET_URL_CACHE_MS` | 5000 |

- **Enable flag:** Steam only exposes this interface if the empty file `<steamPath>/.cef-enable-remote-debugging` exists when Steam starts. Heroic creates it when needed (`nonesteamgame.ts:369-381`, `enableSteamClientApiOnNextStart`).
  - There is **no programmatic Steam restart** anywhere in the code. The "restart logic" consists only of creating this flag and showing a message that asks the user to restart Steam.
- **Discovery** (`:36-63`):
  - `GET http://127.0.0.1:8080/json` (timeout 3000 ms). The response is an array of `{title, webSocketDebuggerUrl}`.
  - Pick the entry whose `title === 'SharedJSContext'` and cache its URL for 5 s.
  - Any error, or a non-array response, gives `null` and clears the cache.
  - `isSteamClientApiAvailable()` is `url !== null`.
- **Eval** (`:80-152`):
  - Open a WebSocket to that URL.
  - On open, send `{"id":1,"method":"Runtime.evaluate","params":{"expression":<js>,"awaitPromise":true,"returnByValue":true}}`.
  - Wait for the message with `id === 1`. If it has `result.exceptionDetails`, reject with `exception.description ?? text ?? 'SteamClient call failed'`. Otherwise resolve `result.result.value`.
  - Socket error or early close clears the URL cache and rejects. The timeout is 10 s.
- **JS expressions** (exact semantics, all async IIFEs):
  - `getSteamClientShortcuts` (`:154-182`):
    - Loop over `appStore.allApps.filter(a => a.BIsShortcut())`.
    - Call `appDetailsStore.GetAppDetails(appid)`. If it is missing, call `RequestAppDetails`, then poll up to 10× at 100 ms.
    - Push `{appid, appName: details?.strDisplayName ?? app.display_name ?? '', exe: details?.strShortcutExe ?? '', launchOptions: details?.strShortcutLaunchOptions ?? ''}`.
    - A non-array result throws "Could not get the shortcut list from the Steam client".
  - `addShortcutViaSteamClient` (`:184-211`), with all strings JSON-encoded:
    1. `appId = await SteamClient.Apps.AddShortcut(name, exe, '', '')`. If it is not a number or is ≤ 0, throw.
    2. `SetShortcutName`, `SetShortcutExe`, `SetShortcutStartDir`, `SetShortcutLaunchOptions`.
    3. `SetShortcutIcon` only if an icon is known.
    4. Return `appId`.
  - `removeShortcutViaSteamClient(appId)` (`:213-220`) calls `SteamClient.Apps.RemoveShortcut(trunc(appId))`.
  - `setShortcutArtworkViaSteamClient` (`:229-258`):
    - Skipped if the file is missing.
    - Calls `SteamClient.Apps.SetCustomArtworkForApp(appId, <base64 file>, <ext without dot, default 'jpg'>, assetType)`.
    - Errors are only logged as warnings.
    - `steamClientArtworkTypes = {grid:0, hero:1, logo:2, wideGrid:3}`.

### A.2.5 App-ID computation (`nonesteamgame/steamhelper.ts:198-223`; ported from steam-rom-manager)

```
crc  = CRC32_IEEE(utf8(exe + appname))   // zlib.crc32; exe is the *quoted* Exe string, appname = AppName (title)
top  = crc | 0x80000000                  // u32
prelim = (top << 32) | 0x02000000        // u64
generateAppId       = decimal(prelim)            // "Big Picture" grid id (u64)
generateShortAppId  = decimal(top)               // grid file id (u32)
generateShortcutId  = top - 2^32  (i32, negative) // value stored as `appid` in shortcuts.vdf
gridAppIDsFromShortAppId(s) = { bigPictureAppID: decimal((s<<32)|0x02000000), otherGridAppID: decimal(s) }  (nonesteamgame.ts:464-474)
```

- Ids from Steam (CEF or VDF) may be negative i32. They are converted with `appid >>> 0` (u32) before grid naming.

### A.2.6 Exe / StartDir / LaunchOptions (`buildShortcutEntryConfig`, `nonesteamgame.ts:311-362`)

- Default: `exe = "\"" + app.getPath('exe') + "\""` and `startDir = "\"" + dirname(exe) + "\""`. Both values include the literal double quotes.
- Linux distro-Electron case: if `exePath.toLowerCase()` does not end with `heroic`:
  - Search `$PATH` for a `heroic` entry (`utils/os/path/index.ts`: first `stat`-able `join(dir,'heroic')`).
  - If found, use it for `exe` and its directory for `startDir`.
- Override precedence: Flatpak first, then AppImage, then Windows portable.
  - Flatpak (`FLATPAK_ID` set): `exe = "\"flatpak\""`, `startDir = "\"$HOME\""`.
  - Otherwise, AppImage (`APPIMAGE` set, not Windows): `exe = "\"$APPIMAGE\""`, `startDir = "\"dirname($APPIMAGE)\""`.
  - Windows portable: `PORTABLE_EXECUTABLE_FILE` / `PORTABLE_EXECUTABLE_DIR`.
- `launchOptions = '--no-gui' + (non-Windows ? ' --no-sandbox' : '') + ' "heroic://launch?appName=<app_name>&runner=<runner>"'`.
- If Flatpak, prefix `run com.heroicgameslauncher.hgl `.

### A.2.7 Heroic-shortcut matching (`matchesHeroicGame`, `nonesteamgame.ts:259-291`)

A shortcut `{launchOptions, exe?, shortcutPath?}` belongs to the game if either of these holds:
- `runner` is set and `exe` or `shortcutPath` **contains** the wrapper path `~/.local/share/applications/heroic-steam-shortcuts/<runner>-<app_name>.desktop`.
- `launchOptions` contains `heroic://launch` **and** contains any of:
  - `appName=<app_name>&runner=<runner>`
  - `launch/<app_name>"`
  - `launch/<runner>/<app_name>"`

  The last two are legacy path-style URLs.

Other points:
- Titles are never compared, so renaming the entry in Steam is harmless and other tools' entries are never touched.
- VDF entries are read case-insensitively through `getAppName`, `getLaunchOptions`, `getExe` and `getShortcutPath` (`:202-257`). Missing values become `''`.
- `checkIfAlreadyAdded` (`:450-458`) returns the **first** matching index or -1. Duplicates are not handled.

### A.2.8 `shortcuts.vdf` binary format and handling

Heroic uses npm `steam-shortcut-editor@^3.1.3`:
- Read: `parseBuffer(buf, {autoConvertArrays:true, autoConvertBooleans:true, dateProperties:['LastPlayTime']})` (`:155-163`)
- Write: `writeBuffer(obj)` (`:172-195`)

The Rust implementation must reproduce the format **byte-exactly**. Tests assert that remove-after-add gives a byte-identical file (`__tests__/nonesteamgame.test.ts:80-112`).

- **Grammar:** a sequence of `type:u8, key:cstring, value`, where `cstring` is UTF-8 terminated by NUL.

  | Type | Value |
  |---|---|
  | `0x00` | nested map; children follow, terminated by `0x08` |
  | `0x01` | string (cstring) |
  | `0x02` | int32 little-endian, **signed** (e.g. `appid` bytes `a4 6e ac 9e` → negative) |
  | `0x08` | end of the current map |

  Steam's format also defines `0x03` float32 and `0x07` uint64. Unknown types should be preserved verbatim if feasible.
- **Root layout:** `00 "shortcuts" 00 { 00 "<i>" 00 <fields...> 08 }* 08 08`.
  - The entry keys are the decimal array index (`"0"`, `"1"`, …), renumbered on write.
  - An empty file is exactly `00 73 68 6f 72 74 63 75 74 73 00 08 08` (test `:131-165`).
- **Conversions:**
  - A map with index keys becomes an array. An empty `shortcuts` map becomes `[]`.
  - int 0/1 on boolean fields becomes bool, and is written back as int32 0/1.
  - `LastPlayTime` int32 epoch **seconds** becomes a Date, and is written back as int32 seconds.
  - Unknown keys and their order must round-trip unchanged, including `tags` (nested map), `FlatpakAppID`, `DevkitGameID`, and `ShortcutPath`.
- **Key case:** Steam writes `appid`, `AppName` (older files: `appname`), `Exe`, `StartDir`, `icon`, `ShortcutPath`, `LaunchOptions`, `IsHidden`, `AllowDesktopConfig`, `AllowOverlay`, `OpenVR`, `Devkit`, `DevkitGameID`, `DevkitOverrideAppID`, `LastPlayTime`, `FlatpakAppID`, `tags`.
- **Validation** (`checkIfShortcutObjectIsValid`, `:418-442`):
  - The root must have `shortcuts`, and it must be an array.
  - Every entry must have `AppName`, `Exe` and `LaunchOptions` (case-insensitive).
  - On failure the error strings are `Could not find entry "shortcuts"!`, `Entry "shortcuts" is not an array!`, and `One of the game entries is missing the <Key> parameter!`.
- **Atomic write** (`:172-195`):
  1. Write to `<file>.tmp`.
  2. `rename` it over the file.
  3. On error, delete the tmp file (warn if that fails) and return the error string.
- **New entry written by Heroic** (`:768-811`). The field order is the insertion order and therefore the byte order:

  | Key | VDF type | Value |
  |---|---|---|
  | `AppName` | 0x01 | `gameInfo.title` |
  | `Exe` | 0x01 | quoted exe (A.2.6) |
  | `StartDir` | 0x01 | quoted dir |
  | `appid` | 0x02 | `generateShortcutId(Exe, AppName)` (negative i32) |
  | `icon` | 0x01 | `getIcon()` path. Only present if `getIcon` resolved; a rejection only logs a warning. |
  | `LaunchOptions` | 0x01 | A.2.6 |
  | `IsHidden` | 0x02 | 0 |
  | `AllowDesktopConfig` | 0x02 | 1 |
  | `AllowOverlay` | 0x02 | 1 |
  | `OpenVR` | 0x02 | 0 |
  | `Devkit` | 0x02 | 0 |
  | `DevkitOverrideAppID` | 0x02 | 0 |
  | `LastPlayTime` | 0x02 | Seconds of `tsStore['<app_name>'].lastPlayed` (ISO string), or now. Source: `$USERDATA/store/timestamp.json`, shape `{ "<appName>": { "firstPlayed": iso, "lastPlayed": iso, ... } }`. |

### A.2.9 Grid artwork (`prepareImagesForSteam`, `steamhelper.ts:39-143`)

- Grid dir is `<cfg>/grid`, created with a non-recursive mkdir if missing.
- With `S = otherGridAppID` (u32 decimal) and `steamID` taken from `getWikiGameInfo(game)` as `pcgamingwiki.steamID ?? gamesdb.steamID`, the files are:

  | File | Role | Source if steamID | Source otherwise |
  |---|---|---|---|
  | `S` + `p.jpg` | portrait cover (Steam asset "grid", 0) | `${steamDBBaseURL}/${steamID}/library_600x900_2x.jpg` | `gameInfo.art_square` |
  | `S` + `.jpg` | wide header ("wideGrid", 3) | `.../header.jpg` | `gameInfo.art_cover` |
  | `S` + `_hero.jpg` | hero ("hero", 1) | `.../library_hero.jpg` | Generated 1920×620 JPEG (see below) |
  | `S` + `_logo.jpg` | logo ("logo", 2) | `.../logo.png` (PNG bytes saved under a `.jpg` name) | `gameInfo.art_logo` if truthy, else the 1×1 transparent PNG bytes are **always written** synchronously (overwrites), to hide Steam's title text |
  | `<bigPictureAppID>.jpg` | only removed, never written | | |

- **Generated hero image:** the backend calls `getMainWindow().webContents.executeJavaScript('window.imageData("<art_cover>", 1920, 620)')` (`steamhelper.ts:23-37`). The renderer (`frontend/index.tsx:176`) draws onto a 1920×620 canvas:
  1. The image scaled to full width, vertically centred, blur 10 px.
  2. The image scaled to full height, horizontally centred, unblurred.

  It returns `toDataURL('image/jpeg', 0.9)`. With no window, the result is `''` and the image is skipped.
- **Write loop:** for each entry, skip it if `checkImageExistsAlready(path)` (a `.png` or `.jpg` sibling exists) or the source is empty.
  - Sources starting with `http` go to `downloadImage`, which is fire-and-forget (not awaited).
  - Anything else is treated as a data URL: `nativeImage.createFromDataURL(src).toJPEG(90)` is written synchronously.
  - A non-data-URL local path gives an empty image, so a 0-byte file is written. Treat that as a quirk.
  - `{ext}` placeholders in art URLs are **not** substituted here, unlike `getIcon`.
- Errors are collected and logged as `Preparing Steam images for <title> failed with:`.
- Returns `{coverArt, headerArt, backGroundArt, logoArt}`.
- **Removal** (`removeImagesFromSteam`, `:145-196`):
  - If the grid dir is missing, return.
  - For each of `Sp.jpg`, `S.jpg`, `S_hero.jpg`, `<bigPicture>.jpg`, `S_logo.jpg`: if a `.png` or `.jpg` sibling exists, `unlink` the **`.jpg` path**. If only a `.png` exists, the unlink fails and the error is logged.

### A.2.10 `addNonSteamGame(game)` (`nonesteamgame.ts:647-863`)

**Path 1: Steam running and CEF available** (`:652-655` → `addNonSteamGameViaSteamClient` `:558-640`):
1. List the shortcuts and find a match (A.2.7). If found, reuse its appid. Otherwise build the config, call `getIcon`, and run `addShortcutViaSteamClient({name: title, exe, startDir, launchOptions, icon})`.
2. Compute `short = appid>>>0`, then run `prepareImagesForAllUsers` (`:480-507`). It calls `prepareImagesForSteam` for every user whose `config` dir exists (no mkdir) and returns the image paths of the **last** user.
3. Push artwork through `SetCustomArtworkForApp`: cover → 0, hero → 1, logo → 2, header → 3. Because downloads are not awaited, the files may not exist yet, in which case they are silently skipped.
4. Notify title `notify.finished.add.steam.title` "Added to Steam", body `notify.finished.add.steam.success-no-restart` "{{game}} was successfully added to Steam.". Return `true`.
5. On any throw: error dialog `box.error.add.steam.title` "Error Adding Game to Steam", body `box.error.add.steam.body` "Adding {{game}} to Steam failed with:\n {{error}}". Return `false`.

**Path 2: Steam running and no CEF** (`:657-718`):
1. Create the `.cef-enable-remote-debugging` flag.
2. If `!(await isAddedToSteam(game))`:
   1. Build the config and call `getIcon`.
   2. Run `addNonSteamGameViaUrlHandler` (A.2.11).
   3. If that returns false, run `showSteamRunningError` (`:383-411`) and return `false`. That function:
      - ensures the flag exists
      - logs `Can't add "<title>" to Steam while Steam is running.`
      - shows an error dialog. The body is `box.error.steam.running-gamemode` ("A one-time Steam restart is required before Heroic can manage Steam shortcuts on this device. Restart Steam and try again.") in Game Mode, otherwise `box.error.steam.running` ("Steam is running. Close Steam and try again, or restart Steam once to let Heroic manage shortcuts while Steam is running.").
   4. Otherwise poll with `findWrapperShortcutAppId` (`:515-551`): up to 20 attempts, 500 ms apart, over every user's `shortcuts.vdf`. Look for the entry with `ShortcutPath === wrapperFile` and a numeric `appid`. Read errors are warned and skipped.
   5. If an id is found, run `prepareImagesForAllUsers` with `gridAppIDsFromShortAppId(appid>>>0)`. Otherwise log the warning "Could not find the Steam shortcut of <title> to set its artwork."
3. Notify with "success-no-restart" and return `true`. This also happens if the game was already added.

**Path 3: Steam not running** (VDF edit, `:720-862`):
1. Get the wiki info for `steamID`.
2. Check the users. On error: log, show the error dialog, return `false`.
3. For each user:
   1. `mkdir cfg` if missing (non-recursive).
   2. If `shortcuts.vdf` is missing, write an empty one.
   3. Read and parse it. A parse exception propagates as a rejected promise.
   4. If the file fails validation, push `Can't add "<title>" to Steam user "<uid>". "<file>" is corrupted!` plus the validation errors, and continue.
   5. If the game is already present, set `added = true` and continue.
   6. Otherwise build the entry, call `getIcon`, and run `prepareImagesForSteam` with `bigPictureAppID = generateAppId(Exe, AppName)` and `otherGridAppID = generateShortAppId(Exe, AppName)`.
   7. Push the entry and write atomically. A write error is pushed and the loop continues; on success set `added = true`.
4. If `!added`: log, show the error dialog with the joined errors, return `false`.
5. If there are no errors: log `<title> was successfully added to Steam.` and notify `notify.finished.add.steam.success` "{{game}} was successfully added to Steam. A restart of Steam is required for changes to take effect.".
6. Otherwise: warn `<title> could not be added to all found Steam users.`, log the errors, and notify `notify.finished.add.steam.corrupt` "{{game}} could not be added to all found Steam users. See logs for more info. A restart of Steam is required for changes to take effect.". This still returns `true`.

### A.2.11 `steam://addnonsteamgame` URL-handler fallback (`nonesteamgame/steamUrlHandler.ts`)

- Only on Linux and not Flatpak. Otherwise it returns `false`.
- `wrapperDir = ~/.local/share/applications/heroic-steam-shortcuts` (mkdir -p).
- `wrapperFile = <wrapperDir>/<runner>-<app_name>.desktop` (`:24-29`).
- `escapeDesktopValue` (`:31-37`) replaces `\`→`\\`, LF→`\n`, CR→`\r`, TAB→`\t`.
- File content (`:66-75`):
  ```
  [Desktop Entry]
  Type=Application
  Name=<escaped title>
  Exec=<exe as quoted> <launchOptions>        (NOT escaped)
  Path=<escaped startDir with all '"' removed>
  Icon=<escaped icon>                          (line only if icon known)
  Terminal=false
  NoDisplay=true
  ```
- Steps:
  1. Write the empty sentinel `os.tmpdir()/addnonsteamgamefile`. Steam only accepts local paths while it exists.
  2. Run `execAsync('steam steam://addnonsteamgame/' + encodeURIComponent(wrapperFile), {timeout:15000})`. This is a shell exec.
  3. Return `true` on success. Any throw is logged and gives `false`.
- `removeSteamShortcutWrapper` (`:101-117`) unlinks the wrapper if it exists. Errors are warned.
- Steam sets the entry's `ShortcutPath` to the `.desktop` path and `Exe` from `Exec`. Matching therefore works through the wrapper path (A.2.7).

### A.2.12 `removeNonSteamGame(game)` (`nonesteamgame.ts:941-1065`)

**Steam running and CEF available** (`removeNonSteamGameViaSteamClient` `:869-933`):
1. Find the match. If there is none, return `true` silently.
2. Call `RemoveShortcut(appid)` and remove the wrapper.
3. For each user with an existing `cfg`, remove the images twice: once for `gridAppIDsFromShortAppId(appid>>>0)`, and once for ids computed from `(existing.exe, existing.appName)`.
4. Notify `notify.finished.remove.steam.title` "Removed from Steam", body `notify.finished.remove.steam.success-no-restart` "{{game}} was successfully removed from Steam.". Return `true`.
5. On a throw: error dialog `box.error.remove.steam.title` "Error Removing Game from Steam", body `box.error.remove.steam.body` "Removing {{game}} from Steam failed with:\n {{error}}". Return `false`.

**Steam running and no CEF:** run `showSteamRunningError(adding:false)` (which creates the flag) and return `false`.

**Steam not running:**
1. If the user dir check errors, warn and return `true`.
2. For each user that has `cfg/shortcuts.vdf`:
   1. Parse it. If it is invalid, push `Can't remove "<title>" from Steam user "<uid>". "<file>" is corrupted!` plus the validation errors.
   2. Find the index. If it is -1, skip.
   3. Splice the entry out and write atomically. A write error is pushed.
   4. Set `removed = true`.
   5. Remove the images using `gridAppIDsFromShortAppId(entry.appid>>>0)` if `appid` is a number, otherwise the ids computed from `(Exe, AppName)`.
3. If `removed`, remove the wrapper.
4. If there are no errors:
   - If nothing was removed, return `true` with no notification.
   - Otherwise log `<title> was successfully removed from Steam.`, notify `notify.finished.remove.steam.success` "... A restart of Steam is required for changes to take effect.", and return `true`.
5. Otherwise warn, log the errors, notify `notify.finished.remove.steam.corrupt`, and return **`false`**.

### A.2.13 `isAddedToSteam(game)` (`nonesteamgame.ts:1072-1121`)

1. If Steam is running and CEF is available, return `some(matchesHeroicGame)`. On a CEF error, warn and fall through.
2. If the user dir check errors, return `false`.
3. Return true if any user's valid `shortcuts.vdf` contains a match. Corrupt files are skipped.

---

## A.3 `heroic://` protocol (`backend/protocol.ts`)

### A.3.1 Entry points (`backend/main.ts`)

| Source | Code | Behaviour |
|---|---|---|
| Second instance | `main.ts:313-321` (`app.on('second-instance', argv)`) | Shows the main window unless `shouldHideWindowForProtocolArgs(argv)`, then runs `handleProtocol(argv)`. |
| In-app navigation | `main.ts:399-402`, `protocol.handle('heroic', req)` | Runs `handleProtocol([req.url])` and responds `201 "Operation initiated."`. |
| Cold start | `main.ts:467-469` (one-time listener `frontendReady`) | Runs `handleProtocol([openUrlArgument, ...process.argv])`. |
| macOS `open-url` | `main.ts:576-584` | Handled immediately if a window exists, otherwise stored in `openUrlArgument`. |
| Tray "recent games" | `tray_icon/tray_icon.ts:104` | `heroic://launch?appName=<appName>` (no runner). |

- OS registration (`main.ts:403-411`): unless `CI=e2e` or already the default, call `app.setAsDefaultProtocolClient('heroic')`. On Linux, packaging declares `MimeType=x-scheme-handler/heroic;` (`flatpak/com.heroicgameslauncher.hgl.desktop:14`).
- Window at startup (`main.ts:413-420`): the window is headless if any of these hold:
  - `--no-gui` is in argv (`isCLINoGui`, `constants/environment.ts:35`)
  - `shouldHideWindowForProtocolArgs([openUrlArgument, ...argv])`
  - `startInTray && !noTrayIcon`

### A.3.2 Parsing

- `parseHeroicUrl(args)` (`:17-25`): take the **first** arg that starts with `heroic://` (case-sensitive) and parse it with WHATWG `new URL`. A parse failure gives `undefined`.
  - This is a non-special scheme, so the host is opaque and its case is preserved. `heroic://Launch` does **not** match `launch`.
- `urlRequestsNoGui(url)` (`:27-30`): true if the `gui` query param is exactly `false`, `0` or `no`.
- `shouldHideWindowForProtocolArgs(args)` (`:35-44`):
  - false unless the host is `launch`
  - true if `urlRequestsNoGui`
  - otherwise the global setting `hideWindowOnProtocolLaunch === true` (default `false`, `config.ts:369`); a thrown error gives false
- `handleProtocol(args)` (`:46-60`) logs `Received <href>` and dispatches on `url.hostname`:
  - `ping`: log only (`Received ping! Args: <searchParams>`).
  - `launch`: run `handleLaunch`.
  - anything else: ignored.

### A.3.3 URL forms (`handleLaunch`, `:66-179`)

| Form | Parsing |
|---|---|
| `heroic://launch/<appName>` | Applies when the pathname is non-empty and not `/`. Split on `/`, drop empty segments, `appName = pop()`, `runner = pop()` (undefined here). Segments are **not** percent-decoded. |
| `heroic://launch/<runner>/<appName>` | Same rule; `runner` is the second-to-last segment. |
| `heroic://launch?appName=X&runner=Y[&arg=a&arg=b][&altExe=/abs/path][&gui=false]` | Also accepts `heroic://launch/?...`, since Windows appends `/` (pathname `/`). Values come from `URLSearchParams`, which already percent-decodes and turns `+` into a space. `args = getAll('arg').map(decodeURIComponent)` is a **second** decode, and a malformed `%` throws `URIError`, which becomes an unhandled rejection. `altExe` is also double-decoded, then validated by `Path` (`backend/schemas.ts:4-7`: `path.parse(v).root` must be non-empty, i.e. absolute). An invalid `altExe` is ignored silently. |

### A.3.4 Launch logic

1. If `appName` is empty, log `No appName in protocol URL` and stop.
2. `runner` is accepted only if it is in `['legendary','gog','nile','sideload']` (`:15`; `zoom` is excluded). Any other value becomes `undefined`.
3. `findGame` (`:181-198`):
   - If a runner is given, return `libraryManagerMap[runner].getGame(appName).getGameInfo()` without checking it.
   - Otherwise iterate legendary → gog → nile → sideload and return the first result with a truthy `app_name`.
   - If nothing is found, log `Could not receive game data for <appName>!`.
   - Edge case: with an explicit runner and an unknown game, managers return `{}`. The code then dereferences `libraryManagerMap[undefined]` and rejects.
4. `settings = await <store>.getGame(appName).getSettings()`.
5. `hideForThisLaunch = urlRequestsNoGui || hideWindowOnProtocolLaunch`.
6. **If installed:**
   - If `altExe` is set, `launchOption = {type:'altExe', executable}`.
   - If `hideForThisLaunch` and the main window is visible, log "Hiding main window for protocol launch" and hide it.
   - Call `launchEventCallback({appName, runner: gameInfo.runner, skipVersionCheck: settings.ignoreGameUpdates, args, launchArguments: launchOption})` (`launcher.ts:102`). If `launchArguments` is undefined, it later falls back to `gameSettings.lastUsedLaunchOption`.
7. **If not installed:**
   1. Log `"<title>" not installed.`. If there is no main window, return.
   2. Show a modal `dialog.showMessageBox`:
      - buttons `[t('box.yes'), t('box.no')]`, `cancelId: 1`
      - message `<title> ` + `t('box.protocol.install.not_installed','Is Not Installed, do you wish to Install it?')`
      - title `<title>`, icon `windowIcon`
   3. Response 0: if `--no-gui` or `hideForThisLaunch`, show the window. Then `sendFrontendMessage('installGame', appName, gameInfo.runner)`.
   4. Response 1: log "Not installing game". With `--no-gui`, `app.quit()`.

URLs Heroic itself emits:
- `.desktop` / `.lnk` / macOS shortcuts: `heroic://launch?appName=<a>&runner=<r>`
- Steam LaunchOptions: same, quoted
- tray: `heroic://launch?appName=<a>`

---

## A.4 Save-path discovery (`backend/save_sync.ts`)

- IPC handler `getDefaultSavePath(appName, runner, alreadyDefinedGogSaves: GOGCloudSavesLocation[])` (`main.ts:1160-1164`). It returns `string | GOGCloudSavesLocation[]`, where `GOGCloudSavesLocation = {name: string, location: string}` (`common/types/gog.ts:72`).
- Dispatch (`save_sync.ts:17-34`):

  | Runner | Result |
  |---|---|
  | `legendary` | `getDefaultLegendarySavePath` |
  | `gog` | `getDefaultGogSavePaths` |
  | `nile`, `sideload`, `zoom` | `''` |

- (Saving or syncing itself happens through `syncSaves` in `main.ts`, which is outside this section.)

### A.4.1 Legendary (`:36-99`)

1. Get `{save_folder, save_path}` from the game info.
2. If `save_path` is already set, **discard it**:
   - Read `$CFG/legendaryConfig/legendary/installed.json` (`legendary/constants.ts:4-10`).
   - Set `[appName].save_path = null`.
   - Write it back as `JSON.stringify(obj, undefined, '  ')`. This is not atomic.
   - On any error, log `Failed to discard save path:` and **return the old `save_path`**.
3. If the game is not native, run `verifyWinePrefix(gameSettings)`, which creates or updates the prefix.
4. Run `legendary sync-saves <appName> --skip-upload --skip-download --accept-path`:
   - `abortId = <appName>-savePath`
   - log prefix "Getting default save path"
   - env = `{}` if native, else `setupWineEnvVars(gameSettings)`
   - Legendary resolves the path through the wine prefix and stores it in `installed.json`.
5. Re-read the game info with force-reload (`getGameInfo(appName, true)`). Return the new `save_path`. If it is empty, log `Unable to compute default save path for` and return `''`.

### A.4.2 GOG (`:101-214`)

1. Get the locations from `gog.getSaveSyncLocation(appName, installInfo)` (`gog/library.ts:243-312`):
   1. `clientId` comes from `$CFG/gogdlConfig/heroic_gogdl/manifests/<appName>` (JSON) when `version === 2`. Otherwise, or on a parse error, it comes from `goggame-<appName>.info` → `clientId` (`readInfoFile`, `gog/library.ts:1234`, at `<install>/` or, for osx, `<install>/Contents/Resources/`).
   2. If there is no clientId, warn and return `undefined`.
   3. `GET https://remote-config.gog.com/components/galaxy_client/clients/<clientId>?component_version=2.0.45`, then read `content[Windows|MacOS].cloudStorage`.
      - The platform key is `MacOS` only for `osx` installs. `windows` **and `linux`** use `Windows`.
      - If `!enabled`, return `undefined`. Otherwise return `locations: [{name, location}]`.
2. If the locations or `install_path` are missing, log an error and return `[]`.
3. If the list is empty, push the default `{name:'__default', location: ...}`:
   - windows installs: `%LocalAppData%/GOG.com/Galaxy/Applications/<clientId>/Storage/Shared/Files`
   - other installs: `$HOME/Library/Application Support/GOG.com/Galaxy/Applications/<clientId>/Storage`

   Here `clientId` comes from `readInfoFile(appName)`.
4. GOG variable map (`<?NAME?>`, regex `/<\?(\w+)\?>/g`):

   | Var | Value |
   |---|---|
   | INSTALL | `install_path` |
   | SAVED_GAMES | `%USERPROFILE%/Saved Games` |
   | APPLICATION_DATA_LOCAL | `%LOCALAPPDATA%` |
   | APPLICATION_DATA_LOCAL_LOW | `%APPDATA%\..\LocalLow` |
   | APPLICATION_DATA_ROAMING | `%APPDATA%` |
   | APPLICATION_SUPPORT | `$HOME/Library/Application Support` |
   | DOCUMENTS | native: `app.getPath('documents')`; else `%USERPROFILE%\Documents` |

   An unknown variable produces a warning and the bare variable name is inserted. Each match replaces the first remaining occurrence.
5. For each location:
   - If `alreadyDefinedGogSaves` has the same `name` with a non-empty `location`, reuse that entry unchanged.
   - Non-native games: `getWinePath({path, gameSettings})` (`launcher.ts:1898-1934`) runs `cmd /c winepath -u <path>` in the prefix with proton verb `runinprefix` and returns the trimmed stdout. Empty output is logged as an error and `''` is returned.
   - Native games: `getShellPath(p)` (`utils.ts:831`) is `normalize((await execAsync('echo ' + p)).stdout.trim())`. This is shell expansion of `$VAR`, unquoted, and therefore a quirk: word splitting collapses repeated spaces. If the path exists, apply `realpathSync`; on failure, warn.
6. Return `[{name, location}]` in the original order.

---

## A.5 Anti-cheat data (`backend/anticheat/**`)

- Cache file: `$CFG/areweanticheatyet.json` (`anticheat/utils.ts:12`). It holds the raw upstream JSON text: an array of `AntiCheatInfo` (`common/types.ts:500-520`):
  ```
  { status: 'Broken'|'Denied'|'Working'|'Running'|'Supported',
    anticheats: AntiCheat[],   // string union, common/types.ts:474-498 (e.g. 'Easy Anti-Cheat','BattlEye','VAC',...)
    notes: string[], native: boolean,
    storeIds: { epic?: {namespace: string, slug: string}, steam?: string },
    reference: string, updates: {name, date, reference}[] }
  ```
- Download trigger (`anticheat/ipc_handler.ts:12-21`):
  - It runs on the `backendEvents` event `releasesInfoReady`. That event is emitted by `fetchLastestReleases()` (`utils/releases.ts:11-28`), which fetches `https://raw.githubusercontent.com/Heroic-Games-Launcher/releases-info/refs/heads/main/release-data.json` once online, at startup (`main.ts`). Release info (`ReleasesInfo`, `common/types.ts:883-904`) includes `anticheatFiles: {shaMac, shaLinux}` and `knownFixesSha`, which are MD5 hex strings.
  - Skipped on Windows.
  - `downloadAntiCheatData(shaLinux)` (`utils.ts:14-55`):
    1. Return if `CI=e2e` or on Windows.
    2. If the file exists and `md5(file) === latestHash` (with `latestHash` truthy), skip.
    3. Otherwise, inside `runOnceWhenOnline` (immediately if online, else on the next `online` event), `GET` `https://raw.githubusercontent.com/Starz0r/AreWeAntiCheatYet/HEAD/games.json` on Linux. macOS uses `https://raw.githubusercontent.com/Heroic-Games-Launcher/MacAnticheatData/main/games.json`.
    4. The request uses `responseType:'text'` with no JSON transform, axios timeout 10 s. The text is written verbatim (not atomically).
    5. Failure logs a warning.
  - `createMD5(path)` (`utils/releases.ts:30-37`) returns `''` if the file is missing, otherwise the hex MD5 of the file.
- IPC handler `getAnticheatInfo(appNamespace)` (`anticheat/ipc_handler.ts:8`) runs `gameAnticheatInfo` (`utils.ts:57-81`):
  - Returns `null` if `appNamespace === undefined` or on Windows.
  - Reads and parses the file and returns the **first** entry where `storeIds.epic.namespace.toLowerCase().includes(appNamespace)`. This is a substring match, and `appNamespace` itself is **not** lower-cased.
  - Returns `undefined` if nothing matches. Read or parse errors produce a warning and `null`.
  - The frontend passes `gameInfo.namespace` (Epic).
- Also used in the game log (`launcher.ts:561-571`): `Anticheat Status: <status>` and `Anticheats: <array joined by ','>` followed by a blank line.

---

## A.6 Known fixes (`backend/known_fixes/**`)

- Cache file: `$CFG/known_fixes.json` (`known_fixes/utils.ts:12`). Its shape is `KnownFixesFile` (`common/types.ts:840-854`):
  ```
  { amazon: Record<appName, KnownFixesInfo>, epic: {...}, gog: {...}, zoom: {...} }
  KnownFixesInfo = { title: string, notes?: Record<string,string>, winetricks?: string[],
                     runInPrefix?: string[], envVariables?: Record<string,string>, wikiLink?: string }
  ```
  Runners map to store keys through `storeMap` (`common/utils.ts:3-9`): legendary→`epic`, gog→`gog`, nile→`amazon`, zoom→`zoom`, sideload→undefined.
- Download (`known_fixes/ipc_handler.ts:10-13` on `releasesInfoReady`, then `downloadFileIfNeeded(knownFixesSha)`, `utils.ts:32-63`). This runs on all platforms:
  1. Return if `CI=e2e`.
  2. If `latestHash` is truthy and equals `md5(local)`, skip. A missing file gives `''`, so it downloads.
  3. Otherwise, inside `runOnceWhenOnline`, `GET https://raw.githubusercontent.com/Heroic-Games-Launcher/known-fixes/main/known_fixes.json` as a stream, piped straight into the file. This is not atomic, so a partial file is possible and is tolerated by the reader.
  4. Failure logs a warning.
- IPC handler `getKnownFixes(appName, runner)` (sync, `ipc_handler.ts:6-8`) runs `getKnownFixesFor` (`utils.ts:14-30`):
  - Returns `null` for `sideload` or when the file is missing.
  - Otherwise returns `parsed[storeMap[runner]][appName]`, which is `undefined` if absent.
  - A parse error or missing store key produces the warning "Known fixes could not be loaded, ignoring." and `null`.
- Consumers (behaviour to replicate):
  - `envVariables` are merged into the launch env of every runner through `getKnownFixesEnvVariables` (`launcher.ts:1018-1022`). Call sites: `legendary/games.ts:918`, `gog/games.ts:577`, `nile/games.ts:340`, `zoom/games.ts:625`, `storeManagerCommon/games.ts:223`.
  - `installFixes` (`launcher.ts:980-1016`) runs only when the wine prefix was just created or updated (`launcher.ts:892`):
    - `winetricks`: send the game status `winetricks`, then run `Winetricks.install(runner, appName, pkg)` for each package.
    - `runInPrefix`: send the status `redist` with context `FIXES`, then run `<install_path>/<file>` for each entry through wine (verb `run`, wait).
  - `wikiLink` is logged into the game log as `Wiki Link, read this: <url>` (`launcher.ts:573-576`).

---

## A.7 Game metadata overrides (`backend/game_overrides/**`)

- Store file `$USERDATA/store/game-overrides.json`, i.e. `~/.config/heroic/store/game-overrides.json` (`electronStores.ts:15-22`). The store name is `gameOverridesStore` and `clearInvalidConfig` is true. Schema (`common/types/electron_store.ts:114-123`):
  ```json
  { "overrides": { "<appName>": { "title"?: string, "art_cover"?: string, "art_square"?: string } } }
  ```
- Images dir: `$USERDATA/game_overrides_images` (`index.ts:10`). The backend never writes here; it only deletes files named `<appName>-*` when an override is cleared (`index.ts:12-28`). Delete errors are logged.
- Functions:
  - `getGameOverrides(appName)` (`:33-44`) returns `overrides[appName] || null`. Errors give `null`.
  - `getAllGameOverrides()` (`:49-59`) returns the map. Errors give `{}`.
  - `setGameOverrides(appName, o)` (`:64-89`):
    - If `title`, `art_cover` and `art_square` are all falsy, delete the entry and remove its images.
    - Otherwise **replace** the entry wholesale (no merge).
    - Persist, log `Saved overrides for <appName>`, and return true. Errors give false.
  - `attachOverrides(gameInfo)` (`:96-102`) returns `{...gameInfo, overrides}` if an override exists, otherwise the original object. The `title`/`art_*` fields themselves are not changed.
- IPC (`main.ts`):
  - listener `setGameMetadataOverride({appName, title?, art_cover?, art_square?})` (`:1343-1347`): runs `setGameOverrides`, then `sendFrontendMessage('metadataChanged', getAllGameOverrides())`.
  - handler `getGameMetadataOverride(appName)` returns the override or `null` (`:1349-1351`).
  - handler `getAllGameOverrides()` (`:1353-1355`).
  - handler `getGameInfo(appName, runner)` (`:707-723`):
    - For legendary, if the store lacks the game, return `null`.
    - If the info object is empty, return `null`.
    - Otherwise return `attachOverrides(info)`.
- Library lists get overrides attached in the frontend (`frontend/helpers/gameOverrides.ts`), which also reads the same store file.

---

## A.8 System information (`backend/utils/systeminfo/**`)

### A.8.1 Output shape (`systeminfo/index.ts:21-66`)

```ts
GPUInfo = { deviceId: string /*4-hex lc*/, vendorId: string, subdeviceId?: string, subvendorId?: string,
            deviceString?: string, vendorString?: string, driverVersion?: string /*Linux: kernel driver name*/ }
SystemInformation = {
  CPU: { model: string, cores: number },
  memory: { used: number, total: number, usedFormatted: string, totalFormatted: string },
  GPUs: GPUInfo[],
  OS: { platform: string, name: string, version: string },
  steamDeckInfo: { isDeck: true, model: 'LCD'|'OLED', mode: 'game'|'desktop' } | { isDeck: false },
  isFlatpak: boolean, isAppImage: boolean,
  softwareInUse: { heroicVersion, legendaryVersion, gogdlVersion, cometVersion, nileVersion: string }
}
```

### A.8.2 `getSystemInfo(cache = true)` (`index.ts:74-122`)

- The result is cached in memory for the life of the process. Pass `cache=false` to recompute.
- `CPU.model = os.cpus()[0].model` (`/proc/cpuinfo` "model name"), and `cores = os.cpus().length` (logical CPUs).
- Memory (`memory/linux.ts:4-17`):
  - Read `/proc/meminfo` and match `^MemTotal:\s*(\d+) kB` and `^MemAvailable:\s*(\d+) kB` (multiline). Each value is ×1024, and missing values count as 0.
  - `used = total>0 ? total-available : 0`.
  - The formatted fields use `filesize(n, {base:2})` from filesize v10, which gives IEC units with 2 decimals, e.g. `"15.48 GiB"`.
  - Windows: `process.getSystemMemoryInfo()` (KB×1024). macOS: `sysctl -n vm.pagesize` and `vm_stat`.
- OS:
  - `platform = process.platform` (`'linux'`).
  - `version` starts as `process.getSystemVersion()` (the kernel release on Linux) and is then **overwritten** by the os-release values via a spread.
  - `osInfo_linux` (`osInfo/linux.ts:15-53`) reads the first existing file of `/run/host/os-release` (the Flatpak host) or `/etc/os-release`. If neither exists: `{name:'Unknown Linux Distribution'}`.
  - Fields use multiline regexes `^NAME=(.*)`, `^PRETTY_NAME=`, `^VERSION=`, `^VERSION_ID=`, `^VERSION_CODENAME=`, with surrounding double quotes stripped (`"…"`, not single quotes).
  - `name = NAME || PRETTY_NAME || 'Linux'`.
  - `version = VERSION || (VERSION_ID && VERSION_CODENAME ? VERSION_ID + ' ' + VERSION_CODENAME : undefined)`. When undefined, the kernel release stays.
  - Windows: a PowerShell CIM `Win32_OperatingSystem` query. macOS: `{name:'macOS'}`.
- GPUs (`gpu/index.ts`, `gpu/linux.ts`):
  1. Get `app.getGPUInfo('basic').gpuDevice[]` → `{deviceId:number, vendorId:number}`, validated with zod. A failure throws.
     - Rust equivalent: enumerate PCI display-class devices (`/sys/bus/pci/devices/*/class` `0x03xxxx`, plus `vendor` and `device`), primary GPU first.
  2. Format the ids as `hex.padStart(4,'0')`.
  3. If `lspci` is on `$PATH` (`hasExecutable`, which stats `<dir>/lspci` for each `$PATH` entry), run `lspci -d <vendorHex>:<deviceHex> -mm -k -v -n` for each GPU. The hex values are not padded. stdout is matched with:
     ```
     /^SVendor:\t(.{4})\n^SDevice:\t(.{4})[\S\s]*^Driver:\t(\S*)/m
     ```
     The groups give `subvendorId`, `subdeviceId` and `driverVersion`. With no match, those fields are omitted.
  4. Name lookup (`gpu/pci_ids.ts`) uses **DNS TXT** queries against `pci.id.ucw.cz`. The first TXT string is taken with the leading `i=` stripped; a failure gives false.
     - Device: if subids are present, query `<subdev>.<subven>.<dev>.<ven>.pci.id.ucw.cz` first, then fall back to `<dev>.<ven>.pci.id.ucw.cz`. A fallback hit is also cached under the sub-id key.
     - Vendor: `<ven>.pci.id.ucw.cz`.
     - All ids are lower-cased.
  5. **A GPU is dropped from the output if either name lookup fails.**
  6. Windows uses PowerShell CIM `Win32_VideoController`. macOS returns `[]`.
- PCI name caches (`CacheStore`, `backend/cache.ts`):
  - Files `$USERDATA/store_cache/pci_ids_device.json` and `pci_ids_vendor.json`.
  - Lifespan is 10080 min (7 days). The shape is:
    ```json
    { "163f_1002": "VanGogh [AMD Custom GPU 0405]", "__timestamp": { "163f_1002": "<JS Date() string, e.g. 'Wed Oct 22 2026 19:38:00 GMT+0300 (...)'>" } }
    ```
  - Device keys are `<dev>_<ven>` or `<subdev>_<subven>_<dev>_<ven>`. Vendor keys are `<ven>`.
  - On `get`, an entry with no timestamp counts as a miss. An entry older than the lifespan is deleted, together with its timestamp, and counts as a miss.
- Steam Deck (`steamDeck.ts:9-28`):
  - It is a Deck only if `cpus[0].model` is `AMD Custom APU 0405` or `AMD Custom APU 0932`, and `GPUs[0].vendorId === '1002'`.
  - Device `163f` → `LCD`, `1435` → `OLED`. Any other device means not a Deck.
  - `mode` is `game` if `XDG_CURRENT_DESKTOP==='gamescope'`, else `desktop`.
- Environment flags: `isFlatpak = !!FLATPAK_ID`, `isAppImage = !!APPIMAGE`.
- Software versions (`heroicVersion.ts`, `utils/helperBinaries/index.ts`), run in parallel:

  | Field | Command | Result |
  |---|---|---|
  | heroic | none | `"<app version> <name>"`. The name is `versionNames.beta` ("Caesar Clown") if the version contains `alpha` or `beta`, else `versionNames.stable` ("Hajrudin"), e.g. `2.22.4 Hajrudin`. |
  | legendary | `legendary --version` (abortId `legendary-version`) | stdout matched with `/"([\d.]*)".*"(.*)"$/m` gives `"<ver> <codename>"`. An error, abort or no match gives `'invalid'`. |
  | gogdl | `gogdl --version` (abortId `gogdl-version`) | Raw stdout, untrimmed. An error gives `'invalid'`. |
  | nile | `nile --version` (abortId `nile-version`) | Raw stdout. An error gives `'invalid'`. |
  | comet | `spawnSync(<cometBin dir>/<bin>, ['--version'])` | stdout with trailing whitespace trimmed. A spawn error gives `'invalid'`. |

### A.8.3 `formatSystemInfo(info)` (`index.ts:124-155`), exact text

This uses `filesize()` with **default** options (base 10, e.g. `16.62 GB`). Here `\n` is LF, and the Linux-only block appears only when `process.platform==='linux'`.

```
CPU: ${cores}x ${model}
Memory: ${filesize(total)} (used: ${filesize(used)})
GPUs:
  GPU ${i}:
    Name: ${vendorString} ${deviceString}
    IDs: D=${deviceId} V=${vendorId} SD=${subdeviceId} SV=${subvendorId}
    Driver: ${driverVersion}
OS: ${OS.name} ${OS.version} (${OS.platform})

The current system is[ not] a Steam Deck[ (model: ${model}) in ${mode} mode]
We are[ not] running inside a Flatpak container
We are[ not] running from an AppImage

Software Versions:
  Heroic: ${heroicVersion}
  Legendary: ${legendaryVersion}
  gogdl: ${gogdlVersion}
  comet: ${cometVersion}
  Nile: ${nileVersion}
```

- GPU blocks are joined with `\n`. Missing optional fields print as `undefined`.

### A.8.4 IPC and consumers

- IPC (`backend/utils/ipc_handler.ts:18-27`):
  - handler `getSystemInfo(cache?: boolean)` → `getSystemInfo(cache)`; `undefined` means true
  - listener `copySystemInfoToClipboard` → formatted text to the clipboard
  - handlers `getLegendaryVersion`, `getGogdlVersion`, `getCometVersion`, `getNileVersion`
  - handler `hasExecutable(name)`
- Consumers:
  - the startup log `System Information:` (`logger/index.ts:90`)
  - every game log `System Info:\n<formatted>\n\n` (`launcher.ts:546-551`)
  - `isMacSonomaOrHigher` (`utils.ts:1309-1320`, macOS only, major `OS.version` ≥ 14)

---

# Part 3: Wiki info, SteamGridDB, discounts, logger, tray, online monitor, migrations, startup, IPC

## B.0 Conventions used in this section

- All file refs are relative to `src/` of Heroic v2.22.4 (e.g. `backend/main.ts:42`).
- `$CFG` = Electron `app.getPath('userData')` = `app.getPath('appData')/heroic`. On Linux that is `${XDG_CONFIG_HOME:-~/.config}/heroic` (Flatpak: `~/.var/app/com.heroicgameslauncher.hgl/config/heroic`). The package `name` is `heroic` (`../package.json:2`), there is no top-level `productName`, and nothing in the backend calls `app.setPath` or `app.setName`. `appFolder` = `join(appData,'heroic')` (`backend/constants/paths.ts:23`), which is the same directory. With `CI=e2e`, `appData` becomes a fresh temp directory (`backend/constants/paths.ts:11-18`).
- `$LOGS` = `${XDG_STATE_HOME:-~/.local/state}/Heroic/logs` (Linux only; see B.4).
- **`CacheStore`** (`backend/cache.ts`) is the TTL'd key/value cache that several modules here use. Its behavior applies to every "cache store" below:
  - It is backed by `electron-store` with `cwd:'store_cache'`, `name:<filename>` and `clearInvalidConfig:true`. File: `$CFG/store_cache/<filename>.json`. If the JSON is corrupt, the file is silently reset to `{}`.
  - Default lifespan is `60*6` minutes (6 h). Passing `null` disables expiry.
  - `set(k,v)` writes `k → v` and `__timestamp.<k> → Date()`. `Date()` yields the JS string form, e.g. `"Thu Oct 22 2026 14:03:07 GMT+0200 (Central European Summer Time)"` (`backend/cache.ts:80-83`).
  - electron-store's **dot-notation is ON** for CacheStore, so the on-disk shape is nested. A key `k` becomes `{"k": <value>, "__timestamp": {"k": "<Date() string>"}}`. A key that contains `.` (for example a game title like `S.T.A.L.K.E.R`) is split into nested objects, `{"S":{"T":{...}}}`, and the same applies under `__timestamp`. A Rust port that reads existing caches needs dot-path get/set. Treating the caches as disposable and using a new format is acceptable.
  - `get(k)` returns the fallback in three cases: the key is missing; a lifespan is set and the timestamp is missing; or the age in minutes is `> lifespan` **and** `invalidateCheck(value)` is true. In the last case it also deletes both keys (`backend/cache.ts:49-78`). The default `invalidateCheck` returns `true`.
  - `use_in_memory()` / `commit()` mirror the store into a Map and write it back. None of the modules in this section use them.
- `axiosClient` (`backend/utils.ts:1578-1581`) is axios with `timeout: 10000` and an `https.Agent({keepAlive:true})`. Requests made with "plain axios" have **no timeout** unless one is passed. If no `User-Agent` is set, axios sends `axios/<version>`.

---

## B.1 Wiki / extra game info (`backend/wiki_game_info/**`)

### B.1.1 Entry point and IPC
- IPC: `getWikiGameInfo(title, appName, runner)` → `WikiInfo | null` (`backend/wiki_game_info/ipc_handler.ts:5-7`). The `title` argument is **ignored**; the handler calls `getWikiGameInfo(getGame(appName, runner))`.
- Orchestrator: `getWikiGameInfo(game)` (`backend/wiki_game_info/wiki_game_info.ts:15-85`):
  1. `title = removeSpecialcharacters(gameInfo.title)` (`backend/utils.ts:357-360`). The regex `/[:|/|*|?|<|>|\\|&|{|}|%|$|@|`|!|™|+|'|"|®]/gi` deletes every one of `: | / * ? < > \ & { } % $ @ \` ! ™ + ' " ®`. Spaces, `-` and `.` are kept.
  2. Cache lookup `wikiGameInfoStore.get(title)`. If the value is truthy, it logs `Using cached ExtraGameInfo data for <title>` (prefix `ExtraGameInfo`) and returns it. **The cache key is the cleaned title only, with no runner or appName**, so two games with the same title in different stores share one entry.
  3. It runs these in parallel (`Promise.all`, `:35-40`):
     - `getInfoFromPCGamingWiki(title, runner==='gog' ? appName : undefined)`
     - `getInfoFromGamesDB(title, appName, runner)`
     - `isMac ? getInfoFromAppleGamingWiki(title) : null` (**always `null` on Linux**)
     - `isLinux ? getUmuId(appName, runner) : null`
  4. Then, sequentially, `howlongtobeat = getHowLongToBeat(game, pcgamingwiki?.howLongToBeatID)` (`:43-46`).
  5. Linux only (`:48-64`): `steamID = gamesdb?.steamID || pcgamingwiki?.steamID`. It then runs `getInfoFromProtonDB(steamID)` and `getSteamDeckComp(steamID)` in parallel. If either is non-null, `steamInfo = {compatibilityLevel: protondb?.level, steamDeckCatagory: steamdeck?.category}`; otherwise `steamInfo = null`. The misspelling `Catagory` is part of the wire format. Fields can be `undefined`, so they are dropped from the JSON on disk.
  6. It builds `{pcgamingwiki, applegamingwiki, howlongtobeat, gamesdb, steamInfo, umuId}`, calls `wikiGameInfoStore.set(title, obj)` and returns `obj`.
  7. If anything throws, it logs `Was not able to get ExtraGameInfo data for <raw title>` and returns `null`. **Nothing is cached in that case.** Sub-fetchers usually swallow their own errors, so partial results (some fields `null`) **are** cached for 30 days.
- Returned type (`common/types.ts:705-751`):
  ```ts
  WikiInfo { pcgamingwiki: PCGamingWikiInfo|null; applegamingwiki: AppleGamingWikiInfo|null;
             howlongtobeat: HeroicHowLongToBeatEntry|null; gamesdb: GamesDBInfo|null;
             steamInfo: SteamInfo|null; umuId: string|null }
  PCGamingWikiInfo { steamID; howLongToBeatID; metacritic: GameScoreInfo; opencritic: GameScoreInfo;
                     igdb: GameScoreInfo; direct3DVersions: string[]; genres: string[]; releaseDate: string[] }
  GameScoreInfo { score: string; urlid: string }
  AppleGamingWikiInfo { crossoverRating; wineRating; crossoverLink }   // strings
  GamesDBInfo { steamID: string }
  SteamInfo { compatibilityLevel: string|null; steamDeckCatagory: number|null }
  HeroicHowLongToBeatEntry { completionist: number; mainStory: number; mainExtra: number;
                             gameId?: number; gameName?: string; gameImageUrl?: string; gameWebLink?: string }
  ```

### B.1.2 Cache stores (`backend/wiki_game_info/electronStore.ts`)

| Store | File | Key | Value | TTL | invalidateCheck |
|---|---|---|---|---|---|
| `wikiGameInfoStore` | `$CFG/store_cache/wikigameinfo.json` | cleaned title | `WikiInfo` | `60*24*30` min = **30 days** | default (always expire) |
| `umuStore` | `$CFG/store_cache/umu.json` | `${runner}_${appName}` | `string \| null` | `60*6` min = **6 h** | `(data) => !data` |

`umuStore` edge case: only falsy (`null`) entries ever expire, so a non-null umu ID is cached **forever**. A cached `null` is useless anyway: `getUmuId` treats a falsy cache hit as a miss (`if (cachedValue)`) and fetches again every time.

### B.1.3 PCGamingWiki (`backend/wiki_game_info/pcgamingwiki/utils.ts`, `constants.ts`)
- Pre-processing: `title = title.replace(' -', ':')`. This replaces only the **first** occurrence (`:21`).
- Page ID lookup (`getPageID`, `:74-100`). The HTTP client is `axiosClient` (10 s).
  1. If `gogID` is set (runner is gog), it uses a Cargo query by GOG ID:
     `GET https://www.pcgamingwiki.com/w/api.php?action=cargoquery&tables=Infobox_game&fields=Infobox_game._pageID%3DpageID%2C&where=Infobox_game.GOGcom_ID%20HOLDS%20<gogID>&format=json`
     and takes `data.cargoquery[0]?.title?.pageID`. If that value is truthy it is used. Otherwise lookup falls through to search.
  2. Search: `GET https://www.pcgamingwiki.com/w/api.php?action=query&list=search&srsearch=<title with ' ' → '%20' only>&format=json`. No other characters are encoded, so a `#` would truncate the URL. It reads `data.query.search` (array of `{pageid,title}`), picks the first result whose `title` is a case-insensitive exact match, else `results[0]`, and returns its `pageid`. An empty array returns `undefined`.
- If there is no ID, it returns `null`.
- Wikitext: `GET https://www.pcgamingwiki.com/w/api.php?action=parse&format=json&pageid=<id>&redirects=true&prop=wikitext` → `data.parse.wikitext['*']`. If that is missing or empty, it returns `null` (`:102-112`).
- Regexes (`constants.ts:1-10`):
  | Field | Regex | Extraction |
  |---|---|---|
  | metacritic | `/game\/row\/reception\|Metacritic\|(\S+)\|([0-9]+)/m` | `{urlid:g1, score:g2}`, `''` if no match |
  | opencritic | `/game\/row\/reception\|OpenCritic\|(\S+)\|([0-9]+)/m` | same |
  | igdb | `/game\/row\/reception\|IGDB\|(\S+)\|([0-9]+)/m` | same |
  | steamID | `/steam appid {2}= ([0-9]+)/m` (exactly 2 spaces) | g1 or `''` |
  | howLongToBeatID | `/hltb {9}= ([0-9]+)/m` (exactly 9 spaces) | g1 or `''` |
  | direct3DVersions | `/direct3d versions {6}= (.+)/m` | g1, all spaces removed, `split(',')` |
  | genres | `/game\/row\/taxonomy\/genres\s*\|\s*(.*?)\s*}}/` | g1, all spaces removed, `split(',')` |
  | releaseDate | `/game\/row\/date\|([^|]+)\|([^}]*)/g` (all matches) | each match gives `"<g1>: <g2>"` |
- Edge case: when nothing matches, `direct3DVersions` and `genres` become `['']` (one empty string), not `[]`.
- Any thrown error makes the function log `Was not able to get PCGamingWiki data for <title>` and return `null`.

### B.1.4 AppleGamingWiki (`backend/wiki_game_info/applegamingwiki/**`), macOS only
- It is never called on Linux. Documented here for completeness.
- `title.replace(' -', ':')`. Search: `GET https://www.applegamingwiki.com/w/api.php?action=query&list=search&srsearch=<title spaces→%20>&format=json` → `data.query.search[0]?.pageid`. There is no exact-match preference.
- Wikitext: `GET https://www.applegamingwiki.com/w/api.php?action=parse&format=json&pageid=<id>&redirects=true&prop=wikitext`.
- Regexes: `crossoverRating /\|crossover {12}= (\S+)/m`, `wineRating /\|wine {17}= (\S+)/m`, `crossoverLink /codeweavers {2}= (\S+)/m`. A missing match gives `''`.

### B.1.5 GamesDB (`backend/wiki_game_info/gamesdb/utils.ts`)
- It maps runner to store with `storeMap` (`common/utils.ts:3-9`): `legendary→epic`, `gog→gog`, `nile→amazon`, `zoom→zoom`, `sideload→undefined`. For `sideload` it returns `{steamID:''}` immediately.
- It calls `libraryManagerMap.gog.getGamesdbData(store, appName, forceUpdate=true)` (`backend/storeManagers/gog/library.ts:1337-1383`):
  - `GET https://gamesdb.gog.com/platforms/<store>/external_releases/<appName>` via `axiosClient`. Because `forceUpdate=true`, no `If-None-Match` is sent. No certificate or auth headers are sent from this path.
  - A 404 returns `{isUpdated:false}` with no data. Any other error throws `Error('connection error')`, which `getInfoFromGamesDB` catches, so the result is `{data: undefined}`.
  - Side effect: on success it writes the response, with an `etag` field added, to CacheStore `gog_api_info` (`$CFG/store_cache/gog_api_info.json`, 6 h TTL) under key `<store>_<appName>`.
- Result: `steamID = data.game.releases.find(r => r.platform_id==='steam')?.external_id ?? ''`. It always returns an object and never `null`.

### B.1.6 HowLongToBeat (`backend/wiki_game_info/howlongtobeat/utils.ts`)
- Dispatch (`getHowLongToBeat`, `:166-206`):
  - **runner == gog**: it scrapes the GOG store page (`getGogHLTBGameData`). The PCGW HLTB ID is **not** used as a fallback. If scraping fails, it logs and returns `null`.
  - **other runners**: if `hltbId` is truthy (non-empty PCGW `hltb` value), it fetches the HLTB game page. Otherwise it returns `null` and logs `No HLTB ID available for <title>, cannot fetch data`.
- Both paths use plain axios with `timeout:10000` and `validateStatus: s < 500`. Any non-200 status returns `null`. Both send browser-like headers:
  ```
  User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36
  Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8
  Accept-Language: en-US,en;q=0.9
  Accept-Encoding: gzip, deflate, br        (the Rust client must decode brotli)
  Connection: keep-alive
  Upgrade-Insecure-Requests: 1
  Referer: https://howlongtobeat.com        (HLTB path only)
  ```
- HLTB path (`getGameDataById`, `:17-91`):
  - `GET https://howlongtobeat.com/game/<id>`.
  - It extracts `<script id="__NEXT_DATA__" type="application/json">(...)</script>` and parses it as JSON. `game = props.pageProps.game.data.game[0]`. If `game_id` is missing, it returns `null`.
  - Hours are `Math.round(comp_main/3600)`, `comp_plus` and `comp_100` (seconds converted to hours, rounded); a falsy value gives 0.
  - Output: `{mainStory, mainExtra, completionist, gameId: game_id, gameName: game_name||undefined, gameImageUrl: game_image ? 'https://howlongtobeat.com/games/'+game_image : undefined, gameWebLink: 'https://howlongtobeat.com/game/'+game_id}`.
- GOG path (`getGogHLTBGameData`, `:93-164`):
  - URL = `(await game.getExtraInfo()).storeUrl`, the GOG store product page. If it is falsy, it returns `null`.
  - Regexes run on the HTML. Every match is required, because the code uses non-null assertions (`!`). Any miss throws, and the function logs and returns `null`.
    - `/<span class="howlongtobeat-box__time"> ?([\d.]+) h ?<\/span><span class="howlongtobeat-box__name">Main<\/span>/`
    - the same with `Main \+ Sides` and with `Completionist`
    - HLTB ID: `/<a target="_blank" href="https:\/\/howlongtobeat.com\/game\/(\d+)">HowLongToBeat<\/a>/`
  - Values are `Math.round(parseFloat(g1))`. Output: `{mainStory, mainExtra, completionist, gameId, gameName: title, gameWebLink}`. There is no `gameImageUrl`.

### B.1.7 ProtonDB (`backend/wiki_game_info/protondb/utils.ts`), Linux only
- If `steamID` is falsy, it logs at debug level and returns `null`.
- `GET https://www.protondb.com/api/v1/reports/summaries/<steamID>.json` (axiosClient, 10 s, `headers:{}`, so the default axios UA is sent).
- On error it logs `Was not able to get ProtonDB data for <id>` plus `error.response?.data` and returns `null`. The full JSON response is logged at debug level.
- If `data.tier` is missing, it logs `No tier in response, API changed?` and returns `null`. Otherwise it returns `{level: data.tier}`. `tier` values are strings such as `platinum`, `gold`, `silver`, `bronze`, `borked` and `pending`.

### B.1.8 Steam Deck (`backend/wiki_game_info/steamdeck/utils.ts`), Linux only
- `GET https://store.steampowered.com/saleaction/ajaxgetdeckappcompatibilityreport?nAppID=<steamID>` (axiosClient, `headers:{}`).
- It requires `Number.isFinite(data.results.resolved_category)`; otherwise it logs `No resolved_category in response, API changed?` and returns `null`. It returns `{category: resolved_category}`. The frontend mapping is `0` unknown, `1` unsupported, `2` playable, `3` verified (`frontend/screens/Game/GamePage/components/CompatibilityInfo.tsx:36-`).

### B.1.9 umu ID (`backend/wiki_game_info/umu/utils.ts`), Linux only
- `runner==='sideload'` returns `null` without a request.
- Store map: `gog→gog`, `legendary→egs`, `nile→amazon`, `sideload→sideload`, `zoom→zoomplatform`.
- If the cache holds a truthy value for `${runner}_${appName}`, that value is returned.
- `GET https://umu.openwinecomponents.org/umu_api.php?codename=<appName.toLowerCase()>&store=<store>` (plain axios, **no timeout**, params URL-encoded by axios).
- Network error or non-200 returns `null` and is not cached. An empty array is cached as `null` and returns `null`. Otherwise it caches and returns `data[0].umu_id`. Response shape: `[{title, umu_id}]`.

---

## B.2 SteamGridDB (`backend/steamgrid/**`)

### B.2.1 Key storage (`backend/steamgrid/secureKey.ts`, `ipc_handler.ts`)
- The key is persisted in the global settings field `steamGridDbApiKey` (default `''`, `backend/config.ts:378`), i.e. in `$CFG/config.json` under `defaultSettings`. The value takes one of three forms:
  - `''`: no key.
  - `sgdb:v1:<base64(ciphertext)>`: encrypted with Electron `safeStorage.encryptString` (prefix constant `CIPHERTEXT_PREFIX`, `secureKey.ts:4`).
  - any other non-empty string: legacy plaintext, from before encryption existed or when `safeStorage` was unavailable.
- `encryptApiKey(plain)` (`secureKey.ts:18-29`): empty input gives `''`. If `safeStorage.isEncryptionAvailable()` is false or throws, it logs the warning `safeStorage unavailable, storing SteamGridDB API key in plaintext` and returns the plaintext.
- `decryptApiKey(stored)` (`secureKey.ts:31-48`): a value without the prefix is returned as is. A prefixed value when encryption is unavailable gives `''`. A decryption failure logs a warning and returns `''`.
- On Linux, Electron `safeStorage` is Chromium OSCrypt. A Rust port that must read existing keys has to reimplement it. The details below are from Chromium's `os_crypt_linux` and **must be verified**:
  - AES-128-CBC, PKCS7 padding, IV of 16 × `0x20`.
  - Key = PBKDF2-HMAC-SHA1(password, salt `saltysalt`, 1 iteration, 16 bytes).
  - Ciphertext prefix `v10` uses the hardcoded password `peanuts`. Prefix `v11` uses the password stored in the Secret Service / KWallet item `"<app name> Safe Storage"`.
  - The simpler alternative is to store the key in the keyring through `secret-service` or the `keyring` crate, and treat existing `sgdb:v1:` values as "re-enter required".
- `getDecryptedApiKey()` (`ipc_handler.ts:12-26`) migrates on read. If the stored value is plaintext, it re-encrypts it and, only if the result is actually encrypted, writes it back. It returns the plaintext.
- **Secret-handling caveat:** `GlobalConfig.setSetting` emits `settingChanged`, and the logger writes `Settings key <key> changed from <old> to <new>` to `heroic.log` (`backend/logger/index.ts:94-103`). So the stored form of the key (ciphertext, or plaintext when safeStorage is unavailable) ends up in the log. A Rust port should redact this key.

### B.2.2 IPC

| Channel | Behavior |
|---|---|
| `steamgriddb.hasApiKey` | Returns `!!stored`. It does not check whether decryption works. |
| `steamgriddb.setApiKey(key)` | `trim()`. An empty value stores `''` (clears the key). Otherwise it stores `encryptApiKey(trimmed)`. |
| `steamgriddb.searchGame(query)` | No key returns `[]`. Otherwise returns `searchGame(...)` mapped to `{id,name}`. On error it logs `SteamGridDB search failed:` and rethrows, so the IPC call rejects. |
| `steamgriddb.getGrids({gameId,dimensions?,styles?})` | No key returns `[]`. Otherwise `{id,url,thumb}[]`; logs and rethrows on error. |
| `steamgriddb.getHeroes({gameId,dimensions?,styles?})` | Same as `getGrids`, using the heroes endpoint. |

### B.2.3 HTTP (`backend/steamgrid/utils.ts`)
- Base URL: `https://www.steamgriddb.com/api/v2`. Plain axios, no timeout, no cache.
- Headers: `Authorization: Bearer <api key>` and `User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) HeroicGamesLauncher/<app version>`.
- Endpoints:
  - `GET /search/autocomplete/<encodeURIComponent(query)>`
  - `GET /grids/game/<gameId>?dimensions=<a,b>&styles=<x,y>` (each param is sent only if its array is non-empty, joined with `,`)
  - `GET /heroes/game/<gameId>` (same params)
- Response envelope: `{success:boolean, data:T, errors?:string[]}`. If `success` is false it throws `Error(errors.join(', ') || 'Search failed' | 'Failed to get grids' | 'Failed to get heroes')`. HTTP 4xx/5xx raise axios errors.
- Types: `SGDBGame {id:number,name:string}`, `SGDBGrid {id:number,url:string,thumb:string}` (`common/types.ts:872-881`).
- Unrelated reference: `backend/storeManagers/gog/library.ts:466` hardcodes a public `cdn2.steamgriddb.com` image URL as fallback art. It is not an API key.

---

## B.3 Discounts (`backend/discounts/**`)

`backend/discounts/index.ts` is imported by `backend/main.ts:23` and itself imports `./gmg` and `./humble`. All handlers are registered at module load. Shared type (`common/types/discounts.ts`):
```ts
CatalogProduct { id; title; store?: 'gog'|'gmg'|'humble' /* absent for GOG = gog */; coverHorizontal?; coverVertical?;
  price: { final: string; base: string; discount: string;
           finalMoney?: {amount: string; currency: string}; baseMoney?: {amount; currency} };
  productType: string; storeLink: string; releaseDate?; developers?: string[]; publishers?: string[];
  operatingSystems?: string[]; genres?: {name,slug}[]; features?: {name,slug}[]; reviewsRating?: number;
  ratings?: {name, ageRating}[] }
CatalogLocaleSettings { countryCode; locale; currencyCode }
GogDealsRegion { countryCode; currencyCode }
```

### B.3.1 GOG catalog (`backend/discounts/index.ts`)
- Constants: `CATALOG_URL=https://catalog.gog.com/v1/catalog`, `WISHLIST_URL=https://embed.gog.com/user/wishlist.json`, `USER_DATA_URL=https://embed.gog.com/userData.json`, `PAGE_LIMIT=48`, `MAX_PAGES=30`, `FALLBACK_LOCALE={countryCode:'US', locale:'en-US', currencyCode:'USD'}`.
- Page URL (`buildUrl`, `:36-57`), with query parameters in this order: `limit=48&order=desc%3Atrending&discounted=eq%3Atrue&productType=in%3Agame%2Cpack%2Cdlc&page=<n>&countryCode=<cc>&locale=<loc>&currencyCode=<cur>`, plus `&hideOwned=true` when hiding owned games. `URLSearchParams` encodes `:` and `,`.
- Headers: `User-Agent: HeroicGamesLauncher/<version>`. `Authorization: Bearer <GOG access_token>` is added only when `hideOwned` is set and a token exists. Timeout 15 s. Plain axios.
- Fetch (`fetchAllDiscounts`, `:83-116`):
  - It fetches page 1 and computes `total = min(first.pages, 30)`. If `total <= 1` it returns `first.products`.
  - It fetches pages 2..total **concurrently**. A failed page logs `Failed to fetch discounts page <n>: ...` and contributes `[]`. A failure on page 1 propagates.
  - It concatenates and **dedupes by `id`**, keeping the first occurrence. Products pass through raw from the API. There is no cache.
- `getGogDiscounts(locale, hideOwned=false)` (`:176-207`):
  - If `hideOwned` is set, it calls `GOGUser.getCredentials()`. With no credentials, `hideOwned` becomes false and it logs the warning `Failed to get user credentials: User maybe is not looged in` (sic).
  - If the result is empty **and** the locale is not exactly the fallback locale, it logs and retries once with `FALLBACK_LOCALE`, keeping the same `hideOwned` and token.
  - On error it logs `Failed to fetch GOG discounts: ...` and rethrows.
- `getGogDealsRegion()` (`:151-174`):
  - If GOG is logged in, it gets a token (credential errors are swallowed).
  - `GET https://embed.gog.com/userData.json` (timeout 10 s, UA, optional Bearer). The response must contain string fields `country` and `selectedCurrency.code`, otherwise it returns `null`. It returns them upper-cased.
  - If the call fails with a token, it retries once without the token. If it still fails, it logs a warning and returns `null`.
- `getGogWishlist()` (`:209-228`):
  - No credentials returns `[]`.
  - `GET https://embed.gog.com/user/wishlist.json` (Bearer, UA, 15 s) returns `Object.keys(data.wishlist ?? {})`, a list of product-ID strings.
  - Errors are logged and return `[]`.

### B.3.2 GreenManGaming (`backend/discounts/gmg.ts`)
- Feed: `https://raw.githubusercontent.com/Heroic-Games-Launcher/deals-listing/gmg-feed/gmg-discounts-{currency}.json`. Plain axios, timeout 30 s, `User-Agent: HeroicGamesLauncher/<version>`.
- Supported currencies: `GBP, USD, EUR, CAD, AUD, BRL, CNY, TRL, KRW`. The input is upper-cased; anything not in the list falls back to `USD`.
- Feed shape: `{Items?: [{CatalogItemId?, Id?, Name?, Url?, ImageUrl?, CurrentPrice?: string, OriginalPrice?: string, Currency?, Text1?}]}` (Impact catalog format).
- Item normalization (`normalizeItem`, `:69-108`):
  - `id = CatalogItemId ?? Id`. If `id`, `Name` or `Url` is missing, the item is skipped.
  - `current = parseFloat(CurrentPrice)`. A non-finite or negative value skips the item.
  - `base = (finite && > 0) ? parseFloat(OriginalPrice) : current`. If `base <= current` the item is skipped, so only discounted items are kept.
  - `discount = round((1 - current/base)*100)`. `currency = Currency ?? 'USD'`.
  - Output:
    ```
    {id: 'gmg-'+id, title: Name, coverHorizontal: ImageUrl,
     price: {final: fmt(current), base: fmt(base), discount: '-'+pct+'%',
             finalMoney: {amount: current.toFixed(2), currency}, baseMoney: {amount: base.toFixed(2), currency}},
     productType: 'game', operatingSystems: platformFromSku(CatalogItemId ?? ''),
     features: Text1 ? [{name: Capitalized(Text1), slug: lower(Text1)}] : undefined,  // DRM, e.g. steam, uplay
     storeLink: Url, store: 'gmg'}
    ```
  - `platformFromSku` uses the regex `/ - ([A-Za-z0-9 +()]+)$/` on the SKU. The lower-cased suffix maps as: starts with `xbox` → `['xbox']`; `mac` → `['osx']`; `pc` or starts with `windows` → `['windows']`; anything else → `undefined`.
  - `fmt(amount, cur)`: if `cur` is not in `Intl.supportedValuesOf('currency')`, the result is `"<CUR> <amount.toFixed(2)>"`. The obsolete code `TRL` most likely takes this path. Otherwise the result is `Intl.NumberFormat(i18next.language with '_'→'-' || 'en-US', {style:'currency', currency})`.
  - Items are deduped by product `id` (first occurrence wins).
- Cache: CacheStore `gmg_deals` (`$CFG/store_cache/gmg_deals.json`), key = currency code (e.g. `"USD"`), value `CatalogProduct[]`, TTL `24*60` min = **24 h**.
- Concurrent requests for the same currency share one in-flight promise (`inflight` Map), which is removed in `finally`. On error it logs `Failed to fetch GMG discounts (<cur>): ...` and rethrows. Errors are not cached.
- IPC: `getGmgDiscounts(currencyCode?)`.

### B.3.3 Humble (`backend/discounts/humble.ts`)
- Identical to GMG except:
  - Feed: `https://raw.githubusercontent.com/Heroic-Games-Launcher/deals-listing/humble-feed/humble-discounts-{currency}.json`.
  - Only `USD` is supported, so every request resolves to `USD`.
  - Ids are `humble-<id>`, `store:'humble'`, `operatingSystems:['windows']` always, and there are no `features`.
  - Cache: CacheStore `humble_deals` (`$CFG/store_cache/humble_deals.json`), 24 h.
- IPC: `getHumbleDiscounts(currencyCode?)`.

---

## B.4 Logger (`backend/logger/**`)

### B.4.1 Paths (`backend/logger/paths.ts`)
- Base directory (`getBaseLogPath`, `:12-24`):
  - Linux: `${XDG_STATE_HOME ?? ~/.local/state}/Heroic/logs`. Flatpak sets `XDG_STATE_HOME` to `~/.var/app/com.heroicgameslauncher.hgl/.local/state`.
  - Windows: `%LOCALAPPDATA%\Heroic\logs`. macOS: `~/Library/Logs/Heroic Games Launcher`. Both are out of scope.
- File (`getLogFilePath(args)`, `:46-58`). `appName` and `runner` are used verbatim, with no sanitization.

  | args | path |
  |---|---|
  | `{}` (neither appName nor runner) | `$LOGS/heroic.log` |
  | `{runner}` | `$LOGS/runners/<runner>.log`, where runner ∈ `legendary, gog, nile, sideload, zoom, comet` |
  | `{appName, runner, type='launch'}` | `$LOGS/games/<appName>_<runner>/<type>.log`, where type ∈ `launch, install, import, repair, update, setup` |

### B.4.2 Writers (`backend/logger/index.ts`, `log_writer.ts`)
- General log: created in `init()` (`index.ts:61-104`) as `LogWriter(heroic.log, outputToOsStreams=true, logsDisabled=settings.disableLogs)`. `disableLogs` defaults to undefined, i.e. false.
  - If logs are disabled, it writes the forced warning `IMPORTANT: Logs are disabled. Enable logs before reporting any issue`.
  - It then logs `System Information:` followed by the formatted system info (async promise, prefix `Backend`).
  - It subscribes to `backendEvents 'settingChanged'` and logs `Settings key <key> changed from <old> to <new>`. Values are JSON-stringified with 2-space indent when they are objects.
  - It installs a one-time `error` handler on `process.stdout` and `process.stderr`. The first error is written to the log as `Error writing to <stdout|stderr>: <stack>`; later errors are silenced.
- Runner logs: `getRunnerLogWriter(runner)` creates one writer per runner per process, cached in a Map, file only, honoring `disableLogs`.
- Game logs: `createGameLogWriter(appName, runner, type='launch')` creates a new writer per call. It is disabled when global `disableLogs` is set, **or**, for `type==='launch'` only, when the game setting `verboseLogs` is false. `verboseLogs` defaults to true (`backend/config.ts:373`).
- `logDebug/logInfo/logWarning/logError(message, options?)` go to the general writer. `options` is either a `LogPrefix` string or `{prefix?, forceLog?}`.
- Rotation (`log_writer.ts:67-75,94-102`): on the **first write performed by a given writer instance**, an existing `<file>` is synchronously renamed to `<file>.old`, overwriting any previous `.old`. This gives one backup generation, and there is no size cap. In practice:
  - `heroic.log` rotates once per app start.
  - Each runner log rotates once per app run, on its first message.
  - Each game log rotates every time a new game writer is created (each launch or install).
- On a non-general writer's first write, it also emits the debug line `Logging to file(s) <path>` (prefix `Backend`) to the general log, then runs `mkdir -p dirname`.
- `writeString(msg | Promise<string>, forceLog=false)` (`log_writer.ts:81-125`):
  - It throws if the writer is closed. If logs are disabled and `forceLog` is false, it returns without writing.
  - Writes are serialized through a promise chain, so order is preserved even for async messages.
  - A whitespace-only message is skipped. A trailing `\n` is appended if missing. Each message is written with `appendFile` (utf-8).
- Disabled logs gate only the file. **Console output still happens** for the general writer, which uses `console.log` for DEBUG and INFO, `console.warn` for WARNING and `console.error` for ERROR.
- `close()` writes `============= End of log =============` and marks the writer closed. Any later write throws.

### B.4.3 Line format (`backend/logger/formatter.ts`, `constants.ts`)
- Line layout: `(HH:MM:SS) ` + `[LEVEL]:` padded with spaces to 10 + ` ` + (if prefix non-empty) `[Prefix]:` padded to 18 + ` ` + message.
  - Time is local, with each part zero-padded to 2 digits.
  - Pad widths: level pad = `MaxLogLevelLength (7, "WARNING") + 3` = 10; prefix pad = `MaxLogPrefixLength (15, "ProtocolHandler"/"DownloadManager") + 3` = 18.
  - Example: `(14:03:07) [INFO]:    [Backend]:         Frontend Ready`.
  - Example with no prefix: `(14:03:07) [WARNING]: Settings key ...`.
- Levels: `DEBUG, INFO, WARNING, ERROR`. There is no level filtering; everything is written.
- Prefixes: `''` (General), `Legendary, Gog, Nile, Zoom, WineDownloader, ToolInstaller, GlobalConfig, GameConfig, ProtocolHandler, Frontend, Backend, Runtime, Shortcuts, Winetricks, Connection, DownloadManager, ExtraGameInfo, Sideload, LogUploader`. Runner-to-prefix map: `legendary→Legendary, gog→Gog, nile→Nile, sideload→Sideload, zoom→Zoom`.
- Message stringification (`convertUnknownToString`):
  - Array: each element is converted recursively and joined with `' '`.
  - String, number or boolean: `toString()`.
  - Promise: awaited, then converted.
  - `Error`: `stack ?? message`.
  - Anything else: `JSON.stringify(x, null, 2)`, so `null` becomes `"null"`.
  - Edge: a top-level `undefined` makes `JSON.stringify` return `undefined`, and the later `.length` access throws. Inside an array it becomes `''`. In Rust, treat it as empty.
- If the formatted message is empty, the whole line is `''` and is skipped.

### B.4.4 IPC (`backend/logger/ipc_handler.ts`)
- `logInfo(msg)` / `logError(msg)` (`on`): log to the general log with prefix `Frontend`.
- `getLogContent(args: GetLogFileArgs)` (`handle`):
  - Reads the **last 1 MiB** (`readLastBytes`, `backend/utils/filesystem/read_last_bytes.ts`).
  - `decodeUTF8` skips up to 4 leading UTF-8 continuation bytes (`(b & 0xC0) == 0x80`), then decodes (`backend/utils/strings.ts:9-21`).
  - A missing file returns `''`.
- `showLogFileInFolder(args)` (`on`): `shell.showItemInFolder(path)`.
- `uploadLogFile(name, args)`, `deleteUploadedLogFile(url)` and `getUploadedLogFiles()` (`handle`): see B.4.5.

### B.4.5 Uploader (`backend/logger/uploader.ts`)
- Endpoint: `POST https://dpaste.com/api/v2/` using global `fetch`.
  - Headers: `User-Agent: HeroicGamesLauncher/<version>`, `Content-Type: application/x-www-form-urlencoded`.
  - Body: `content=<encodeURIComponent(first 10 MiB of the file decoded as UTF-8)>&expiry_days=2`.
  - The file read throws if the file is missing, which rejects the IPC call.
- Response:
  - A network error is logged (`Failed to send data to dpaste.com:`) and returns `false`.
  - Otherwise `text().trim()` must be a valid URL (zod `.url()`) and `response.ok` must be true. If not, it logs the status and text and returns `false`.
  - `token` is hardcoded to `'1'` because dpaste has no delete token.
- Store: `TypeCheckedStoreBackend('uploadedLogs', {cwd:'store', name:'uploadedLogs', accessPropertiesByDotNotation:false})`. File `$CFG/store/uploadedLogs.json`, shape `{ "<paste url>": {"name": string, "token": "1", "uploadedAt": <epoch ms>} }`. URL keys are literal because dot-notation is off.
- On success it stores the entry, sends frontend message `logFileUploaded(url, data)`, logs `Uploaded log file <name> to <url>` and returns `[url, data]`.
- `deleteUploadedLogFile(url)`:
  - Unknown URL returns `false`.
  - Otherwise it POSTs a multipart `FormData {token, delete:''}` to the paste URL itself, still with the forced url-encoded Content-Type header. dpaste does not support deletion, so this is effectively dead (the UI hides it).
  - If `ok`, it deletes the entry, sends `logFileUploadDeleted(url)` and returns `true`.
- `getUploadedLogFiles()` returns entries younger than 2 days and **deletes** entries aged `>= 2` days (`EXPIRY_DAYS=2`).

---

## B.5 Tray icon (`backend/tray_icon/tray_icon.ts`)
- `initTrayIcon(mainWindow)` is called at `backend/main.ts:459` without being awaited. If `settings.noTrayIcon` (default false) is set, it returns `null` and registers nothing, including the `changeTrayColor` listener. Changing that setting takes effect only after a restart.
- Icon:
  - Source: `public/icon-dark.png` if `settings.darkTrayIcon` (default undefined, i.e. false), else `public/icon-light.png`. The path goes through `fixAsarPath`.
  - Resized to **32×32 on Linux** (win32 32×32, darwin 20×20).
  - Tooltip: `Heroic`.
- Context menu, rebuilt by `loadContextMenu(recentGames?)`; on macOS it is also set as the dock menu:
  1. One item per recent game, `label = title`. Click calls `handleProtocol(['heroic://launch?appName=<appName>'])`, with appName not URL-encoded and no runner parameter. The list is `configStore 'games.recent'` sliced to `maxRecentGames` (setting; `|| 5`).
  2. Separator.
  3. `tray.show`: `mainWindow.show()`.
  4. `tray.about` (default "About"): `showAboutWindow()`.
  5. `tray.reload` ("Reload"), accelerator `Ctrl+R` (`Cmd+R` on mac): `mainWindow.reload()`.
  6. `Debug` (not translated), accelerator `Ctrl+Shift+I` (`Alt+Cmd+I` on mac): open devtools.
  7. `tray.quit` ("Quit"), accelerator `Ctrl+Q` (`Cmd+Q` on mac): `handleExit()`.
- Events:
  - Tray `click`: if `settings.exitToTray && mainWindow.isVisible()` it hides the window, otherwise it shows it.
  - `backendEvents 'languageChanged'`: rebuild the menu.
  - IPC `changeTrayColor` (`on`): after 500 ms, `setImage(getIcon())` and rebuild the menu.
  - `backendEvents 'recentGamesChanged'(list)`: slice to `maxRecentGames()` and rebuild.
- Related window behavior in `backend/main.ts:206-225`: on window close, if `exitToTray && !noTrayIcon` the window is hidden instead of quitting. Startup with `startInTray && !noTrayIcon` keeps the window hidden (`backend/main.ts:417-421`).
- Rust note: on Linux this needs a StatusNotifierItem/AppIndicator, for example with the `ksni` or `tray-icon` crate.

---

## B.6 Online monitor (`backend/online_monitor.ts`)
- States (`ConnectivityStatus`): `'online' | 'offline' | 'check-online'`. Module state: `status`, `retryIn=0`, `timeBetweenRetries=5` (s), an `AbortController` and a `retryTimer`.
- `initOnlineMonitor()` (`:99-126`), called at `backend/main.ts:327`:
  - Registers `on 'connectivity-changed'(newStatus)`, which calls `setStatus(newStatus)`. The frontend pushes browser online/offline events here.
  - If `net.isOnline()` it calls `setStatus('check-online')`, otherwise `setStatus('offline')`. When offline there is **no polling**; the state stays offline until the frontend reports a change.
  - Registers `handle 'get-connectivity-status'`, which returns `{status, retryIn}`.
  - Registers `on 'set-connectivity-online'`, which calls `setStatus('online')`. This is a manual override.
- `setStatus(s)` (`:17-41`):
  - Logs `Connectivity: <s>` (prefix `Connection`).
  - For `check-online` it calls `pingSites()`.
  - For any other state it resets `retryIn=0` and `timeBetweenRetries=5`, aborts in-flight pings and clears the retry timer.
  - It always sends the frontend message `connectivity-changed {status, retryIn}` and emits the internal event `<status>`.
- `pingSites()` (`:71-97`):
  - If `CI==='e2e'`, it sets `online` immediately.
  - Otherwise it logs `Pinging external endpoints` and creates a new AbortController.
  - It sends `HEAD` (plain axios, `timeout:10000`, header `Cache-Control: no-cache`, default 2xx validation, redirects followed) in parallel to:
    - `https://github.com`
    - `https://store.epicgames.com`
    - `https://gog.com`
    - `https://cloudflare-dns.com/`
  - `Promise.any`: the first success calls `setStatus('online')`, aborts the rest and resets the backoff to 5.
  - If all fail, it logs `All ping requests failed:` and the AggregateError, calls `retry(timeBetweenRetries)`, then `timeBetweenRetries += 5`.
- `retry(n)` (`:43-61`): a 1 Hz countdown. Each second it sends frontend `connectivity-changed {status:'check-online', retryIn:n}` and re-arms a 1000 ms timeout with `n-1`. At 0 it calls `pingSites()` again. The backoff is **linear**: 5 s, 10 s, 15 s and so on, with no cap.
- Exports:
  - `onConnectivityChange(cb)` subscribes to all three states.
  - `runOnceWhenOnline(cb)` runs immediately if online, else once on the next `online`.
  - `isOnline()` returns `status === 'online'`.
- Edge: a repeated `check-online` while pings are in flight replaces `abortController` without aborting the old one, so the old pings keep running.

---

## B.7 Migrations (`backend/migration/**`)
- `MigrationSystem.get().applyMigrations()` is awaited at `backend/main.ts:325`, after `initLogger()` and before everything else.
- Store: `TypeCheckedStoreBackend('migrationsStore', {cwd:'store', name:'migrations'})`, file `$CFG/store/migrations.json`, shape `{"appliedMigrations": ["<id>", ...]}`.
- Algorithm (`index.ts:29-71`):
  - Migrations run **sequentially** in list order. Any whose id is already in `appliedMigrations` is skipped.
  - The log shows `Applying migration "<id>"`.
  - If `run()` resolves truthy, the run is a success: it logs `successfully applied` and appends the id.
  - If it resolves falsy, the run counts as a graceful failure: it logs `failed. More details will be available above this message` and the id is not recorded.
  - If it throws, it logs `encountered error while applying:` with the error, and the id is not recorded.
  - Unrecorded migrations are retried on the next start. The final list is written back once.
- Migrations, in order:
  1. `legendary-move-global-config-folder` (`migrations/legendary.ts`):
     - If `$CFG/legendaryConfig/legendary` already exists, it returns true and does nothing.
     - Otherwise the source is `app.getPath('appData')/legendary` on Linux (i.e. `~/.config/legendary`), or `~/.config/legendary` on other OSes. If the source does not exist, it returns true.
     - Otherwise it runs `mkdir -p` on the target, then `cp -r` from source to target. This copies; the source is not moved.
  2. `umu-steam-runtime` (`migrations/config.ts`):
     - Non-Linux returns true.
     - On Linux, for every regular file `*.json` in `$CFG/GamesConfig` (appName = basename without `.json`), it loads `GameConfig.get(appName).getSettings()`. If the legacy setting `useSteamRuntime` is truthy, it calls `setSetting('steamRuntime', 'umu-scout')` and logs at debug level.
     - It returns true.
     - `GamesConfig` exists by then because `GlobalConfig.getSettings()`, called from `initLogger`, creates it (`backend/config.ts:294-296`).

---

## B.8 `backend/main.ts`: process startup

### B.8.1 Command-line flags and environment

| Input | Where | Effect |
|---|---|---|
| `--no-gui` | `backend/constants/environment.ts:35` (`isCLINoGui`) | Window is never shown (`main.ts:417-421`). Fullscreen is skipped (`main.ts:153`). The download queue is **not** started on `frontendReady` (`main.ts:472-474`). |
| `--fullscreen` | `environment.ts:34` (`isCLIFullscreen`) | `setFullScreen(true)` (`main.ts:153-164`). Window bounds are not persisted on close (`main.ts:209`). `isFullscreen` IPC returns true. |
| `--console` | `environment.ts:36` (`isCLIConsoleMode`) | Frontend loads with hash `/console` (`main.ts:229-230`). The same happens when setting `startInConsoleMode` is on. |
| `--spoof-windows` | `main.ts:332-339` | Adds request header `sec-ch-ua-platform: Windows` to every request of the default session. |
| `heroic://...` URL anywhere in argv | `main.ts:413-416, 469`; `backend/protocol.ts:17-25` | The first argv entry starting with `heroic://` is handled after `frontendReady`. A `heroic://launch` URL with `gui=false\|0\|no`, or with setting `hideWindowOnProtocolLaunch`, keeps the window hidden. |
| env `XDG_CURRENT_DESKTOP=gamescope` | `environment.ts:27-28` | Steam Deck game mode: fullscreen and no window-bounds persistence. |
| env `WAYLAND_DISPLAY` | `main.ts:422-436` | The window is shown on `did-finish-load` instead of `ready-to-show`. |
| env `ELECTRON_RENDERER_URL` | `main.ts:232-243` | Dev mode: React devtools, `loadURL`, devtools opened, and no auto-update check. |
| env `CI=e2e` | various | Temp config dir. No protocol registration. No uncaught-exception dialog. Online state is set immediately. No releases fetch or analytics. Test-only IPC listeners are registered. |
| env `FLATPAK_ID`, `APPIMAGE`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME` | `environment.ts:37-38`, `paths.ts`, `logger/paths.ts` | Packaging and path decisions. `checkForUpdatesOnStartup` defaults to `!isFlatpak`. |
| Chromium switches added | `main.ts:146` (Linux `--gtk-version=3`), `:196` (`enable-spatial-navigation`), `:370` (`disable-smooth-scrolling` if set) | Electron-specific and not relevant to a Rust backend. The last one is appended after `ready`, so it most likely has no effect. |

### B.8.2 Startup sequence

**Module-load phase.** ES imports are hoisted, so this happens before any `main.ts` body code, including the `import './…/ipc_handler'` lines at `main.ts:1441-1454`:
1. `backend/constants/paths.ts` resolves `appFolder` and related paths. `environment.ts` evaluates the argv and env flags. With Flatpak, it reads `/app/manifest.json`.
2. `import 'backend/updater'` (`main.ts:22`) sets `autoUpdater.autoDownload=false` and `autoInstallOnAppQuit=false`, and attaches `update-available`, `update-downloaded` and `error` listeners (`backend/updater.ts:10-11,59-77`).
3. `import 'backend/discounts'` (`main.ts:23`) registers the GOG, GMG and Humble handlers (B.3). This constructs the `gmg_deals` and `humble_deals` CacheStores.
4. Every other side-effect module registers its IPC handlers at import time: logger, wine manager, shortcuts, anticheat, known_fixes, EOS overlay, runtimes, download manager, utils, wiki_game_info, recent_games, tools, progress_bar (backendEvents → taskbar progress) and steamgrid. CacheStores and electron-stores are constructed now.
5. `main.ts:146`: on Linux, append `--gtk-version 3`.
6. `main.ts:296`: `gotTheLock = app.requestSingleInstanceLock()`.
7. `main.ts:309-311`: if the lock was **not** obtained, `console.log('Heroic is already running, quitting this instance')` and `app.quit()`. The primary instance receives `second-instance`.
8. `main.ts:313-321`: `second-instance(argv)` shows the main window unless `shouldHideWindowForProtocolArgs(argv)`, then calls `handleProtocol(argv)`.
9. Module-level listeners, registered regardless of the lock:
   - `notify` (`:465`)
   - `once frontendReady` (`:467-480`)
   - `process.on('uncaughtException')` (`:483-504`): logs, then shows the error dialog `box.error.uncaught-exception.*` unless `CI=e2e`
   - `lock` / `unlock` (`:509-535`)
   - all `main.ts` IPC handlers (`:537-1430`)
   - `app.on('window-all-closed')` (`:570-574`): quits when not on macOS
   - `app.on('open-url')` (`:576-585`): macOS; stores the URL in `openUrlArgument` if there is no window yet

**`app.whenReady()` phase** (`main.ts:322-462`), in order:
1. `:323` `initLogger()` (B.4.2). This also calls `GlobalConfig.get()`, which creates `$CFG/GamesConfig` if missing.
2. `:325` `await MigrationSystem.get().applyMigrations()` (B.7).
3. `:327` `initOnlineMonitor()` (B.6), which starts pinging.
4. `:328` `initStoreManagers()`: `Promise.all(manager.init())` for each library manager (`backend/storeManagers/index.ts:62-66`). It is **not awaited**.
5. `:329` `initImagesCache()`: `mkdir $CFG/images-cache` and register the `imagecache://` scheme. A cache file is named by the sha256 of the real URL and downloaded in the background on a miss (`backend/images_cache.ts`).
6. `:332-339` `--spoof-windows` header hook. `:342-344` Windows only: `setAppUserModelId`.
7. `:346-361` `runOnceWhenOnline`:
   - If Legendary is not logged in, log (forced) `User Not Found, removing it from Store` and delete `configStore 'userInfo'`.
   - If GOG is logged in, call `GOGUser.getUserDetails()`.
8. `:363-367` If `analyticsOptIn === true` (default false), call `startPlausible()` (endpoint `https://analytics.heroicgameslauncher.com/api/event`, `backend/utils/plausible.ts:19`).
9. `:369-371` `disableSmoothScrolling` switch.
10. `:374` `playtimeSyncQueue.delete('lock')` (CacheStore `gog_playtime_sync_queue`). `:375-381`: unless `disablePlaytimeSync`, `runOnceWhenOnline(gog.syncQueuedPlaytime)`.
11. `:382` `runOnceWhenOnline(gogPresence.setPresence)`.
12. `:383-395` `await i18next.init` with:
    - `loadPath public/locales/{{lng}}/{{ns}}.json`
    - `fallbackLng:'en'`
    - `lng: settings.language` (default `en`)
    - `supportedLngs` from `common/languages`
    - `returnEmptyString:false`, `returnNull:false`
13. `:397` `await initializeWindow()` (`:148-291`):
    1. `createNecessaryFolders()`: mkdir `$CFG/GamesConfig`, `$CFG/icons` and, on Linux and macOS, `$CFG/tools` (`backend/utils.ts:503-517`).
    2. `configStore.set('userHome', homedir)`. `configStore` is `$CFG/store/config.json`.
    3. `createMainWindow()` (`backend/main_window.ts:20-72`):
       - Uses the bounds from `configStore 'window-props'`, or defaults of 1200×690 clamped to 80% of the work area.
       - With the `framelessWindow` setting on, uses `frame:false` on Linux.
       - `minHeight` 345, `minWidth` 600, `show:false`, `webviewTag`, `contextIsolation`, `nodeIntegration`, and a preload script.
    4. Steam Deck game mode or `--fullscreen`, and not `--no-gui`: `setFullScreen(true)`.
    5. After **2500 ms**:
       - `getAlternativeWine()`. If no Wine is found, `downloadDefaultWine()`. On Apple Silicon macOS, it also downloads when no `toolkit` Wine is present.
       - `DXVK.getLatest()` and `Winetricks.download()`.
       - On macOS, `checkRosettaInstall()`.
    6. `setIcon`. Window events `maximize`, `unmaximize`, `enter-full-screen` and `leave-full-screen` are forwarded to the frontend as `maximized`, `unmaximized` and `fullscreen(bool)`.
    7. `close` handler:
       - `preventDefault`.
       - Unless fullscreen mode is active, persist `window-props = {...bounds, maximized}`.
       - If `exitToTray && !noTrayIcon`, hide the window. Otherwise call `handleExit()`.
    8. `detectVCRedist` (no-op outside Windows).
    9. Load the frontend:
       - Dev mode: `loadURL`.
       - Otherwise: `Menu.setApplicationMenu(null)`, then `loadFile(public/index.html, {hash})`. If `checkForUpdatesOnStartup`, also call `autoUpdater.checkForUpdates()`.
    10. Navigation guard: `will-navigate` to a URL that does not match `publicDir` (or `localhost:5173` in dev) is cancelled and opened externally. `setWindowOpenHandler` allows **non**-matching URLs and denies matching ones.
    11. Zoom: `setZoomFactor` IPC and re-application on window state changes. The factor is `(zoom-1)/0.2`, scaled by `width/1200` when the work-area width is below 1200. The value comes from `configStore 'zoomPercent'` (default 100).
14. `:399-402` `protocol.handle('heroic')` calls `handleProtocol([url])` and responds `201 "Operation initiated."`.
15. `:403-411` Unless `CI=e2e` or Heroic is already the default client, `app.setAsDefaultProtocolClient('heroic')`, logging success or failure. Otherwise it logs `Protocol already registered.`.
16. `:413-437` `headless = --no-gui || hideForProtocol([openUrlArgument, ...argv]) || (startInTray && !noTrayIcon)`. If not headless, the window is shown on `ready-to-show` (or `did-finish-load` on Wayland) and maximized if `window-props.maximized`.
17. `:440-447` After **200 ms**, apply the initial zoom.
18. `:449-455` `changeLanguage(lang)` listener:
    - `i18next.changeLanguage`
    - clear the Legendary `gameInfoStore` (CacheStore `legendary_gameinfo`)
    - `setSetting('language')`
    - emit `languageChanged`, which the tray handles by rebuilding its menu
19. `:457` `fetchLastestReleases()`:
    - Once online, `GET https://raw.githubusercontent.com/Heroic-Games-Launcher/releases-info/refs/heads/main/release-data.json` and emit `releasesInfoReady`.
    - That event triggers Wine list refresh (non-Windows), anticheat data download and known-fixes download (`backend/utils/releases.ts:11-28`).
20. `:459` `initTrayIcon(mainWindow)` (B.5).

**Post-frontend** (`main.ts:467-480`): `once 'frontendReady'` logs `Frontend Ready` and calls `handleProtocol([openUrlArgument, ...process.argv])`. Unless `--no-gui`, it calls `initQueue()` (download queue) after **5000 ms**.

**Exit** (`handleExit`, `backend/utils.ts:241-280`):
1. If `$CFG/GamesConfig/lock` exists or a game is running, show a yes/no dialog `box.quit.message`. "No" aborts the exit. "Yes" runs `killPattern` for `legendary`, `gogdl` and `nile`, then all abort controllers.
2. Hide the window.
3. `gogPresence.deletePresence()`.
4. `app.exit()`.

**Power-save** (`main.ts:509-535`):
- `lock(playing)`:
  - `playing=false`, when no app-suspension blocker is held: starts `prevent-app-suspension`.
  - `playing=true`, when no display-sleep blocker is held: starts `prevent-display-sleep`.
- `unlock` stops both. A Rust port on Linux can use an `org.freedesktop.ScreenSaver.Inhibit` or `systemd-inhibit` equivalent.

### B.8.3 Single-instance
- Electron's lock is a per-user lock in `userData`. A Rust port needs an equivalent, for example a UNIX-socket or D-Bus name lock under `$XDG_RUNTIME_DIR`.
- A second instance must forward its `argv` to the primary and exit. The primary then shows its window, unless the forwarded args are a hide-for-protocol launch, and runs `handleProtocol(argv)`.
- Non-Linux only: macOS `open-url`; Windows `setAppUserModelId`.

---

## B.9 IPC surface overview (all `addHandler` / `addListener` / `addOneTimeListener` / `addTestOnlyListener` in `backend/`)

The wrappers are in `backend/ipc.ts`:
- `addHandler` → `ipcMain.handle`: request/response, the renderer awaits a result.
- `addListener` → `ipcMain.on`: fire and forget.
- `addOneTimeListener` → `ipcMain.once`.
- `addTestOnlyListener` → `ipcMain.on`, only when `CI=e2e`.

There are 187 registrations. They match the type map in `common/types/ipc.ts` (`SyncIPCFunctions` :59, `TestSyncIPCFunctions` :145, `AsyncIPCFunctions` :155), except that `clearDMFinished` and `updateAll` are declared in the types but **never registered**.

Kind column: H = handle, L = on, O = once, T = test-only on.

### B.9.1 `backend/main.ts`

| Line | Kind | Channel | Purpose |
|---|---|---|---|
| 269 | L | setZoomFactor | Apply the UI zoom factor to the window. |
| 449 | L | changeLanguage | Switch i18n language, clear Legendary info cache, persist, emit languageChanged. |
| 465 | L | notify | Show an OS notification `{title, body}`. |
| 467 | O | frontendReady | Handle the startup protocol URL; start the download queue after 5 s. |
| 509 | L | lock | Start power-save blockers (app suspension / display sleep). |
| 524 | L | unlock | Stop power-save blockers. |
| 537 | H | checkDiskSpace | Free/total space, writability and Flatpak accessibility for a path. |
| 555 | H | isFrameless | Whether the window is frameless. |
| 556 | H | isMinimized | Window minimized state. |
| 557 | H | isMaximized | Window maximized state. |
| 558 | L | minimizeWindow | Minimize. |
| 559 | L | maximizeWindow | Maximize. |
| 560 | L | unmaximizeWindow | Unmaximize. |
| 561 | L | closeWindow | Close the window (goes through the close handler: tray or exit). |
| 562 | L | setFullscreen | Toggle fullscreen. |
| 565 | L | quit | `handleExit()`. |
| 587 | L | openExternalUrl | Open a URL or path externally. |
| 588 | L | openFolder | Open a folder. |
| 589 | L | openSupportPage | Open Support.md on GitHub. |
| 590 | L | openReleases | Open the GitHub latest release page. |
| 591 | L | openWeblate | Open Weblate. |
| 592 | L | showAboutWindow | Show the About panel. |
| 593 | L | openLoginPage | Open `https://legendary.gl/epiclogin`. |
| 594 | L | openDiscordLink | Open Discord. |
| 595 | L | openPatreonPage | Open Patreon. |
| 596 | L | openKofiPage | Open Ko-fi. |
| 597 | L | openGithubSponsorsPage | Open GitHub Sponsors. |
| 600 | L | openWinePrefixFAQ | Open the WineHQ prefix FAQ. |
| 601 | L | openWebviewPage | Open a URL externally. |
| 602 | L | openWikiLink | Open the Heroic wiki. |
| 603 | L | openSidInfoPage | Open the Epic alternative-login wiki page. |
| 604 | L | openCustomThemesWiki | Open the custom-themes wiki page. |
| 607 | L | showConfigFileInFolder | Open `config.json` or `GamesConfig/<app>.json`; flush it first if missing. |
| 618 | L | removeFolder | Remove a folder `[path, folderName]`. |
| 622 | H | runWineCommand | Run an arbitrary Wine command. |
| 626 | H | checkGameUpdates | List updatable games across runners, auto-queueing if `autoUpdateGames`. |
| 642 | H | getEpicGamesStatus | Whether the Epic service is offline (status API). |
| 644 | H | getMaxCpus | CPU count. |
| 646 | H | getHeroicVersion | App version. |
| 647 | H | isFullscreen | Steam Deck game mode or `--fullscreen`. |
| 648 | H | getGameOverride | Legendary game overrides. |
| 651 | H | getGameSdl | Legendary selective-download list. |
| 655 | H | showUpdateSetting | `!isFlatpak`. |
| 657 | H | getLatestReleases | Heroic GitHub releases, if `checkForUpdatesOnStartup`; else `[]`. |
| 666 | H | getCurrentChangelog | Changelog of the current version. |
| 670 | L | clearCache | Clear caches, send `refreshLibrary`, optional dialog. |
| 688 | L | clearAchievementCache | Clear the achievement cache for an app. |
| 696 | L | resetHeroic | Factory reset. |
| 698 | L | createNewWindow | Open a URL in a new 1200×700 BrowserWindow. |
| 702 | H | isGameAvailable | Whether the game files are reachable. |
| 707 | H | getGameInfo | GameInfo with overrides; `null` if unknown or empty. |
| 725 | H | getAchievements | Achievements for a game and language. |
| 732 | H | getExtraInfo | Store extra info (description, storeUrl, …). |
| 743 | H | getGameSettings | Per-game settings, or `null` on error. |
| 752 | H | getGOGLinuxInstallersLangs | Languages of GOG Linux installers. |
| 756 | H | getInstallInfo | Install info for platform, build and branch. |
| 780 | H | getUserInfo | Legendary user info. |
| 784 | H | getAmazonUserInfo | Nile user data. |
| 787 | H | isLoggedIn | Legendary logged-in state. |
| 789 | H | login | Legendary login (sid/code). |
| 790 | H | authGOG | GOG login with an auth code. |
| 791 | H | logoutLegendary | Epic logout. |
| 792 | L | logoutGOG | GOG logout. |
| 794 | H | getAmazonLoginData | Amazon login URL and PKCE data. |
| 795 | H | authAmazon | Amazon login. |
| 796 | H | logoutAmazon | Amazon logout. |
| 798 | H | authZoom | Zoom login, then fetch user details. |
| 806 | L | logoutZoom | Zoom logout. |
| 807 | H | getZoomUserInfo | Zoom user details. |
| 809 | H | getAlternativeWine | Detected Wine/Proton installations. |
| 813 | H | readConfig | `'library'` returns the Legendary game list after refresh; otherwise the display name. |
| 822 | H | requestAppSettings | Global settings. |
| 823 | H | requestGameSettings | Per-game settings. |
| 828 | H | toggleDXVK | Install or remove DXVK in the prefix. |
| 836 | H | toggleDXVKNVAPI | Install or remove DXVK-NVAPI. |
| 844 | H | toggleVKD3D | Install or remove VKD3D. |
| 852 | H | writeConfig | Write a full config (global or game). |
| 856 | L | setSetting | Set one key (global if appName is `default`, else game). |
| 864 | H | refreshLibrary | Refresh one or all library managers. |
| 877 | H | launch | Launch a game. |
| 881 | H | openDialog | Native open dialog; first path or `false`. |
| 894 | L | showItemInFolder | Reveal a path in the file manager. |
| 896 | H | uninstall | Uninstall a game. |
| 898 | H | repair | Repair a game (online only) with status updates and notifications. |
| 934 | H | moveInstall | Move an install, refusing a subfolder of the current path. |
| 1003 | H | importGame | Import an existing install, optionally writing Wine settings. |
| 1090 | H | kill | Abort the controller and stop the game. |
| 1095 | H | changeInstallPath | Change the recorded install path. |
| 1103 | H | egsSync | Toggle EGS game sync (Legendary). |
| 1107 | H | syncGOGSaves | GOG cloud-save sync. |
| 1111 | H | getLaunchOptions | Launch options, with a "Default" entry added when needed. |
| 1137 | H | syncSaves | Cloud-save sync (checks Epic status and online state). |
| 1160 | H | getDefaultSavePath | Compute the default save path. |
| 1167 | H | gamepadAction | Translate gamepad actions into synthetic input events. |
| 1295 | H | getShellPath | Resolve a path through the shell. |
| 1297 | H | getWebviewPreloadPath | Webview preload script path. |
| 1299 | H | clipboardReadText | Read the clipboard. |
| 1301 | L | clipboardWriteText | Write the clipboard. |
| 1303 | H | getCustomThemes | List `*.css` files in `customThemesPath`. |
| 1315 | H | getThemeCSS | Read a theme CSS file. |
| 1327 | H | getCustomCSS | `settings.customCSS`. |
| 1331 | L | setTitleBarOverlay | Set the title-bar overlay (Windows/macOS). |
| 1339 | L | addNewApp | Add a sideloaded app. |
| 1343 | L | setGameMetadataOverride | Set title/art overrides and broadcast `metadataChanged`. |
| 1349 | H | getGameMetadataOverride | Overrides for one app. |
| 1353 | H | getAllGameOverrides | All overrides. |
| 1357 | H | isNative | Whether the game is native for the host. |
| 1361 | H | pathExists | `existsSync(path)`. |
| 1365 | L | processShortcut | Keyboard shortcuts: ctrl+r reload, ctrl+q quit, ctrl+k settings, ctrl+j downloads, ctrl+l library, ctrl+shift+i devtools. |
| 1395 | H | getPlaytimeFromRunner | GOG playtime, unless `disablePlaytimeSync`. |
| 1410 | H | getPrivateBranchPassword | GOG private-branch password. |
| 1413 | H | setPrivateBranchPassword | Set the GOG private-branch password. |
| 1417 | H | getAvailableCyberpunkMods | GOG Cyberpunk mod list. |
| 1420 | H | setCyberpunkModConfig | Set Cyberpunk mod config. |
| 1424 | L | changeGameVersionPinnedStatus | Pin or unpin a game version. |
| 1428 | H | wine.isValidVersion | Validate a Wine installation. |

### B.9.2 Other modules

| File:line | Kind | Channel | Purpose |
|---|---|---|---|
| backend/online_monitor.ts:101 | L | connectivity-changed | Frontend pushes a connectivity status. |
| backend/online_monitor.ts:116 | H | get-connectivity-status | `{status, retryIn}`. |
| backend/online_monitor.ts:123 | L | set-connectivity-online | Force online. |
| backend/tray_icon/tray_icon.ts:51 | L | changeTrayColor | Reload the tray icon and menu after 500 ms. |
| backend/logger/ipc_handler.ts:16 | L | logInfo | Frontend info log. |
| backend/logger/ipc_handler.ts:17 | L | logError | Frontend error log. |
| backend/logger/ipc_handler.ts:19 | H | getLogContent | Last 1 MiB of a log. |
| backend/logger/ipc_handler.ts:29 | L | showLogFileInFolder | Reveal the log file. |
| backend/logger/ipc_handler.ts:33 | H | uploadLogFile | Upload to dpaste. |
| backend/logger/ipc_handler.ts:34 | H | deleteUploadedLogFile | Delete an upload (non-functional). |
| backend/logger/ipc_handler.ts:37 | H | getUploadedLogFiles | Unexpired uploads. |
| backend/wiki_game_info/ipc_handler.ts:5 | H | getWikiGameInfo | Aggregated extra info (B.1). |
| backend/steamgrid/ipc_handler.ts:28 | H | steamgriddb.hasApiKey | Whether a key is stored. |
| backend/steamgrid/ipc_handler.ts:30 | H | steamgriddb.setApiKey | Store (encrypt) or clear the key. |
| backend/steamgrid/ipc_handler.ts:36 | H | steamgriddb.searchGame | SGDB autocomplete. |
| backend/steamgrid/ipc_handler.ts:54 | H | steamgriddb.getGrids | SGDB grids. |
| backend/steamgrid/ipc_handler.ts:77 | H | steamgriddb.getHeroes | SGDB heroes. |
| backend/discounts/index.ts:151 | H | getGogDealsRegion | GOG country and currency. |
| backend/discounts/index.ts:176 | H | getGogDiscounts | GOG discounted catalog. |
| backend/discounts/index.ts:209 | H | getGogWishlist | GOG wishlist product IDs. |
| backend/discounts/gmg.ts:175 | H | getGmgDiscounts | GMG deals (cached 24 h). |
| backend/discounts/humble.ts:146 | H | getHumbleDiscounts | Humble deals (cached 24 h). |
| backend/downloadmanager/ipc_handler.ts:13 | H | install | Queue an install (plus Legendary DLCs). |
| backend/downloadmanager/ipc_handler.ts:46 | H | updateGame | Queue an update. |
| backend/downloadmanager/ipc_handler.ts:64 | L | removeFromDMQueue | Remove from the queue. |
| backend/downloadmanager/ipc_handler.ts:65 | L | resumeCurrentDownload | Resume. |
| backend/downloadmanager/ipc_handler.ts:66 | L | pauseCurrentDownload | Pause. |
| backend/downloadmanager/ipc_handler.ts:67 | L | cancelDownload | Cancel, optionally removing downloaded files. |
| backend/downloadmanager/ipc_handler.ts:70 | H | getDMQueueInformation | Queue elements and state. |
| backend/shortcuts/ipc_handler.ts:14 | L | addShortcut | Create desktop and menu shortcuts, then notify. |
| backend/shortcuts/ipc_handler.ts:33 | H | shortcutsExists | Whether shortcut files exist. |
| backend/shortcuts/ipc_handler.ts:41 | L | removeShortcut | Remove shortcuts, then notify. |
| backend/shortcuts/ipc_handler.ts:60 | H | addToSteam | Add as a non-Steam game. |
| backend/shortcuts/ipc_handler.ts:65 | H | removeFromSteam | Remove the non-Steam game. |
| backend/shortcuts/ipc_handler.ts:70 | H | isAddedToSteam | Whether it is in Steam shortcuts. |
| backend/tools/ipc_handler.ts:8 | H | runWineCommandForGame | Run a command in the game prefix (`runinprefix`). |
| backend/tools/ipc_handler.ts:25 | H | callTool | Run winetricks, winecfg or runExe in the prefix; for GOG, check offline-installer changes. |
| backend/tools/ipc_handler.ts:64 | L | winetricksInstall | Install a winetricks component. |
| backend/tools/ipc_handler.ts:68 | H | winetricksAvailable | List available components (`[]` on error). |
| backend/tools/ipc_handler.ts:76 | H | winetricksInstalled | List installed components. |
| backend/utils/ipc_handler.ts:15 | L | abort | Fire the abort controller by id. |
| backend/utils/ipc_handler.ts:18 | H | getLegendaryVersion | Helper binary version. |
| backend/utils/ipc_handler.ts:19 | H | getGogdlVersion | Helper binary version. |
| backend/utils/ipc_handler.ts:20 | H | getCometVersion | Helper binary version. |
| backend/utils/ipc_handler.ts:21 | H | getNileVersion | Helper binary version. |
| backend/utils/ipc_handler.ts:22 | H | getSystemInfo | System information (optionally cached). |
| backend/utils/ipc_handler.ts:23 | L | copySystemInfoToClipboard | Copy formatted system info. |
| backend/utils/ipc_handler.ts:28 | H | hasExecutable | Whether a binary is on PATH. |
| backend/utils/ipc_handler.ts:32 | H | isIntelMac | Intel macOS flag. |
| backend/wine/manager/ipc_handler.ts:15 | H | installWineVersion | Download and install a Wine/Proton release with progress and notifications. |
| backend/wine/manager/ipc_handler.ts:47 | H | refreshWineVersionInfo | Refresh the release lists. |
| backend/wine/manager/ipc_handler.ts:57 | H | removeWineVersion | Remove a Wine version. |
| backend/wine/runtimes/ipc_handler.ts:4 | H | downloadRuntime | Download a runtime (e.g. umu, EAC, BattlEye). |
| backend/wine/runtimes/ipc_handler.ts:6 | H | isRuntimeInstalled | Runtime presence. |
| backend/anticheat/ipc_handler.ts:8 | H | getAnticheatInfo | AreWeAntiCheatYet info by namespace. |
| backend/known_fixes/ipc_handler.ts:6 | H | getKnownFixes | Known fixes for an app. |
| backend/recent_games/ipc_handler.ts:4 | H | removeRecent | Remove from recent games. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:13 | H | getEosOverlayStatus | EOS overlay install status. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:14 | H | getLatestEosOverlayVersion | Latest overlay version. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:15 | H | updateEosOverlayInfo | Refresh overlay info. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:16 | H | installEosOverlay | Install the overlay. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:17 | H | removeEosOverlay | Remove the overlay. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:18 | H | enableEosOverlay | Enable it for the game prefix. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:19 | H | disableEosOverlay | Disable it for the game prefix. |
| backend/storeManagers/legendary/eos_overlay/ipc_handler.ts:20 | H | isEosOverlayEnabled | Enabled state. |
| backend/storeManagers/legendary/e2eMock.ts:42 | T | setLegendaryCommandStub | e2e: stub legendary CLI calls. |
| backend/storeManagers/legendary/e2eMock.ts:46 | T | resetLegendaryCommandStub | e2e: reset stubs. |
| backend/storeManagers/gog/e2eMock.ts:38 | T | setGogdlCommandStub | e2e: stub gogdl. |
| backend/storeManagers/gog/e2eMock.ts:42 | T | resetGogdlCommandStub | e2e: reset. |
| backend/storeManagers/nile/e2eMock.ts:38 | T | setNileCommandStub | e2e: stub nile. |
| backend/storeManagers/nile/e2eMock.ts:42 | T | resetNileCommandStub | e2e: reset. |

### B.9.3 Backend → frontend messages (`sendFrontendMessage`, `common/types/ipc.ts:376-421`)

`gameStatusUpdate`, `wineVersionsUpdated`, `showDialog`, `changedDMQueueInformation`, `maximized`, `unmaximized`, `fullscreen`, `refreshLibrary`, `openScreen`, `connectivity-changed`, `launchGame`, `installGame`, `recentGamesChanged`, `pushGameToLibrary`, `progressOfWinetricks`, `progressOfWineManager`, `installing-winetricks-component`, `logFileUploaded`, `logFileUploadDeleted`, `progressUpdate`, `metadataChanged`, and `message` (tests). They are delivered only if a main window exists (`backend/ipc.ts:55-64`).

---

## B.10 Store and cache files touched by this section (under `$CFG`)

| File | Owner | Shape |
|---|---|---|
| `store_cache/wikigameinfo.json` | wiki_game_info | `{<title>: WikiInfo, __timestamp: {<title>: DateString}}`, 30 d |
| `store_cache/umu.json` | umu | `{<runner>_<appName>: string\|null, __timestamp:{...}}`, 6 h (null entries only) |
| `store_cache/gog_api_info.json` | gamesdb (gog library) | `{<store>_<appName>: GamesDBData+etag, __timestamp:{...}}`, 6 h |
| `store_cache/gmg_deals.json` | discounts/gmg | `{<CUR>: CatalogProduct[], __timestamp:{...}}`, 24 h |
| `store_cache/humble_deals.json` | discounts/humble | `{USD: CatalogProduct[], __timestamp:{...}}`, 24 h |
| `store/uploadedLogs.json` | logger/uploader | `{<url>: {name, token:"1", uploadedAt}}` (no dot-notation) |
| `store/migrations.json` | migration | `{appliedMigrations: string[]}` |
| `store/config.json` | configStore (main.ts, tray) | keys used here: `window-props`, `zoomPercent`, `userHome`, `userInfo`, `games.recent`, `settings` (mirror) |
| `config.json` | GlobalConfig | `{defaultSettings: AppSettings, version: "v0"}`; includes `steamGridDbApiKey` (encrypted form) |
| `images-cache/<sha256(url)>` | images_cache | raw image bytes |
| `$LOGS/heroic.log[.old]`, `$LOGS/runners/<r>.log[.old]`, `$LOGS/games/<app>_<runner>/<type>.log[.old]` | logger | text lines (B.4.3) |
