# Parity spec: Amazon (nile), Zoom, Sideload, Store-manager interface, Download manager

Reference: Heroic Games Launcher v2.22.4 (TypeScript). This document is the
behavioral contract for the Rust reimplementation. Engineers should not need to
read the TS. All file references are relative to `src/` of the reference tree.
Abbreviations used in references:

| Abbrev | Path |
|---|---|
| `SM/` | `backend/storeManagers/` |
| `DM/` | `backend/downloadmanager/` |
| `T` | `common/types.ts` |
| `GM` | `common/types/game_manager.ts` |
| `ES` | `common/types/electron_store.ts` |

Notation: "**[QUIRK]**" marks reference behavior that looks accidental. Parity
means reproducing externally observable behavior (files on disk, IPC payloads,
CLI invocations). Where a quirk is clearly a bug, the spec says so and gives a
recommended behavior; the Rust team should decide per item, but must keep file
formats and IPC shapes identical either way.

---

## 0. Shared infrastructure the stores rely on

### 0.1 Paths

* `appData` = `$XDG_CONFIG_HOME` or `~/.config` (Flatpak:
  `~/.var/app/com.heroicgameslauncher.hgl/config`). `backend/constants/paths.ts:8`.
* `appFolder` = `<appData>/heroic` (`paths.ts:23`). Electron's
  `app.getPath('userData')` is the same directory, and every electron-store
  `cwd` below is relative to it.
* E2E: when `CI=e2e`, config folder is a temp dir (`paths.ts:11-18`) and nile
  commands are stubbed (`SM/nile/e2eMock.ts`, see 1.11).

### 0.2 electron-store file semantics (must match byte-for-byte shape)

Both the backend and the **renderer** read/write these JSON files (the frontend
reads libraries directly via `storeGet` IPC, `frontend/helpers/electronStores.ts`),
so file layout is part of the contract.

* File = `<appFolder>/<cwd>/<name>.json`; default `name` is `config`.
* Serialized with `JSON.stringify(obj, null, '\t')` (tab-indented).
* Keys are **dot-path** keys: `set('a.b', v)` writes `{"a":{"b":v}}`; `get('a.b')`
  traverses. This matters for Amazon ids, which contain dots
  (`amzn1.adg.product.<uuid>`): `nile_install_info.json` is therefore *nested*
  (`{"amzn1":{"adg":{"product":{"<uuid>":{...}}}}}`).
* Values `undefined` are dropped on serialization.
* `clearInvalidConfig: true` → a corrupt file is replaced by `{}`; when false/unset
  a corrupt file throws at construction.

`TypeCheckedStoreBackend` (`backend/electron_store.ts:11-58`): thin wrapper —
`has/get(key, default)/get_nodefault/set/delete/clear`.

`CacheStore<V>` (`backend/cache.ts`):
* File `<appFolder>/store_cache/<filename>.json`, `clearInvalidConfig: true`.
* `set(key, v)` writes `key` = v and `__timestamp.<key>` = JS `Date()` string,
  e.g. `"Thu Oct 22 2026 15:26:59 GMT+0300 (Eastern European Summer Time)"`.
  (Because of dot-paths this is stored as `{"__timestamp": {"<key>": "..."}}`.)
* `get(key, fallback)`: missing key → fallback. If `lifespan` (minutes) is
  non-null: missing timestamp → fallback; age > lifespan (and invalidateCheck,
  default true) → delete key and timestamp, return fallback.
* Default lifespan = `60*6` minutes (6 h). `null` = never expires.
* `delete(key)` removes key and its timestamp; `clear()` empties the file.

### 0.3 Helper behaviors referenced below

* `getFileSize(bytes)` — `filesize` with `{base: 2}` → IEC strings with 2-decimals
  max, e.g. `"73.59 GiB"`, `"512 MiB"`, `"0 B"` (`backend/utils.ts:142`).
* `parseSize(str)` (`backend/utils.ts:1494-1512`): lower-case + trim; check
  suffixes in order `bytes, kb, mb, gb, tb, pb`; first matching suffix stripped,
  multiplier `1024^index`; returns `round(parseFloat(rest) * mult)`.
  **[QUIRK]** no suffix match → index 6 → multiplier 1024^6; `"500 B"` parses as
  500·1024^6. NaN possible (parseFloat never throws).
* `removeSpecialcharacters(text)` (`backend/utils.ts:357-360`): removes every
  char of the set `: | / * ? < > \ & { } % $ @ ` ! ™ + ' " ®` (global). Used to
  derive Amazon folder names; nile uses the same set internally.
* `killPattern(p)` (`backend/utils.ts:802-814`): Linux/macOS `pkill -f <p>`;
  Windows `powershell Stop-Process -name <p>`.
* `shutdownWine(settings)` (`utils.ts:816`): `wineserver -k` with WINEPREFIX if
  `wineVersion.wineserver` set, else `wineboot -k` via runWineCommand.
* `removeFolder(path, folderName)` (`utils.ts:1232-1252`): if
  `path==='default'` → `<defaultInstallPath without '>/<folderName>`, delete
  recursively after **5000 ms**; else `"<path>/<folderName>"` with all `'`
  removed, delete after **2000 ms**. Only if it exists at call time.
* `sendGameStatusUpdate(GameStatus)` → renderer event `gameStatusUpdate` + backend
  event bus `gameStatusUpdate` (`utils.ts:1327`).
* `sendProgressUpdate(GameStatus)` → renderer `progressUpdate` + backend bus
  `progressUpdate-<appName>` (`utils.ts:1332`).
* `clearCache(library?)` (`utils.ts:369-398`): always clears `wikigameinfo`;
  `'nile'` (or none) clears `nile_install_info` + `nile_library`; `'zoom'` clears
  nothing zoom-specific; also clears pci-id caches unless `fromVersionChange`.

### 0.4 `callRunner` — how helper binaries are spawned (`backend/launcher.ts:1658-1830`)

Every nile call and Zoom native launch goes through this. Semantics to preserve:

1. `appName` for logging/abort is derived from args (`launcher.ts:1615-1656`):
   for `nile`, if any arg ∈ {`launch`,`install`,`update`,`verify`} → appName =
   **last** arg; otherwise `''`.
2. Adds the runner's global log writer to `logWriters`.
3. Drops falsy args. `bin` = runner.bin; `fullRunnerPath = join(dir, bin)` if dir.
   On non-Windows, if `bin` is not absolute and `dir` is set, spawn `./<bin>`
   with `cwd = options.cwd || runner.dir`.
4. Windows + powershell present: wrap as `powershell -NoProfile Start-Process
   "`"<full>`"" -Wait -NoNewWindow -ArgumentList <quoted,args>`.
5. Logs `"<logMessagePrefix ?? 'Running command'>: <safe command>"` where safe
   command = changed env vars (`K=V`, only those differing from process env) +
   quoted path + args, with the value after `--code` / `--token` replaced by
   `<redacted>`. Each log writer additionally gets `"Game Output:"` if appName.
6. **Dedup**: key = `"<runner> <args joined by ','>"` (JS array→string). If an
   identical command is already running, return the **same promise**.
7. Abort: `abortId = options.abortId || appName || random`; an AbortController is
   registered under that id (`utils/aborthandler`); `callAbortController(id)`
   kills the child (SIGTERM via spawn `signal`).
8. env = `process.env ∪ options.env`. `options.wrappers` is **ignored** by
   callRunner (only used by callers).
9. stdout/stderr are read as UTF-8 chunks. For each chunk: sanitized copy
   (`logSanitizer`) to every log writer; raw chunk to `onOutput(chunk, child)`;
   chunk `.trim()`-ed into a ring buffer of the **last 50 chunks**
   (`memoryLog`, `utils.ts:1216`).
10. On close: `errorHandler(stdout+stderr, appName, runner)` (shows dialogs for
    "appears to be deleted" → force-uninstall prompt, "No saved credentials").
    Result `stdout` = last ≤50 trimmed chunks joined with `''`; `stderr` = joined
    with `'\n'`. **Exit code is ignored**: a non-zero exit is NOT an error.
    If terminated by a signal and `child.killed` is false → error.
11. Errors: if aborted → `{stdout:'', stderr:'', fullCommand, abort:true}`; other
    spawn/signal error → `{stdout:'', stderr:String(err), fullCommand, error}`.
    Success → `{stdout, stderr, fullCommand}`.

`ExecResult` = `{stderr, stdout, fullCommand?, error?, abort?}` (`T:150-156`).

---

## 1. Amazon Games (runner id `nile`)

Heroic shells out to the bundled `nile` CLI (tested: `nile --version` →
`1.2.0 Robert Speedwagon`). Nile logs to **stderr** in the form
`LEVEL [MODULE]:\t message` (e.g. `ERROR [UNINSTALL]:\t Game isn't installed`);
machine output (JSON) goes to stdout. Nile returns exit code 0 for many failures.

### 1.1 Binary resolution (`backend/utils.ts:491-501`)

* If global setting `altNileBin` is non-empty → `{dir: dirname, bin: basename}` of it.
* Else `archSpecificBinary('nile')`: `<publicDir>/bin/<process.arch>/<platform>/nile[.exe]`
  if it exists, else `<publicDir>/bin/x64/<platform>/nile[.exe]`, asar path
  fixed to `app.asar.unpacked`. On this system:
  `/usr/lib64/heroic/resources/app.asar.unpacked/build/bin/x64/linux/nile`.

### 1.2 Environment and config locations

`runRunnerCommand(args, options)` (`SM/nile/library.ts:464-489`):
* If `CI=e2e` → stub (1.11).
* Always sets `options.env.NILE_CONFIG_PATH = <appFolder>/nile_config` (the
  dirname of `nileConfigPath`). Nile appends `nile/` itself. Note: `env` here is
  merged *on top of* `process.env` by callRunner; for `launch` the caller passes a
  full env which is then extended.
* Spawned via callRunner with `{name:'nile', logPrefix: Nile, bin, dir}`.

Files (`SM/nile/constants.ts`):

| Constant | Path | Owner / shape |
|---|---|---|
| `nileConfigPath` | `<appFolder>/nile_config/nile` | nile's config dir |
| `nileInstalled` | `.../nile/installed.json` | written by nile; Heroic also rewrites it. `NileInstallMetadataInfo[]` = `[{id, version, path, size?}]` (`common/types/nile.ts:29-34`). `[]` when empty. |
| `nileLibrary` | `.../nile/library.json` | written by `nile library sync`; `NileGameInfo[]` (1.4) |
| `nileUserData` | `.../nile/current_user.json` | read by Heroic; `NileUserData` = `{user_id, name}` |
| (nile-private) | `.../nile/user.json` | nile tokens. **Never read by Heroic.** |
| (nile-private) | `.../nile/SDK/...` | Amazon Games SDK DLLs nile extracts on every start |

Heroic-owned stores (`SM/nile/electronStores.ts`):

| Store | File | Keys |
|---|---|---|
| `installStore` (CacheStore, 6 h) | `store_cache/nile_install_info.json` | `<appName>` → `NileInstallInfo` (nested by dots, see 0.2) |
| `libraryStore` (CacheStore, never expires) | `store_cache/nile_library.json` | `library` → `GameInfo[]` |
| `configStore` | `nile_store/config.json` | `userData` → `NileUserData` (`ES:82-84`). Presence = logged in. |

### 1.3 Authentication flow

Frontend route `/loginweb/nile` (`frontend/screens/WebView/index.tsx`).

1. **Get login data** — IPC handler `getAmazonLoginData` → `NileUser.getLoginData()`
   (`SM/nile/user.ts:28-41`):
   `nile auth --login --non-interactive` (abortId `nile-auth`, logSanitizer replaces
   JSON fields `url`, `code_verifier`, `serial`, `client_id` with `"<redacted>"`).
   stdout parsed as JSON `NileLoginData {url, code_verifier, serial, client_id}`
   (`common/types/nile.ts:109-114`). Logged (unsanitized) at info level — Rust
   should **not** log secrets.
   Observed `url`: `https://amazon.com/ap/signin?` with query params
   `openid.ns=http://specs.openid.net/auth/2.0`,
   `openid.claimed_id` / `openid.identity` = `.../identifier_select`,
   `openid.mode=checkid_setup`, `openid.oa2.scope=device_auth_access`,
   `openid.ns.oa2=http://www.amazon.com/ap/ext/oauth/2`,
   `openid.oa2.response_type=code`, `openid.oa2.code_challenge_method=S256`,
   `openid.oa2.client_id=<device client id>`, `language=en_US`,
   `marketPlaceId=ATVPDKIKX0DER`, `openid.return_to=https://www.amazon.com`,
   `openid.pape.max_auth_age=<n>`, `openid.assoc_handle=amzn_sonic_games_launcher`,
   `pageId=amzn_sonic_games_launcher`, `openid.oa2.code_challenge=<43 chars>`.
   (serial is 32 chars, client_id 94 chars.) All generated locally by nile.
2. The renderer opens `url` in a `<webview>` with partition `persist:amazon`.
3. **Code extraction**: on every `dom-ready` of the webview, parse
   `webview.getURL()`; if query param `openid.oa2.authorization_code` is present,
   that is the `code` (`WebView/index.tsx:217-225`). (Happens after redirect to
   `https://www.amazon.com/?...openid.oa2.authorization_code=...`.)
4. **Register** — IPC `authAmazon(NileRegisterData {code, code_verifier, serial,
   client_id})` → `NileUser.login` (`user.ts:43-87`):
   `nile register --code <code> --code-verifier <cv> --serial <serial> --client-id <cid>`
   (abortId `nile-login`; `--code` value redacted in logs).
   Success iff **stderr** matches `/\[AUTH_MANAGER]:.*Succesfully registered a device/`
   (sic, misspelled). Else → `{status:'failed', user: undefined}`.
   On success → `getUserData()`; if it returns undefined → failed; else
   `{status:'done', user}`.
5. `getUserData()` (`user.ts:109-127`, also IPC `getAmazonUserInfo`): if
   `current_user.json` missing → log error, delete `userData` from configStore,
   return undefined. Parse JSON; if `{}` → delete `userData`, undefined. Else
   `configStore.set('userData', user)` and return it.
6. `isLoggedIn()` (`user.ts:129-131`): `configStore.userData || false` (truthy check
   only; no network).
7. **Logout** — IPC `logoutAmazon` (`user.ts:89-107`): `nile auth --logout`
   (abortId `nile-logout`; nile deregisters the device remotely). If result
   `abort` → log and return without clearing. Otherwise: delete
   `configStore.userData`, `clearCache('nile')`, and clear Electron session
   `persist:amazon` (storage data, cache, auth cache).

Renderer displays user from `nile_store/config.json` keys `userData.user_id`,
`userData.name` (`GlobalState.tsx:206-207`).

### 1.4 Library refresh and GameInfo mapping

`init()` (`SM/nile/library.ts:35-44`): migration — if `<appFolder>/nile_config/nile`
does **not** exist and `<appData>/nile` (global nile config, e.g. `~/.config/nile`)
exists, copy the whole directory recursively and call `getUserData()`. Then
`refresh()` (not awaited).

`refresh()` (`library.ts:269-287`): if not logged in → `{stdout:'',stderr:''}`.
Else:
1. `refreshNile()` **not awaited**: `nile library sync` (abortId `nile-refresh`);
   on `error` logs; always returns empty ExecResult. **[QUIRK]** Because it is not
   awaited, steps 2-4 read the *previous* `library.json`; new purchases appear on
   the next refresh. Recommended: await it (no file-format impact).
2. `refreshInstalled()`: clear in-memory `installedGames`; if `installed.json`
   exists parse it and index by `id` (parse error → log "Corrupted installed.json").
3. `loadGamesInAccount()` (`library.ts:58-116`): if `library.json` exists parse
   `NileGameInfo[]` and for each entry **set** (never removes stale entries)
   `library[product.id] = GameInfo`:

| GameInfo field | Value |
|---|---|
| `app_name` | `product.id` |
| `art_cover` | `details.backgroundUrl2 \|\| productDetail.iconUrl` |
| `art_logo` | `details.logoUrl` |
| `art_background` | `details.backgroundUrl1 \|\| details.backgroundUrl2` |
| `art_square` | `productDetail.iconUrl` |
| `canRunOffline` | `true` |
| `install` | if installed metadata exists: `{install_path: meta.path, install_size: getFileSize(meta.size ?? 0), version: meta.version, platform: 'Windows'}`; else `{}` |
| `folder_name` | `removeSpecialcharacters(title ?? '')` |
| `is_installed` | metadata exists |
| `runner` | `'nile'` |
| `title` | `product.title ?? '???'` |
| `description` | `details.shortDescription` |
| `developer` | `details.developer` |
| `extra` | `{reqs: [], genres: details.genres, releaseDate: details.releaseDate}` |
| `is_linux_native` / `is_mac_native` | `false` |

4. `libraryStore.set('library', Array.from(library.values()))`, log count.

`NileGameInfo` input shape (`common/types/nile.ts:44-85`): `{id, product:{id,
title?, productDetail:{iconUrl, details:{backgroundUrl1, backgroundUrl2,
developer, esrbRatins, gameModes[], genres[], keywords[], legacyProductIds[],
logoUrl, otherDevelopers[], pegiRating, pgCrownImageUrl, publisher, releaseDate,
screenshots[], shortDescription, trailerImageUrl, uskRating, videos[],
websites:{official,steam,support,gog}}}}}`.

`getGameInfo(appName, forceReload=false)` (`library.ts:296-317`): if not forced
and in memory → return. Else `refreshInstalled()` + `loadGamesInAccount()` and
look up again; missing → log error, `undefined`. (Does **not** write the cache.)

`NileGame.getGameInfo()` (`SM/nile/games.ts:70-93`): library lookup, or the empty
sentinel `{app_name:'', runner:'nile', art_cover:'', art_square:'', install:{},
is_installed:false, title:'', canRunOffline:false}`.

`getExtraInfo()` (`games.ts:95-107`): `{reqs: [], about: description ?
{description, shortDescription: description} : undefined, releaseDate:
extra.releaseDate}`.

`getGame(id)` caches one `NileGame` instance per id (`library.ts:46-53`) — needed
because progress accumulation is per-instance state.

### 1.5 Install info and update listing

`getInstallInfo(appName)` (`library.ts:322-390`; platform/options ignored):
1. `installStore.get(appName)` cached (≤6 h) → return.
2. `refreshInstalled()`. If game in `library`:
   `nile install --info --json <appName>` (abortId = appName); stdout parsed as
   `{download_size: number}`. Result (and cached via `installStore.set`):
   ```
   { game: { id: appName, path: '', version: '', launch_options: [], owned_dlc: [],
             app_name, cloud_saves_supported: false, external_activation: '',
             is_dlc: false, platform_versions: { Windows: meta?.version ?? '' },
             title, ...installedMetadata /* id, version, path, size override */ },
     manifest: { download_size, disk_size: download_size } }
   ```
3. Not in library → log, return all-empty object (strings `''`, sizes `0`,
   `platform_versions.Windows: ''`), not cached.
   JSON parse failure throws → IPC handler returns `null`.

`listUpdateableGames()` (`library.ts:173-197`): not logged in → `[]`.
`nile list-updates --json` (abortId `nile-list-updates`); empty stdout → `[]`;
else `JSON.parse(stdout)` → `string[]` of ids. (Logged-out/no-installed prints `[]`.)

### 1.6 Install (`SM/nile/games.ts:231-272`)

Args: `InstallArgs` — only `path` used.
1. `maxWorkers` from global settings; if truthy → `--max-workers <n>`.
2. `nile install --base-path <path> [--max-workers N] <appName>`
   (abortId = appName; log writer `createGameLogWriter(appName,'nile','install')`;
   prefix `Installing <appName>`; `onOutput` → progress parser with action
   `installing`). Nile appends the sanitized folder name to base path.
3. `abort` → `{status:'abort'}`. `error` → log unless it contains `signal`;
   `{status:'error', error}`.
4. Success: `addShortcuts()` (not awaited), `installState(id, true)`, read
   install metadata; **Windows host only**: `await setup(id, metadata.path)` (1.10).
5. `{status:'done'}`.

### 1.7 Progress parsing (`games.ts:151-229`)

Per-instance `tmpProgress` (initially `{bytes:'', eta:'', percent:undefined,
diskSpeed:undefined, downSpeed:undefined}`). For every stdout/stderr chunk:

| Field | Only parsed when | Regex (multiline) | Value |
|---|---|---|---|
| percent | `!percent` (0 counts as unset) | `/Progress: (\d+\.\d+) /m` | `Number(g1)` or undefined |
| eta | `eta === ''` | `/ETA: (\d\d:\d\d:\d\d)/m` | `g1` or `''` |
| bytes | `bytes === ''` | `/Downloaded: (\S+) MiB/m` | `` `${g1}MB` `` or `''` |
| downSpeed | `!downSpeed` | `/Download\t- (\S+.) MiB/m` | `Number(g1)` |
| diskSpeed | `!diskSpeed` | `/Disk\t- (\S+.) MiB/m` | `Number(g1)` |

(`\t` is a literal TAB. `(\S+.)` still captures the whole number, e.g. `12.34`.)
Expected nile output lines (legendary-style):
`= Progress: 12.34 1234/5678, Running for: 00:00:10, ETA: 00:01:00`,
`= Downloaded: 123.45 MiB, Written: ...`, ` + Download\t- 12.34 MiB/s`,
` + Disk\t- 12.34 MiB/s`.

When **all five** values are non-undefined and non-empty (note `downSpeed`/
`diskSpeed` of `0` count as set since only undefined/'' fail): log
`Progress for <title>: <percent>%/<bytes>/<eta> Down: <d>MB/s / Disk: <w>MB/s`,
emit `progressUpdate` `{appName, runner:'nile', status: action, progress}`, then
reset `tmpProgress`. Fields accumulate across chunks until complete.

### 1.8 Update / verify / uninstall / import / move

* **update()** (`games.ts:518-554`; overrides ignored):
  `nile update [--max-workers N] <appName>` (abortId appName; log writer type
  `update`; prefix `Updating <appName>`; progress action `updating`).
  abort → `abort`; error → `error` (log unless "signal"); success →
  `gameStatusUpdate {appName, runner:'nile', status:'done'}` and `done`.
* **repair()** (`games.ts:458-488`): needs `install.install_path`, else
  `{stderr:'',stdout:'',error:'Could not find install path for <id>'}`.
  `nile verify --path <install_path> <appName>` (log writer `repair`, prefix
  `Repairing <id>`). Returns the ExecResult.
* **uninstall(RemoveArgs)** (`games.ts:496-516`, RemoveArgs ignored):
  `nile uninstall <appName>` (prefix `Uninstalling <id>`). If `error` → log. Else
  if not abort → `removeShortcuts`, `removeNonSteamGame`, `installState(id,false)`.
  Always emit renderer event `refreshLibrary` with `'nile'`. Returns ExecResult.
  (nile deletes files listed in the manifest and updates installed.json.)
* **forceUninstall()** (`games.ts:556-558` → `library.ts:123-139`): clear entire
  in-memory installed map; rewrite `installed.json` without `id === appName`
  (compact `JSON.stringify`, no indentation).
* **importGame(folderPath)** (`games.ts:109-149`): `nile import --path <folder> <id>`
  (log writer `import`, prefix `Importing <id>`). abort/error → return as is.
  If stderr matches `/ERROR \[IMPORT]:\t(.*)/` → error = group 1 (keeps the
  leading space nile prints). Else `addShortcuts()`, `installState(id,true)`.
* **moveInstall(newPath)** (`games.ts:435-456`): `moveOnUnix` (Linux/mac; rsync
  `--archive --compress --no-human-readable --remove-source-files
  --info=name,progress <old>/ <newPath>/<basename(old)>`, else mv fallback) or
  `moveOnWindows`; then `changeGameInstallPath(id, result.installPath)`.
* **changeGameInstallPath(appName, newPath)** (`library.ts:398-441`, also IPC
  `changeInstallPath`): set `library[appName].install.install_path`; reload
  installed.json, set `path`, and if the file exists rewrite it with
  `JSON.stringify(Array.from(installedGames.values()))` (compact).
* **installState(appName, state)** (`library.ts:449-462`): false → remove from
  in-memory installed map and `installStore.delete(appName)`; true → read
  metadata from installed.json and add to map (missing → log).
* `getLaunchOptions` → `[]`; `changeVersionPinnedStatus` → warn only.
* `syncSaves` → `''` (no cloud saves). `isNative()` = host is Windows.
* **stop(stopWine=true)** (`games.ts:560-568`): `killPattern(isLinux ? appName :
  'nile')`; if `stopWine` and not native → `shutdownWine(settings)`.
* **isGameAvailable()**: `is_installed && install_path && exists(install_path)`.

### 1.9 Launch (`games.ts:298-433`)

1. `prepareLaunch(settings, logWriter, gameInfo, isNative)` (shared launcher;
   see launcher spec). Failure → log, `launchCleanup()`, error dialog
   "Launch aborted", return false.
2. Exe override: `launchArguments.type==='altExe'` → `--override-exe <executable>`;
   else if `settings.targetExe` → `--override-exe <targetExe>`. (Hidden nile flag;
   accepted by nile 1.2.0 even though not in `--help`.)
3. env = `process.env` + `setupWrapperEnvVars({appName, appRunner:'nile'})`
   (`HEROIC_APP_NAME`, `HEROIC_APP_RUNNER=nile`, `GAMEID=umu-0`,
   `HEROIC_APP_SOURCE=amazon`, `STORE=amazon`; `launcher.ts:1072-1101`) +
   (non-Windows) `setupEnvVars(settings, install_path)` + known-fixes env for
   `('nile')`.
4. `wrappers = setupWrappers(settings, mangohud, gamemode, gamescope, steamRuntime)`.
   Default `wineFlag = wrappers.length ? ['--wrapper', shlex.join(wrappers)] : []`.
5. Non-native (Linux/mac): `prepareWineLaunch(game, logWriter)` (on failure:
   dialog if reason, return false); merge its env vars; if UMU supported and
   `getUmuId(app_name,'nile')` → `GAMEID=<id>`. Then
   `wineFlag = [...getWineFlagsArray(settings, shlex.join(wrappers)),
   '--wine-prefix', settings.winePrefix]` where `getWineFlags`
   (`backend/utils/compatibility_layers.ts:518-576`):
   * wine / toolkit: `--wine <bin>` (+ `--wrapper <w>` if non-empty)
   * proton: `--no-wine --wrapper "<w> \"<protonBin>\" waitforexitandrun"`, or
     just `--wrapper <w>` when UMU is used
   * crossover: `--wine <bin>` (+ `--wrapper`)
   (bin has surrounding single quotes stripped).
   `prepareWineLaunch` also runs nile `setup()` the first time the game id is
   added to `<prefix>/installed_games` (`launcher.ts:854-887`).
6. Launch-option parameters: if `launchArguments.type` is undefined/`'basic'` →
   `launchArguments.parameters`, else `''`.
7. Command: `nile launch [--override-exe X] <wineFlag...>
   <shlex.split(params)...> <shlex.split(settings.launcherArgs ?? '')...>
   <appName> <extra args...>`.
8. Emit `gameStatusUpdate {status:'playing'}`; run with abortId appName, env,
   log writer, prefix `Launching <title>`. On error → log. `launchCleanup(rpcClient)`.
   Return `!error`.

### 1.10 FuelPump / `fuel.json` / post-install setup

`fetchFuelJSON(appName, installedPath?)` (`library.ts:144-166`): base =
`installedPath ?? gameInfo.install.install_path` (none → log, null);
`<base>/fuel.json` missing → null; parsed with **JSON5** (comments, trailing
commas allowed); parse failure → log, null.

`FuelSchema` (`common/types/nile.ts:87-102`):
```
{ SchemaVersion: string,
  PostInstall: [{ Command: string, Args: string[], ValidReturns?: number[],
                  AlwaysRun?: boolean, HideWindow?: boolean }],
  Main: { Command: string, Args: string[] } }
```
(`Main` is consumed by nile itself on launch; Heroic only uses `PostInstall`.)

`setup(appName, installedPath?)` (`SM/nile/setup.ts`):
1. Missing game info / base path / fuel.json → log, return.
2. Empty `PostInstall` → log, return.
3. Warn "Running setup instructions...". On non-Windows: `checkWineBeforeLaunch`
   (failure → log and return) and `verifyWinePrefix(settings)`.
4. `gameStatusUpdate {appName, runner:'nile', status:'redist', context:'AMAZON'}`.
5. For each action sequentially (no ValidReturns / AlwaysRun / HideWindow
   handling — all ignored):
   * Windows: `powershell -NoProfile Start-Process -FilePath <Command>
     [-ArgumentList <args...>]`, cwd = base.
   * Else: `runWineCommand({commandParts:[Command, ...Args], wait:true,
     protonVerb:'run', startFolder: base, gameInstallPath: base})`.
   `Command` paths are as written in fuel.json (usually relative to install dir).
Called from: install on Windows hosts (1.6) and from `prepareWineLaunch` on
first launch in a prefix (Linux/mac).

### 1.11 E2E stub (`SM/nile/e2eMock.ts`)

When `CI=e2e`, `runRunnerCommand` returns the first stub whose `commandParts`
are all contained in the args; default stub: `['--version']` → stdout
`1.0.0 Jonathan Joestar`. Test-only IPC `setNileCommandStub(stubs)` /
`resetNileCommandStub()`. Optional for Rust.

### 1.12 Misc

* `getNileVersion` IPC (`backend/utils/helperBinaries/index.ts:53-64`):
  `nile --version` (abortId `nile-version`) → stdout, or `'invalid'` on error.
* Known-fixes file section key for Amazon is `amazon` (`T:849-854`).
* Save sync default path for nile is `''` (`backend/save_sync.ts`).
* Analytics flag: `amazon: !!NileUser.isLoggedIn()`.

### 1.13 Complete nile CLI call table

| Purpose | Args | abortId | Output used |
|---|---|---|---|
| version | `--version` | `nile-version` | stdout |
| login data | `auth --login --non-interactive` | `nile-auth` | stdout JSON |
| register | `register --code C --code-verifier V --serial S --client-id I` | `nile-login` | stderr regex |
| logout | `auth --logout` | `nile-logout` | abort flag |
| sync library | `library sync` | `nile-refresh` | error only |
| update list | `list-updates --json` | `nile-list-updates` | stdout JSON `string[]` |
| install info | `install --info --json <id>` | `<id>` | stdout JSON `{download_size}` |
| install | `install --base-path <p> [--max-workers N] <id>` | `<id>` | progress, abort/error |
| update | `update [--max-workers N] <id>` | `<id>` | progress, abort/error |
| verify | `verify --path <install_path> <id>` | `<id>` | ExecResult |
| uninstall | `uninstall <id>` | `<id>` | error/abort |
| import | `import --path <folder> <id>` | `<id>` | stderr regex |
| launch | `launch [--override-exe X] [wine flags] --wine-prefix P [args] <id> [extra]` | `<id>` | error |

All with `NILE_CONFIG_PATH=<appFolder>/nile_config`.

---

## 2. Zoom Platform (runner id `zoom`)

Implemented **directly over HTTPS** with axios; no helper binary. Entire store is
gated by global setting `experimentalFeatures.zoomPlatform` — when false, nearly
every library-manager method returns early (`undefined`, `[]`, `{}` or
`{stdout:'', stderr:'Zoom Support disabled'}` for `refresh`).

### 2.1 Constants and storage (`SM/zoom/constants.ts`, `SM/zoom/electronStores.ts`)

* `embedUrl` = `apiUrl` = `https://www.zoom-platform.com`.
* Token file: `<appFolder>/zoom_store/.zoom.token` — raw token text (UTF-8, no newline).

| Store | File | Keys |
|---|---|---|
| `installedGamesStore` | `zoom_store/installed.json` | `installed` → `InstalledInfo[]` |
| `configStore` | `zoom_store/config.json` | `isLoggedIn: bool`, `username?: string`, `credentials?` (declared, unused) |
| `libraryStore` (CacheStore, never expires) | `store_cache/zoom_library.json` | `games` → `GameInfo[]` |
| `installInfoStore` (CacheStore 6 h) | `store_cache/zoom_install_info.json` | `<appName>` → `ZoomInstallInfo` (written, never read) |
| `libraryCache` (CacheStore 6 h) | `store_cache/zoom-library.json` | `library` → raw `ZoomGameInfo[]` (cleared at start of every `refresh`) |

In-memory: module-level `library: Map<app_name, GameInfo>` and
`installedGames: Map<appName, InstalledInfo>` (`SM/zoom/library.ts:33-35`).
`getGame(id)` returns a **new** `ZoomGame` each time.

### 2.2 HTTP client (`SM/zoom/user.ts:139-156`)

`makeRequest<T>(url)`: credentials = `getCredentials()`; none → throws
`Error('Not authenticated with Zoom')`. `GET url` with headers
`Authorization: Bearer <token>`, `Accept: application/json`. Non-2xx → axios
throws (logged "Zoom API request failed: <message>"). Returns parsed body.

`getCredentials()` (`user.ts:67-89`): offline → undefined (warn); token file
missing/empty/unreadable → undefined; else `{access_token: <file content>}`.

### 2.3 Endpoints

| Method | URL | Response | Used by |
|---|---|---|---|
| GET (browser) | `https://www.zoom-platform.com/login?li=heroic&return_li_token=true` | redirects with `li_token` query param | login |
| GET | `/li/loggedin` | `{name: string}` (2xx = valid token) | `isLoggedIn`, `getUserDetails` |
| GET | `/li/games` then `/li/games?page=N` | `ZoomLibraryResponse {current_page, total_pages, games: ZoomGameInfo[]}` | library |
| GET | `/li/game/<id>/files` | `ZoomFilesResponse {manual[], misc[], soundtrack[], windows?[], linux?[]}` of `ZoomDownloadFile` | install info, installers, extras |
| GET | `/li/download/<fileId>` | `{url: string}` (direct download URL) | installers, extras |
| GET | `<url from /li/download>` | binary stream (no auth header) | install |

`ZoomGameInfo` = `{id: number, slug, name, poster_url, description, store_url,
operating_systems: string[] /* 'windows'|'linux'|'osx' */, developers: string[],
publishers: string[]}`. `ZoomDownloadFile` = `{id: string, name, size: string
/* human, e.g. "1.2 GB" */, url?, filename, total_size: number}`
(`common/types/zoom.ts`).

### 2.4 Auth flow

1. Renderer opens the login URL above in a webview, partition `persist:zoom`.
2. On every `did-navigate`, if the URL has query param `li_token`, renderer calls
   IPC `authZoom(fullUrl)` (`WebView/index.tsx:295-307`).
3. `ZoomUser.login(url)` (`user.ts:17-38`): parse URL, take `li_token`; absent →
   `{status:'error'}`. Write token to `.zoom.token`, set `configStore.isLoggedIn=true`,
   `{status:'done'}` (write failure → error). Handler then calls
   `getUserDetails()` when done (`backend/main.ts:798-805`).
4. `getUserDetails()` (IPC `getZoomUserInfo`, `user.ts:40-65`): offline → undefined;
   `!isLoggedIn()` → undefined; `GET /li/loggedin` → `configStore.username = name`,
   return `{username}`; errors → undefined.
5. `isLoggedIn()` (`user.ts:108-137`): token file missing → if store flag true set
   false; return false. Offline → return stored flag. Else `GET /li/loggedin`:
   success → ensure flag true, return true; failure → flag false, **logout()**,
   return false.
6. `isLoggedInSync()`: token file exists && flag.
7. `logout()` (listener `logoutZoom`, `user.ts:91-102`): `clearCache('zoom')`,
   `configStore.clear()`, delete token file, clear session `persist:zoom`.

### 2.5 Library

`init()`: if enabled → `await refresh()`.

`refresh()` (`library.ts:49-111`):
1. disabled → `{stdout:'', stderr:'Zoom Support disabled'}`.
2. `libraryCache.clear()`, `refreshInstalled()` (load `installed` array into map
   keyed by `appName`, skipping entries without appName).
3. `!await isLoggedIn()` → `{'' ,''}`. Offline → `{stdout:'', stderr:'App offline'}`.
4. Fetch all pages: `GET /li/games`; while `current_page < total_pages`: sleep
   **1000 ms**, `GET /li/games?page=<current+1>`. Concatenate `games`. Cache in
   `libraryCache.library`. Any error → `[]`.
5. Empty → log, `{stdout:'', stderr:'Error loading library'}`.
6. `library.clear()`; for each game build GameInfo (`zoomToUnifiedInfo`,
   `library.ts:146-176`):

| Field | Value |
|---|---|
| `runner` | `'zoom'` |
| `app_name` | `String(id)` |
| `title` | `name` |
| `art_cover`, `art_square`, `art_background` | `poster_url` |
| `cloud_save_enabled` | `false` |
| `extra` | `{about:{description, shortDescription:''}, reqs:[], genres:[]}` |
| `developer` | `developers.join(', ')` |
| `folder_name` | `slug` |
| `install` | `{is_dlc:false}`; replaced by installed entry if installed |
| `store_url` | `store_url` |
| `is_installed` | `false`; `true` if installed entry exists |
| `save_folder` | `''` |
| `canRunOffline` | `true` |
| `is_mac_native` | `operating_systems` includes `'osx'` |
| `is_linux_native` | includes `'linux'` |
| `thirdPartyManagedApp` | `undefined` |

   Each object is pushed to the renderer via `pushGameToLibrary`.
7. `libraryStore.set('games', [...])`; write "Games List" to the zoom runner log
   (`* <title> (App name: <id>)[ - DLC]`, case-insensitive sorted, `Total: N`).
8. Return `{stdout:'Library refreshed', stderr:''}`.

`getGameInfo(id)`: disabled → undefined; in-memory map, else
`getInstallAndGameInfo`: find in `libraryStore.games` and overlay installed info.

`getInstallInfo(appName, platform='windows')` (`library.ts:203-267`):
`GET /li/game/<id>/files`; `files = response[platform]` (**case-sensitive**: the
download manager passes e.g. `'Windows'`, which yields no files → `undefined`
→ size `'?? MB'`). Empty → warn, undefined. Else sum `parseSize(file.size)` and
return/cache:
```
{ game: { app_name, title, owned_dlc: [], version: 'N/A', launch_options: [],
          branches: [], buildId: 'N/A' },
  manifest: { disk_size: S, download_size: S, app_name, languages: [], versionEtag: '',
              dependencies: [], perLangSize: { '*': {download_size:S, disk_size:S} } } }
```

`getInstallers(platform, appName)` (`library.ts:318-363`): files for key
`platform`; for each file sequentially `GET /li/download/<file.id>` →
`{url, filename: file.name, total_size: parseSize(size), id, name, size}`.
Errors → `[]`. (Note `filename` is the API `name`, not the API `filename`.)

`getExtras(appName)` (unused): for `manual`, `misc`, `soundtrack` files fetch
download URLs → `{name, url, filename: name, total_size: getFileSize(size)}`.

Other LibraryManager methods: `listUpdateableGames` → `[]` (warn);
`installState` → warn; `getLaunchOptions` → `[]`;
`changeGameInstallPath(app, p)` updates in-memory + `installed[idx].install_path`
(**[QUIRK]** idx -1 crashes; game must be in in-memory `library`);
`changeVersionPinnedStatus(app, s)` sets `pinnedVersion` on both maps, replaces
the entry in `installed`, emits `pushGameToLibrary`;
`updateGameInLibrary(game)` replaces map entry if present.

### 2.6 Install (`SM/zoom/games.ts:215-551`)

Input: `{path, platformToInstall, installLanguage}`.
1. gameInfo missing or no `folder_name` → `{status:'error', error:'Game info not found'}`.
2. `installPlatform = platformToInstall.toLowerCase()` (`'windows'|'linux'`).
3. `installers = getInstallers(installPlatform, id)`; none or first has no url →
   `{error:'No installer found'}`.
4. `installPath = <path>/<folder_name(slug)>`; `downloadRoot = <path>/.zoom-download`;
   `infFilePath = <downloadRoot>/zoom_installer.inf`; `totalSize = Σ parseSize(size)`.
5. **Download** each file sequentially to `<downloadRoot>/<filename>` (mkdir -p).
   If the file already exists → skip (resume-by-skip; no partial check). Else
   streaming `GET file.url` piped to disk. Progress on each axios progress event:
   `fileDownloaded += event.bytes`; `percent = round((downloaded +
   fileDownloaded)*100/totalSize)`; call `onInstallOrUpdateOutput('installing',
   String(percent), totalSize)`. After file completes `downloaded +=
   parseSize(file.size)`. Download error → `{status:'error', error:'Failed to
   download installer: <err>'}`. **No abort controller is registered for the
   HTTP download**: pause/cancel do not interrupt it (only the later wine step).
6. **Execute**:
   * linux: `downloadPath = <installPath>/<installers[0].filename>`
     **[QUIRK/BUG]** — files were downloaded into `downloadRoot`, and `installPath`
     is never created on this branch. If name ends `.tar.xz`: `tar -xf
     <downloadPath> -C <installPath>`; then `gamePath` = first entry of
     `installPath` that is a directory (else installPath); if `<gamePath>/start.sh`
     exists → executable, `chmod 755`. Otherwise (non-tar): `chmod('',755)` then
     spawn `''` **[BUG]**. Recommended: use `<downloadRoot>/<filename>`, mkdir
     `installPath`, keep the rest.
   * windows: `executable` = first installer filename ending `.exe`. Write INF:
     `"[Setup]\nLang=english\nDisableWelcomePage=yes\nDisableDirPage=yes\nDisableProgramGroupPage=yes\nDisableReadyPage=yes\n"`.
     If `installPath` exists, record pre-existing `*.conf` files (recursive,
     case-insensitive) else mkdir. Run via `runWineCommand({commandParts:
     [<downloadRoot>/<exe>, '/NORESTART', '/NOICONS', '/SP-',
     '/DIR=Z:<installPath with / → \>',
     '/LOADINF=Z:<infFilePath with / → \>', '/LOG=C:\zoom_installer.log'],
     gameSettings, gameInstallPath: path, wait: true, options:{logMessagePrefix:
     'Installing <id>', logWriters:[install log], abortId: id}})`.
     (Exact args: `/NORESTART /NOICONS /SP- /DIR=Z:\...\<slug> /LOADINF=Z:\...\.zoom-download\zoom_installer.inf /LOG=C:\zoom_installer.log` — InnoSetup silent install.)
   `installResult.error` → `{error:'Installer execution failed: <e>'}`.
7. **Executable detection** (windows installs):
   * New `.conf` files (after minus before). If any: `gameDirectory =
     dirname(first)`; search recursively for `dosbox.exe` (case-insensitive) under
     it. If found → DOSBox game:
     * Windows host: `finalExecutable = relative(installPath, dosbox.exe)`.
     * Other hosts: `finalExecutable = 'dosbox'` (system DOSBox from PATH),
       platform becomes `'linux'`, copy every item of `gameDirectory` into
       `<path>/<slug>` (recursive cp), `dosboxConf = new conf files re-pointed to
       <path>/<slug>/<basename>`.
   * Else collect all `*.exe` (recursive) relative to installPath, drop names
     matching `/setup|unins|redist/i`. One → it. Several → first whose
     alnum-lowercased path contains alnum-lowercased title; else the largest file.
   * linux installs: `finalExecutable = executable` (absolute start.sh path).
8. `rm -rf downloadRoot`. Empty executable → error dialog
   "Executable not found" (install still recorded as done).
9. Record `InstalledInfo`:
   ```
   { platform: finalInstallPlatform, executable: finalExecutable.replace('{app}', installPath),
     install_path: installPath, isDosbox, dosboxConf, install_size: getFileSize(totalSize),
     is_dlc: false, version: '1.0', appName: id, installedDLCs: [], language: installLanguage,
     versionEtag: '', buildId: '', pinnedVersion: false }
   ```
   **appended** (no dedupe) to `installed.json:installed`; `refresh()` (not
   awaited); update in-memory + `zoom_library.json` entry with `is_installed:true`,
   `install`.
10. `{status:'done'}`.

Progress (`games.ts:181-213`): ignore empty data; `percent = parseInt(data)`
capped at 100 (`NaN` passes through when data is `"undefined"`); `bytes='N/A'`,
`eta='N/A'`, `downSpeed=0`, `diskSpeed=0`; emit `progressUpdate` every call.

### 2.7 Launch (`games.ts:578-742`)

1. Require `install.install_path`, `platform`, `executable` (else false). Missing
   dir → `errorHandler('appears to be deleted', id, 'zoom')` (force-uninstall
   prompt), false.
2. `prepareLaunch` (failure → dialog, false). env = process.env + wrapper env
   (`HEROIC_APP_SOURCE=zoom`, `STORE=zoomplatform`) + `setupEnvVars` (non-Windows)
   + known fixes (`zoom`). wrappers as nile.
3. `commandParts = [...shlex.split(basic launch params), ...shlex.split(launcherArgs),
   ...args]`; if `isDosbox && dosboxConf` append `-conf <conf>` per file.
4. `gameStatusUpdate playing`.
5. `isNative()` = Windows host, or mac host && platform `'osx'`, or linux host &&
   platform `'linux'`. Native → `callRunner(commandParts, {name:'zoom', bin:
   install.executable, dir: isDosbox ? undefined : install_path}, {env, wrappers,
   logWriters, abortId: id, cwd: install_path})`. abort → true; else `!error`.
   (Wrappers are not applied for native zoom launches, since callRunner ignores them.)
6. Non-native → `prepareWineLaunch` (failure → dialog, false). `executable =
   settings.targetExe || install.executable`; `startFolder = dirname(abs exe)`
   (relative exe resolved against install_path). UMU GAMEID as nile.
   `runWineCommand({commandParts: [basename(executable), ...commandParts],
   gameSettings, gameInstallPath, installFolderName: folder_name, protonVerb:
   'waitforexitandrun', startFolder, options:{env: env+wineEnv, wrappers,
   logWriters, abortId}})`. Error iff `code !== 0 && stderr`. Return `!hasError`.

### 2.8 Other game methods

* `uninstall()` (`games.ts:767-789`): find in `installed` (absent → **throws**
  "Game isn't installed"); remove entry; `rm -rf install_path` if exists; save;
  `refresh()` (not awaited); mutate in-memory gameInfo (`is_installed=false`,
  `install={is_dlc:false}`); remove shortcuts + non-Steam shortcut;
  `pushGameToLibrary`; returns `{stdout:'Uninstalled', stderr:''}`.
* `forceUninstall()`: filter out of `installed`, save, refresh, mutate, push.
* `importGame` → `{stdout:'', stderr:'Import not fully implemented'}` (no error
  field ⇒ the import IPC reports success!). `moveInstall` → `{status:'error',
  error:'Move install not implemented'}`. `repair` → `{stdout:'', stderr:'Repair
  not implemented'}`. `syncSaves` → `'Sync saves not implemented'`. `update` →
  `{status:'error', error:'Update not implemented'}`. `stop()` → warn only (no kill).
* `getExtraInfo` → `{about:{description:'',shortDescription:''}, reqs:[],
  releaseDate, storeUrl, changelog: undefined}`.
* `isGameAvailable` → installed && install_path exists.

---

## 3. Sideloaded apps (runner id `sideload`)

### 3.1 Storage

`libraryStore` = `TypeCheckedStoreBackend('sideloadedStore', {cwd:'sideload_apps',
name:'library', clearInvalidConfig:true})` → `<appFolder>/sideload_apps/library.json`:
```
{ "games": GameInfo[]  /* the only key used; "installed" is declared but unused */ }
```
Per-app settings (wine prefix/version, targetExe, launcherArgs…) live in the
normal per-game config `<appFolder>/GamesConfig/<app_name>.json`.

### 3.2 Data model (sideload GameInfo)

Written by `addNewApp` (`SM/sideload/library.ts:19-80`):

| Field | Value |
|---|---|
| `runner` | `'sideload'` |
| `app_name` | from renderer: new apps get `short-uuid` `generate()` (22-char flickrBase58); edits keep id |
| `title` | user title |
| `install` | `{executable, platform, is_dlc:false}` — `platform` ∈ `'Windows'|'linux'|'Mac'|'Browser'` (InstallModal values); executable is an absolute path (undefined/`''` for browser apps) |
| `folder_name` | `dirname(executable)` if executable defined |
| `art_cover`, `art_square` | URLs or `file://<path>`; renderer fills `heroUrl || imageUrl || fallback` / `imageUrl || heroUrl || fallback` |
| `is_installed` | `true` unless explicitly given |
| `canRunOffline` | `!browserUrl` |
| `browserUrl` | URL for Browser apps (renderer forces `https://` prefix) |
| `description`, `customUserAgent`, `launchFullScreen` | passed through |

macOS: if executable ends `.app`, replace with `<app>/Contents/MacOS/<first entry
of readdir>`.

Add vs edit: find by `app_name`; existing → `current[i] = {...current[i], ...game}`
(fields present-but-undefined in `game` overwrite → dropped on save); new → push and
`addShortcuts(new SideloadGame(id))` (not awaited). Save, emit renderer
`refreshLibrary('sideload')`. IPC: listener `addNewApp(GameInfo)`
(`backend/main.ts:1339-1341`). The renderer then (new apps only) writes game config
`{winePrefix, wineVersion, wineCrossoverBottle}` via `writeConfig` and refreshes.

Renderer "run installer in prefix" (SideloadDialog) uses IPC `runWineCommand
{commandParts:[exe], wait:true, protonVerb:'runinprefix', gameSettings}` — generic.

### 3.3 Library manager

`init` resolves; `getGame(id)` → new `SideloadGame`; `refresh` → warn, `null`;
`getGameInfo` (manager-level) → empty sentinel (warn); `listUpdateableGames` → `[]`;
`getInstallInfo` → undefined; `changeGameInstallPath`, `installState`,
`changeVersionPinnedStatus`, `runRunnerCommand` → warn/no-op;
`getLaunchOptions` → `[]`.

### 3.4 Game methods (`SM/sideload/games.ts`)

* `getGameInfo()`: find in `games`; missing → `{}` (empty object, not sentinel).
* `isGameAvailable()`: Browser → true; executable set → `exists(executable)`.
  **[QUIRK]** neither → promise never resolves. Recommended: resolve false.
* `isNative()`: platform set and (Browser, or Windows host, or mac host &&
  `'Mac'`, or linux host && lower(platform)==`'linux'`).
* `uninstall({shouldRemovePrefix, deleteFiles=false})`: status `uninstalling`;
  if `shouldRemovePrefix` → `removePrefix(id,'sideload')`; remove from `games`;
  if `deleteFiles && executable` → `rm -rf dirname(executable)` (the IPC never
  passes deleteFiles, so never happens); notify "uninstalled"; remove shortcuts,
  recent-games entry, non-Steam shortcut (not awaited); status `done`. Note the
  generic uninstall IPC also calls `removePrefix` again when requested, and
  `removeSettingsAndLogs` when `shouldRemoveSetting` (`backend/utils/uninstaller.ts:82-128`).
* `stop()`: if executable: `killPattern(last path segment of executable)`; if not
  native → `shutdownWine` (not awaited).
* `launch(logWriter, _opts, args)` → `launchGame(this, logWriter, args)` (3.5).
  Launch options are ignored for sideload.
* Not implemented (warn): `getExtraInfo` → `{about:{'',''}, reqs:[], storeUrl:''}`;
  `onInstallOrUpdateOutput`; `moveInstall` → `{status:'error'}`; `repair` →
  empty; `syncSaves` → `''`; `forceUninstall`; `install` → `{status:'error'}`;
  `importGame` → empty ExecResult; `update` → `{status:'error'}`.

### 3.5 `launchGame` (shared, `SM/storeManagerCommon/games.ts:116-274`)

1. `executable = install.executable`; overridden by `GameConfig.getSettings().targetExe`
   when non-empty.
2. **Browser apps** (`browserUrl` set): open a new Electron `BrowserWindow`
   (`openNewBrowserGameWindow`, lines 47-114): icon, `fullscreen:
   launchFullScreen ?? false`, `autoHideMenuBar`, session partition
   `persist:<hostname of browserUrl>`; menu with Close / Reload /
   Toggle fullscreen / Toggle devtools (hidden menubar); user agent =
   `customUserAgent ?? "Mozilla/5.0 (Windows NT 10.0; Win64; x64)
   AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36"`;
   `loadURL(browserUrl)`; show on ready. Abort controller registered under
   app_name → closes window. `will-prevent-unload` → Yes/No dialog "Are you sure
   you want to quit?" / "Any unsaved progress might be lost"; Yes → allow close.
   On `closed` → delete abort controller, resolve `true`. (No playing status is
   emitted on this path.) Rust equivalent: whatever webview host the new shell
   uses; preserve per-hostname persistent storage, UA, fullscreen, confirm-on-unload.
3. Else `extraArgs = [...shlex.split(launcherArgs ?? ''), ...args]`. If no
   executable → return false.
4. `prepareLaunch(...)`; if not native → `prepareWineLaunch(game, logWriter)`
   (result ignored). `wrappers = setupWrappers(settings, mangohud, gamemode,
   gamescope, isNative && steamRuntime.length ? steamRuntime : undefined)` (UMU
   wrapper only for native; runWineCommand adds it itself).
5. prepareLaunch failure → `launchCleanup()`, dialog, false.
6. `gameStatusUpdate playing` (runner = gameInfo.runner).
7. **Native**: if not executable (`access X_OK` fails) → on Linux, or mac when
   path not ending `.app` → `chmod 0o775`. env = wrapper env + `setupEnvVars` +
   known fixes (no process.env merge here, callRunner merges). If wrappers exist:
   `extraArgs = [...wrappers, executable, ...extraArgs]` then
   `executable = extraArgs.shift()` (i.e. run first wrapper element with the rest
   as args). `callRunner(extraArgs, {name: runner, logPrefix: Backend, bin:
   basename(executable), dir: dirname(executable)}, {env, wrappers, logWriters,
   logMessagePrefix: 'Backend'})` (no abortId → abort id = random). Then
   `launchCleanup(rpcClient)`, return **true** regardless of result.
8. **Non-native**: `runWineCommand({commandParts: [executable, ...extraArgs],
   gameSettings, wait: true, protonVerb: 'waitforexitandrun', startFolder:
   dirname(executable), options: {wrappers, logWriters, logMessagePrefix}})`;
   `launchCleanup`; return true.

---

## 4. Store-manager interfaces and shared types

### 4.1 Registry (`SM/index.ts`)

`libraryManagerMap: Record<Runner, LibraryManager>` with keys **in this order**:
`sideload, gog, legendary, nile, zoom` (order matters for `checkGameUpdates`
iteration and init). `initStoreManagers()` = `Promise.all(map.values().init())`,
called at app ready (`backend/main.ts:328`, not awaited). `getGame(id, runner)`
helper = `libraryManagerMap[runner].getGame(id)` (`backend/utils.ts:91`).

### 4.2 `Runner` (`T:24`)
`'legendary' | 'gog' | 'sideload' | 'nile' | 'zoom'`.

### 4.3 `LibraryManager` (`GM:191-213`)

| Method | Semantics |
|---|---|
| `init(): Promise<void>` | Startup: migrations, initial refresh. |
| `getGame(id): Game` | Return the Game object for id (nile caches instances; zoom/sideload create new). Must never fail. |
| `refresh(): Promise<ExecResult \| null>` | Re-sync library from store/CLI, rebuild caches the renderer reads. IPC `refreshLibrary(runner \| 'all' \| undefined)` calls one or all (`Promise.allSettled`). |
| `getGameInfo(appName, forceReload?): GameInfo \| undefined` | Library lookup. |
| `getInstallInfo(appName, platform, {branch?, build?, lang?, retries?}): Promise<InstallInfo \| undefined>` | Download/disk size and metadata pre-install. IPC `getInstallInfo` maps undefined/throw → `null`. |
| `listUpdateableGames(): Promise<string[]>` | App names with updates. |
| `changeGameInstallPath(appName, newPath): Promise<void>` | Point an install at a new path (no file move). |
| `changeVersionPinnedStatus(appName, status): void` | Pin/unpin (GOG/zoom only). |
| `installState(appName, state): void` | Update in-memory installed flag without full reload. |
| `getLaunchOptions(appName): LaunchOption[] \| Promise<…>` | IPC `getLaunchOptions` prepends a `{name:'Default', parameters:'', type:'basic'}` entry when the list is non-empty and has no default. |

Store-specific extras used by other modules: nile `runRunnerCommand`,
`fetchFuelJSON`, `getInstallMetadata`, `removeFromInstalledConfig`; zoom
`getInstallers`, `getExtras`, `updateGameInLibrary`, `zoomToUnifiedInfo`;
sideload `addNewApp`, `runRunnerCommand`; (legendary: `hasGame`, `getListOfGames`,
`getGameOverride`, `getGameSdl`, `toggleGamesSync`, `commandToArgsArray`;
gog: `getLinuxInstallersLanguages` — see their specs).

### 4.4 `Game` (`GM:150-189`)

| Method | Semantics |
|---|---|
| `getSettings(): Promise<GameSettings>` | `GameConfig.get(id).config ?? await getSettings()` (all three stores). |
| `getGameInfo(): GameInfo` | Never throws; empty sentinel / `{}` if unknown. IPC `getGameInfo` returns `null` when `Object.keys(info).length === 0`, else applies metadata overrides. |
| `getExtraInfo(): Promise<ExtraInfo>` | Description/reqs/etc. |
| `importGame(path, platform): Promise<ExecResult>` | Register an existing install. IPC treats `abort \|\| error` as failure. |
| `onInstallOrUpdateOutput(action, data, totalDownloadSize)` | Progress parser hook. |
| `install(InstallArgs): Promise<InstallResult>` | Called by download manager only. |
| `isNative(): boolean` | True if no Wine needed. |
| `addShortcuts(fromMenu?)` / `removeShortcuts()` | Desktop/menu shortcuts. |
| `launch(logWriter, launchArguments?, args?, skipVersionCheck?): Promise<boolean>` | Blocks until game exits. |
| `moveInstall(newPath): Promise<InstallResult>` | Move files + update path. |
| `repair(): Promise<ExecResult>` | Verify/repair. |
| `syncSaves(arg, path, gogSaves?): Promise<string>` | Cloud saves. |
| `uninstall(RemoveArgs): Promise<ExecResult>` | Remove; may throw (zoom). |
| `update(overrides?: {build?, branch?, language?, dlcs?, dependencies?}): Promise<InstallResult>` | Called by download manager. |
| `forceUninstall(): Promise<void>` | Drop install record without CLI. |
| `stop(stopWine?): Promise<void>` | Kill game/runner processes. |
| `isGameAvailable(): Promise<boolean>` | Install dir/executable exists. |
| `getAchievements?(lang)` | GOG only. |

`InstallResult` = `{status: 'done'|'error'|'abort', error?}` (`GM:140-143`).
`RemoveArgs` = `{shouldRemovePrefix?, deleteFiles?}` (`GM:145-148`).

### 4.5 Implementation matrix (this doc's stores)

| Method | nile | zoom | sideload |
|---|---|---|---|
| install | CLI §1.6 | HTTP+InnoSetup §2.6 | error |
| update | CLI §1.8 | error "Update not implemented" | error |
| repair | `verify` | stub | stub |
| uninstall | CLI | delete dir | remove entry |
| importGame | CLI | stub (reports success) | stub |
| moveInstall | rsync/mv | error | error |
| launch | `nile launch` | native spawn / wine | §3.5 |
| stop | pkill appName (Linux) | no-op | pkill exe name |
| listUpdateableGames | `list-updates` | `[]` | `[]` |
| syncSaves | `''` | message | `''` |

### 4.6 `GameInfo` (`T:185-226`)

| Field | Type | Meaning |
|---|---|---|
| `runner` | Runner | owning store |
| `store_url?` | string | store page (zoom sets) |
| `app_name` | string | store-unique id |
| `art_cover` | string | wide/cover art URL |
| `art_logo?` | string | logo (nile) |
| `art_background?` | string | hero/background |
| `art_icon?` | string | icon (gog) |
| `art_square` | string | square/tall art |
| `cloud_save_enabled?` | bool | |
| `developer?` | string | |
| `extra?` | ExtraInfo | |
| `folder_name?` | string | install folder name (nile: sanitized title; zoom: slug; sideload: exe dir) |
| `install` | `Partial<InstalledInfo>` | `{}` when not installed |
| `installable?` | bool | (legendary) |
| `is_installed` | bool | |
| `namespace?` | string | (legendary) |
| `save_folder?` | string | unexpanded save path template |
| `save_path?` | string | expanded save path |
| `gog_save_location?` | `GOGCloudSavesLocation[]` | `{name, location}` |
| `title` | string | |
| `canRunOffline` | bool | |
| `thirdPartyManagedApp?` | string | e.g. Origin/Ubisoft (legendary) |
| `isEAManaged?` / `isUbisoftManaged?` | bool | DM skips size lookup when set |
| `is_mac_native?` / `is_linux_native?` | bool | |
| `browserUrl?` | string | sideload web apps |
| `description?` | string | |
| `version?` | string | store release version |
| `dlcList?` | `GameMetadataInner[]` | (legendary) |
| `customUserAgent?`, `launchFullScreen?` | string / bool | sideload web apps |
| `overrides?` | `{title?, art_cover?, art_square?}` | user metadata overrides (attached by IPC) |

### 4.7 `InstalledInfo` (`T:315-352`)

| Field | Type | Notes |
|---|---|---|
| `manifest?` | `{disk_size, download_size, app_name, languages[], versionEtag, dependencies[], perLangSize: {[lang]: {download_size, disk_size}}}` | GOG |
| `executable` | string | zoom: relative (windows) / absolute (linux, start.sh) / `'dosbox'`; sideload: absolute |
| `install_path` | string | |
| `install_size` | string | human (`getFileSize`) |
| `is_dlc` | bool | |
| `isDosbox?` / `dosboxConf?` | bool / string[] | zoom DOSBox |
| `version` | string | |
| `platform` | InstallPlatform | |
| `appName?` | string | (gog/zoom store entries) |
| `installedWithDLCs?` | bool | legacy |
| `installedDLCs?` | string[] | |
| `language?` | string | |
| `versionEtag?`, `buildId?`, `branch?` | string | GOG |
| `pinnedVersion?` | bool | skip update checks |
| `cyberpunk?` | `{modsEnabled, modsToLoad[]}` | |

### 4.8 `InstallPlatform` (`T:608-613`)

Union of `LegendaryInstallPlatform` (`'Windows'|'Win32'|'Mac'|'Android'|'iOS'`,
`common/types/legendary.ts:6-11`), `GogInstallPlatform` (`'windows'|'osx'|'linux'`,
`common/types/gog.ts:3`), `NileInstallPlatform` (`'Windows'`), `ZoomInstallPlatform`
(`'windows'|'linux'`), and `'Browser'`. Case matters per store. The install modal
offers values `'linux'`, `'Mac'`, `'Windows'`, `'Browser'` for every store.

### 4.9 `InstallInfo` (`T:833-838`)

`LegendaryInstallInfo | GogInstallInfo | NileInstallInfo | ZoomInstalledInfo |
ZoomInstallInfo`. Every variant has `manifest.download_size` (number, bytes) — the
DM relies on it. `NileInstallInfo` (`nile.ts:36-39`): `{manifest:{download_size,
disk_size}, game:{id, version, path, app_name, cloud_save_folder?,
cloud_save_folder_mac?, cloud_saves_supported, external_activation, is_dlc,
launch_options[], owned_dlc: DLCInfo[], platform_versions:{Windows}, title}}`.
`ZoomInstallInfo` see §2.5.

### 4.10 `DLCInfo` variants

* nile/zoom (private): `{app_name, title}` (`nile.ts:8-11`, `zoom.ts:73-76`).
* legendary: `{app_name, title, is_installed?}` (`legendary.ts:155-159`).
* gog: `{app_name, title, perLangSize: {'*': {download_size, disk_size}, [lang]: …}}`
  (`gog.ts:55-59`).

### 4.11 `ExtraInfo` (`T:158-165`)
`{about?: {description, shortDescription}, reqs: {minimum, recommended, title}[],
releaseDate?, storeUrl?, changelog?, genres?}`.

### 4.12 `LaunchOption` (`T:43-66`)
* basic: `{type?: 'basic', name, parameters}` — parameters appended (shlex-split).
* altExe: `{type: 'altExe', executable}` — nile maps to `--override-exe`.
* dlc: `{type: 'dlc', dlcAppName, dlcTitle}` — legendary only.

### 4.13 Install/update params (`T:376-402`)

* `InstallArgs` = `{path, platformToInstall, installDlcs?: string[], sdlList?:
  string[], installLanguage?, branch?, build?, dependencies?: string[]}`.
* `InstallParams` = `InstallArgs & {appName, gameInfo: GameInfo, runner, size?:
  string}` — this is `DMQueueElement.params`.
* `UpdateParams` = `{appName, runner, gameInfo, installDlcs?, installLanguage?,
  build?, branch?}` (IPC `updateGame` payload).

### 4.14 Status/progress types (`T:274-314`)

`Status` = `installing | importing | updating | launching | playing | uninstalling
| repairing | done | canceled | moving | queued | error | syncing-saves |
notAvailable | notSupportedGame | notInstalled | installed | redist | extracting |
winetricks`.
`GameStatus` = `{appName, progress?: InstallProgress, folder?, context?, runner?,
status}`. `InstallProgress` = `{bytes: string, eta: string, folder?, percent?:
number, downSpeed?: number /*MB/s*/, diskSpeed?: number, file?}`.

---

## 5. Download manager

### 5.1 Types (`T:631-639, 809`)

* `DMStatus` = `'done' | 'error' | 'abort' | 'paused'` (`'paused'` never produced).
* `DMQueueElement` = `{type: 'update' | 'install', params: InstallParams,
  addToQueueTime: ms, startTime: ms (0 until started), endTime: ms (0 until
  finished), status?: DMStatus /* only in finished list */}`.
* `DownloadManagerState` = `'idle' | 'running' | 'paused' | 'stopped'`
  (`'stopped'` never produced).

### 5.2 Persistence (`DM/downloadqueue.ts:16-19`)

`<appFolder>/store/download-manager.json` (electron-store, tab-indented):
```json
{
	"queue":    [ DMQueueElement, ... ],   // index 0 = current/next
	"finished": [ DMQueueElement & {"status": "done"|"error"|"abort"}, ... ]
}
```
Observed real element:
```json
{ "params": { "appName": "1423049311", "path": "/home/u/Games/Heroic",
    "installDlcs": ["1256837418"], "sdlList": [], "installLanguage": "en-US",
    "runner": "gog", "platformToInstall": "Windows", "gameInfo": { …full GameInfo… },
    "size": "73.59 GiB" },
  "type": "install", "addToQueueTime": 1792666530526, "startTime": 1792666533666,
  "endTime": 1792666896907, "status": "done" }
```
The renderer also writes this file directly: "clear finished" sets `finished=[]`
and per-item clear filters `finished` (`frontend/screens/DownloadManager/index.tsx:70-80`).
IPC `clearDMFinished` is declared but has **no backend handler**.

### 5.3 In-memory state

`queueState` (init `'idle'`), `currentElement` (init null), `autoPaused` (false).
`isPaused` = state `paused`; `isRunning` = state `running`;
`isIdle` = state `idle` **or** `currentElement == null`.

### 5.4 Startup

`initQueue()` is invoked 5000 ms after app ready unless CLI no-GUI mode
(`backend/main.ts:470-479`). Any elements left in `queue` from the previous
session (including one that was paused) start automatically from index 0.

### 5.5 `addToQueue(element)` (`downloadqueue.ts:118-196`)

1. Falsy → log error, return.
2. `gameStatusUpdate {appName, runner, folder: params.path, status:'queued'}`.
3. Load `queue`. If an element with same `appName` **and** `runner` exists →
   replace it in place (no size recomputation).
4. Else:
   * gameInfo = manager `getGameInfo(appName)`. If not EA/Ubisoft managed →
     `installInfo = await getInstallInfo(appName, platformToInstall, {branch,
     build})`; `params.size = download_size ? getFileSize(download_size) : '?? MB'`.
     For `runner==='gog'` && platform lower `'windows'` && `'dependencies' in
     manifest`: if dependencies non-empty or `gogRedistPath` missing → push a
     redist element **before** the game (`createRedistDMQueueElement`:
     `{params:{appName:'gog-redist', runner:'gog', path: gogRedistPath,
     platformToInstall:'windows', gameInfo, size:'?? MB', dependencies},
     addToQueueTime: now, startTime:0, endTime:0, type:'update'}`;
     `SM/gog/redist.ts:114-131`).
   * EA/Ubisoft → `size = '?? MB'`.
   * push element.
5. Save `queue`; log `<title> was added to the download queue.`; emit
   `changedDMQueueInformation(queue, queueState)`.
6. If `isIdle()` → start `initQueue()` (not awaited).

### 5.6 Processing loop `initQueue()` (`downloadqueue.ts:82-116`)

```
element = queue[0] ?? null
while element:
    element.startTime = now; queue[0] = element; save
    currentElement = element; queueState = 'running'
    emit changedDMQueueInformation(queue, 'running')
    {status} = element.type == 'install' ? installQueueElement(params)
                                         : updateQueueElement(params)
    element.endTime = now
    processNotification(element, status)
    if not paused:
        addToFinished(element, status); removeFromQueue(params.appName)
        element = queue[0] ?? null
    else:
        element = null          # stays in queue with startTime set
if queueState != 'paused': queueState = 'idle'
```
`currentElement` is **not** cleared when the loop ends. No guard against two
concurrent loops (e.g. `resume` while running starts a second loop) — Rust should
guard (only one worker) while preserving visible behavior.

### 5.7 `installQueueElement(params)` (`DM/utils.ts:12-106`)

1. `title` from game info. Offline → warn, `{status:'error'}` (element then goes to
   finished as error).
2. legendary: Epic services offline → error dialog, `error`.
3. gog: delete `<gogdlConfigPath>/manifests/<appName>` if exists.
4. `gameStatusUpdate {appName, runner, status:'installing', folder: path}`;
   notification "Installation Started".
5. `game.install({path: path without "'", installDlcs, sdlList without '' entries,
   platformToInstall, installLanguage, build, branch})`. `error` status → log
   `Installation of <app> failed with: <error>`. Throw → log, `error`.
6. `finally`: `gameStatusUpdate {status:'done'}`. Returns `{status}`.

### 5.8 `updateQueueElement(params)` (`DM/utils.ts:108-180`)

Same offline / Epic checks (dialog key `box.warning.epic.update`); status
`updating`; notification "Update Started"; `game.update({build, branch,
language: installLanguage, dlcs: installDlcs, dependencies})`; finally `done`.

### 5.9 `removeFromQueue(appName)` (`downloadqueue.ts:198-222`)

Only if `queue` key exists: remove first element with matching **appName** (runner
ignored); persist via delete+set; **always** emit `gameStatusUpdate {appName,
status:'done'}` (no runner) and `changedDMQueueInformation`. Does not stop a running
download. IPC listener `removeFromDMQueue(appName)`.

### 5.10 `addToFinished(element, status)` (`downloadqueue.ts:58-76`)

Replace existing finished entry with same appName (`status ?? 'abort'`), else push
`{...element, status}`. Unbounded list.

### 5.11 Pause / resume / cancel / connectivity

* **pause** (listener `pauseCurrentDownload`, lines 257-268): if `currentElement`
  → `stopCurrentDownload()`; `queueState='paused'`; `autoPaused=false`; emit.
  `stopCurrentDownload` = `callAbortController(appName)` (kills the runner process
  whose abortId is the appName) + `game.stop(false)` (nile: `pkill -f <appName>`).
  The install call returns `abort` → notification "<type> Paused" → element remains
  at `queue[0]`, loop exits, state stays `paused`.
* **resume** (listener `resumeCurrentDownload`): `autoPaused=false`;
  `initQueue()` — restarts `queue[0]` from the top (runners resume from their own
  on-disk state; zoom skips already-downloaded files).
* **cancel(removeDownloaded)** (listener `cancelDownload`, lines 231-255): if
  `currentElement`: remove each `installDlcs` id from queue; if running →
  `stopCurrentDownload()`; `removeFromQueue(current.appName)`; if
  `removeDownloaded` → `removeFolder(params.path, folder_name)` (delayed delete,
  §0.3) when folder_name known; `currentElement = null`. The loop's pending install
  then returns `abort` → notification "Canceled" (state not paused) →
  `addToFinished(…,'abort')` → `removeFromQueue` (no-op + events) → next element
  starts automatically. `queueState` is not modified by cancel (if canceled while
  paused, it stays `paused` but `isIdle()` becomes true, so the next `addToQueue`
  starts the loop).
* Renderer cancel dialog: "Yes" (keep files) → `cancelDownload(false)` and stores
  progress in renderer localStorage; "No" → `cancelDownload(true)`.
* **connectivity** (`onConnectivityChange`, lines 29-39): `offline` while
  running → pause, then `autoPaused=true`; `online` with `autoPaused` → clear flag,
  resume.

### 5.12 Notifications (`processNotification`, lines 282-333)

Skipped for `gog`/`gog-redist`. Title = game title. Body i18n key
`notify.<install|update>.<paused|canceled|failed|finished>` for status abort
(paused vs not), error, done.

### 5.13 Renderer events / IPC summary

| Name | Kind | Payload |
|---|---|---|
| `install` | handler | `InstallParams` → enqueue `type:'install'`; for `runner==='legendary'` also enqueue one element per `installDlcs` id (`{...args, appName: dlc}`) |
| `updateGame` | handler | `UpdateParams` → enqueue `type:'update'` with `path = gameInfo.install.install_path`, `platformToInstall = gameInfo.install.platform` |
| `removeFromDMQueue` | listener | appName |
| `pauseCurrentDownload` / `resumeCurrentDownload` | listener | — |
| `cancelDownload` | listener | removeDownloaded: bool |
| `getDMQueueInformation` | handler | → `{elements: queue, finished, state}` |
| `changedDMQueueInformation` | renderer event | `(elements: DMQueueElement[], state)` |
| `gameStatusUpdate` / `progressUpdate` | renderer events | `GameStatus` |

### 5.14 Update checks and auto-update

IPC `checkGameUpdates` (`backend/main.ts:626-640`), invoked by the renderer on
refresh with `checkForUpdates` (e.g. at startup):
```
result = []
for runner in [sideload, gog, legendary, nile, zoom]:
    list = await manager.listUpdateableGames()
    if settings.autoUpdateGames: list = autoUpdate(runner, list)
    result += list
return result
```
`autoUpdate(runner, list)` (`SM/index.ts:42-60`): for each appName, **async without
awaiting**: read game settings; if `!ignoreGameUpdates && await isGameAvailable()`
→ log "Auto-Updating <title>", `addToQueue({params:{appName, gameInfo, runner,
path: install.install_path, platformToInstall: install.platform}, type:'update',
addToQueueTime: now, startTime:0, endTime:0})`; else log skip.
**[QUIRK]** The filter that removes queued games from the list runs after the
function already returned, so the returned list is the unfiltered input: the
renderer still shows all games as "update available" even when auto-queued.
`autoUpdateGames` default `false`; `maxWorkers` default `0` (= let runner choose).

---

## 6. Behaviors to double-check during implementation

1. Amazon ids contain dots → nested JSON in `nile_install_info.json` (electron-store
   dot-paths). The renderer only reads `nile_library.json` (`library` key) and
   `nile_store/config.json` (`userData.user_id`, `userData.name`).
2. Nile exit codes are not used for success; only abort/spawn errors and the
   specific stderr regexes. `library sync` returns code 1 when logged out.
3. callRunner keeps only the last 50 output chunks; very large JSON on stdout
   would be truncated in the reference too.
4. Concurrent identical runner commands share one process/promise.
5. Zoom download phase cannot be paused/canceled in the reference.
6. Zoom installs append duplicate `installed` entries on reinstall.
7. Sideload `isGameAvailable` can hang; zoom `uninstall` throws when not installed
   (IPC catches and shows "Error uninstalling").
