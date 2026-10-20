# Heroic frontend: feature-parity spec

**Purpose.** This spec lists every user-facing capability of the Heroic Games Launcher renderer. It is the parity checklist for the native Rust/GPUI rewrite. The spec covers **behaviour only**. Layout, visual style and interaction design are ours to redesign. Wherever the spec describes Heroic's arrangement (for example "the dots menu"), that tells you where the capability lives today. It is not a requirement to copy it. Each area ends with a list of UX pain points the redesign should fix.

**Reference.** Heroic Games Launcher **v2.22.4** (commit `59fd6cc`, "[Release] 2.22.4").
- Frontend: `src/frontend`.
- Shared types: `src/common` (IPC contract: `src/common/types/ipc.ts`).
- UI strings: `public/locales/en/{translation,gamepage,login}.json`.
- File references (`path:line`) are relative to the Heroic repo root. When a section abbreviates a path prefix, it says so at its top.

## Conventions

- **Runners (stores):** `legendary` (Epic), `gog`, `nile` (Amazon), `zoom` (ZOOM Platform, behind an experimental flag) and `sideload` ("Other": user-added apps and browser games). Type: `src/common/types.ts:24`.
- **IPC kinds.** All three are defined in `src/common/types/ipc.ts`:
  - **invoke**: request/response (`AsyncIPCFunctions`, `ipcRenderer.invoke`).
  - **send**: fire-and-forget (`SyncIPCFunctions`, `ipcRenderer.send`).
  - **event**: a backend→frontend push (`FrontendMessages`).
- **Channel names vs aliases.** The renderer calls `window.api.<alias>`; preload maps each alias to a channel name (`src/preload/api/*.ts`). This spec always names the **channel**. When the alias differs, it is written `alias → channel` (for example `removeRecentGame → removeRecent`, `sendKill → kill`, `fetchPlaytimeFromServer → getPlaytimeFromRunner`). Appendix A has the full channel list with the frontend files that use each one.
- **Persistence that bypasses IPC.** The renderer reads and writes several electron-store JSON files directly through preload (`storeNew/storeGet/storeSet/storeHas/storeDelete`; see `src/frontend/helpers/electronStores.ts` and `src/common/types/electron_store.ts`). Examples are `store/config.json` (`configStore`: favourites, hidden, recent, custom categories, theme, zoom, fonts, language), `store_cache/*_library.json`, `gog_store/installed.json`, `sideload_apps/library.json`, `store/download-manager.json`, `store/game-overrides.json` and `store/timestamp.json`.
  - Much UI state is in Chromium **localStorage**: filters, sort, layout, category selection, resume progress, tour state, the last-used settings tab and more. The Library and Shell sections list every key.
  - The native app needs an equivalent store. It must keep the on-disk file formats the backend also reads (config.json, GamesConfig/*.json, game-overrides.json, timestamp.json, etc.).
- **Platform flags exposed to the renderer:** `window.platform` (`linux|darwin|win32`), `isFlatpak`, `isSteamDeck`, `isSteamDeckGameMode`, `flatpakRuntimeVersion`, `isE2ETesting` (`src/preload/index.ts`). Many controls are conditional on these.
- **Status model.** `Status` (`src/common/types.ts:274-294`) has these values: `installing`, `importing`, `updating`, `launching`, `playing`, `uninstalling`, `repairing`, `done`, `canceled`, `moving`, `queued`, `error`, `syncing-saves`, `notAvailable`, `notSupportedGame`, `notInstalled`, `installed`, `redist`, `extracting`, `winetricks`.
  - Live statuses arrive via the `gameStatusUpdate` event.
  - Progress arrives via the `progressUpdate` event.
  - `installed`, `notInstalled`, `notAvailable` and `notSupportedGame` are synthesised client-side.
  - The Game page section has the full status → UI → actions table.

## Contents

1. [App shell & routes](#app-shell--routes)
2. [Global state & backend events](#global-state--backend-events)
3. [Sidebar / navigation](#sidebar--navigation)
4. [Library](#library)
5. [Install, Import & Sideload dialogs](#install-import--sideload-dialogs)
6. [Game page](#game-page)
7. [Settings](#settings) (global, Game Defaults, per-game, Tools/Winetricks, logs, save sync, system info)
8. [Wine manager](#wine-manager)
9. [Accessibility screen](#accessibility-screen)
10. [Download manager](#download-manager)
11. [Login](#login)
12. [WebView](#webview) (store pages, login pages, wiki)
13. [Discounts](#discounts)
14. [Console Mode](#console-mode)
15. [Gamepad navigation](#gamepad-navigation)
16. [Keyboard shortcuts](#keyboard-shortcuts)
17. [Tray](#tray)
18. [Notifications & dialogs](#notifications--dialogs)
19. [First-run / onboarding / tour](#first-run--onboarding--tour)
20. [Heroic self-update & changelog](#heroic-self-update--changelog)
21. [Connectivity / offline](#connectivity--offline)
22. [Window management](#window-management)
23. [i18n](#i18n)
24. [Cross-cutting redesign priorities](#cross-cutting-redesign-priorities)
25. [Known bugs and dead code not to replicate](#known-bugs-and-dead-code-not-to-replicate)
26. [Appendix A: IPC channel catalogue](#appendix-a-ipc-channel-catalogue)

Area-specific UX pain point lists are at the end of the Library/install, Game page, Settings/Wine/Accessibility and Shell sections. Sections 24 and 25 consolidate the most important ones.

---

## App shell & routes

### Boot sequence (renderer)
`src/frontend/index.tsx` does the following, in order:
1. `initOnlineMonitor()` hooks the browser `online`/`offline` events (`:17`).
2. Unhandled `window` errors are forwarded to `logError` (send) (`:19-21`).
3. `initGamepad()` and `initShortcuts()` set up global input (`:30-31`).
4. The cached `nonAvailableGames` key is removed from localStorage (`:34`).
5. The language is resolved from `configStore.language`, then `localStorage.language`, then `'en'`. It is written back to `configStore` and to `<html lang>` (`:36-39`).
6. Custom CSS is loaded from `getCustomCSS` (invoke) and injected into `#customCSS` (`:41-50`). `window.setCustomCSS` is exposed so it can be live-updated.
7. i18next is set up with the HTTP backend, which loads `locales/{{lng}}/{{ns}}.json` (`:52-115`).
8. The tree renders as `<GlobalState><I18nextProvider><Suspense fallback={<Loading/>}><App/>`. `Loading` is a spinner labelled "Loading" (`src/frontend/screens/Loading/index.tsx:4-7`).
9. Theme: `window.setTheme(themeClass)` is called with `configStore.theme`, which defaults to `midnightMirage`. Themes that are not built in are fetched with `getThemeCSS` (invoke) and injected. When the native Window Controls Overlay is visible, CSS vars `--titlebar-height/color/symbol-color` are read and sent through `setTitleBarOverlay` (send) (`:134-169`).
10. `window.imageData(src, w, h)` is a canvas helper the backend calls to generate padded and blurred Steam artwork (`:176-225`). This is not a UI feature, but the backend needs it to work.

The app has no `React.StrictMode` and no error boundary at the root.

### Root layout (`src/frontend/App.tsx:26-115`)
- The root `div#app` carries the classes `isRTL`, `frameless`, `fullscreen`, `disableAnimations` and `consoleMode`. HTML drag-start is disabled globally (`:81`).
- A MUI theme sets `direction: rtl|ltr` and styles tooltips and paper (`:41-68`).
- **Console mode** (pathname starts with `/console`) renders only `<main class="content consoleContent"><Outlet/>`. There is no sidebar, no dialogs, no offline banner and no controller hints (`:84-87`).
- **Normal mode** renders, inside `<TourProvider>` (`:89-110`):
  - `<OfflineMessage/>` (banner)
  - `<Sidebar/>`
  - `<main class="content">` with these global, always-mounted modals:
    - `DialogHandler`: the generic message box
    - `InstallGameWrapper`: the install modal (Library section)
    - `SettingsModalWrapper`: the game settings, log and categories modal
    - `ExternalLinkDialog`
    - `LogFileUploadDialog`
    - `UploadedLogFilesList`
    - the route `<Outlet/>`
    - `AnalyticsDialog`
  - `.controller` with `<ControllerHints/>` and the `<dialog class="simple-keyboard-wrapper">` host for the virtual keyboard
  - `<WindowControls/>`, only if `isFrameless && !navigator.windowControlsOverlay.visible`
  - `<Help/>`, only if `experimentalFeatures.enableHelp`

### Routes (hash router, `src/frontend/App.tsx:126-189`)
All routes are children of `/` and lazy-loaded.

| Path | Renders | Notes |
|---|---|---|
| `/` (index) | `screens/Library` | Library section |
| `/login` | `screens/Login` | store accounts page |
| `/store/:store` | `screens/WebView` | `:store` ∈ `epic`, `gog`, `amazon`, `zoom` |
| `/wiki` | `screens/WebView` | Heroic GitHub wiki in a webview |
| `/gamepage/:runner/:appName` | `screens/Game/GamePage` | GamePage section; nav state `{fromDM, gameInfo}` or `{fromGameCard}` |
| `/store-page` | `screens/WebView` | `?store-url=<encoded url>` opens an arbitrary store page (used by Deals and the game page) |
| `/discounts` | `screens/Discounts` | Deals |
| `/loginweb/:runner` | `screens/WebView` | `:runner` ∈ `legendary`, `gog`, `nile`, `zoom`: login flows |
| `/settings/:type` | `screens/Settings` | `general`, `games_settings`, `advanced`, `systeminfo`, `log` (Settings section) |
| `/wine-manager` | `screens/WineManager` | hidden on Windows in the sidebar |
| `/download-manager` | `screens/DownloadManager` | |
| `/accessibility` | `screens/Accessibility` | |
| `/console` | `screens/ConsoleMode` | full-screen controller UI |
| `*` | `<Navigate replace to="/">` | catch-all. Note that the backend `openScreen('/library')` from Ctrl+L lands here and redirects to `/` |

`WebView` also has dead URL-map entries for `/loginEpic` and `/loginGOG` that no route reaches (`src/frontend/screens/WebView/index.tsx:100-101`).

### Accessibility screen (`/accessibility`, `src/frontend/screens/Accessibility/index.tsx`)
- **Zoom**: a range from 60–200% in steps of 10 with tick labels (`:115-135`). It calls `setZoomPercent`, which persists `configStore.zoomPercent` and, after a 500 ms debounce, sends `setZoomFactor` (`src/frontend/state/GlobalState.tsx:294-303`).
- **Fonts**: content font and actions font pickers. The list comes from the browser `queryLocalFonts()` plus the two defaults (`:62-69`). Choices persist to `contentFontFamily` and `actionsFontFamily` and are applied through CSS vars.
- **Theme selector**: `<ThemeSelector/>` (`:175`).
- **Toggles**:
  - All tiles in color
  - Titles always visible
  - Disable closing dialogs by clicking outside
  - Disable smooth scrolling (requires restart; app setting `disableSmoothScrolling`, read at backend start, `src/backend/main.ts:369-371`)
  - Disable UI animations
- Registers a help item named `accessibility`.

### Wine Manager screen (`/wine-manager`, `src/frontend/screens/WineManager/index.tsx`)
- **Tabs**: on Linux, Proton-CachyOS and GE-Proton. On macOS, Game-Porting-Toolkit, Wine-Crossover and Wine-Staging-macOS (`:44-56`). The default tab is the first, except Intel Mac, which defaults to Wine-Crossover (`:58-66`).
- **Header**: a version search box, a refresh button (`refreshWineVersions(true)` → `refreshWineVersionInfo` invoke; on failure an OS notification reads "Couldn't fetch releases…", `src/frontend/state/GlobalStateV2.ts:87-104`), and a Linux-only Settings modal containing CustomWineProton and DownloadProtonToSteam.
- **Each row**: release notes (opens in browser with `openWebviewPage`), install, abort while downloading (`abort`), open folder (`showItemInFolder`), update and uninstall (`installWineVersion`/`removeWineVersion`). Progress comes from the `progressOfWineManager` event, with an "Unzipping" state. The list refreshes on the `wineVersionsUpdated` event.
- **Empty state**: "No Wine versions found. Please click the refresh icon…"

---

## Global state & backend events

### Global context (`src/frontend/state/GlobalState.tsx`, defaults in `src/frontend/state/ContextProvider.tsx`)
Shell-relevant state (Library-specific state is listed in the Library section):
- **Per store**: `epic {library, username}`, `gog {library, username}`, `amazon {library, user_id, username}` and `zoom {library, username, enabled}`. Zoom is only enabled when `experimentalFeatures.zoomPlatform` is on (`:195-213`).
- `libraryStatus: GameStatus[]`, the live per-game operation status, and `gameUpdates: string[]` (persisted to localStorage `updates`).
- `refreshing` / `refreshingInTheBackground`.
- `platform`, `isIntelMac`, `isFullscreen`, `isFrameless`.
- `language` and `isRTL` (RTL only for `fa` and `ar`, `:48`). The body class `isRTL` is toggled in `componentDidUpdate` (`:1051-1052`).
- `connectivity {status, retryIn}`. The initial value is `offline` until `get-connectivity-status` resolves (`:248`, `:1031-1033`).
- `dialogModalOptions` / `showDialogModal`, `showResetDialog`, and `externalLinkDialogOptions` / `handleExternalLinkDialog`.
- `hideChangelogsOnStartup` and `lastChangelogShown` (localStorage `last_changelog`).
- `help.items` with `addHelpItem`/`removeHelpItem`, used by `hasHelp()` in screens such as login, downloadManager, library, gamePage, settings, wineManager and accessibility.
- `experimentalFeatures {enableHelp, cometSupport, zoomPlatform}`.
- `activeController`: the gamepad id string, or `''` (`:1014-1023`).
- Appearance: `theme`, `zoomPercent`, fonts, `allTilesInColor`, `titlesAlwaysVisible`, `disableDialogBackdropClose`, `disableAnimations`.
- `sidebarCollapsed` / `setSideBarCollapsed` exist and persist to localStorage `sidebar_collapsed`, but **nothing in the UI uses them**. Collapse is driven by sidebar width (see Sidebar).
- `showInstallModal` is set by the `installGame` event but **nothing renders it** (see below).

Zustand stores:
- `GlobalStateV2` (`src/frontend/state/GlobalStateV2.ts`):
  - `uploadLogFileProps` and `showUploadedLogFileList`
  - `settingsModalProps` with `openGameSettingsModal`, `openGameLogsModal`, `openGameCategoriesModal` and `closeSettingsModal`
  - `refreshingWineVersions` / `refreshWineVersions`
  - `gameOverrides`
- `InstallGameModal` (`src/frontend/state/InstallGameModal.ts`): `openInstallGameModal({appName, runner, gameInfo, action: 'install'|'import'})`.
- `InstallProgress` (`src/frontend/state/InstallProgress.ts`): `Record<"${appName}_${runner}", InstallProgress>`, fed by the `progressUpdate` event.
- `UploadedLogFiles` (`src/frontend/state/UploadedLogFiles.ts`): initialised from `getUploadedLogFiles` (invoke) and kept in sync by the `logFileUploaded` / `logFileUploadDeleted` events.
- `TourContext` (`src/frontend/state/TourContext.tsx`): see First-run / onboarding / tour.

### Startup work in `componentDidMount` (`src/frontend/state/GlobalState.tsx:881-1039`)
1. Subscribes to events: `installGame`, `gameStatusUpdate`, `refreshLibrary`, `metadataChanged`, `pushGameToLibrary` and `fullscreen`.
2. On macOS, calls `isIntelMac` (invoke). Then `isFullscreen` and `isFrameless` (invoke).
3. When users are stored, calls `getUserInfo`, `getAmazonUserInfo` and, if Zoom is enabled, `getZoomUserInfo` (invoke).
4. Restores `gameUpdates` from localStorage.
5. If any store is logged in: `refreshLibrary({checkForUpdates: true, runInBackground: <library cache non-empty>})`.
6. Listens for the window `controller-changed` event, toggles the body class `controllerLayout` and sets `activeController`.
7. Subscribes to `connectivity-changed` (event) and calls `get-connectivity-status` (invoke, alias `getConnectivityStatus`).
8. Applies fonts, then sends `frontendReady`. The backend then runs pending `heroic://` protocol args and starts the download queue 5 s later (`src/backend/main.ts:467-480`).

### Power-save lock (`src/frontend/state/GlobalState.tsx:1059-1083`)
On every state update, the app counts `libraryStatus` entries in installing, updating, launching, playing, redist, winetricks, extracting, repairing, moving, syncing-saves or uninstalling.
- If any exist, it sends `lock(playing)`. The backend `powerSaveBlocker` prevents app suspension, and also prevents display sleep while playing (`src/backend/main.ts:509-522`).
- Otherwise it sends `unlock` (`:524-535`).

### Backend → frontend events (all of `FrontendMessages`, `src/common/types/ipc.ts:376-420`)

| Event | Frontend alias | Consumer / effect |
|---|---|---|
| `gameStatusUpdate` | `handleGameStatus` | `GlobalState.handleGameStatus`. Adds, replaces or removes entries in `libraryStatus`. On `done`/`error` it triggers a background `refreshLibrary(runner)`. If the game was updating, it is removed from `gameUpdates` and updates are re-checked (`GlobalState.tsx:799-879`) |
| `progressUpdate` | `onProgressUpdate` | InstallProgress store |
| `changedDMQueueInformation` (elements, state) | `handleDMQueueInformation` | Sidebar current-download widget; Download Manager |
| `showDialog` (title, message, type, buttons) | `handleShowDialog` | `DialogHandler` → `MessageBoxModal`. Button `onClick` functions cannot cross IPC, so backend dialogs only close |
| `openScreen` (path) | `handleGoToScreen` | Sidebar runs `navigate(path, {state:{fromGameCard:false}})` (`components/UI/Sidebar/index.tsx:58-63`) |
| `refreshLibrary` (runner?) | `handleRefreshLibrary` | Background library refresh. Sent after clear-cache, after legendary/nile/sideload changes |
| `installGame` (appName, runner) | `handleInstallGame` | Sets `state.showInstallModal`, **which nothing renders**. The protocol "install?" flow is effectively a no-op in v2.22.4. Parity: open the install dialog |
| `launchGame` | — | Declared in the type, but nothing in the code sends or consumes it |
| `recentGamesChanged` | `handleRecentGamesChanged` | Library "recently played" row. The backend also uses an internal event to rebuild the tray menu |
| `pushGameToLibrary` (GameInfo) | `handleGamePush` | Upserts a GOG or Zoom game into the library |
| `metadataChanged` (overrides) | `handleMetadataChanged` | Re-applies title/art overrides to all libraries |
| `connectivity-changed` ({status, retryIn}) | `onConnectivityChanged` | `connectivity` state |
| `maximized` / `unmaximized` | `handleMaximized` / `handleUnmaximized` | WindowControls icon state |
| `fullscreen` (bool) | `handleFullscreen` | `isFullscreen` |
| `wineVersionsUpdated`, `progressOfWineManager` | — | Wine Manager |
| `progressOfWinetricks`, `installing-winetricks-component` | — | Winetricks dialog (Settings) |
| `logFileUploaded` / `logFileUploadDeleted` | `logFileUploadedSlot` / `logFileUploadDeletedSlot` | Uploaded log list |

### Shell frontend → backend calls (non-exhaustive list of the shell's own calls)
- **invoke**:
  - `getHeroicVersion`, `getLatestReleases`, `getCurrentChangelog`
  - `isFullscreen`, `isFrameless`, `isMaximized`, `isIntelMac`
  - `getDMQueueInformation`
  - `get-connectivity-status`
  - `getCustomCSS`, `getThemeCSS`, `getWebviewPreloadPath`
  - `login`, `authGOG`, `getAmazonLoginData`, `authAmazon`, `authZoom`, `getZoomUserInfo`, `getUserInfo`, `getAmazonUserInfo`, `logoutLegendary`, `logoutAmazon`
  - `clipboardReadText`
  - `requestAppSettings`
  - `gamepadAction`
  - `getGogDealsRegion`, `getGogDiscounts`, `getGmgDiscounts`, `getHumbleDiscounts`, `getGogWishlist`
  - `systemInfo.get`
- **send**:
  - `frontendReady`, `lock`/`unlock`, `setZoomFactor`, `setTitleBarOverlay`
  - `logInfo`/`logError`, `notify`
  - `quit`, `processShortcut`
  - `openExternalUrl`, `openWebviewPage`, `openDiscordLink`, `openPatreonPage`, `openKofiPage`, `openGithubSponsorsPage`, `openLoginPage`, `openSidInfoPage`, `openWeblate`
  - `clipboardWriteText`, `changeLanguage`, `clearCache`
  - `cancelDownload`, `pauseCurrentDownload`, `resumeCurrentDownload`, `removeFromDMQueue`
  - `set-connectivity-online` (alias `setConnectivityOnline`), `connectivity-changed` (alias `connectivityChanged`)
  - `minimizeWindow`, `maximizeWindow`, `unmaximizeWindow`, `closeWindow`, `setFullscreen`
  - `logoutGOG`, `logoutZoom`
  - `setSetting`

### Persisted UI state (renderer storage). A native port needs an equivalent store.
- **localStorage**:
  - `language`, `updates`, `last_version`, `last_changelog`, `hide_changelogs`, `sidebar_collapsed` (dead), `sidebar-width`
  - `heroic-tour-state`, `show_external_link_dialog`, `analytics-modal-shown`, `adtraction-warning`
  - `discounts.filters`, `discounts.regionOverride`, `discounts.gmgCurrencyOverride`
  - `scrollPosition`, `category`, `current_custom_categories`
  - `${appName}_${runner}_progress` and `${appName}` (partial download progress kept when cancelling while keeping files)
- **sessionStorage**: `last-store` and `last-url-<store>`.
- The frontend writes directly to these electron-store files: `configStore` (theme, zoomPercent, fonts, language, …) and the `download-manager` store's `finished` list.

---

## Sidebar / navigation

### Structure (`src/frontend/components/UI/Sidebar/index.tsx:138-155`)
From top to bottom:
1. Heroic logo (decorative)
2. `SidebarLinks`
3. The current-download widget
4. `HeroicVersion`
5. A drag `resizer`
6. `SidebarTour`

### Resize and collapse (`Sidebar/index.tsx:14-136`)
- The user drags the right edge (the left edge in RTL). Width is clamped to 60–400 px and defaults to 240. It is persisted to localStorage `sidebar-width` when the drag ends.
- Below 120 px the sidebar gets the `collapsed` class. Labels are hidden, only icons show, and submenus open as hover/focus flyouts (`Sidebar/index.scss:74-121`, `SidebarItem/index.css:85-110`).
- During a drag, `pointer-events` is disabled on any `<webview>` so the drag isn't swallowed.
- There is no explicit collapse button and no keyboard or gamepad way to resize.

### Links, in order (`components/UI/Sidebar/components/SidebarLinks/index.tsx:110-306`)
`loggedIn` = `epic.username || gog.username || amazon.user_id || zoom.username` (`:53-54`).

| # | Item | Condition | Target / action | data-tour |
|---|---|---|---|---|
| 1 | Login | `!loggedIn` | `/login` | `sidebar-login` |
| 2 | Library | always | `/`. Also active on `/gamepage/*`. On click it resets `scrollPosition` and refreshes the library in the background if any logged-in store has an empty library (`:56-68`) | `sidebar-library` |
| 3 | Stores | always | `/store/<default>`. Also active on any path containing `store`. The default is chosen in this order: (a) `zoom` if Zoom is enabled and it is the only logged-in store; (b) `amazon` if only Amazon is logged in; (c) `gog` if Epic is not logged in but GOG is; (d) otherwise `epic`. Overridden by sessionStorage `last-store` (`:85-108`) | `sidebar-stores` |
| 3a | ↳ Epic Store / GOG Store / Amazon Luna / Zoom Store | submenu shown only while on a webview path (`store` or `last-url` in pathname). Zoom only if enabled | `/store/epic`, `/store/gog`, `/store/amazon`, `/store/zoom` | — |
| 4 | Deals | always | `/discounts` | `sidebar-discounts` |
| — | divider | | | |
| 5 | Settings | always | `/settings/general`. Active on any settings path | `sidebar-settings` |
| 5a | ↳ General, Game Defaults (not Windows), Advanced, System Information, Log | submenu only while on `/settings/*` | `/settings/{general,games_settings,advanced,systeminfo,log}` | — |
| 6 | Console Mode | always | `/console` | `sidebar-console` |
| 7 | Downloads | always | `/download-manager` | `sidebar-downloads` |
| 8 | Wine Manager | not Windows | `/wine-manager` | `sidebar-wine` |
| 9 | Manage Accounts | `loggedIn` | `/login` | `sidebar-manage-accounts` |
| 10 | Accessibility | always | `/accessibility` | `sidebar-accessibility` |
| — | divider | | | |
| 11 | Documentation | always | `/wiki` (in-app webview) | `sidebar-docs` |
| 12 | Discord, Patreon, Ko-fi, GitHub Sponsors | always (group `sidebar-community`) | Each goes through the external-link confirmation (`handleExternalLink`, `:70-83`) and then sends `openDiscordLink`, `openPatreonPage`, `openKofiPage` or `openGithubSponsorsPage`. URLs are in `src/backend/constants/urls.ts:10-18` | `sidebar-community` |
| 13 | Quit | always | Confirm dialog "Quit / Are you sure you want to quit?" with Quit and Cancel. Quit sends `quit`, which calls the backend `handleExit` and shows a second native confirm if operations are pending (`QuitButton/index.tsx:16-31`) | `sidebar-quit` |

`SidebarItem` (`components/UI/Sidebar/components/SidebarItem/index.tsx`) renders either a `NavLink` with an `active` class, or a `<button>` for actions.

### Current download widget (`components/UI/Sidebar/components/CurrentDownload/index.tsx`)
- Its data source is the first element of `getDMQueueInformation` (invoke), kept current by `changedDMQueueInformation` (`Sidebar/index.tsx:25-44`).
- It is hidden when `libraryStatus` is empty (`:50-52`).
- It shows:
  - the game title, fetched via `getGameInfo`; it is special-cased to "EOS Overlay" for app id `98bc04bc842e4906993fd6d6644ffb8d`
  - a download icon with a `NN%` badge
  - a status line reading "Installing", or "Processing files, please wait" when above 98%
  - a linear progress bar with a rounded-up %
- The status always says "Installing", even for updates.
- Clicking it navigates to `/download-manager`.

### Version / update indicator (`components/UI/Sidebar/components/HeroicVersion/index.tsx`)
- Shows "Heroic Version: x.y.z" from `getHeroicVersion` (invoke). Clicking toggles the changelog modal (`:79-92`).
- **On version change** (compared with localStorage `last_version`), it calls `clearCache(false, true)` (send) to clear caches silently after an upgrade (`:33-42`).
- Calls `getLatestReleases` (invoke). This returns only newer stable and/or beta releases, and is empty if `checkForUpdatesOnStartup` is off (`src/backend/main.ts:657-664`). When releases exist, "Update Available!" shows with links "Stable (vX)" and "Beta (vY)" that open the GitHub release page with `openExternalUrl` (`:95-115`).
- A tour "?" button (`TourButton` for `sidebar-tour`) sits next to the version (`:93`).
- The changelog auto-shows on startup (see Heroic self-update & changelog).

---

## Library

Route: `/` (index route, lazy-loaded) — `src/frontend/App.tsx:126-134`. The screen is `src/frontend/screens/Library/index.tsx` and owns a `LibraryContext` (`src/frontend/screens/Library/LibraryContext.tsx`) that every sub-component (header, filters, cards) reads. The global library data, game status, favourites/hidden/categories and refresh logic live in the class component `src/frontend/state/GlobalState.tsx` (exposed via `ContextProvider`), plus a few zustand stores (`GlobalStateV2.ts`, `InstallGameModal.ts`, `InstallProgress.ts`).

Conventions in this section:
- "IPC `x`" = channel name in `src/common/types/ipc.ts`. If the frontend alias (`window.api.<alias>`) differs, it is given as `alias → channel`.
- Persistent electron-store access (`configStore.get/set`, `libraryStore.get`, …) is **not IPC**: the preload creates `electron-store` instances directly and the renderer calls them synchronously (`src/preload/api/misc.ts:87-…`, `src/frontend/helpers/electronStores.ts:12-58`). A native rewrite should treat these as direct reads/writes of the same JSON files.
- `localStorage` = Chromium renderer localStorage (would need a native equivalent: a small UI-state file).

### Screen layout (top to bottom)

`src/frontend/screens/Library/index.tsx:686-775`

1. **Header** (`src/frontend/components/UI/Header/index.tsx`): search bar (`LibrarySearchBar`), **Categories** dropdown (`CategoryFilter`), **Filters** dropdown (`LibraryFilters`).
2. **Library tour** host (`LibraryTour`, invisible until started).
3. Optional **top section** (exactly one of): *Played Recently* row or *Favourites* row (driven by the app setting `libraryTopSection`).
4. **Library header** (`LibraryHeader`): title "All Games" / "Favourites", game count, **Add Game** button, and the **ActionIcons** toolbar (layout toggle, sort A↔Z, installed-first toggle, alphabet-filter toggle, refresh, start-tour).
5. **Alphabet filter** bar (A–Z, `#`), if enabled.
6. Blocking refresh spinner (`UpdateComponent`) — only when `refreshing && !refreshingInTheBackground` (index.tsx:755).
7. **Empty state** when the filtered list is empty (index.tsx:757).
8. **Games list** (grid or list) — hidden while a foreground refresh is running (index.tsx:759-766).
9. Floating **Back-to-top** button (index.tsx:769-771), visible once `document.body.scrollTop > 450` (index.tsx:246-266).
10. **Categories Manager** dialog when opened from the Categories dropdown (index.tsx:773).
11. A help item `'library'` ("Shows all owned games.") registered via `hasHelp` (index.tsx:70-74) — only visible if the experimental help feature is on.
12. Fallback `ErrorComponent` "No Games found - Try to logout and login again…" if `!epic && !gog && !amazon && !zoom` (index.tsx:675-684) — practically unreachable (those objects always exist).

Globally (outside the Library route but launched from it) `App.tsx:94` renders `InstallGameWrapper` (install/import/sideload modal) and `SettingsModalWrapper` (per-game Settings / Logs / Categories modal).

### Library data sources and loading

`src/frontend/state/GlobalState.tsx`

| Store (runner) | Source at startup | Logged-in check | Notes |
|---|---|---|---|
| Epic (`legendary`) | `libraryStore.get('library')` = `store_cache/legendary_library.json` (CacheStore, no expiry) — GlobalState.tsx:146-151 | `configStore 'userInfo.displayName'` (:198) | Install info already embedded. |
| GOG (`gog`) | `gogLibraryStore.get('games')` (`store_cache/gog_library.json`) merged with `gogInstalledGamesStore.get('installed')` (`gog_store/installed.json`) by `appName` → sets `install` + `is_installed` (:153-169) | `gogConfigStore 'userData.username'` (:202) | |
| Amazon (`nile`) | `nileLibraryStore.get('library')` (`store_cache/nile_library.json`) (:170-176) | `nileConfigStore 'userData.user_id'` / `.name` (:206-207) | Filter/visibility uses `user_id`, not username. |
| ZOOM (`zoom`) | `zoomLibraryStore.get('games')` + `zoomInstalledGamesStore.get('installed')` (:178-193) | `zoomConfigStore 'username'` (:211) | Only exposed when experimental flag `settings.experimentalFeatures.zoomPlatform` is on (:212, :1145-1151). |
| Sideloaded (`sideload`) | `sideloadLibrary.get('games')` (`sideload_apps/library.json`) (:255) | always shown | |

- **Metadata overrides** (custom title / cover / square art) are loaded from `gameOverridesStore 'overrides'` (`store/game-overrides.json`) into zustand `GlobalStateV2.gameOverrides` (`GlobalStateV2.ts:83`) and attached to every game as `game.overrides` by `attachGameOverrides` (`src/frontend/helpers/gameOverrides.ts:5-20`; applied in GlobalState.tsx:134-143). Display code always uses `overrides.title || title` etc.
- **Startup** (`componentDidMount`, GlobalState.tsx:881-1039):
  - Subscribes to backend events: `installGame` (alias `handleInstallGame`), `gameStatusUpdate` (`handleGameStatus`), `refreshLibrary` event (`handleRefreshLibrary`), `metadataChanged` (`handleMetadataChanged`), `pushGameToLibrary` (`handleGamePush`), `connectivity-changed` (`onConnectivityChanged`), plus `fullscreen`.
  - If logged in: IPC `getUserInfo` (Epic), `getAmazonUserInfo`, `getZoomUserInfo` (zoom only if enabled) (:986-996).
  - Restores cached `gameUpdates` from localStorage `updates` (:998-1001).
  - If any account logged in → `refreshLibrary({checkForUpdates: true, runInBackground: <any cached library non-empty>})` (:1003-1012). So with an empty cache the first refresh is a *foreground* refresh (blocking spinner, list hidden).
  - `get-connectivity-status` (alias `getConnectivityStatus`) for initial online state (:1031-1033). **Initial connectivity state is `'offline'`** (:248) until that resolves.
- **`refreshLibrary(options)`** (:779-797): ignored if a refresh is already running (`if (this.state.refreshing) return`). Sets `refreshing` + `refreshingInTheBackground = runInBackground (default true)`, calls IPC `refreshLibrary(library?)` (backend refreshes one runner or all with `Promise.allSettled`, `src/backend/main.ts:864-875`), then `refresh()`.
- **`refresh(library, checkUpdates)`** (:693-777): optionally IPC `checkGameUpdates` → `string[]` of app_names with updates (backend also runs auto-update if `autoUpdateGames`, `main.ts:626-642`); reloads each store from cache; **if a logged-in store's cache is empty it re-fetches**: Epic via IPC `readConfig('library')` + `readConfig('user')` (alias `readConfig`, helpers/index.ts:46-59), GOG/Nile/Zoom via another IPC `refreshLibrary(<runner>)`. Re-applies overrides, sets `gameUpdates`, clears `refreshing`. Forces a re-render if Epic library length changed.
- **Game pushed by backend** (`pushGameToLibrary`, :928-965): replaces/appends a single GOG or ZOOM GameInfo in state (used after installs). Note: does **not** re-apply overrides.
- **Metadata changed** (`metadataChanged` event, :924-926): rebuilds all libraries with new overrides (:667-691).
- **Backend-requested refresh** (`refreshLibrary` event with runner, :916-922) → background refresh without update check.
- **Game status events** (`gameStatusUpdate`, :799-879): maintains `libraryStatus: GameStatus[]` (`{appName, status, folder, context, progress, runner}`).
  - First status for an app is always added.
  - Subsequent changes are kept only for `installing, updating, playing, extracting, launching, winetricks, redist, queued`; `done`/`error` remove the entry and trigger `refreshLibrary({runInBackground:true, library: runner})`; if the previous status was `updating` it also drops the app from `gameUpdates` (and persists `updates`) and refreshes with `checkForUpdates: true`.
  - Other transitions (e.g. `installing → moving`) just remove the entry.
- **Power lock** (`componentDidUpdate`, :1041-1084): while any status in {installing, updating, launching, playing, redist, winetricks, extracting, repairing, moving, syncing-saves, uninstalling} exists → IPC `lock(playing)` else `unlock`.
- Sidebar "Library" link (`src/frontend/components/UI/Sidebar/components/SidebarLinks/index.tsx:56-68,119-126`) resets localStorage `scrollPosition` to 0 and triggers `refreshLibrary({runInBackground:true})` only if some logged-in store has an empty library.
- Login success (from Login screen) calls `handleSuccessfulLogin(runner)` → sets legacy localStorage `category='all'` and a **foreground** refresh of that runner (:529-535). Logout of any store reloads the whole window (:555-665).

### Refresh library (user action)

- Button in ActionIcons (`src/frontend/components/UI/ActionIcons/index.tsx:113-131`): `refreshLibrary({checkForUpdates: true})` (background). Icon spins and button gets `active` class while `refreshing`. Tooltip `generic.library.refresh` "Refresh Library".
- Because it is a background refresh, the list stays visible; the blocking spinner only appears for foreground refreshes.
- Edge: clicks while a refresh is in flight are silently dropped (GlobalState.tsx:784).

### Search

Two different search mechanisms share one input (`filterText` in LibraryContext; **not persisted**, reset on remount).

1. **Grid/list filtering — fuzzy** (`src/frontend/screens/Library/index.tsx:519-551`):
   - Fuse.js over `{title: overrides.title || title, normalizedTitle: normalizeTitle(title)}`.
   - Options: `threshold: 0.4`, `minMatchCharLength: 1`, `useExtendedSearch: true`, keys `['title','normalizedTitle']`. Extended search means Fuse operators work: space = AND, `|` = OR, `=exact`, `'include`, `^prefix`, `suffix$`, `!not`, `!^`, `!suffix$`.
   - `normalizeTitle` = remove every char that is not `\w` or whitespace, lowercase (`src/frontend/helpers/library.ts:468-470`).
   - Search runs *after* store/category/flag filters and platform filter, *before* hidden filter and alphabet filter. Fuse relevance order is then **discarded** because the list is re-sorted alphabetically (index.tsx:613-619).
   - Only titles are searched (no developer, store, tags, etc.).
2. **Autocomplete dropdown** (`src/frontend/components/UI/LibrarySearchBar/index.tsx`):
   - Shown whenever input non-empty (`SearchBar/index.tsx:73-87`).
   - Source: *all* games from all stores (ignores store/platform/category/flag filters **and hidden state**), excluding DLC (:34-57).
   - Matching: plain substring `normalizeTitle(el.title).includes(normalizeTitle(fixFilter(text)))` on the **original** (non-override) title; `fixFilter` strips `? \ | * + ( ) [ ]` (:10-13).
   - Sorted by original title (`<` comparison). Each entry shows override title (if any) + `(Epic|GOG|Amazon|Zoom|<runner>)`.
   - Clicking an entry clears the search and navigates to `/gamepage/:runner/:appName` with `state.gameInfo` (:59-64).
   - Clear (×) button empties the search and refocuses (`SearchBar/index.tsx:47-53`).
   - Input is uncontrolled with a native `input` listener (needed for the virtual keyboard); `id="search"` is referenced by the gamepad/virtual keyboard helpers (`SearchBar/index.tsx:21-45,63-72`).
- Placeholder `search` "Search for Games". Tour anchor `data-tour="library-search"`.

### Filters dropdown

`src/frontend/components/UI/LibraryFilters/index.tsx` (opens on hover or click, `Dropdown` with `popUpOnHover`; tour anchor `library-filters`). All values persist in localStorage (index.tsx:82-192).

| Filter | UI label (i18n key) | Default | localStorage key | Logic (index.tsx) |
|---|---|---|---|---|
| Store: Epic | `Epic Games` | true | `storesFilters.legendary` | Toggle only shown if `epic.username` (LibraryFilters:183) |
| Store: GOG | `GOG` | true | `storesFilters.gog` | only if `gog.username` |
| Store: Amazon | `Amazon Games` | true | `storesFilters.nile` | only if `amazon.user_id` |
| Store: ZOOM | `ZOOM Platform` | true | `storesFilters.zoom` | only if `zoom.enabled && zoom.username` |
| Store: Other (sideload) | `Other` | true | `storesFilters.sideload` | always shown |
| Platform: Windows | `platforms.win` | true | `platformsFilters.win` | always |
| Platform: Linux | `platforms.linux` | true | `platformsFilters.linux` | toggle only on Linux host |
| Platform: Mac | `platforms.mac` | true | `platformsFilters.mac` | toggle only on macOS host |
| Platform: Browser | `platforms.browser` | true | `platformsFilters.browser` | always |
| Show Hidden | `header.show_hidden` | false | `show_hidden` | :553-562 |
| Show non-Available games | `header.show_available_games` | **true** | `show_non_available` | :498-504 (duplicated :510-516) |
| Show Favourites only | `header.show_favourites_only` | false | `show_favorites` | :444-447 |
| Show Installed only | `header.show_installed_only` | false | `show_installed_only` | :506-508 |
| Show offline-supported only | `header.show_support_offline_only` | false | `show_support_offline_only` | `game.canRunOffline` :486-488 |
| Show third-party managed only | `header.show_third_party_managed_only` | false | `show_third_party_managed_only` | `!!game.thirdPartyManagedApp` :490-492 |
| Show games with updates only | `header.show_updates_only` | false | `show_updates_only` | `gameUpdates.includes(app_name)` :494-496 |

- Each store and platform toggle has an **"only"** button (`header.only`) that turns that one on and all others off (LibraryFilters:82-108).
- **Reset** (`header.reset`, LibraryFilters:152-173): all stores on, all platforms on, **Show Hidden = true** (note: not the default false), non-available on, favourites-only/installed-only/offline-only/third-party-only/updates-only off.
- **Store filter semantics** (`makeLibrary`, index.tsx:398-439): a store is included only if its toggle is on *and* the account is logged in (sideload always). **If no store ends up selected, all stores are shown.** Concatenation order: sideload, Epic, GOG, Amazon, ZOOM.
- **Legacy migration**: if `storesFilters` missing, derive from old key `category` (`all|legendary|epic|gog|nile|amazon|sideload|zoom`, index.tsx:87-97, category lists in helpers/library.ts:472-476). If `platformsFilters` missing, derive from old `filterPlatform` (`all|win|linux|mac|browser`, index.tsx:115-125).
- **Platform filter semantics** (`filterByPlatform`, index.tsx:285-334):
  - Enabled set = win + (mac only on darwin) + (linux only on linux) + browser; **if empty → all keys**. Aliases added: win→`windows`; mac→`osx`,`Mac`.
  - Installed game: platform = `install.platform.toLowerCase()` (default `windows`).
  - Not installed: `linux` if `is_linux_native` on Linux host, `mac` if `is_mac_native` on mac host, and **always `windows`**. ⇒ not-installed games are never "browser"; Browser filter only matches installed sideloaded browser apps.
- **Non-available filter**: reads localStorage `nonAvailableGames` (array of app_names). That list is maintained lazily by the card status hook (`handleNonAvailableGames`, `src/frontend/hooks/constants.ts:51-79`) which calls IPC `isGameAvailable({appName, runner})` for each installed, non-third-party card when it mounts/updates; the module caches the array once at import time.
- **"Favourites only" mode bypasses** DLC/category/offline/third-party/updates/non-available/installed filters (they are all in the `else` branch, index.tsx:444-517); store, platform, search, hidden and alphabet filters still apply. The header title switches to "Favourites" (LibraryHeader:34-36).
- DLC entries (`install.is_dlc`) are always removed from the main list (index.tsx:449; GamesList:140-147).

### Categories (custom categories)

Data:
- Definitions + membership: `configStore 'games.customCategories'` = `Record<categoryName, string[]>` where each member is **`${app_name}_${runner}`** (`src/common/types/electron_store.ts:27-32`; GlobalState.tsx:230).
- Current selection: localStorage `current_custom_categories` (JSON array of names, may include `preset_uncategorized`); legacy single-value key `current_custom_category` is migrated (GlobalState.tsx:119-132, 274-280).
- Empty selection = no category filtering.

Category filter dropdown (`src/frontend/components/UI/CategoryFilter/index.tsx`, tour anchor `library-categories`, hover/click to open):
- One toggle per category (sorted, `listCategories()` = sorted unique keys, GlobalState.tsx:401-402) + **Uncategorized** (`preset_uncategorized`), each with an "only" button.
- Empty hint: `header.no_categories` "No custom categories. Add categories using each game menu."
- **Select All** = all categories + uncategorized (CategoryFilter:32-36).
- **Manage Categories** button opens CategoriesManager.
- Filtering (index.tsx:451-484): union of members of all selected categories; `preset_uncategorized` adds every game not present in any category. Not applied in favourites-only mode.

Categories Manager dialog (`src/frontend/screens/Library/components/CategoriesManager/index.tsx`):
- Lists categories; per row: **rename** (pencil → inline text input; confirm disabled if empty/whitespace or unchanged; cancel) and **remove** (trash → confirm ✓ / cancel). Empty: `categories-manager.no-categories`.
- Add input + "+" button (disabled when blank). Uses `customCategories.addCategory(rawName)` (no trim, no duplicate check).
- Backend ops (all GlobalState, persisted to `games.customCategories`):
  - `setCustomCategory` (:407-423): sets `list[name] = []` — **overwrites an existing category's membership if the name already exists**; if a category filter is active, auto-adds the new category to the selection.
  - `removeCustomCategory` (:425-442): deletes; removes from selection; **if the resulting selection is empty it is set to `['preset_uncategorized']`** (so deleting any category while no filter was active switches the library to "Uncategorized only").
  - `renameCustomCategory` (:444-458): moves membership to new key (overwrites if exists), updates selection.

Per-game category assignment — context menu "Categories" → `openGameCategoriesModal(gameInfo)` (`GlobalStateV2.ts:68-76`) → `SettingsModal` type `category` (`src/frontend/screens/Settings/components/SettingsModal/index.tsx:25-70`, header `"<title> (Categories)"`, the word is not translated) → `CategorySettings` (`src/frontend/screens/Settings/sections/CategorySettings/index.tsx`):
- "New category" input (maxLength 33, trimmed; submit disabled if empty or already exists) + "Add new category" → creates and immediately assigns the game.
- Grid of toggles, one per category, on = game is member (`addGameToCustomCategory` / `removeGameFromCustomCategory`, GlobalState.tsx:460-489).
- Per-category delete icon → confirm dialog (`category-settings.delete-question`) → removes the category globally.

### Sorting

Toolbar buttons in `ActionIcons` (`src/frontend/components/UI/ActionIcons/index.tsx:73-98`); logic index.tsx:612-631.

| Control | State / key | Default | Behaviour |
|---|---|---|---|
| Sort A→Z / Z→A (icon `faArrowDownAZ` / `faArrowDownZA`; tooltip "Sort Ascending/Descending") | localStorage `sortDescending` | false (A→Z) | `localeCompare` on `title.toUpperCase().replace('THE ', '')` — uses the **original** title (not the override) and removes the first "THE " occurrence anywhere. |
| Sort by Status (hard-drive icon solid/outline; tooltip `library.sortByStatus`) | localStorage `sortInstalled` | **true** | When on: `[installed…, installing…, not installed…]`, each group keeping the alphabetical order. "installing" = libraryStatus `installing` (not queued/updating). |

There are no other sort modes (no last played, playtime, size, date added, release date).

### Alphabet filter

`src/frontend/screens/Library/components/AlphabetFilter/index.tsx`, toggle in ActionIcons (`library.showAlphabetFilter` / `hideAlphabetFilter`, ActionIcons:99-112).
- Visibility persisted in localStorage `showAlphabetFilter` (default **true**). Selected letter is in-memory only (index.tsx:196-206).
- Buttons `A`–`Z` and `#`. A letter is enabled only if at least one game in the *currently filtered* list (`gamesForAlphabetFilter`) starts with it; clicking the active letter clears it.
- First char = first `[a-zA-Z0-9]` of the (override) title after stripping a leading "the " (case-insensitive); digits map to `#`; titles without ASCII alphanumerics never match any letter (index.tsx:594-610).

### Layout: grid vs list

- Toggle in ActionIcons (`library.toggleLayout.list` / `.grid`, tour anchor `library-view-toggle`), persisted in localStorage `layout` (`'grid'` default | `'list'`) (index.tsx:76-80).
- **List layout** renders a header row: Game Title | Status | Store | Action (`GamesList/index.tsx:128-135`), cards use `gameListItem` class; in list mode an installed game always shows its status label ("Installed <size>") (`GameCard/constants.ts:50`).
- Top-section rows (recent/favourites) always render as cards regardless of layout.
- Appearance settings that affect the list (set elsewhere, read here via context): `allTilesInColor` (configStore `allTilesInColor`; otherwise not-installed tiles are greyscale) and `titlesAlwaysVisible` (configStore `titlesAlwaysVisible`) (`GamesList/index.tsx:52-53,119-125`).
- Scroll position persisted in localStorage `scrollPosition` and restored on mount (index.tsx:226-243).
- Gamepad: when a controller is active, focused cards are scrolled into view with margins (`GamesList/index.tsx:22-41,100-114`).

### Rendering / virtualization

- Every game in the filtered list mounts a `GameCard`. Until the card's placeholder intersects the viewport (+500px rootMargin), it renders an empty `<div data-invisible>`; an `IntersectionObserver` in `GamesList` dispatches a window `CustomEvent('visible-cards', {appNames})` and each card listens for its own app_name (`GamesList/index.tsx:58-98`, `GameCard/index.tsx:78-94,445-454`). Once visible a card stays rendered (no recycling).
- Each mounted, visible card additionally: calls IPC `getGameInfo(appName, runner)` on mount and whenever its status changes (`GameCard/index.tsx:146-155`); runs `hasStatus` which may call IPC `isGameAvailable` for installed games every time `libraryStatus`, Epic or GOG library changes (`src/frontend/hooks/hasStatus.ts:48-116`).

### Library top section (Recently played / Favourites)

Setting `libraryTopSection` (AppSettings, edited in Settings → `src/frontend/screens/Settings/components/LibraryTopSection.tsx:9-50`, live-applied via `handleLibraryTopSection`, GlobalState.tsx:518-520; initial value from `configStore 'settings.libraryTopSection'`, GlobalState.tsx:219). Options (`src/common/types.ts:144-148`):

| Value | Label | Effect |
|---|---|---|
| `disabled` (default) | Disabled | nothing |
| `recently_played` | Recently Played Games | Played Recently row |
| `recently_played_installed` | Recently Played Games (Only Installed) | row limited to installed games |
| `favourites` | Favourite Games | Favourites row |

**Played Recently row** (`src/frontend/screens/Library/components/RecentlyPlayed/index.tsx`):
- Source list: `configStore 'games.recent'` (`RecentGame[] = {appName,title}`, most-recent first; maintained by backend `src/backend/recent_games/recent_games.ts` — added on launch, emits `recentGamesChanged`).
- Limit: AppSettings `maxRecentGames` (1–10, default 5; via IPC `requestAppSettings`; setting UI `src/frontend/screens/Settings/components/MaxRecentGames.tsx`).
- Matches by `app_name` across all store libraries (first match wins), skips DLC, optionally non-installed, and hidden games unless Show Hidden is on.
- Reloads on `recentGamesChanged` event (alias `handleRecentGamesChanged`) and when libraries/hidden change.
- Header `Recent` "Played Recently". Renders nothing when empty.
- The **first card is a large "just played" hero card** (uses `art_cover`, shows a "PLAY" text button) (`GamesList/index.tsx:139`, `GameCard/index.tsx:490-509,260`).
- Its cards get the extra context-menu item **Remove From Recent** → IPC `removeRecent` (alias `removeRecentGame`).

**Favourites row** (index.tsx:339-396, 737-749):
- Favourites stored in `configStore 'games.favourites'` (`FavouriteGame[] = {appName,title}`, keyed by **appName only**) (GlobalState.tsx:229, 376-399).
- Shown when `libraryTopSection === 'favourites'`, there is ≥1 favourite (after removing hidden ones unless Show Hidden), and Favourites-only mode is off.
- Not affected by store/platform/category/search filters. Sorted alphabetically by original title. The first (alphabetical) favourite gets the "just played" hero style (`GamesList/index.tsx:139`).

### Library header & count

`src/frontend/screens/Library/components/LibraryHeader/index.tsx`
- Title "All Games" (`title.allGames`) or "Favourites" when favourites-only.
- Count = list length minus non-sideload DLC; shows `0` when empty.
- **Add Game** button (`add_game`, `AddGameButton`) → opens the install modal in sideload mode (`openInstallGameModal({appName:'', runner:'sideload', gameInfo:null})`, index.tsx:715).
- Tour anchors: `library-header`, `library-add-game`.

### Empty library state

`src/frontend/screens/Library/components/EmptyLibrary/index.tsx`
- If every library (Epic+GOG+Amazon+ZOOM+sideload) is empty: `emptyLibrary.noGames` — "Your library is empty… Click **here** (link to `/login`) to log in with your Epic, GOG.com, Amazon, or Zoom accounts… To use games or apps from other sources, click **[Add Game]** to add them manually."
- Otherwise (filters produced nothing): `emptyLibrary.noResults` "The current filters produced no results." (no indication of which filter).

### Game card

`src/frontend/screens/Library/components/GameCard/index.tsx`, `constants.ts`, `src/frontend/hooks/hasStatus.ts`, `src/frontend/hooks/constants.ts`.

**Content** (GameCard:460-565):
- Whole card is a link to `/gamepage/:runner/:appName` (state `{gameInfo}`).
- Store logo (`StoreLogos`: Epic, GOG, Amazon, ZOOM; anything else → Heroic icon) (`src/frontend/components/UI/StoreLogos/index.tsx`).
- Image: `overrides.art_square || art_square`; `'fallback'`/empty → bundled `heroic_card.jpg`; Epic images get `?h=400&resize=1&w=300` (`constants.ts:4-14`). Hero card uses `overrides.art_cover || art_cover`.
- Logo overlay `art_logo?h=400&resize=1&w=300` (not shown for Amazon unless hero card).
- Status label (top badge + list column), title, store name (`getStoreName`: Epic Games / GOG / Amazon Games / translated "Other"; ZOOM falls into "Other") (`src/frontend/helpers/index.ts:124-135`).
- Install progress effect: CSS var `--installing-effect = (125 - percent)%` grayscale while installing (GameCard:175-177).
- CSS state classes: `installed`, `hidden` (hidden game shown due to Show Hidden), `notAvailable`, `gamepad`, `justPlayed`.
- "New Version Available!" badge (`status.hasUpdates`) only when a **gamepad is active** and an update exists (GameCard:457-458,477-481).
- Images go through `CachedImage` (below).

**Derived status** (`hasStatus`, hooks/hasStatus.ts:48-116), priority order:
1. libraryStatus entry (any status ≠ `done`) → label from `getStatusLabel` (hooks/constants.ts:13-49).
2. `thirdPartyManagedApp` and not EA- or Ubisoft-managed → `notSupportedGame` ("Not Supported").
3. installed & not third-party: IPC `isGameAvailable` false → `notAvailable` ("Game not available"); else `installed` ("Installed <install_size>", size omitted for sideload).
4. otherwise `notInstalled` ("This game is not installed").

Status labels (`gamepage:` namespace): `playing` Playing; `queued` Queued; `uninstalling` Uninstalling; `updating` "Updating Game N%"; `installing` "Downloading N%"; `extracting` Extracting; `syncing-saves` Syncing Saves; `moving` Moving; `repairing` Repairing; `launching` Launching; `winetricks` Applying Winetricks fixes; `redist` "Installing Redistributables ({{context}})". `importing` has no label (falls back to "not installed"). Progress percent comes from the `progressUpdate` event store (`src/frontend/state/InstallProgress.ts`, alias `onProgressUpdate`), merged with a stored percentage from localStorage key `${appName}_${runner}_progress` (`hooks/hasProgress.ts:8-11`) — **that key is never written anywhere**, so this merge is dead code.

Card-state booleans (`getCardStatus`, constants.ts:16-64): `isInstalling` = installing|updating|extracting; `isUpdating`; `isQueued`; `isPlaying`; `isUninstalling`; `notAvailable`; `notSupportedGame`; plus moving/repairing/syncing/launching/winetricks/redist for "has status" display.

**Card buttons** (right side, GameCard:537-561, renderIcon :183-274):

| Button | Shown when | Action |
|---|---|---|
| Update (repeat icon, tooltip "Update (title)") | `hasUpdate && !isUpdating && !isQueued && !notAvailable` | `updateGame({appName, runner, gameInfo})` → IPC `updateGame` (queued in download manager, `src/backend/downloadmanager/ipc_handler.ts:46-62`); not for sideload. `hasUpdate = is_installed && gameUpdates.includes(app_name)` (GamesList:152). |
| Settings (gear) | installed && !uninstalling && not Browser platform | `openGameSettingsModal(gameInfo)` (per-game settings modal) |
| Main action, first matching: | | |
| • Ban icon "Game is NOT Installable" | `gameInfo.installable === false` | none |
| • Ban icon "Third-Party Game NOT Supported" | `notSupportedGame` | none |
| • Disabled blank button | uninstalling | none |
| • Remove-from-queue (−) | queued | IPC `removeFromDMQueue(appName)` |
| • Stop | playing | IPC `kill(appName, runner)` (alias `sendKill`) |
| • Cancel (stop icon) | installing/updating/extracting | not installed → stop-installation dialog (below); installed & updating → IPC `kill` |
| • Play (icon; text "PLAY" on hero card) | installed | launch flow (below); disabled while local `isLaunching` or status in syncing-saves/launching/winetricks/redist; `notAvailableIcon` style if not available |
| • Download | otherwise | opens install modal (`openInstallGameModal({appName, runner, gameInfo})`); no-op for sideload |

**Stop-installation dialog** (`handleStopInstallation`, `src/frontend/helpers/library.ts:102-134`): title/message `gamepage:box.stopInstall.*`; buttons: "Keep installing" (no-op); **Yes** = stop but keep downloaded files → saves `{...progress, folder}` into localStorage key **`<appName>`** (used later to offer "Continue Download") and IPC `cancelDownload(false)`; **No** = IPC `cancelDownload(true)` (delete) and remove localStorage `<appName>`.

**Launch flow** (`launch`, helpers/library.ts:150-266, `checkLaunchOptionsAndLaunch` :268-462):
1. If not online (`connectivity.status !== 'online'`) and `!gameInfo.canRunOffline` → "Offline Warning" dialog ("This game might not work properly offline…"): OK = abort, **Play Anyway** = continue.
2. If `hasUpdate`: IPC `requestGameSettings(appName)`; if `ignoreGameUpdates` → skip; else dialog `box.update.title/message`: **Yes** → `updateGame` (queues update, no launch); **No** → launch with `skipVersionCheck: true`.
3. Launch options: if caller provided → launch. Else IPC `getLaunchOptions(appName, runner)`; 0 or 1 option → launch with it; >1 → use saved `lastUsedLaunchOption` from game settings if present; else dialog "Select Launch Option (Ns)" with one button per option (label = `name` / `dlcTitle` / `executable` by type `basic|dlc|altExe`), **10-second countdown auto-selects the first option**; the chosen option is saved via IPC `setSetting({appName, key:'lastUsedLaunchOption', value})`; closing the dialog cancels.
4. IPC `launch({appName, runner, launchArguments, args, skipVersionCheck})` → `{status}`.

**Context menu** (right-click on card; `ContextMenu/index.tsx`, items GameCard:312-429), in display order:

| Item (gamepage ns) | Visible when | Action / IPC |
|---|---|---|
| Remove from Queue (`button.queue.remove`) | queued && !installing | IPC `removeFromDMQueue` |
| Stop (`label.playing.stop`) | playing | IPC `kill` |
| Play (`label.playing.start`) | installed && !playing && !updating && !queued | launch flow |
| Update (`button.update`) | hasUpdate && !updating && !queued | IPC `updateGame` |
| Install (`button.install`) | !installed && !queued && installable | open install modal |
| Cancel (`button.cancel`) | installing \|\| updating | stop-installation dialog / `kill` |
| Details (`button.details`) | always | navigate to game page |
| Settings (`submenu.settings`) | installed && !uninstalling && !Browser | per-game settings modal |
| Logs (`submenu.logs`) | installed && !uninstalling && !Browser | per-game log modal (`openGameLogsModal`) |
| Edit Game / Edit App/Game (`edit-game.title` / `button.sideload.edit`) | always | non-sideload → EditGameDialog in a dialog modal; sideload → install modal in sideload edit mode |
| Hide Game (`button.hide_game`) | not hidden | `configStore 'games.hidden'` += `{appName, title}` (GlobalState.tsx:353-363) |
| Unhide Game (`button.unhide_game`) | hidden | remove from `games.hidden` (:365-374) |
| Add To Favourites (`button.add_to_favourites`) | not favourite | `games.favourites` (de-duplicated, appended) |
| Categories (`submenu.categories`) | always | per-game categories modal |
| Remove From Favourites (`button.remove_from_favourites`) | favourite | remove from `games.favourites` |
| Remove From Recent (`button.remove_from_recent`) | card is in the recent row | IPC `removeRecent` (alias `removeRecentGame`) |
| Uninstall (`button.uninstall`) | installed && !updating && !playing | opens `UninstallModal` |

**Uninstall modal** (`src/frontend/components/UI/UninstallModal/index.tsx`, reachable from the card): fetches IPC `getGameInfo`, `isNative`, `getGameSettings`, `requestGameSettings('default')`; blocks with a message if an Epic game is installing and this is an Epic game ("Epic games cannot be uninstalled while another Epic game is being installed.") or if the game is running; otherwise confirm with toggles "remove prefix (path)" (only non-native, non-DLC, disabled when the prefix equals the default prefix) and "remove settings" (non-DLC) → IPC `uninstall(appName, runner, deletePrefix, deleteSettings)`; removes localStorage `<appName>`; if sideload on a game page, navigates to library.

### Favourites & hidden games (summary)

- Favourites: `configStore 'games.favourites'`; hidden: `configStore 'games.hidden'`; both `{appName, title}[]`, keyed by appName only (shared across runners).
- Hidden games are removed from the main list, recent row and favourites row unless "Show Hidden" is on; when shown they get a `hidden` style.

### Edit game (metadata overrides) — non-sideload

`src/frontend/components/UI/EditGameDialog/index.tsx`, opened via `showDialogModal({title: 'Edit Game', message: <EditGameDialog/>})` (GameCard:294-310).
- Fields: **Title** (max 40 chars; default override or original), collapsible **Advanced** with **Cover Image** URL and **Square Image** URL. Each URL field has an icon: paste-from-clipboard (via `navigator.clipboard.readText`, not IPC) when empty, clear when filled.
- Previews "Cover Art" and "Square Art" (square falls back to cover, then bundled image). If a SteamGridDB API key exists (IPC `steamgriddb.hasApiKey`) clicking a preview opens the **SteamGridDB picker** (cover → `heroes` mode, square → `grids` mode); otherwise a warning "To search SteamGridDB for cover art, add an API key in **Settings → Advanced**" with a link that closes the dialog and navigates to `/settings/advanced`.
- **Reset to default** (only if `gameInfo.overrides` exists): resets the form (title = original, art fields empty); still needs Finish.
- **Finish**: IPC `setGameMetadataOverride({appName, title, art_cover, art_square})` where each field equal to the original is sent as `''`; backend stores in `game-overrides.json`, deletes the entry when all empty, and emits `metadataChanged` with all overrides (`src/backend/main.ts:1343-1347`) → GlobalState rebuilds libraries.
- Related IPC available but unused here: `getGameMetadataOverride`, `getAllGameOverrides`.

### SteamGridDB picker

`src/frontend/components/UI/SteamGridDBPicker/index.tsx` (used by EditGameDialog and SideloadDialog).
- Props: `initialTitle`, `mode: 'grids'|'heroes'`, optional dimensions/styles. Auto-searches `initialTitle` on mount.
- Search box (Enter or icon) → IPC `steamgriddb.searchGame(query)` → `{id,name}[]`. 0 results → "No games found."; exactly 1 → auto-select; >1 → list "Select a Game:".
- Select → IPC `steamgriddb.getGrids({gameId, styles: ['material','alternate','blurred'], dimensions: ['600x900','342x482','660x930']})` or `steamgriddb.getHeroes({gameId, styles: [], dimensions: []})` → `{id,url,thumb}[]` shown as thumbnails; clicking returns the full `url`. Errors: "No covers found for this game.", "Failed to fetch grids", "Failed to search for games, please check your SteamGridDB API key and try again".
- Back arrow returns to the game list; × closes. API key itself is configured in Settings (IPC `steamgriddb.setApiKey`, out of scope here).

### Image loading (CachedImage)

`src/frontend/components/UI/CachedImage/index.tsx`: `http(s)` sources are loaded through the custom protocol `imagecache://<encodeURIComponent(url)>` (backend disk cache); on error retries the direct URL, then the optional `fallback`. `loading="lazy"`; adds `loading`/`loaded` classes.

### Offline behaviour

- Library renders entirely from cached electron-store files, so it works offline; refresh calls still go to the backend.
- Connectivity from `get-connectivity-status` / `connectivity-changed` (`{status: 'offline'|'check-online'|'online', retryIn}`).
- Offline launch warning for games with `canRunOffline === false` (see launch flow). Filter "Show offline-supported only". Sideloaded native apps are `canRunOffline: true`, browser apps `false` (set by backend, `src/backend/storeManagers/sideload/library.ts:44`).

### Library tour (onboarding)

`src/frontend/screens/Library/components/LibraryTour.tsx`, `src/frontend/components/Tour/*`, `src/frontend/state/TourContext.tsx`.
- Started **manually only** via the "?" button in ActionIcons (`TourButton`, tooltip `tour.startTour`); re-running a completed tour resets it first. Not auto-started on first launch.
- intro.js steps: welcome; "managing the library" (log in / Add Game); game card (only if any library non-empty); search; categories; filters; view toggle; sort A–Z; sort installed; refresh; add game; final "Enjoy your games!". Buttons Next/Back/Skip/Done, exit on overlay click.
- State persisted in localStorage `heroic-tour-state` = `{activeTour, tourProgress, completedTours[]}`; completing adds `library-tour` to `completedTours`.

### Persistent state used by the Library (complete list)

| Where | Key | Type / default | Purpose |
|---|---|---|---|
| localStorage | `layout` | `'grid'`\|`'list'`, `'grid'` | layout |
| localStorage | `storesFilters` | `{legendary,gog,nile,sideload,zoom: bool}` | store filter |
| localStorage | `category` (legacy) | Category | migrated to `storesFilters`; set to `'all'` on login |
| localStorage | `platformsFilters` | `{win,linux,mac,browser: bool}` | platform filter |
| localStorage | `filterPlatform` (legacy) | string | migrated |
| localStorage | `show_hidden` | bool, false | |
| localStorage | `show_favorites` | bool, false | favourites-only |
| localStorage | `show_installed_only` | bool, false | |
| localStorage | `show_non_available` | bool, **true** | |
| localStorage | `show_support_offline_only` | bool, false | |
| localStorage | `show_third_party_managed_only` | bool, false | |
| localStorage | `show_updates_only` | bool, false | |
| localStorage | `showAlphabetFilter` | bool, **true** | |
| localStorage | `sortDescending` | bool, false | |
| localStorage | `sortInstalled` | bool, **true** | |
| localStorage | `scrollPosition` | number string | restore scroll |
| localStorage | `nonAvailableGames` | string[] app_names | non-available filter cache |
| localStorage | `current_custom_categories` | string[] | category selection |
| localStorage | `current_custom_category` (legacy) | string | migrated |
| localStorage | `updates` | string[] | cached `gameUpdates` |
| localStorage | `<appName>` | `InstallProgress` (`{bytes, eta, percent, folder,…}`) | paused/stopped download, enables "Continue Download" |
| localStorage | `${appName}_${runner}_progress` | InstallProgress | read only, never written |
| localStorage | `heroic-tour-state` | TourState | tour |
| configStore (`store/config.json`) | `games.favourites` | `{appName,title}[]` | |
| configStore | `games.hidden` | `{appName,title}[]` | |
| configStore | `games.recent` | `{appName,title}[]` (backend-maintained) | |
| configStore | `games.customCategories` | `Record<string, "appName_runner"[]>` | |
| configStore | `settings.libraryTopSection`, `settings.maxRecentGames`, `settings.defaultInstallPath`, `settings.experimentalFeatures.zoomPlatform` | AppSettings | |
| configStore | `userHome`, `userInfo` | | default install path base; Epic login |
| configStore | `allTilesInColor`, `titlesAlwaysVisible` | bool | card look |
| gameOverridesStore (`store/game-overrides.json`) | `overrides` | `Record<appName,{title?,art_cover?,art_square?}>` | |
| cache stores (`store_cache/*.json`) | `legendary_library:library`, `gog_library:games`, `nile_library:library`, `zoom_library:games` | GameInfo[] | |
| `gog_store/installed.json`, `zoom_store/installed.json` | `installed` | InstalledInfo[] | |
| `gog_store/config.json`, `nile_store/config.json`, `zoom_store/config.json` | `userData`, `isLoggedIn`, `username` | | login state |
| `sideload_apps/library.json` | `games` | GameInfo[] | sideloaded apps |
| backend `gogPrivateBranches` store | `<appName>` | string | GOG private branch password (via IPC) |

### IPC used by the Library screen (summary)

Invoke/handlers: `refreshLibrary`, `checkGameUpdates`, `readConfig`, `getUserInfo`, `getAmazonUserInfo`, `getZoomUserInfo`, `getGameInfo`, `isGameAvailable`, `getConnectivityStatus → get-connectivity-status`, `requestAppSettings`, `requestGameSettings`, `getLaunchOptions`, `launch`, `kill` (alias `sendKill`), `updateGame`, `removeRecent` (alias `removeRecentGame`), `uninstall`, `isNative`, `getGameSettings`, `steamgriddb.hasApiKey|searchGame|getGrids|getHeroes`.
Fire-and-forget: `removeFromDMQueue`, `cancelDownload`, `setSetting`, `setGameMetadataOverride`, `lock`, `unlock`, `logInfo`, `logError`.
Events: `gameStatusUpdate` (`handleGameStatus`), `progressUpdate` (`onProgressUpdate`), `refreshLibrary` (`handleRefreshLibrary`), `metadataChanged` (`handleMetadataChanged`), `pushGameToLibrary` (`handleGamePush`), `recentGamesChanged` (`handleRecentGamesChanged`), `installGame` (`handleInstallGame`), `connectivity-changed` (`onConnectivityChanged`).

Note on `installGame` event (sent by the `heroic://install` protocol handler after the user confirms, `src/backend/protocol.ts:171`): GlobalState stores it into `state.showInstallModal` (GlobalState.tsx:892-910), but **nothing renders `showInstallModal`** — the actual install modal uses the zustand `useInstallGameModal` store. A rewrite should open the install dialog for that event.

## Install, Import & Sideload dialogs

### Modal host and routing

- State: zustand `useInstallGameModal` `{isOpen, appName, runner, gameInfo, action: 'install'|'import'}` (`src/frontend/state/InstallGameModal.ts:5-43`). Opened by `openInstallGameModal(...)` from: library card download button / context menu Install, Add Game button (sideload, empty appName, `gameInfo=null`), context menu "Edit App/Game" (sideload), game page main/import buttons, DownloadDialog "Import Game" button.
- Rendered globally by `InstallGameWrapper` (`src/frontend/screens/Library/components/InstallModal/index.tsx:288-302`) inside one `Dialog` with close button (:166-285).
- Variant selection (:160-282):
  1. `gameInfo.thirdPartyManagedApp` set → **ThirdPartyDialog**
  2. `action === 'import'` && runner ≠ sideload && gameInfo → **ImportDialog**
  3. runner ≠ sideload && gameInfo → **DownloadDialog**
  4. otherwise → **SideloadDialog** (add or edit)
- Shared state passed to children: `winePrefix` (initial `'...'`), `wineVersion`, `wineVersionList`, `crossoverBottle`, `platformToInstall`, `sideloadTitle` (initial translated "Title").

### Platform selection (shared)

`InstallModal/index.tsx:57-158`
- Candidates: **Linux** (host linux && (sideload || `is_linux_native`)), **macOS** (`'Mac'`; host darwin && (sideload || `is_mac_native`)), **Windows** (always), **Browser** (sideload only).
- Select "Select Platform Version to Install:" (`game.platform`) shown only if >1 candidate; **disabled when editing an existing sideload app**.
- Default: `'Mac'` if mac host and mac-native, else `'Windows'` — note: **Linux-native games default to the Windows version on Linux**.
- Before calling `getInstallInfo` the platform is mapped for non-Epic runners: `Mac→osx`, `Windows→windows` (`src/frontend/helpers/index.ts:88-103`). InstallPlatform union: Epic `Windows|Win32|Mac|Android|iOS`, GOG `windows|osx|linux`, Nile `Windows`, ZOOM `windows|linux`, `Browser`.
- `hasWine = platformToInstall === 'Windows' && host !== win32` → fetch IPC `getAlternativeWine` → `WineInstallation[]` (clears the selected version if it disappeared) and render WineSelector.

### Wine selector (shared)

`src/frontend/screens/Library/components/InstallModal/WineSelector/index.tsx`
- Collapsible "Show Wine settings" (`setting.show-wine-settings`); collapsed by default, open by default only in ThirdPartyDialog.
- Loads global settings (IPC `requestAppSettings`).
- **Use shared Wine prefix** toggle (description shows global wine version name and `defaultWinePrefix`): when on, sets prefix = `settings.winePrefix`, version = `settings.wineVersion`, bottle = `settings.wineCrossoverBottle`, disables the fields and shows warning `setting.warn-use-shared-wine-config`.
- Otherwise the suggested prefix is `${defaultWinePrefixDir}/${removeSpecialcharacters(title ?? appName)}` and updates live as the (sideload) title changes. `removeSpecialcharacters` strips `: / * ? < > \ & { } % $ @ \` ! ™ + ' " ®` and `|` (`helpers/index.ts:119-122`).
- **WinePrefix** directory chooser (hidden for CrossOver versions) or **CrossOver Bottle** text field (CrossOver only).
- **Wine version** select listing all `getAlternativeWine` entries (rendered with `WineVersionListItem`); defaults to the **first list entry**; disabled when shared prefix or the list is empty.
- The selection is only persisted on install (see each dialog) via IPC `writeConfig({appName, config})` on non-Windows hosts.

### DownloadDialog (install from a store)

`src/frontend/screens/Library/components/InstallModal/DownloadDialog/index.tsx`

**Initial state**
- `previousProgress` = localStorage `<appName>` (a previously stopped download; :107-109).
- Install path = `previousProgress.folder` || `settings.defaultInstallPath` || `${userHome}/Games/Heroic` (:86-93,130-132). If the game currently has a status with a folder, that folder wins (`installFolder`, :144).
- IPC `getPrivateBranchPassword(appName)` on mount (:175-183).

**Install info fetch** (:313-439), re-run when platform, selected build, branch, saved branch password or UI languages change:
- IPC `getInstallInfo(appName, runner, platform, build, branch)` — note the actual positional order used by frontend and backend (`src/backend/main.ts:757-778`) is `build` then `branch`; the type in `ipc.ts:192-198` names them in the opposite order. Backend returns `null` on error.
- If `manifest.disk_size === 0 && manifest.download_size === 0` → dialog "Game is NOT Installable" / "This game doesn't appear to be installable. Check downloadable content on https://gog.com/account" and close (GOG goodies).
- Exceptions → "Error! Something Went Wrong! <error>" and close.
- Race guard: ignores results if platform/build changed meanwhile.
- **DLCs**: `game.owned_dlc` (Epic/GOG) — **all owned DLCs are preselected**.
- **Builds** (GOG): `manifest.builds` filtered to the current branch (both falsy, or equal), sorted newest first.
- **Languages**: `manifest.languages` sorted; if the current choice isn't available, pick `getPreferredInstallLanguage(languages, i18n.languages)` (first available starting with a UI language, else the first) (`helpers/index.ts:137-153`).
- **Branches** (GOG): `game.branches` (`(string|null)[]`, `null` = default channel).

**Selective downloads (Epic SDL)** (:441-454, 757-775): only for `legendary`; IPC `getGameOverride()` → `sdl_config`; if it contains the appName → IPC `getGameSdl(appName)` → `SelectiveDownload[] {tags[], name, description, required?}` (`src/common/types/legendary.ts:244-249`). Rendered as toggles under "Select components to Install:"; `required` ones are forced on and disabled. Resulting `sdlList` = union of `tags` of required + selected items (:161-173).

**DLC selection** (`DownloadDialog/DLCDownloadListing.tsx`), only for Epic/GOG with ≥1 DLC: master toggle "Install all DLCs" (`dlc.installDlcs`, reflects whether all are selected) + one toggle per DLC title.

**Sizes** (:492-558, 602-653):
- If manifest has `perLangSize` (GOG): download/install = `perLangSize[lang] + perLangSize['*']` + the same for each **selected** DLC that has `perLangSize`; install size also becomes `diskSize` for the disk check.
- Else: `manifest.download_size` / `manifest.disk_size` (Epic/Amazon/ZOOM — selected DLC sizes not included). When resuming into the same folder the download size shown is `(100 - percent)% × disk_size`.
- While unknown: spinner + "Geting download size..." / "Geting install size..." (typos in the source strings `game.getting-download-size`, `game.getting-install-size`).
- When resuming (stored folder == chosen path): extra row "Total Downloaded: N%".
- Sizes formatted with `filesize` base 2 (`helpers/index.ts:27`).

**Language selector** (`DownloadDialog/GameLanguageSelector.tsx`): select "Language:" shown when >1 language; names via `Intl.DisplayNames` in the UI locale (fallback raw code).

**Install path** (:663-726): `PathSelectionBox` (directory picker titled "Select Install Path", default path = default install dir). After sizes are known, IPC `checkDiskSpace(path)` → `{free, diskSize, message: "<free> / <total>", validPath (writable), validFlatpakPath}` (`src/backend/main.ts:537-553`), re-run on path or size change. Shown inline:
- "Space Available: <free/total> - After Install: <free - needed>"; when resuming in the same folder, needed = remaining% × diskSize.
- `!validPath` → warning "Warning: path might not be writable." (does not block).
- `validPath && !validFlatpakPath` → error "Error: Sandbox access not granted to this path, data loss will occur." (**blocks** install).
- `notEnoughDiskSpace` → warning "(Not enough disk space)" and the button label becomes **Force Install** (still allowed).

**GOG beta branch selector** (`DownloadDialog/BranchSelector.tsx`), shown when platform ≠ linux and >1 branch:
- Select "Select beta channel": each branch (null shown as "Disabled") + special entry "Set private channel password".
- Password entry opens a small dialog (password field, Cancel/OK). OK → IPC `setPrivateBranchPassword(appName, password)` (stored backend-side in `gogPrivateBranches` store), then the saved password changes, which re-fetches install info (revealing private branches).
- Selecting a branch re-fetches install info and rebuilds the builds list.

**GOG build (version pin) selector** (`DownloadDialog/BuildSelector.tsx`), shown when platform ≠ linux and builds exist:
- Toggle "Keep the game at specific version" → selects the newest build; off → latest.
- Select "Select game version": "Version <version_name> - <localized date>"; disabled if only one build. Changing it re-fetches install info. The build id is sent as `build` to install.

**Anticheat panel** (`src/frontend/components/UI/Anticheat/index.tsx`; data from IPC `getAnticheatInfo(namespace)` via `hooks/hasAnticheatInfo.ts`, only non-sideload games with a `namespace`): "This game includes anticheat software", warning if status Denied/Broken/Unknown, list of anticheats ("Anticheat removed" if empty), status + "(Reference)" link (opens via `createNewWindow`), and on Linux a "Source: AreWeAntiCheatYet" link.

**Footer**
- **Import Game** → closes and reopens the modal with `action: 'import'` (:303-311).
- **Install** button label (:569-580): "No path selected" if empty path; "Force Install" if not enough space; "Continue Download" if the stored download folder equals the chosen path; else "Install". Disabled (with spinner) until `installPath && diskSize > 0 && !gettingInstallInfo && validFlatpakPath` (:582-583).

**On Install** (:195-301):
1. Anticheat gate: if status in `Denied|Broken|Unknown` → if AppSettings `allowInstallationBrokenAnticheat` → confirm dialog ("Yes (I understand the multiplayer features will not work)" / "No"); else an informational dialog explaining it can be enabled in Settings → Advanced, with only "Ok" (install blocked).
2. Close modal. On non-Windows hosts, if a wine version is set: IPC `requestGameSettings(appName)` then `writeConfig({appName, config: {...settings, winePrefix, wineVersion, wineCrossoverBottle}})`.
3. `install()` helper (`src/frontend/helpers/library.ts:34-100`): returns if no path or already installed; `'default'` path resolves to `defaultInstallPath` (IPC `requestAppSettings`); if a stored progress folder differs from the new path, removes localStorage `<appName>`; IPC `install(InstallParams{appName, path, installDlcs, sdlList, installLanguage (default 'en-US'), runner, platformToInstall, gameInfo, build, branch})`.
4. Backend adds the item to the download queue; for Epic each selected DLC becomes its own queue entry (`src/backend/downloadmanager/ipc_handler.ts:13-44`).

### ThirdPartyDialog (EA app / Ubisoft Connect / other launchers)

`src/frontend/screens/Library/components/InstallModal/ThirdPartyDialog/index.tsx`
- Header: title + platform icons. Notice: "This game is managed by a third-party application", "…: \"{{EA app | thirdPartyManagedApp}}\"", "After clicking Install, Heroic will run the application in order to complete the installation process".
- Anticheat panel; platform select + WineSelector (expanded).
- **Install** button enabled **only for `legendary`** games (:136-142). On click: write wine config (non-Windows, if version set), close, `install({installPath: 'thirdParty', platformToInstall, …})` → IPC `install` with `path: 'thirdParty'`.
- Cards for third-party games that are not EA/Ubisoft managed show "Not Supported" status and a ban icon instead of install.

### ImportDialog (import an existing installation)

`src/frontend/screens/Library/components/InstallModal/ImportDialog/index.tsx`
- Header title + platform icons; path chooser: **directory**, or **file** when platform is `Mac` (to pick `.app`); dialog title "Select Folder to Import"; field label reuses "Select Install Path"; placeholder = default install path.
- Platform select + WineSelector (Windows platform on non-Windows host).
- **Import** button disabled if no path or the game already has status `importing` (spinner).
- On import: close, IPC `importGame({appName, path, runner, platform, winePrefix, wineVersion, wineCrossoverBottle})` → `{status}`. Backend (`src/backend/main.ts:1003-…`): for Epic, refuses during an Epic service outage (error dialog "Epic Servers are having major outage right now, the game cannot be imported!"); emits status `importing` → `done`; on abort/error notifies "Importing Failed".
- No validation of the folder in the UI.

### SideloadDialog (Add Game / Edit App)

`src/frontend/screens/Library/components/InstallModal/SideloadDialog/index.tsx`

**Modes**: add (no appName → new id from `short-uuid`) or edit (appName given → loads IPC `getGameInfo(appName, 'sideload')` and fills exe, URL, user agent, fullscreen, title, square image, hero image if different from square) (:89-132).

**Layout**: left column = square image preview (+ spinner overlay while searching/loading; search overlay if SGDB key), hero/cover preview, title + platform icon; right column = form, or the SteamGridDB picker when an image preview was clicked.

**Fields**
- Info box (collapsible): "Important! Are you adding a game from Epic/GOG/Amazon? Click here!" → explains to log in and use "Import Game" instead (link to `/login`).
- **Game/App Title** (`sideload-title`): max **40** chars; special characters stripped while typing. **On blur** → image search: with a SGDB key opens the picker in grid mode; without a key, HTTP GET `https://steamgrid.usebottles.com/api/search/<title>` (3.5 s timeout) and uses the returned URL as the square image (errors logged via IPC `logError`) (:134-161).
- Collapsible **Images**: **Square Art** URL field with folder icon (local file → `file://<path>`; filters jpg/jpeg/png/webp/gif/avif/All) and **Cover/Hero Art** URL field with folder icon (:163-180, 421-453). Clicking either preview opens the SGDB picker (`grids` for square, `heroes` for cover) when a key exists. Without a key: warning linking to Settings → Advanced.
- (Add mode only) **Platform select + WineSelector** (`{!editMode && children}`, :471).
- Non-Browser platforms: **Select Executable** file chooser (default dir = current wine prefix) with filters per platform (:221-250): Windows `exe, msi` / `bat` / All; Linux `AppImage` / `sh, py, bin` / All; Mac `App` / `sh, py, bin` / All.
- Browser platform: **BrowserURL** (every change is forced to start with `https://` — `handleGameUrl`, :291-297), **Custom User Agent**, toggle **Launch Fullscreen (F11 to exit)**.
- There is no launch-arguments field here; arguments/env for sideloaded apps are set in per-game Settings.

**Footer**
- **Run Installer First** (only add mode, non-Windows host, platform Windows): pick an exe (filters per platform) → IPC `getGameSettings(app_name, 'sideload')` → `writeConfig` with prefix/version → IPC `runWineCommand({commandParts: [exe], wait: true, protonVerb: 'runinprefix', gameSettings})`; button shows "Running Setup" and is disabled while running or when the title is empty (:252-289, 529-539).
- **Finish**: disabled when (no exe and no URL), while adding, or while searching an image (:540-547). On click (:182-219):
  1. IPC `addNewApp(GameInfo{runner:'sideload', app_name, title, install:{executable, platform (existing platform in edit mode)}, art_cover: hero||square||fallback, art_square: square||hero||fallback, is_installed: true, canRunOffline: true, browserUrl, customUserAgent, launchFullScreen})`.
  2. Add mode only: IPC `writeConfig({appName, config: {winePrefix, wineVersion, wineCrossoverBottle}})`.
  3. `refreshLibrary({library:'sideload', runInBackground: true, checkForUpdates: true})`, close.
- Backend `addNewApp` (`src/backend/storeManagers/sideload/library.ts:19-80`): builds the entry (`folder_name = dirname(executable)`, `canRunOffline = !browserUrl`, `is_dlc:false`); on macOS resolves a `.app` bundle to `Contents/MacOS/<first file>`; edit = shallow merge into existing entry; new apps also get desktop/start-menu shortcuts per settings (`addShortcuts`); emits `refreshLibrary('sideload')`.

### Shared UI primitives used by the dialogs

- **PathSelectionBox** (`src/frontend/components/UI/PathSelectionBox/index.tsx`): editable text input that commits on blur; icon button opens a native dialog through IPC `openDialog({buttonLabel, properties: ['openDirectory'|'openFile'], title, filters, defaultPath})` → path or `false`; when `noDeleteButton` is false and a path exists the icon is a "clear" (backspace) button instead. Supports `afterInput` slot, `disabled`, `canEditPath`.
- **Dialog / DialogHeader / DialogContent / DialogFooter**, **ToggleSwitch**, **SelectField**, **TextInputField**, **TextInputWithIconField**, **InfoBox**, **WarningMessage** (`src/frontend/components/UI/*`). Generic dialogs (`showDialogModal`) carry `{title, message, buttons[{text,onClick}], type: 'MESSAGE'|'ERROR', className, onClose}` (`src/frontend/types.ts:132-140`).

### UX pain points (Library & install)

Discoverability / information architecture
- Filtering is spread over four places: the hover-opening **Filters** dropdown, the **Categories** dropdown, the icon-only toolbar (sort, installed-first, alphabet, layout), and **Settings** (top section, max recent games, tile colour, titles always visible). No indication of how many filters are active, and the empty state doesn't say which filter excluded everything.
- Hover-to-open dropdowns (`popUpOnHover`) are easy to trigger by accident and close when the pointer leaves.
- All per-game actions except Play/Install/Settings/Update live in a **right-click-only context menu** (no visible "more" button), with odd ordering (Add to favourites / Categories / Remove from favourites interleaved). "Remove From Recent" appears only on cards in the recent row; "Details" duplicates the card click.
- The library tour is opt-in through a small "?" icon and is never shown on first run.
- The "update available" badge is only displayed when a gamepad is active; with a mouse the only hint is a small repeat icon.
- "Reset" in Filters turns **Show Hidden on**, which isn't the default state.
- Sideload vs. store games use different "Edit" dialogs; edit mode for sideloaded apps can't change platform or Wine, and there's no launch-arguments field in the add flow.
- The info box warning not to sideload store games is collapsed and easy to miss; the import entry point for store games sits in the install dialog footer and on the game page, not in the library.

Inconsistent behaviour
- Search uses two different algorithms: Fuse fuzzy match (threshold 0.4, extended syntax that isn't documented) for the grid, and plain substring match on the **original** title for the dropdown. The dropdown ignores filters and shows hidden games. Fuzzy results are re-sorted alphabetically, so relevance is lost.
- Sorting uses the original title while display, search and the alphabet filter use the override title, so renamed games sort in the "wrong" place. `replace('THE ', '')` strips the first "THE " anywhere in the title, not just a leading article.
- "Favourites only" silently bypasses the installed/category/offline/third-party/updates/non-available filters.
- Identity keys are inconsistent: favourites, hidden, recent, overrides and the non-available list are keyed by `appName` only, while categories use `appName_runner`. The same id in two stores can collide.
- In the favourites row, the alphabetically first favourite gets the big "just played" hero card.
- The platform filter treats every not-installed game as Windows-capable, and "Browser" only matches installed sideloaded browser apps.
- The alphabet filter only knows ASCII letters and digits; titles in other scripts are unreachable through it.
- The install dialog defaults to **Windows** even for Linux-native games on Linux. The header shows icons for every available platform, not the selected one. Branch/build selectors are hidden for the Linux platform.
- Wine version defaults to the first entry of `getAlternativeWine`, not the global default; the Wine section is collapsed, so the prefix choice is easy to miss.
- The Import dialog's path field is labelled "Select Install Path".
- Sideload title is capped at 40 chars and special characters are silently stripped. Browser URL forces `https://` on every keystroke, which mangles `http://` URLs. Leaving the title field triggers an image lookup against a third-party server (`steamgrid.usebottles.com`) without asking.
- Cancelling an **update** from the card calls `kill` (the stop-game handler) instead of the download-cancel flow.
- The anticheat gate checks for an `'Unknown'` status that isn't part of the `AntiCheatInfo.status` type.

Bugs / dead code a rewrite should not copy
- Category manager: adding a name that already exists **wipes that category's members**; renaming onto an existing name overwrites it; names aren't trimmed (the per-game modal does trim and check duplicates). Deleting a category while no category filter is active switches the library to "Uncategorized only". The per-game modal title "Categories" isn't translated.
- `heroic://install` → `installGame` event writes `GlobalState.showInstallModal`, which nothing renders, so the protocol "install" confirmation probably never opens the dialog.
- `hasProgress` reads `${appName}_${runner}_progress`, which is never written, so the resume-percentage merge is dead. Stopped-download state lives under the bare `<appName>` localStorage key.
- `pushGameToLibrary` replaces GOG/ZOOM entries without re-applying metadata overrides, so custom titles/art vanish until the next refresh.
- `refreshLibrary` drops requests while another refresh is running. Completion of an operation can therefore miss its refresh and leave stale install state.
- `getInstallInfo` returning `null` (backend error) leaves the install button spinning forever with "Geting download size…" (typo in the source strings); there's no retry or error message.
- Size accounting: Epic/Amazon DLC sizes aren't added to download/install size or to the disk-space check. All DLCs are preselected by default.
- The non-available filter relies on `nonAvailableGames`, which only gets populated as installed cards scroll into view (one `isGameAvailable` IPC per card). The module-level cache is read once, so the filter is incomplete and can go stale.
- Connectivity starts as `'offline'` until the first status query resolves, so an early launch can show a spurious offline warning.
- The flatpak-sandbox error disables Install with a spinner, so it looks like loading rather than a blocking error.

Performance
- No virtualization: every filtered game mounts a `GameCard` component. Lazy loading only defers inner content, and once rendered a card stays mounted. Every visible card makes at least one `getGameInfo` IPC call, plus `isGameAvailable` for installed games, repeated whenever `libraryStatus` or the Epic/GOG library changes.
- The filtered list is rebuilt from scratch on any change, with O(n·m) `Array.includes` checks (favourites, hidden, categories, updates, non-available) and a new Fuse index per recompute.
- Every finished operation (`done`/`error`) triggers a library refresh for that store. Starting with an empty cache uses a foreground refresh that hides the whole list behind a spinner.
- Each card keeps its own copy of `gameInfo` (refetched on status change) next to the props copy, which leads to stale or duplicated state.

---

## Game page

Route: `/gamepage/:runner/:appName`, with router state `{ gameInfo, fromDM? }` (src/frontend/screens/Game/GamePage/index.tsx:74-81). The page is one screen plus a set of dialogs. Most behaviour hangs off a `GameContext` (src/frontend/screens/Game/GameContext.tsx:5-39) that holds a set of `is.*` booleans built in src/frontend/screens/Game/GamePage/index.tsx:329-364.

Conventions in this section:
- **IPC** names the channel. When the frontend alias differs, it is written `alias → channel` (for example `fetchPlaytimeFromServer → getPlaytimeFromRunner`). "invoke" means a request/response handler. "send" means fire-and-forget (`makeListenerCaller`).
- **Runners**: `legendary` (Epic), `gog`, `nile` (Amazon), `zoom`, `sideload`.
- **Paths** are relative to the repo root.

### Data loaded by the page (lifecycle)

| Data | When | IPC | Ref |
|---|---|---|---|
| Game info refresh | every change of `status`, `gog.library`, `epic.library`, `isMoving` | `getGameInfo` (invoke), then `getExtraInfo` (invoke) | src/frontend/screens/Game/GamePage/index.tsx:185-196 |
| Install info (download/disk size, manifest, DLC list, builds, branches) | on status/library/gameInfo/settings-modal/offline change; **skipped** for sideload, third-party, not-installable, or offline | `getInstallInfo(appName, runner, platform, build?, branch?)` (invoke). The backend's positional order is `build, branch` (src/backend/main.ts:756-776), while the type in src/common/types/ipc.ts:192-198 names them `branch, build`. Platform is normalised: non-legendary `Windows→windows`, `Mac→osx` (src/frontend/helpers/index.ts:88-103) | src/frontend/screens/Game/GamePage/index.tsx:198-256 |
| Per-game settings | same effect as install info | `requestGameSettings` (invoke) | src/frontend/screens/Game/GamePage/index.tsx:239-245 |
| Settings context (used by launch-option selector and cloud-save widget) | on mount, language change, settings-modal open/close | `requestGameSettings` / `setSetting` (send) | src/frontend/hooks/useSettingsContext.ts:31-51 |
| Wiki info (PCGamingWiki, AppleGamingWiki, HLTB, GamesDB, Steam/ProtonDB/Deck) | once per appName | `getWikiGameInfo(title, appName, runner)` (invoke). Called **again** by the dots menu to build the ProtonDB URL | src/frontend/screens/Game/GamePage/index.tsx:258-267; src/frontend/screens/Game/GameSubMenu/index.tsx:250-258 |
| Achievements | on mount and when `isPlaying` flips. Cache is cleared when the game stops | `getAchievements(appName, runner, lang?)` (invoke), `clearAchievementCache` (send) | src/frontend/screens/Game/GamePage/index.tsx:173-183 |
| Anticheat info | when gameInfo has a `namespace` and is not sideload | `getAnticheatInfo(namespace)` (invoke) | src/frontend/hooks/hasAnticheatInfo.ts:4-22 |
| Known fixes | once | `getKnownFixes(appName, runner)` (invoke) | src/frontend/hooks/hasKnownFixes.ts:4-6 |
| Status label / availability | on libraryStatus/library/progress change | `isGameAvailable({appName, runner})` (invoke). The result is cached in localStorage `nonAvailableGames` | src/frontend/hooks/hasStatus.ts:48-116; src/frontend/hooks/constants.ts:55-79 |
| Install progress | push | `progressUpdate` (frontend listener, `onProgressUpdate`), stored in zustand keyed `${appName}_${runner}` | src/frontend/state/InstallProgress.ts:10-13 |
| Game status | push | `gameStatusUpdate` (frontend listener, `handleGameStatus`) into `libraryStatus` | src/frontend/state/GlobalState.tsx:799-873 |
| Playtime | on status change | `fetchPlaytimeFromServer → getPlaytimeFromRunner(runner, appName)` (invoke). Only GOG returns a value, and only if the global `disablePlaytimeSync` is off (src/backend/main.ts:1395-1408) | src/frontend/screens/Game/TimeContainer/index.tsx:24-50 |
| Launch options | on mount | `getLaunchOptions(appName, runner)` (invoke) | src/frontend/hooks/useLaunchOptions.ts:23-40 |
| Steam / desktop shortcut / EOS overlay state | on mount / when `isInstalled` changes | `isAddedToSteam`, `shortcutsExists`, `isEosOverlayEnabled` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:220-248 |

Error handling: if `getInstallInfo` throws or returns null, or `requestGameSettings` throws, the **whole page is replaced** by `ErrorComponent` with the error text (src/frontend/screens/Game/GamePage/index.tsx:232-236, 242-245, 312-323). While `gameInfo.install` or settings context is missing, a full-page spinner (`UpdateComponent`) is shown (src/frontend/screens/Game/GamePage/index.tsx:292, 579).

Derived flags (src/frontend/screens/Game/GamePage/index.tsx:105-165):
- `notAvailable` = installed, but `isGameAvailable` returned false (for example, the install folder is missing).
- `notSupportedGame` = non-sideload, `thirdPartyManagedApp` set, and not EA- or Ubisoft-managed.
- `notInstallable` = install info has `disk_size === 0 && download_size === 0`, as with GOG "goodies" (src/frontend/screens/Game/GamePage/index.tsx:222-229).
- `notPlayableOffline` = connectivity is not `online` and `!gameInfo.canRunOffline`.
- `isBrowserGame` = `install.platform === 'Browser'`.
- `hasUpdate` = installed and `appName ∈ gameUpdates`. That list is filled by `checkGameUpdates` (invoke) during library refresh (src/frontend/state/GlobalState.tsx:700-767) and persisted to localStorage `updates`.
- Native flags: `macNative` (install platform `osx`/`Mac`), `linuxNative` (`linux`/`Linux`), and `native` = win, macNative or linuxNative (src/frontend/screens/Game/GamePage/index.tsx:325-347).

### Page layout (functional inventory)

Top row (src/frontend/screens/Game/GamePage/index.tsx:413-428):
- **Back** goes to `/download-manager` if the page was opened from the download manager (`fromDM`), otherwise to `/library` (src/frontend/screens/Game/GamePage/index.tsx:165).
- **Settings (gear)** opens the per-game settings modal. It is hidden when the game is not installed or is a browser game (src/frontend/screens/Game/GamePage/components/SettingsButton.tsx:10-25).
- **Dots menu** (the `GameSubMenu`, revealed on hover or focus) is covered in its own subsection below.

Main info column (src/frontend/screens/Game/GamePage/index.tsx:429-472), in order:
1. **Art**: cover art (`overrides.art_cover || art_cover`) with a fallback image. Legendary covers get CDN resize params `?h=800&resize=1&w=600`. If `art_logo` exists it is overlaid and the text `<h1>` title is made transparent. The background image is `art_background || art_cover` (src/frontend/screens/Game/GamePicture/index.tsx:20-51; src/frontend/screens/Game/GamePage/index.tsx:393-398, 440).
2. **Store logo** for the runner (src/frontend/screens/Game/GamePage/index.tsx:436-438).
3. **Title**: `overrides.title || title`.
4. **Genres**: `extra.genres`, else PCGamingWiki genres, rendered as chips (src/frontend/screens/Game/GamePage/components/Genres.tsx:7-21).
5. **Developer**: hidden for sideload (src/frontend/screens/Game/GamePage/components/Developer.tsx:9-17).
6. **Release date**: locale-formatted. Uses the runner's `extra.releaseDate` first, otherwise picks from the PCGamingWiki `releaseDate[]` entries (`"Windows: …"`, `"Linux: …"`, `"OS X: …"`), preferring the native platform entry. **Quirk:** nothing renders if the wiki date array is empty, even when the runner date exists (src/frontend/screens/Game/GamePage/components/ReleaseDate.tsx:34-75).
7. **Description**: `about.shortDescription || about.description || "No description available"`. Empty for sideload (src/frontend/screens/Game/GamePage/components/Description.tsx:5-19).
8. **TimeContainer** (playtime). Hidden when `notInstallable`. See its own subsection.
9. **GameStatus** line plus progress bar. See the status table.
10. **Launch option selector**. Shown only when there is more than one option (src/frontend/screens/Settings/components/LaunchOptionSelector.tsx:49-51).
11. **Main button(s)**. See the next subsection.
12. **Known-fixes wiki notice**: "Important information about this game, read this: Open page", shown when `knownFixes.wikiLink` is set (src/frontend/screens/Game/GamePage/index.tsx:375-388).

Extra-info tabs (src/frontend/screens/Game/GamePage/index.tsx:473-565):
- **Install info** (always): DownloadSizeInfo, InstalledInfo and CloudSavesSync.
- **Achievements · N%** (only if achievements are non-empty). N = unlocked / total, rounded (src/frontend/screens/Game/GamePage/index.tsx:113-121).
- **Extra info** (only if any of AppleGamingWiki, HLTB, Metacritic score, OpenCritic score or steamInfo exists): Scores, HLTB, CompatibilityInfo and AppleWikiInfo. An IGDB score alone does **not** enable the tab (src/frontend/screens/Game/GamePage/index.tsx:366-371).
- **Requirements** (only if `extra.reqs.length > 0`).

Below the tabs: the **Anticheat** box (src/frontend/screens/Game/GamePage/index.tsx:566) and the **"Report a problem running this game"** link (src/frontend/screens/Game/GamePage/index.tsx:568).

Global help: the page registers a help item, "Game Page — Show all game details and actions. Use the 3 dots menu for more options." (src/frontend/screens/Game/GamePage/index.tsx:91-100; src/frontend/hooks/hasHelp.ts:4-21).

### Main button: all states

Source: src/frontend/screens/Game/GamePage/components/MainButton.tsx:28-256. Handlers: `handlePlay` and `handleInstall` in src/frontend/screens/Game/GamePage/index.tsx:581-623.

There are two button groups.
- **Play group**: rendered when `is_installed && !queued && !uninstalling` (src/frontend/screens/Game/GamePage/components/MainButton.tsx:175).
- **Install group**: rendered when `!is_installed || queued` (src/frontend/screens/Game/GamePage/components/MainButton.tsx:205).

`disabledPlayButtons` = repairing, moving, updating, uninstalling, syncing, launching, installingWinetricksPackages or installingRedist (src/frontend/screens/Game/GamePage/components/MainButton.tsx:34-42).

`disabledInstallButtons` = playing, updating, repairing, moving, uninstalling, notSupportedGame, notInstallable or importing (src/frontend/screens/Game/GamePage/components/MainButton.tsx:44-52).

| Condition (first match wins) | Label | Enabled | Click action | IPC |
|---|---|---|---|---|
| installed, `syncing-saves` | ☁ "Syncing Saves" | no | — | — |
| installed, `redist` | "Installing Redistributables" | no | — | — |
| installed, `winetricks` | "Installing Winetricks Packages" | no | — | — |
| installed, `launching` (or the local `playClicked` flag, which is set on click and cleared when status reaches `launching`) | "Launching" | no | — | — |
| installed, `playing` | ■ "Playing (Stop)" | yes | kill the game | `kill(appName, runner)` (invoke). The backend aborts the process and calls `game.stop()` (src/backend/main.ts:1090-1093) |
| installed, idle, global/game setting `verboseLogs` = true | ▶ "Play Now (with logs)" | yes, unless one of the disabled states above applies | launch flow | see the Launch flow subsection |
| installed, idle, `verboseLogs` = false | ▶ "Play Now" | same | launch flow | same |
| installed, `updating` | Play label, `is-disabled` style | no | — (`handlePlay` would kill, but the button is disabled) | — |
| installed, `notAvailable` | Play label, tertiary style | **yes** | launch. The backend detects the missing folder and asks "Game folder appears to be deleted, do you want to remove the game from the installed list?", then force-uninstalls on Yes (src/backend/launcher.ts:110-123; src/backend/utils.ts:282-298) | `launch` |
| **Alt-play** split arrow next to Play, revealed on hover or focus (src/frontend/screens/Game/GamePage/index.css:285-330). Hidden when the play buttons are disabled | the opposite of the main label: "Play Now" or "Play Now (with logs)" | yes | **permanently toggles** the `verboseLogs` setting, then launches (src/frontend/screens/Game/GamePage/components/MainButton.tsx:99-119, 168-171) | `setSetting` (send) and `launch` |
| not installed, `notInstallable` | ⚠ "Not installable" | no | — | — |
| not installed, `notSupportedGame` | ⚠ "Not supported" | no | — | — |
| `queued` (installed or not) | ✖ "Remove from Queue" | yes (unless one of the disabled-install states applies) | remove from download queue and clear the localStorage key `appName` | `removeFromDMQueue(appName)` (send) |
| not installed, `installing` | ⏸ "Pause/Cancel" | yes | Stop Installation dialog: "Do you wish to KEEP downloaded files?" with buttons **Keep Installing** / **YES** (keep files: store resume progress in localStorage, then `cancelDownload(false)`) / **NO** (`cancelDownload(true)`, clear storage) (src/frontend/helpers/library.ts:102-134) | `cancelDownload(removeDownloaded)` (send) |
| not installed, idle | ⬇ "Install" | yes | opens the Install modal (`action: 'install'`) (src/frontend/screens/Game/GamePage/components/MainButton.tsx:208-217) | — (the modal later calls `install`) |
| not installed (secondary) | "Import Game" | disabled when an install-button disable applies, or when installing or importing | opens the Install modal in `action: 'import'` mode | — (the modal calls `importGame`) |

Notes:
- **Install path from the page**: `handleInstall` → `install()` → `window.api.install({appName, path, installDlcs, sdlList, installLanguage, runner, platformToInstall, gameInfo, build, branch})`. This queues into the download manager. For legendary, each `installDlcs` entry is queued as its own element (src/frontend/helpers/library.ts:34-100; src/backend/downloadmanager/ipc_handler.ts:13-44). An install path of `'default'` resolves through `requestAppSettings().defaultInstallPath`. If the path differs from the previously stored partial-progress folder, the stored resume progress is dropped.
- **Sideload** is never "installed" through this page (`handleInstall` returns early, src/frontend/screens/Game/GamePage/index.tsx:612). Its uninstall navigates back to the library (src/frontend/components/UI/UninstallModal/index.tsx:99-101).
- **Browser games** (`install.platform === 'Browser'`, sideloaded web apps) use the same Play button. The backend opens an Electron `BrowserWindow` with `browserUrl`, `customUserAgent` and `launchFullScreen` (src/backend/storeManagers/storeManagerCommon/games.ts:41-129). The settings gear, Report-issue link and most InstalledInfo fields are hidden for them.
- **Third-party managed games** (EA app / Ubisoft): `isEAManaged` and `isUbisoftManaged` count as supported, so Play and Install work. Any other `thirdPartyManagedApp` gets "Not supported".
- **No cancel-update control on the page.** While `updating`, the play button is disabled, and the progress link only exists for `installing` (see GameStatus).

### Launch flow (Play)

Source: src/frontend/helpers/library.ts:150-462.
1. **Offline guard.** If `notPlayableOffline`, show "Offline Warning — This game might not work properly offline. Do you want to play anyway?" with buttons **OK** (abort) / **Play Anyway** (src/frontend/helpers/library.ts:236-263).
2. **Update guard.** If `hasUpdate`, read `requestGameSettings(appName).ignoreGameUpdates`. If true, skip ahead. Otherwise show "Game Needs Update — This game has an update, apply update now?":
   - **YES** → `updateGame` (invoke, queues an update).
   - **NO** → launch with `skipVersionCheck: true` (src/frontend/helpers/library.ts:162-222).
3. **Launch options.**
   - If `launchArguments` were given → `launch`.
   - Otherwise fetch `getLaunchOptions`. With 0 or 1 options, launch directly (passing the single option).
   - With several options and a saved `lastUsedLaunchOption` in game settings, launch with it.
   - Otherwise show the "Select Launch Option ({{seconds}}s)" dialog: one button per option and a **10-second countdown that auto-selects the first option**. The choice is saved via `setSetting(lastUsedLaunchOption)`. Closing the dialog cancels the launch (src/frontend/helpers/library.ts:268-462).
4. `launch({appName, runner, launchArguments, args, skipVersionCheck})` (invoke) returns `{status: 'done'|'error'|'abort'}`. Backend side effects the UI must reflect:
   - missing install path → force-uninstall prompt;
   - `autoSyncSaves` and online → status `syncing-saves` (download saves), then `launching`, then `playing`; after exit, `syncing-saves` (upload) and finally `done` (src/backend/launcher.ts:101-320);
   - known-fix winetricks/runInPrefix → status `winetricks`; redistributables → status `redist` with `context` (src/backend/launcher.ts:989-1003);
   - first/last played timestamps are written to the `timestamp` store (src/backend/launcher.ts:140-142).

### Launch options / alternative executables selector

- **Component**: `LaunchOptionSelector showTitle={false}` (src/frontend/screens/Game/GamePage/index.tsx:462; src/frontend/screens/Settings/components/LaunchOptionSelector.tsx:10-78). It is a dropdown with the placeholder "Launch Options..." while nothing is selected. It is hidden when there is one option or fewer.
- **Persistence**: the selection is saved to per-game setting `lastUsedLaunchOption` (via `setSetting`). The previously used option is matched by type plus name and params, or dlcAppName, or executable (src/frontend/hooks/useLaunchOptions.ts:98-136).
- **Labels**:
  - `basic` with name `Default` and empty params → "Default";
  - `basic` → name;
  - `dlc` → dlcTitle;
  - `altExe` → executable path (src/frontend/hooks/useLaunchOptions.ts:53-77).
- **Per-store option sources**:
  - legendary: the manifest's `launch_options`, plus installed DLCs that are executable (`type:'dlc'`), plus DLCs with an `AdditionalCommandLine` (`basic`) (src/backend/storeManagers/legendary/library.ts:878-932);
  - gog: visible non-document `playTasks`, as `--prefer-task N`, but only if there are 2 or more (src/backend/storeManagers/gog/library.ts:1503-1523);
  - nile, zoom, sideload: none.
  - The backend prepends a "Default" option whenever any options exist (src/backend/main.ts:1111-1134).
  - `altExe` options are only produced by `heroic://` protocol launches (src/backend/protocol.ts:70-122). There is no UI to create them.

### GameStatus line, progress and ETA

Source: src/frontend/screens/Game/GamePage/components/GameStatus.tsx:16-153. A determinate progress bar shows while `installing` or `updating`. While installing, the status text is a link to `/download-manager`. Text colour is green while installing, otherwise warning-coloured.

The progress object is `InstallProgress {bytes, eta, folder?, percent?, downSpeed?, diskSpeed?, file?}` (src/common/types.ts:306-314). `hasProgress` rescales percent when resuming. The previous partial percent is read from localStorage `${appName}_${runner}_progress`, and new = cur/100·(100−stored)+stored (src/frontend/hooks/hasProgress.ts:7-59).

The "current progress" string is `"{percent}% [{bytes}] ETA: {eta}"`. It becomes `"..."` when percent or bytes are missing, and empty once ≥99%. `downSpeed` and `diskSpeed` are **not shown** on the game page.

### GameStatus values: UI and available actions

`Status` union (src/common/types.ts:274-294). The frontend keeps `libraryStatus: GameStatus[]` keyed by **appName only**. `done` and `error` remove the entry (src/frontend/state/GlobalState.tsx:799-873). `hasStatus` synthesises `notSupportedGame`, `notAvailable`, `installed` and `notInstalled` when there is no live entry (src/frontend/hooks/hasStatus.ts:48-107).

| Status | Emitted by | GameStatus text on page | Main button | Dots-menu / other actions |
|---|---|---|---|---|
| `notInstalled` (synth) | hasStatus | "This game is not installed" | Install + Import Game | Add/Remove from Steam, Categories, Store page, Changelog, Check Compatibility (Linux), Requirements. No install-only items |
| `installed` (synth) | hasStatus | "Installed". With an update: "Installed - New Version Available! (Click to Update)", clickable → `updateGame` (src/frontend/screens/Game/GamePage/components/GameStatus.tsx:104-117) | Play / Play (with logs) + alt-play | All installed items (see the dots menu) |
| `notAvailable` (synth) | `isGameAvailable` = false | "Game not available" | Play (tertiary, enabled) | Installed items |
| `notSupportedGame` (synth) | third-party, not EA/Ubisoft | "This game uses third party launcher and it is not supported yet" | "Not supported" (disabled) | Store/Steam/Categories |
| `queued` | download queue (src/backend/downloadmanager/downloadqueue.ts:131) | "Queued" | "Remove from Queue". Import Game is also shown (enabled even when the game is installed with a queued update) | Installed items if installed; Force Update is disabled only for installing/updating |
| `installing` | download manager (src/backend/downloadmanager/utils.ts:62) | link: "Installing {p}% [{bytes}] ETA: {eta}" or "Processing files, please wait..." | "Pause/Cancel" → stop dialog | Steam/Categories/Store. Uninstall of other **Epic** games is blocked while an Epic install runs (src/frontend/components/UI/UninstallModal/index.tsx:107-131) |
| `importing` | `importGame` (src/backend/main.ts:1037) | "Importing {progress}" | Install and Import disabled | — |
| `updating` | download manager (src/backend/downloadmanager/utils.ts:141; legendary src/backend/storeManagers/legendary/games.ts:496) | "Updating Game {progress}". If eta contains "verifying": "Repairing Game, please wait: {p} [{bytes}]". At ≥99%: "Processing files…" | Play disabled. **No cancel** | Force Update disabled |
| `launching` | launcher | (installed text) | "Launching" (disabled) | — |
| `playing` | store `launch()` (for example src/backend/storeManagers/storeManagerCommon/games.ts:197) | (installed text) | "Playing (Stop)" → `kill` | Uninstall disabled. The uninstall modal also refuses with "{{title}} is running. Close the game to uninstall it." |
| `syncing-saves` | launcher before/after play | (installed text) | "Syncing Saves" (disabled) | — |
| `winetricks` | launcher (known fixes) | **no special text** (falls through to Installed) | "Installing Winetricks Packages" (disabled) | — |
| `redist` | launcher / nile setup (src/backend/storeManagers/nile/setup.ts:93), `context` = redist name | "Installing Redistributables ({{redist}})" | "Installing Redistributables" (disabled) | — |
| `repairing` | `repair` handler (src/backend/main.ts:898-931) | "Repairing Game, please wait {p}%" or "..." | Play disabled | — |
| `moving` | `moveInstall` (src/backend/main.ts:933-999) and per-file progress (src/backend/utils.ts:1072, 1178) | "Moving file '{{file}}': {{percent}}%" or "Moving Installation, please wait ..." | Play disabled | — |
| `uninstalling` | uninstaller (src/backend/utils/uninstaller.ts:92) | "Uninstalling" | **No main button at all** (both groups hidden) | — |
| `extracting` | **never emitted by the backend** (only listed in frontend arrays such as src/frontend/state/GlobalState.tsx:836) | no page-specific text | if not installed, Install is shown (no `is.extracting` flag) | — |
| `done` / `error` | all operations | removes the status entry and refreshes the runner's library (src/frontend/state/GlobalState.tsx:855-872) | — | — |
| `canceled` | in the type only; unused | — | — | — |

In addition, the frontend calls `lock(playing)` / `unlock` (send) to block app quit while any of installing, updating, launching, playing, redist, winetricks, extracting, repairing, moving, syncing-saves or uninstalling is active (src/frontend/state/GlobalState.tsx:1056-1081).

### Update button / ignore updates

- **Click-to-update status text** when `hasUpdate` → `updateGame({appName, runner, gameInfo})` (invoke). The backend queues a DM element of type `update` with `path = install.install_path` and `platformToInstall = install.platform` (src/frontend/screens/Game/GamePage/index.tsx:275-278; src/backend/downloadmanager/ipc_handler.ts:46-62). Sideload is excluded.
- **"Force Update if Available"** dots item calls the same `updateGame`, even when no update is known. It is hidden for sideload and third-party games, and disabled while installing or updating (src/frontend/screens/Game/GameSubMenu/index.tsx:329-338).
- **Play-time update prompt** is covered in Launch flow step 2. It is skipped when per-game `ignoreGameUpdates` is true. That setting lives in the per-game settings modal, not on this page.
- **Zoom and sideload** `update()` are not implemented on the backend (they return error) (src/backend/storeManagers/zoom/games.ts:791-794; src/backend/storeManagers/sideload/games.ts:229-231).

### Dots menu (GameSubMenu)

Source: src/frontend/screens/Game/GamePage/components/DotsMenu.tsx:22-94 and src/frontend/screens/Game/GameSubMenu/index.tsx:55-476. Hover/focus reveal: src/frontend/screens/Game/GamePage/index.css:589-660.

Items in render order, with visibility rules:

| Item | Visible when | Action | IPC | Ref |
|---|---|---|---|---|
| **Edit Game** / **Edit App/Game** (sideload) | installed | Non-sideload: opens the EditGameDialog (see below) in the global dialog. Sideload: opens the Install modal (sideload editor) | see Edit | src/frontend/screens/Game/GameSubMenu/index.tsx:169-185, 303-311 |
| **Add shortcut** / **Remove shortcuts** | installed | toggles desktop and start-menu (Linux/Windows) or Applications-folder (macOS) shortcuts. The backend shows a notification | `addShortcut(appName, runner, fromMenu=true)` (send), `removeShortcut` (send). State comes from `shortcutsExists` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:159-167, 312-320; src/backend/shortcuts/ipc_handler.ts:14-57 |
| **Uninstall** | installed. Disabled while playing | opens the UninstallModal | see Uninstall | src/frontend/screens/Game/GameSubMenu/index.tsx:321-328 |
| **Force Update if Available** | installed, not sideload, not third-party. Disabled while installing or updating | `updateGame` | `updateGame` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:329-338 |
| **Move Game** | installed, not sideload, not third-party | confirm "Move Game Installation — This can take a long time, are you sure?" → directory picker (default = `defaultInstallPath`) → move | `requestAppSettings` (invoke), `openDialog({properties:['openDirectory']})` (invoke), `moveInstall({appName, path, runner})` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:92-115, 339-347 |
| **Change Install Location** (label "Change install path") | installed, not sideload, not third-party | confirm "Change Games Install Path — This will change the install location of this game, are you sure?" → directory picker → re-point the install **without moving files** → refresh the library | `openDialog`, `changeInstallPath({appName, path, runner})` (invoke), `refreshLibrary` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:117-141, 348-356; src/backend/main.ts:1095-1101 |
| **Verify and Repair** | installed, not sideload, not third-party | confirm "Verify and Repair — Do you want to try to repair this game? This can take a long time." → repair | `repair(appName, runner)` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:143-157, 357-365; src/frontend/helpers/library.ts:136-137 |
| **Enable/Disable EOS Overlay** | **Linux only**, legendary, installed. A spinner shows while toggling or while the overlay app (`98bc04bc842e4906993fd6d6644ffb8d`) is `installing` | Enable → `enableEosOverlay(appName)`. If the result is `installNow`, call `installEosOverlay()` (downloads the overlay, **with no confirmation**) and retry the enable. Disable → `disableEosOverlay` | `enableEosOverlay`, `installEosOverlay`, `disableEosOverlay`, `isEosOverlayEnabled` (invoke) | src/frontend/screens/Game/GameSubMenu/index.tsx:187-204, 235-247, 366-380 |
| **Add to Steam** / **Remove from Steam** | **always**, even for not-installed games and sideload. Spinner while working | adds or removes a non-Steam shortcut in Steam | `addToSteam`, `removeFromSteam`, `isAddedToSteam` (invoke, each returns a boolean) | src/frontend/screens/Game/GameSubMenu/index.tsx:206-218, 383-397; src/backend/shortcuts/ipc_handler.ts:59-72 |
| **Categories** | always | opens the settings modal of type `category` (custom categories editor) | — | src/frontend/screens/Game/GameSubMenu/index.tsx:398-404; src/frontend/state/GlobalStateV2.ts:68-76 |
| **Store Page** | not sideload and a `storeUrl` exists (`extra.storeUrl` or `gameInfo.store_url`) | navigates in-app to `/store-page?store-url=<url>` (webview) | — | src/frontend/screens/Game/GameSubMenu/index.tsx:405-413; src/frontend/screens/Game/GamePage/components/DotsMenu.tsx:45-50 |
| **Show Changelog** | not sideload and `extra.changelog` is non-empty | opens the GameChangeLog dialog: "Changelog for {{gameTitle}}", HTML-sanitized (`sanitize-html`, disallowed tags discarded) | — | src/frontend/screens/Game/GameSubMenu/index.tsx:414-422; src/frontend/screens/Game/GameChangeLog/index.tsx:16-44 |
| **Check Compatibility** (ProtonDB) | **Linux only**, not sideload | opens a new window at `protondb.com/app/<steamID>` (from PCGamingWiki or GamesDB) or `protondb.com/search?q=<title>` | `createNewWindow(url)` (send) | src/frontend/screens/Game/GameSubMenu/index.tsx:85-87, 250-258, 423-433 |
| **System Requirements** | `extra.reqs.length > 0` (duplicates the Requirements tab) | dialog with the requirements table | — | src/frontend/screens/Game/GamePage/components/DotsMenu.tsx:56-72 |
| **Modify Installation** | runner ∈ {legendary, gog}, installed, not third-party | opens the ModifyInstallModal | see Modify | src/frontend/screens/Game/GameSubMenu/index.tsx:264-268, 443-451 |
| **Browse Files** | installed | opens `install.install_path \|\| folder_name` in the OS file manager | `openFolder(path)` (send) | src/frontend/screens/Game/GameSubMenu/index.tsx:273-279, 452-460 |
| **Browse Wine Prefix** | not Windows, not native, and wine type is not `crossover` | opens `gameSettings.winePrefix` | `openFolder` (send) | src/frontend/screens/Game/GameSubMenu/index.tsx:270-287, 461-471 |

Store and platform availability, summarised from the rules above plus backend stubs:

| Capability | legendary | gog | nile | zoom | sideload | Platform limits |
|---|---|---|---|---|---|---|
| Move | ✓ | ✓ | ✓ | backend returns error "Move install not implemented" (src/backend/storeManagers/zoom/games.ts:744-752) but the item is shown | hidden | — |
| Change install path | ✓ | ✓ | ✓ | ✓ (only with the experimental zoom flag) | hidden | — |
| Verify/Repair | ✓ | ✓ | ✓ | no-op stub (src/backend/storeManagers/zoom/games.ts:754-757) but the item is shown | hidden | backend skips silently when offline (src/backend/main.ts:898-905) |
| Modify install | DLC only | branch/build/lang/DLC/REDmod | ✗ | ✗ | ✗ | GOG build/branch changes ignored when install platform is `linux` |
| EOS overlay | ✓ | ✗ | ✗ | ✗ | ✗ | Linux only |
| Cloud saves | ✓ (if `cloud_save_enabled`) | ✓ | stub ("Amazon Games doesn't support cloud saves", src/backend/storeManagers/nile/games.ts:490-493) | `cloud_save_enabled:false` | hidden | Linux-native installs show "Unsupported" |
| Achievements | ✗ | ✓ (only `gog/games.ts` implements `getAchievements`) | ✗ | ✗ | ✗ | — |
| Playtime sync | ✗ | ✓ | ✗ | ✗ | ✗ | — |
| Launch options | ✓ | ✓ (≥2 tasks) | ✗ | ✗ | ✗ | — |
| Version pin | ✗ | ✓ (via Modify) | ✗ | backend implemented, no UI | ✗ | — |
| Anticheat box | needs `namespace` (Epic) | if namespace | if namespace | if namespace | ✗ | AWACY link on Linux |

### Verify / repair

Covered in the dots table. Backend sequence: offline → returns without UI feedback. Otherwise status `repairing` → `game.repair()` → notifications "Error Repairing" or "finished repairing" → `done` (src/backend/main.ts:898-931). Progress text: "Repairing Game, please wait {percent}%" (src/frontend/screens/Game/GamePage/components/GameStatus.tsx:55-57).

### Move install / change install location

Move backend sequence (src/backend/main.ts:933-999):
1. status `moving`, notification "Moving Game".
2. Rejects a destination inside the current install path, with the error dialog "Error Moving Game {{error}}".
3. Per-file progress arrives via status `moving` with `progress.file` and `percent`.
4. Notifications "moved" or "Error Moving Game", then `done`.

Change install path only re-points the install record (no file copy), then the frontend refreshes the library.

### Uninstall dialog

Source: src/frontend/components/UI/UninstallModal/index.tsx:22-233. The same component serves the game page, the dots menu, library cards and DLC rows (`isDlc`).

On open it calls:
- `getGameInfo` (invoke) for the title;
- `isNative({appName, runner})` (invoke);
- for Windows-platform installs, `getGameSettings(appName, runner)` (invoke) and `requestGameSettings('default')` (invoke) to learn the prefix and whether it equals the default prefix.

Variants:
- **An Epic game is installing and this is a legendary game**: dialog "Epic games cannot be uninstalled while another Epic game is being installed." with **Close**.
- **Game running**: "{{title}} is running. Close the game to uninstall it." with **Close**.
- **Normal**: "Do you want to uninstall "{{title}}"?", or for a DLC "…(DLC)?". Options:
  - **Remove prefix: {{prefix}}** (+ "This can't be undone and will also remove not backed up save files."). Shown only when not native and not a DLC. Disabled, with the explanation "The Wine prefix for this game is the default prefix… delete it manually", when the prefix is the default.
  - **Erase settings and remove log** (+ "Any modified settings will be forgotten and log will be deleted."). Not shown for DLC.
  - Buttons **YES** / **NO**.
- On YES it calls `uninstall(appName, runner, removePrefix, removeSettings)` (invoke) and clears the localStorage `appName` key. For sideload on the game page it navigates to the library.

### Modify installation dialog

Container: "Modify Installation" dialog. It shows a spinner until the page's `gameInstallInfo` is loaded, so it is unusable offline (src/frontend/screens/Game/ModifyInstallModal/index.tsx:29-65).

**Legendary (Epic): DLC manager.** `DLCList` over `installInfo.game.owned_dlc` (src/frontend/screens/Game/ModifyInstallModal/Legendary/index.tsx:11-24; src/frontend/components/UI/DLCList/index.tsx:15-46; src/frontend/components/UI/DLCList/DLC/index.tsx:25-140):
- Header columns: Title / Size / Actions. Empty state: "No DLCs found".
- Each row:
  - title;
  - size = `download_size` from `getInstallInfo(dlcAppName, runner, mainPlatform)`, shown as "..." with a spinner while loading;
  - action button. Not installed: **Install** closes the modal and calls `install()` with the main game's `install_path` and platform, which queues it. Installed: **Uninstall** opens the UninstallModal with `isDlc`. While the DLC is `installing`: **Cancel** → stop-install dialog.
- Installed-state lookup is `getGameInfo(dlcAppName, runner)`.
- **SDL (selective download) is not editable after install.** SDL appears only in the Install modal (`getGameSdl`).

**GOG** (src/frontend/screens/Game/ModifyInstallModal/GOG/index.tsx:28-424). It loads its own `getInstallInfo(appName,'gog', platform, undefined, branch)`, reloaded on branch or password change, and `getPrivateBranchPassword(appName)` (invoke). Tabs:
- **Game Version**:
  - *Beta channel select* ("Select beta channel"). Options: each branch (`null` → "Disabled") plus "Set private channel password". That option opens a password dialog with **Cancel** / **OK** → `setPrivateBranchPassword(appName, pw)` (invoke), then the branch list refetches (src/frontend/screens/Library/components/InstallModal/DownloadDialog/BranchSelector.tsx:16-111).
  - *Language select*. Languages come from the manifest, shown with `Intl.DisplayNames`. If the current language is unavailable it falls back to the preferred UI language. Disabled when only one language exists (src/frontend/screens/Library/components/InstallModal/DownloadDialog/GameLanguageSelector.tsx:14-55).
  - *Version pin*. Toggle "Keep the game at specific version", then a "Select game version" dropdown ("Version {name} - {date}") of builds for the current branch, sorted newest first. It is disabled when only one build exists (src/frontend/screens/Library/components/InstallModal/DownloadDialog/BuildSelector.tsx:12-63).
- **DLC**: "Install all DLCs" toggle plus a toggle per owned DLC. Empty state: "No DLC available" (src/frontend/screens/Library/components/InstallModal/DownloadDialog/DLCDownloadListing.tsx:12-83).
- **REDmod Integration** (Cyberpunk only). Visible when installed DLC `1597316373` is present:
  - "Enable mods" toggle;
  - drag-and-drop reorderable list of detected mods (`getAvailableCyberpunkMods` invoke), each with an enable toggle. **At least one mod must stay enabled.** List order is load order (later overrides earlier);
  - help InfoBox.
- **Apply** (src/frontend/screens/Game/ModifyInstallModal/GOG/index.tsx:68-129):
  - if REDmod is installed → `setCyberpunModConfig → setCyberpunkModConfig({enabled, modsToLoad})` (invoke);
  - if build or branch changed (not linux-native), or language changed, or the DLC set changed → `updateGame({…, installDlcs, installLanguage, branch, build})`, which queues an update;
  - **always** → `changeGameVersionPinnedStatus(appName, runner, pinned)` (send);
  - then closes. There is no size or diff preview and no confirmation.

### Cloud save sync (game page widget)

Source: src/frontend/screens/Game/GamePage/components/CloudSavesSync.tsx:18-250. Shown in the Install info tab only when the game is installed, not sideload, and `cloud_save_enabled`.
- **Linux-native install**: red "☁̸ Sync Saves: Unsupported" with an info tooltip ("This game does not support cloud saves…"). This text is misleading, because the actual reason is the native platform (src/frontend/screens/Game/GamePage/components/CloudSavesSync.tsx:50, 229-247).
- **Otherwise**: "Sync Saves: Enabled/Disabled", reflecting per-game `autoSyncSaves` and coloured when enabled. While a manual sync runs: "Syncing… please wait".
- **Quick menu** opens on hover, and only if per-game setting `enableQuickSavesMenu` is true. That setting is toggled in the settings modal's Sync Saves section (src/frontend/screens/Settings/sections/SyncSaves/index.tsx:20-42). Items:
  - **Download** (`--skip-upload`);
  - **Upload** (`--skip-download`);
  - **Force download** (`--force-download`), confirm "Local saves will be erased. Do you want to proceed?";
  - **Force upload** (`--force-upload`), confirm "Cloud saves will be overwritten. Do you want to proceed?";
  - divider;
  - **Open Saves Folder** → `openFolder(savesPath)`, or for GOG `openFolder` for each `gogSaves[].location`;
  - **Auto sync** toggle → `setSetting(autoSyncSaves)`.
  - All items are disabled when no save path is configured (legendary `savesPath === ''`; GOG no location) or while syncing.
- **IPC**:
  - GOG: `syncGOGSaves(locations, appName, arg)` (invoke). If `gogSaves` is empty it first calls `getDefaultSavePath(appName,'gog',[])`.
  - Others: `syncSaves({arg, path, appName, runner})` (invoke). If no path, it calls `getDefaultSavePath(appName,'legendary',[])`.
  - stderr/output goes to `logError`.
  - Backend: legendary refuses when Epic services are offline, and every runner refuses when the app is offline. Both cases return a message string that the UI **does not display** (src/backend/main.ts:1136-1158).
- **Automatic sync** (download before launch, upload after exit) is backend-driven and shown through the `syncing-saves` status.
- **Save-path detection and full sync controls** (path fields, `ProgressDialog`, `TextWithProgress`) live in the per-game Settings → Sync Saves section (src/frontend/screens/Settings/sections/SyncSaves/legendary.tsx, src/frontend/screens/Settings/sections/SyncSaves/gog.tsx), not on the page.

### Install info tab: sizes and installed details

- **Not installed and online** (src/frontend/screens/Game/GamePage/components/DownloadSizeInfo.tsx:13-63): "Download Size" and "Install Size", formatted from manifest `download_size` / `disk_size` with base-2 file sizes (src/frontend/helpers/index.ts:27). They show "Getting … size..." until loaded. Third-party games instead show "Third-Party Manager: EA app | <name>". The block is hidden when offline or for sideload.
- **Installed** (src/frontend/screens/Game/GamePage/components/InstalledInfo.tsx:12-131):
  - Size (not sideload or third-party);
  - Installed Platform (`osx` → "MacOS");
  - Version (not sideload or third-party);
  - **Online Required**: Yes/No (from `canRunOffline`);
  - Third-Party Manager;
  - **Install Path**: truncated, click → `openFolder`. Not shown for third-party;
  - on non-Windows, non-native installs: **Wine** version name, then either **Bottle** (CrossOver) or **WinePrefix** (click → `openFolder`).
  - Browser games show only Installed Platform.

### Playtime (TimeContainer)

Source: src/frontend/screens/Game/TimeContainer/index.tsx:18-118.
- **Data source**: the local store `timestamp` (`store/timestamp.json`, src/frontend/helpers/electronStores.ts:164-167), shaped `{firstPlayed, lastPlayed, totalPlayed(minutes)}`. On each status change it calls `getPlaytimeFromRunner` (GOG only). If the server value is greater, it overwrites `totalPlayed`.
- **Display**:
  - no record: "Last Played: Never";
  - otherwise: "Time Played: HH:MM", or "Never" when 0, with a popover showing **First Played** (locale datetime, or "Never") and **Last Played** (if any).
  - Hidden when `notInstallable`.

### Achievements tab

Source: src/frontend/screens/Game/GamePage/components/Achievements.tsx:8-57; type `GOGAchievement` (src/common/types.ts:169-183). The data is GOG only.
- **Order**: unlocked first, newest unlock first. Then locked, sorted by **rarity descending** (most common first).
- **Each item**:
  - icon (`image_url_unlocked` or `image_url_locked`), with border class `rarity-<rarity_level_slug>`;
  - name and description;
  - "<rarity_level_description> · <rarity>%".
  - Hidden achievements (locked and `!visible`) show the hard-coded English "Hidden Achievement" with no description.
- The tab label shows the completion %.
- **No filters, search, or unlock date display.**
- `lang` is never passed (backend default `en-US`).

### Requirements

Table with columns *(blank)* / MINIMUM / RECOMMENDED and one row per `Reqs {title, minimum, recommended}` (src/frontend/screens/Game/GameRequirements/index.tsx:11-48). It appears both as a tab (src/frontend/screens/Game/GamePage/components/Requirements.tsx:8-24) and as a dots-menu dialog.

### Extra info tab: scores, HowLongToBeat, compatibility, AppleGamingWiki

All external links open in a new Electron window via `createNewWindow(url)` (send).
- **Scores** (src/frontend/components/UI/WikiGameInfo/components/GameScore/index.tsx:11-92): MetaCritic, OpenCritic and IGDB circles, coloured green above 66, red below 33, yellow otherwise. Click targets:
  - Metacritic: `metacritic.com/game/pc/<urlid>`, or search by title;
  - OpenCritic: `opencritic.com/game/<urlid>` (no fallback);
  - IGDB: **uses the metacritic urlid** (`igdb.com/games/<metacritic.urlid>`), else search. This is a bug.
- **HowLongToBeat** (src/frontend/screens/Game/GamePage/components/HLTB.tsx:9-47; src/frontend/components/UI/WikiGameInfo/components/HowLongToBeat/index.tsx:10-58): an accordion "How Long To Beat" with three circles (Main Story / Main + Extras / Completionist, in "Hours"). Each opens `gameWebLink`.
- **Compatibility** (src/frontend/screens/Game/GamePage/components/CompatibilityInfo.tsx:18-93):
  - "Proton Compatibility Tier: <Level>" from `steamInfo.compatibilityLevel`. Click opens ProtonDB (app page by PCGamingWiki steamID, else search). Shown on **all** platforms, unlike the Linux-only dots item.
  - "SteamDeck Compatibility" renders only an icon for `steamDeckCatagory` 0 = unknown (?), 1 = unsupported (⊘), 2 = playable (!), 3 = verified (✓). There is no text label.
- **AppleGamingWiki** (src/frontend/screens/Game/GamePage/components/AppleWikiInfo.tsx:12-55): "AppleGamingWiki Rating: <crossoverRating>". Click opens the CodeWeavers compatibility page, or a search by title. Shown on all platforms.

### Anticheat info and known fixes

- **Anticheat box** (src/frontend/components/UI/Anticheat/index.tsx:19-99). Type: `AntiCheatInfo` (src/common/types.ts:500-520). Contents:
  - heading "This game includes anticheat software";
  - for status Denied/Broken/"Unknown": "Multiplayer features may not work due to denied or broken anticheat support.";
  - "Anticheats: <list>", or "Anticheat removed";
  - "Status: <status> (Reference)", where Reference is the latest `reference` / `updates[]` link;
  - "Source: AreWeAntiCheatYet" (Linux only).
  - Icon: denied for Denied/Broken, allowed for Running/Supported, info otherwise.
  - Data comes from `getAnticheatInfo(namespace)`.
- **Known fixes**: `KnownFixesInfo {title, notes?, winetricks?, runInPrefix?, envVariables?, wikiLink?}` (src/common/types.ts:840-847). The page surfaces **only** `wikiLink`, as a notice rendered with a react-router `<Link to={wikiLink}>`. That is an internal route link, so an external URL will not open externally (src/frontend/screens/Game/GamePage/index.tsx:375-388). Winetricks, runInPrefix and env vars are applied silently by the backend at launch, visible only as the `winetricks` status. Notes are never shown.

### Edit game (metadata override)

EditGameDialog (src/frontend/components/UI/EditGameDialog/index.tsx:28-228):
- **Fields**: title (max length 40), and under "Advanced", Cover Image URL and Square Image URL. Each URL field has an icon button that pastes from the clipboard when empty and clears it when filled.
- **Previews**: live previews of cover and square art. If a SteamGridDB API key is set (`steamgriddb.hasApiKey` → channel `steamgriddb.hasApiKey`, invoke), clicking a preview opens the SteamGridDB picker:
  - cover → `steamgriddb.getHeroes`;
  - square → `steamgriddb.getGrids`;
  - search via `steamgriddb.searchGame`.
- **No API key**: a warning links to Settings → Advanced.
- **Buttons**:
  - **Reset to default** (only if overrides exist) restores the original values;
  - **Finish** → `setGameMetadataOverride({appName, title, art_cover, art_square})` (send). Fields equal to the original are sent as `''`, and the backend deletes the override when all are empty.

### Per-game settings, categories, logs and report issue

All three open one shared `SettingsModal` (src/frontend/screens/Settings/components/SettingsModal/index.tsx:18-80), whose title is "{title} (Settings|Log|Categories)". State comes from `openGameSettingsModal`, `openGameLogsModal` and `openGameCategoriesModal` (src/frontend/state/GlobalStateV2.ts:50-79).
- **Settings**: `GamesSettings` (the full per-game settings form, documented in the Settings section).
- **Report a problem running this game**: shown when installed and not a browser game (src/frontend/screens/Game/GamePage/components/ReportIssue.tsx:11-33). It opens the `log` type, `LogSettings` scoped to the game (src/frontend/screens/Settings/sections/LogSettings/index.tsx:70-312):
  - "How to report a problem?" instructions (Discord #support);
  - log box polled **every 1 s** via `getLogContent({appName, runner})` (invoke). It shows the last 1000 lines with "Log truncated…" when cut, colours lines containing ` err` / ` warn`, and shows "No log file found" or "Internal error reading log file" when appropriate;
  - when the file exists, three buttons:
    - **Show log file in folder** → `showLogFileInFolder` (send);
    - **Upload log file**, which opens the LogUploadDialog;
    - **Join our Discord** → `openDiscordLink` (send).
- **Log upload dialog** (src/frontend/components/UI/LogFileUploadDialog/index.tsx:8-127) has four states:
  1. Confirm: "Upload log file? Do you really want to upload "{{name}}"?" with YES / NO.
  2. Uploading spinner.
  3. Done: "Uploaded to {{url}} (URL copied to your clipboard)". The URL is written to the clipboard.
  4. Failed, with advice to upload manually.
  - IPC: `uploadLogFile(name, {appName, runner})` (invoke) → `false | [url, UploadedLogData]`.
  - The "uploaded logs list / delete" is only in global settings.
- **Categories**: `CategorySettings` (custom categories checklist).

### DLC list with install

DLC install and uninstall is reachable on the game page **only** through Modify Installation: legendary uses `DLCList`, GOG uses toggles applied as an update. There is no standalone DLC section. Nile, Zoom and not-installed games have no DLC UI on the page (DLC selection for new installs is in the Install modal).

### Offline behaviour

`connectivity.status !== 'online'` (src/frontend/screens/Game/GamePage/index.tsx:162-163):
- `getInstallInfo` is skipped, so the download/install sizes are hidden and Modify Installation spins forever.
- Launching a game with `canRunOffline=false` shows the Offline Warning dialog.
- Repair silently no-ops in the backend.
- Manual and automatic save sync no-op (auto-sync is skipped when offline).
- Wiki, achievement and anticheat calls still fire and fail or return cached data silently.

### Misc / edge behaviour worth preserving

- `playClicked` debounce: Play is disabled from click until `launching` arrives (src/frontend/screens/Game/GamePage/index.tsx:269-273, 586-595).
- Achievement cache is invalidated when the game exits (src/frontend/screens/Game/GamePage/index.tsx:175-178).
- The game page re-fetches settings when the settings modal closes (dependency on `settingsModalProps.isOpen`, src/frontend/screens/Game/GamePage/index.tsx:254).
- Queue removal also clears resume progress in localStorage (src/frontend/screens/Game/GamePage/index.tsx:599-602).
- **Library-card-only actions not available on the game page**: Hide/Unhide game, Add/Remove Favourites, Remove from Recent, and direct "Update" (src/frontend/screens/Library/components/GameCard/index.tsx:315-426). A native redesign should expose them on the game page as well.

### UX pain points (Game page)

1. **The hover-only dots menu hides 17 important actions** (move, repair, uninstall, modify, shortcuts, EOS, Steam, browse files/prefix…). It is hover/focus revealed (src/frontend/screens/Game/GamePage/index.css:589-660), unordered, mixes destructive and benign items, has no grouping or separators, and is undiscoverable on touch or gamepad.
2. **Two similarly named path actions**: "Move Game" (moves files) and "Change install path" (re-points without moving). The only difference is a confirmation text.
3. **Alt-play arrow** is hover-revealed and **silently and permanently flips the `verboseLogs` setting**, instead of being a one-off "launch with logs".
4. **Stop-install dialog wording** ("Do you wish to KEEP downloaded files?" with Keep Installing / YES / NO) is ambiguous. Pause vs cancel is conflated under the "Pause/Cancel" label.
5. **No cancel or pause for updates** on the page. The Play button just greys out, and the progress link to the download manager appears only for installs.
6. **Inconsistent queued state**: an installed game with a queued update loses its Play button and gains an enabled "Import Game" button.
7. **Uninstalling hides the main button entirely**, so there is no visual anchor or progress; the only feedback is the status text.
8. **`notAvailable` still shows an enabled Play button.** The problem is only discovered through a backend native message box after clicking.
9. **One failed `getInstallInfo` (network blip, auth expiry) replaces the whole page with an error screen**, even for installed games that don't need it.
10. **Silent failures**: repair offline, save sync offline or Epic-down, and EOS overlay download give no UI feedback. Sync results are only logged.
11. **Cloud saves "Unsupported" for Linux-native installs** is mislabelled as a developer limitation. The quick saves menu requires an obscure per-game toggle and opens on mouse-enter (not keyboard accessible). Its menu items are disabled when no path is set, even though the code could auto-detect the default path.
12. **Duplicate entry points**: Requirements exists as both a tab and a dots dialog. ProtonDB exists as both an Extra-info link (all platforms) and a dots item (Linux only). `getWikiGameInfo` is fetched twice.
13. **"Add to Steam" shows for not-installed games**, while "Add shortcut" doesn't, which is inconsistent.
14. **Store-unsupported actions are shown anyway**: Zoom Move/Repair are visible but stubbed in the backend.
15. **Modify Installation** gives no size or diff preview and no confirmation, always rewrites the pin status, ignores build/branch silently for Linux GOG installs, spins forever offline, and lacks Epic SDL editing.
16. **Achievements** have no filters (locked/unlocked/hidden), no search, no unlock dates, use a hard-coded English "Hidden Achievement", and sort locked items with most-common first.
17. **Steam Deck compatibility is icon-only**, with no text or tooltip. The IGDB score link uses the Metacritic id (bug). An IGDB-only score doesn't enable the Extra tab.
18. **Release date** is hidden when the wiki has no dates, even if the store provides one.
19. **Known-fixes wiki link** uses an in-app router link for what is an external URL. Known-fix notes, winetricks and env vars are never shown to the user, and the `winetricks` status has no page status text.
20. **Status is keyed by appName only** (cross-store collisions possible). The status reducer only accepts some statuses when an entry already exists, so the state machine is fragile. A native implementation should use a typed `(runner, appName) → Status` map.
21. **Playtime** shows as zero-padded "HH:MM" without units. First/last played sit behind a popover, and server playtime (GOG) silently overrides local.
22. **The launch-option auto-select dialog** fires a 10-second countdown that launches the first option if the user is slow.
23. **No in-page refresh** of game metadata. The only recovery is a library refresh elsewhere.
24. **Accessibility**: many clickable `<div>`, `<span>` and `<a>` elements without roles or keyboard handling (install path, prefix, scores, HLTB circles, report issue, achievements). Tooltips use `title` only.

---

## Settings

Reference: Heroic Games Launcher v2.22.4. All refs are relative to the repo root. To keep tables readable, two path prefixes are abbreviated:

- `SC/` = `src/frontend/screens/Settings/components/`
- `SS/` = `src/frontend/screens/Settings/sections/`

"Factory default" means `GlobalConfig.getFactoryDefaults()` in `src/backend/config.ts:323-382`. Where a key has no factory default, the value is the component's `useSetting(key, fallback)` fallback (marked "UI fallback"). Where the two disagree, both are listed.

Platform shorthands: **L** = Linux, **M** = macOS, **W** = Windows, **FP** = Flatpak build (`window.isFlatpak`), **Deck** = Steam Deck (`window.isSteamDeck`), **DeckGM** = Steam Deck Game Mode (`window.isSteamDeckGameMode`). "Native" = the game's installed platform matches the host OS (see `isLinuxNative`/`isMacNative` below, or `window.api.isNative`).

### Navigation, routes and page chrome

| Item | Behaviour | Ref |
|---|---|---|
| Route | `/settings/:type`. `type` is one of `general` (default), `games_settings`, `advanced`, `systeminfo`, `log`. `sync` is also handled (it renders `SyncSaves`), but no link points to it, and in the global context it renders nothing because `runner` is undefined. | `src/frontend/App.tsx:164`, `src/frontend/screens/Settings/index.tsx:33-40,112-117` |
| Sidebar sub-menu | Shown while the URL contains `settings`. Items: General, **Game Defaults** (`games_settings`, hidden on **W**), Advanced, System Information, Log. | `src/frontend/components/UI/Sidebar/components/SidebarLinks/index.tsx:171-224` |
| Page header | A back arrow (NavLink to `/library`) and the title "Global Settings". | `src/frontend/screens/Settings/index.tsx:105-110` |
| Loading | Shows the `UpdateComponent` spinner until `requestAppSettings` resolves and the settings context is non-empty. | `src/frontend/screens/Settings/index.tsx:59-75` |
| Right-click context menu (whole page) | **Copy All Settings to Clipboard**: `clipboardWriteText(JSON.stringify({appName:'default', title, ...config}))`. **Open Config File**: `showConfigFileInFolder('default')`, which opens `config.json` with the OS handler. Both items are hidden on the `log` page. | `src/frontend/screens/Settings/index.tsx:80-101`, `src/backend/main.ts:607-616` |
| Footer | The text "Settings are saved automatically" (`info.settings`). In the per-game context it adds "AppName: `<appName>`" as a button that calls `showConfigFileInFolder(appName)`; the backend flushes `GamesConfig/<appName>.json` first if it does not exist yet. | `SS/FooterInfo/index.tsx:5-31`, `src/backend/main.ts:607-616` |
| Help entry | `hasHelp('settings', …)` registers a help topic (only used when the experimental "Help component" is on). | `src/frontend/screens/Settings/index.tsx:47-56` |

### Persistence model and data flow

| Aspect | Behaviour | Ref |
|---|---|---|
| Context | `useSettingsContext({appName, gameInfo?, runner?})` loads `requestAppSettings` when `appName==='default'`, otherwise `requestGameSettings(appName)`. It reloads when `appName`, the UI language or `settingsModalProps.isOpen` changes. It returns `null` (the caller shows a spinner) while the config is empty. | `src/frontend/hooks/useSettingsContext.ts:15-66` |
| Derived flags | `isDefault` (`appName==='default'`). `isLinuxNative` = L and `gameInfo.install.platform==='linux'`. `isMacNative` = M and `install.platform` is `Mac` or `osx`. | `src/frontend/hooks/useSettingsContext.ts:21-28` |
| Read | `useSetting(key, fallback)` returns `config[key] ?? fallback`. | `src/frontend/hooks/useSetting.ts:5-18` |
| Write | Changes apply instantly; there is no Save or Cancel. `setSetting` is skipped when the new value is JSON-equal to the current one. Otherwise it updates local state optimistically, then fires the `setSetting` IPC listener `{appName, key, value}` (fire-and-forget, no error path). The guard `currentValue !== undefined \|\| currentValue !== null` is always true. | `src/frontend/hooks/useSettingsContext.ts:43-51` |
| Backend (global) | `GlobalConfig.setSetting` writes `config.json` as `{defaultSettings, version:'v0'}`, mirrors the value into electron-store `configStore.settings`, and emits `settingChanged`. Listeners: the logger logs the change, Plausible analytics, and GOG presence (`disableGOGPresence`). | `src/backend/main.ts:856-862`, `src/backend/config.ts:384-397`, `src/backend/storeManagers/gog/presence.ts:96` |
| Backend (game) | `GameConfig.setSetting` writes `GamesConfig/<appName>.json` as `{<appName>: {...}, version:'v0', explicit:true}`. | `src/backend/game_config.ts:320-358` |
| Bulk write | `writeConfig({appName, config})` replaces the whole config. It is used by the theme path selector and by install dialogs. | `src/backend/main.ts:852-854`, `src/backend/utils.ts:1583+` |
| Values that bypass `setSetting` | UI language (`changeLanguage` plus `configStore.language`). Accessibility values and theme (frontend `configStore`, not `AppSettings`). `customThemesPath` (`writeConfig`). SteamGridDB key (`steamgriddb.setApiKey`, stored encrypted). `egsLinkedPath` (`egsSync`, then `setSetting`). | see the respective sections |
| Reset | There is no per-field "reset to default" and no per-game "reset all". The only reset is the global **Reset Heroic**, which deletes `GamesConfig/` and `config.json` and relaunches. | `src/backend/utils.ts:404-414` |

### Inheritance: Game Defaults vs per-game settings ("use default" semantics)

- **Two editing contexts reuse the same components.** The global page `/settings/games_settings` ("Game Defaults") renders `GamesSettings` with `appName='default'` and writes to `config.json`. The per-game modal renders the same `GamesSettings` with the game's `appName` and writes to `GamesConfig/<appName>.json`. Components hide or show themselves via `isDefault`, `runner` and `gameInfo`.
- **Explicit hint.** In Game Defaults, a banner says "Changes in this section only apply as default values when installing games. If you want to change the settings of an already installed game, use the Settings button in the game page." (`SS/GamesSettings/index.tsx:139-147`)
- **How inheritance works.**
  - `GameConfigV0.getSettings()` merges a fixed list of keys from the global config with the game's own stored keys; stored keys win (`src/backend/game_config.ts:203-318`).
  - The current config version is `'v0'` with `isExplicit = true` (`src/backend/constants/others.ts:3`, `src/backend/game_config.ts:181`). `load()` caches the merged object in `this.config`, and `flush()` writes the whole merged object.
  - The first write for a game therefore snapshots every inherited default into the game file. From then on, changes to the global defaults no longer propagate to that game. In practice, a game's settings are a **copy-on-first-write snapshot** of the defaults.
  - The v0.1 "non-explicit" mode, which strips keys equal to the defaults so they stay inherited, exists but is unused (`src/backend/game_config.ts:336-392`).
- **Inherited keys.** `autoInstallDxvk`, `autoInstallDxvkNvapi`, `autoInstallVkd3d`, `DXVKFpsCap`, `preferSystemLibs`, `autoSyncSaves`, `enableEsync`, `enableFSR`, `enableMsync`, `enableFsync`, `enableWineWayland`, `enableHDR`, `enableDXVKFpsLimit`, `enableWoW64`, `maxSharpness`, `launcherArgs`, `nvidiaPrime`, `offlineMode`, `enviromentOptions` (copied), `wrapperOptions`, `savesPath`, `showFps`, `showMangohud`, `targetExe`, `useGameMode`, `battlEyeRuntime`, `eacRuntime`, `beforeLaunchScriptPath`, `afterLaunchScriptPath`, `gamescope`, `verboseLogs`, `advertiseAvxForRosetta`. On non-W also `wineVersion` and `winePrefix` (global `winePrefix` or `sharedWinePrefix`), plus `wineCrossoverBottle` on M only (`src/backend/game_config.ts:210-305`).
- **Keys with game-specific defaults that are *not* inherited.**
  - `language`: always `''`, meaning "use Heroic's language".
  - `enableQuickSavesMenu`: `false`.
  - `steamRuntime`: `false`. GOG installs set `umu-scout` for Linux-native games (`src/backend/storeManagers/gog/games.ts:230,481`).
  - `disableUMU`: `false` only for games with no config file yet, `undefined` otherwise.
  - `ignoreGameUpdates`, `gogSaves`, `lastUsedLaunchOption`: no default.
- **Install-time seeding.** On non-W, the Download/Install dialog writes the game config right before installing, as `{...requestGameSettings(appName), winePrefix, wineVersion, wineCrossoverBottle}`. That materializes the snapshot at install time (`src/frontend/screens/Library/components/InstallModal/DownloadDialog/index.tsx:268-283`). Sideload and third-party dialogs do the same (`SideloadDialog/index.tsx:202,268`, `ThirdPartyDialog/index.tsx:63`).
- **No UI indication.** The per-game modal does not show which values differ from the defaults, and offers no "use default" toggle or "reset to default" action.

### Factory-default vs UI-fallback mismatches (spec must pick one)

| Key | Factory default (`src/backend/config.ts`) | UI fallback | Ref |
|---|---|---|---|
| `autoUpdateGames` | `false` (`:339`) | `true` | `SC/AutoUpdateGames.tsx:7-10` |
| `gamepadInitialRepeatDelay` | `300` (`:366`) | `250` | `SC/GamePadDelayRepeat.tsx:8-11` |
| `autoInstallDxvk` | `isLinux \|\| isMac` | `false` | `SC/AutoDXVK.tsx:12-15` |
| `autoInstallVkd3d` / `autoInstallDxvkNvapi` | `isLinux` | `false` | `SC/AutoVKD3D.tsx:11`, `SC/AutoDXVKNVAPI.tsx:12` |
| `enableEsync` | `true` | `false` | `SC/EnableEsync.tsx:9` |
| `enableFsync` / `enableMsync` | `isLinux` / `isMac` | `false` | `SC/EnableFsync.tsx:14`, `SC/EnableMsync.tsx:15` |
| `eacRuntime` / `battlEyeRuntime` | `isLinux` | `false` | `SC/EacRuntime.tsx:12`, `SC/BattlEyeRuntime.tsx:12` |
| `useGameMode` | `isFlatpak` | `false` | `SC/GameMode.tsx:12` |
| `checkForUpdatesOnStartup` | `!isFlatpak` | `true` | `SC/CheckUpdatesOnStartup.tsx:8` |
| `maxRecentGames` | none | `5` (backend also falls back to 5, `src/backend/recent_games/recent_games.ts:7-10`) | `SC/MaxRecentGames.tsx:12` |
| `advertiseAvxForRosetta` | `isMac && defaultWine.type==='toolkit'` | `false` | `SC/AdvertiseAvxForRosetta.tsx:16` |
| Keys with **no** factory default (UI fallback only) | `darkTrayIcon`, `exitToTray`, `startInTray`, `discordRPC`, `disableController`, `disablePlaytimeSync`, `startInConsoleMode`, `customCSS`, `customThemesPath`, `egsLinkedPath`, `downloadNoHttps`, `disableLogs`, `allowInstallationBrokenAnticheat`, `experimentalFeatures`, `showMangohud`, `enableFSR`, `maxSharpness`, `enableDXVKFpsLimit`, `DXVKFpsCap`, `gamescope`, `launcherArgs`, `targetExe`, `savesPath`, `autoSyncSaves`, `offlineMode`, `altLegendaryBin`, `altGogdlBin`, `altNileBin`, `disableSmoothScrolling` | — | — |

### General section (`/settings/general`)

Render order follows `SS/GeneralSettings/index.tsx:34-76`. Every field here is global-only (`AppSettings`).

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Choose App Language | `language` (also `configStore.language`, `localStorage.language`) | Select with 43 locales: ar, az, be, bg, bs, ca, cs, de, el, en, es, et, eu, fa, fi, fr, ga, gl, he, hu, hr, ja, ko, id, it, lt, ml, nb_NO, nl, pl, pt, pt_BR, ro, ru, sk, sr, sv, ta, tr, uk, vi, zh_Hans, zh_Hant. Native-script labels; an optional flag position and a "Help Improve this translation" Weblate link exist but are unused here. | `'en'` | global | always | `changeLanguage` listener: backend `i18next.changeLanguage`, clears `gameInfoStore`, `GlobalConfig.setSetting('language')`, emits `languageChanged`. Frontend: `i18n.changeLanguage`, sets `<html lang>`. Triggers a reload of the settings context. | `src/frontend/components/UI/LanguageSelector/index.tsx:21-172`, `src/backend/main.ts:449-455` |
| Select Theme | frontend `configStore.theme` (not AppSettings) | Select: 14 built-in themes (midnightMirage "Midnight Mirage", cyberSpaceOasis, cyberSpaceOasisAlt, high-contrast, old-school, dracula, marine, marine-classic, zombie, zombie-classic, nord-light, nord-dark, gruvbox_dark, sweet) plus `*.css` files in the custom themes dir | `midnightMirage` | global | always (also on the Accessibility screen) | `setTheme`: `configStore.set('theme')`, `window.setTheme` sets the body class. For custom themes it calls `getThemeCSS(file)` and injects a `<style class="customTheme">`. On Windows-controls-overlay it updates the title bar via `setTitleBarOverlay`. | `src/frontend/components/UI/ThemeSelector/index.tsx:10-83`, `src/frontend/index.tsx:134-169`, `src/frontend/state/GlobalState.tsx:287-291`, `src/backend/main.ts:1303-1323` |
| Custom Themes Path | `customThemesPath` | Directory path box with "Check the Wiki…" help link (`openCustomThemesWiki`) and the warning "Do not use CSS files from untrusted sources." | `''` | global | always | `writeConfig({appName:'default', config:{...appConfig, customThemesPath}})` rewrites the whole config, then `getCustomThemes` lists `*.css` in the dir. | `src/frontend/components/UI/ThemeSelector/index.tsx:41-117` |
| Default Installation Path | `defaultInstallPath` | Directory path box, no clear button | `~/Games/Heroic` | global | always | `openDialog` (directory). Registers a help topic. | `SC/DefaultInstallPath.tsx:6-36`, `src/backend/constants/paths.ts:31` |
| Set Folder for new Wine Prefixes | `defaultWinePrefixDir` (legacy alias `defaultWinePrefix`) | Directory path box, no clear button | `~/Games/Heroic/Prefixes` | global | hidden on W; Game-Defaults context only | `openDialog` | `SC/WinePrefixesBasePath.tsx:8-38`, `src/backend/config.ts:306-307` |
| Default Steam path | `defaultSteamPath` | Directory path box plus help box ("lets Heroic determine what version of Proton Steam uses, for adding non-Steam games to Steam") | `getSteamCompatFolder()` | global | `isDefault` only | `openDialog`. Also used by "Download GE-Proton to Steam". | `SC/DefaultSteamPath.tsx:8-46` |
| Sync with Installed Epic Games (EGL sync) | `egsLinkedPath` | **W**: a toggle (on sends `'windows'`, off sends `'unlink'`). **L/M**: a directory path box for the prefix where EGL is installed (an empty path sends `'unlink'`), plus a help box. Disabled while syncing. | `''` | global | always | `egsSync(path)`. On `'Error'` it shows an error dialog ("Invalid Path") and clears the key. On success it shows a "Sync Complete"/"Unsync Complete" dialog, stores the path, and calls `refreshLibrary({fullRefresh:true})`. | `SC/EgsSettings.tsx:9-83` |
| Check for Heroic Updates on Startup | `checkForUpdatesOnStartup` | Toggle | `!isFlatpak` | global | hidden when `showUpdateSetting()` is false (i.e. on FP) | `showUpdateSetting`. Backend `getLatestReleases` honours it. | `SC/CheckUpdatesOnStartup.tsx:6-36`, `src/backend/main.ts:655-664` |
| Automatically update games | `autoUpdateGames` | Toggle | factory `false` / UI `true` | global | always | used in `checkGameUpdates` | `SC/AutoUpdateGames.tsx:5-20`, `src/backend/main.ts:625-640` |
| Don't show changelogs on Startup | `hideChangelogsOnStartup` | Toggle | `false` | global | always | also updates global state `setHideChangelogsOnStartup` | `SC/HideChangelogOnStartup.tsx:7-29` |
| Start in Console Mode | `startInConsoleMode` | Toggle | `false` | global | always | the backend opens `/console` at startup | `SC/StartInConsoleMode.tsx:5-20`, `src/backend/main.ts:230` |
| Hide System Tray Icon (requires restart) | `noTrayIcon` | Toggle | `false` | global | always | disables the next three tray-dependent toggles | `SC/TraySettings.tsx:13-21` |
| Exit to System Tray | `exitToTray` | Toggle; shown as off and disabled when `noTrayIcon` | `false` | global | always | — | `SC/TraySettings.tsx:23-29` |
| Start Minimized | `startInTray` | Toggle; disabled unless `exitToTray` and not `noTrayIcon` | `false` | global | always | — | `SC/TraySettings.tsx:31-37` |
| Minimize Heroic After Game Launch | `minimizeOnLaunch` | Toggle; disabled and shown as off when `noTrayIcon` | `false` | global | always | — | `SC/MinimizeOnGameLaunch.tsx:5-25` |
| Use Dark Tray Icon (needs restart) | `darkTrayIcon` | Toggle | `false` | global | always | `changeTrayColor` listener | `SC/UseDarkTrayIcon.tsx:5-22`, `src/backend/tray_icon/tray_icon.ts:51` |
| Use frameless window (requires restart) | `framelessWindow` | Toggle. Turning it on opens the confirm dialog "Experimental feature ahead" (OK/Cancel); turning it off applies directly. | `false` | global | hidden on DeckGM | — | `SC/UseFramelessWindow.tsx:7-57` |
| Add desktop shortcuts automatically | `addDesktopShortcuts` | Toggle | `false` | global | `isDefault`; W or L only | — | `SC/Shortcuts.tsx:46-56` |
| Add games to start menu automatically / Add games to Applications automatically (M label) | `addStartMenuShortcuts` | Toggle | `false` | global | `isDefault` | — | `SC/Shortcuts.tsx:33-63` |
| Add games to Steam automatically | `addSteamShortcuts` | Toggle | `false` | global | `isDefault` | — | `SC/Shortcuts.tsx:65-70` |
| Disable playtime synchronization | `disablePlaytimeSync` | Toggle plus info icon ("currently only GOG is supported") | `false` | global | `isDefault` | backend startup reads it | `SC/PlaytimeSync.tsx:8-39`, `src/backend/main.ts:375` |
| Enable Discord Rich Presence | `discordRPC` | Toggle | `false` | global | `isDefault` | — | `SC/DiscordRPC.tsx:7-24` |
| Disable Heroic navigation using controller | `disableController` | Toggle | `false` | global | always | `toggleControllerIsDisabled(value)` takes effect live in the gamepad helper | `SC/DisableController.tsx:7-29`, `src/frontend/helpers/gamepad.ts:835` |
| Send anonymous data to help Heroic development | `analyticsOptIn` | Toggle plus info icon ("Needs restart to take effect.") | `false` | global | always | backend `startPlausible()` at startup if true | `SC/AnalyticsOptIn.tsx:6-32`, `src/backend/main.ts:365-367` |
| Library Top Section | `libraryTopSection` | Select: `recently_played` "Recently Played Games", `recently_played_installed` "Recently Played Games (Only Installed)", `favourites` "Favourite Games", `disabled` "Disabled" | `'disabled'` | global | always | also `handleLibraryTopSection` (global state) | `SC/LibraryTopSection.tsx:9-50` |
| Recent Games to Show | `maxRecentGames` | Select 1–10 | `5` | global | `isDefault` | used by the tray recent-games menu | `SC/MaxRecentGames.tsx:8-33`, `src/backend/tray_icon/tray_icon.ts:60` |
| Maximum Number of Workers when downloading | `maxWorkers` | Select 1..N (N = `getMaxCpus()` = `os.cpus().length`) plus `0` "Max" | `0` | global | always | `getMaxCpus`. Used by legendary download/update/repair. | `SC/MaxWorkers.tsx:7-38`, `src/backend/main.ts:644`, `src/backend/storeManagers/legendary/games.ts:498` |

#### First-run analytics dialog

On app start, `AnalyticsDialog` (mounted in `src/frontend/App.tsx:100`) checks `localStorage['analytics-modal-shown']`. If it is absent, a modal titled "Send Anonymous Analytics" explains the data collected (Plausible, self-hosted; app version, OS, arch, distro, stores, country). Its Enable and Disable buttons both call `setSetting({appName:'default', key:'analyticsOptIn', value})` and set the flag. The dialog is skipped in E2E mode. (`SC/AnalyticsDialog.tsx:5-99`)

### Game Defaults (`/settings/games_settings`) and per-game settings: tab structure

`GamesSettings` (`SS/GamesSettings/index.tsx`) uses MUI `Tabs` (scrollable). Tab visibility:

| Tab | Shown when | Ref |
|---|---|---|
| Wine | not W, the game is not native, the game is not a Browser game. In Game Defaults it is shown on L/M. | `SS/GamesSettings/index.tsx:90-97,155` |
| Other | L, or M with a non-native game | `SS/GamesSettings/index.tsx:98-103,156-158` |
| Advanced | always | `SS/GamesSettings/index.tsx:159-162` |
| Cloud Saves Sync | per-game only, runner `gog` or `legendary` | `SS/GamesSettings/index.tsx:85-86,164-169` |
| Gamescope | L | `SS/GamesSettings/index.tsx:170-175` |
| Legacy | L and not native. Banner: "Warning: The settings on this tab are mostly deprecated and might not work at all." | `SS/GamesSettings/index.tsx:176-178,251-263` |

- **Initial tab.** Defaults context: `wine` (or `advanced` on W). Per-game: `advanced` on W; `wine` if the installed platform is Windows; `advanced` on M; otherwise `other`.
- **Last tab memory.** The last used tab is persisted per game in `localStorage['<app_name>-setting_tab']`, or `'default'` for Game Defaults. A stale value (e.g. `wine` for a game that is now native) renders an empty panel. (`SS/GamesSettings/index.tsx:57-73,105-119`)
- **Native detection.** `isNative` is fetched per game via `window.api.isNative({appName, runner})` (`SS/GamesSettings/index.tsx:121-132`).

The Wine, Other and Advanced tabs follow. Scope "both" means both Game Defaults and per-game.

#### Wine tab

Render order: `SS/GamesSettings/index.tsx:181-205`. If `wineVersion.type==='crossover'`, everything after CrossOver Bottle is hidden (DXVK/VKD3D, sync, FSR, Tools and so on).

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Wine Version / "Crossover/Wine Version" (M label) | `wineVersion` (`{bin,name,type:'wine'\|'proton'\|'crossover'\|'toolkit',lib?,lib32?,wineserver?}`) | Select over `getAlternativeWine()`. Each item has a type icon (wine glass, GE logo, CachyOS logo, Proton logo, CodeWeavers, Apple) and an auto-renew badge for `*-latest`; display names rewrite `Proton-GE(-Proton)` to `GE-Proton`. **Below the select:** if no versions were found, a link to `/wine-manager` ("No Wine version was found…"). If `pathExists(bin)` is false, "Wine Path is invalid". On **L**: the "Valve Proton versions ignored" info box (when `!showValveProton`), a box showing the current bin path, and a help box listing the search dirs (`~/.config/heroic/tools/wine`, `…/tools/proton`, `~/.steam/root/compatibilitytools.d`, `~/.steam/steamapps/common`, `~/.local/share/lutris/runners/wine`, Steam Flatpak, `/usr/share/steam`). | `getDefaultWine()` (`which wine`, else "Default Wine - Not Found"). The UI fallback is `{bin:'/usr/bin/wine', name:'Wine Default', type:'wine'}`. | both | Wine tab | `getAlternativeWine` (the backend fetches release info first if the list is empty). `handleWineVersionsUpdated` re-lists. `wine.isValidVersion` keeps a selected version that has left the list. `pathExists`. `requestAppSettings` reads `showValveProton`. **Auto-mutation:** the version is re-matched by `bin` and saved if its name changed; if only one version exists it is auto-selected and saved. | `SC/WineVersionSelector.tsx:24-227`, `src/frontend/screens/Settings/util.ts:3-7`, `src/backend/utils/compatibility_layers.ts:27-49` |
| WinePrefix folder | `winePrefix` | Directory path box, no clear button, plus the "Wine Prefix" info box with a "WinePrefix FAQ" link (`openWinePrefixFAQ`) | `~/Games/Heroic/Prefixes/shared` (`sharedWinePrefix`); a per-game value is set at install | both | not W; not crossover | `openDialog` | `SC/WinePrefix.tsx:9-55`, `src/backend/constants/paths.ts:38` |
| CrossOver Bottle | `wineCrossoverBottle` | Text input | `'Heroic'` | both | `wineVersion.type==='crossover'` | — | `SC/CrossoverBottle.tsx:6-26` |
| Auto Install/Update DXVK on Prefix | `autoInstallDxvk` | Toggle plus info. The label reads "Please wait..." and fades while working. **L only:** disabled if `autoInstallDxvkNvapi` or `autoInstallVkd3d` is on. | `isLinux \|\| isMac` | both | only when `wineVersion.type==='wine'` and the bin does not contain `toolkit` and the name does not end with `-DXMT` (hidden for Proton, GPTK and DXMT) | `toggleDXVK({appName, action:'backup'\|'restore'})`; the backend runs `DXVK.installRemove(gameSettings,'dxvk',action)`. The setting flips only if the call returns true. In Game Defaults `appName` is `'default'`. | `SC/AutoDXVK.tsx:10-74`, `src/backend/main.ts:827-833` |
| Auto Install/Update DXVK-NVAPI on Prefix | `autoInstallDxvkNvapi` | Toggle plus info (DLSS on Nvidia); disabled unless `autoInstallDxvk` | `isLinux` | both | L and not Deck | Non-Proton: `toggleDXVKNVAPI({appName, action})`. Proton: only flips the setting. | `SC/AutoDXVKNVAPI.tsx:9-67`, `src/backend/main.ts:835-841` |
| Auto Install/Update VKD3D on Prefix | `autoInstallVkd3d` | Toggle plus info (DX12); disabled unless `autoInstallDxvk` | `isLinux` | both | L; hidden for Proton | `toggleVKD3D({appName, action})` | `SC/AutoVKD3D.tsx:9-65`, `src/backend/main.ts:843-849` |
| Enable Esync | `enableEsync` | Toggle plus info | `true` | both | Wine tab (L/M) | — | `SC/EnableEsync.tsx:6-30` |
| Enable Fsync | `enableFsync` | Toggle plus info | `isLinux` | both | L, not Linux-native | — | `SC/EnableFsync.tsx:9-39` |
| Enable Wine-Wayland (Experimental) | `enableWineWayland` | Toggle plus info | `false` | both | L, not Linux-native | reveals Enable HDR | `SC/EnableWineWayland.tsx:9-44` |
| ↳ Enable HDR | `enableHDR` | Toggle plus info | `false` | both | only when `enableWineWayland` | — | `SC/EnableWineWayland.tsx:45-61` |
| Enable WoW64 (Experimental) | `enableWoW64` | Toggle plus info | `false` | both | L, not Linux-native | — | `SC/EnableWoW64.tsx:9-41` |
| Enable Msync | `enableMsync` | Toggle plus info | `isMac` | both | M, not Mac-native, name does not end with `-DXMT` | — | `SC/EnableMsync.tsx:10-41` |
| Advertise AVX for Rosetta | `advertiseAvxForRosetta` | Toggle plus info (e.g. Death Stranding) | `isMac && defaultWine.type==='toolkit'` | both | M, not Mac-native, `wineVersion.type !== 'wine'` | — | `SC/AdvertiseAvxForRosetta.tsx:10-45` |
| Enable FSR Hack | `enableFSR` | Toggle plus info | `false` | both | L, not Linux-native | reveals Sharpness | `SC/EnableFSR.tsx:10-42` |
| ↳ FSR Sharpness Strength | `maxSharpness` | Select 1–5 | `5` | both | only when `enableFSR` | — | `SC/EnableFSR.tsx:43-57` |
| Limit DirectX Games FPS (M placement) | `enableDXVKFpsLimit` / `DXVKFpsCap` | See the Legacy tab | — | both | **in the Wine tab only on M**; on L it lives in Legacy | — | `SS/GamesSettings/index.tsx:201` |
| Tools (Winecfg, Winetricks, Run EXE on Prefix) | — | Three buttons; see **Tools** below | — | per-game only | not `isDefault`, not W, has a runner | `callTool`, Winetricks IPC | `SC/Tools/index.tsx:13-108` |

#### Other tab

Render order: `SS/GamesSettings/index.tsx:207-220`.

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Show FPS (DX9, 10 and 11) / "Show Stats Overlay" (M label) | `showFps` | Toggle | `false` | both | not native (also hidden in its own guard for W and L-native) | — | `SC/ShowFPS.tsx:8-33`, `SS/GamesSettings/index.tsx:208` |
| Enable Mangohud (Mangohud needs to be installed) | `showMangohud` | Toggle plus info | `false` | both | L (shown for native games too) | — | `SC/Mangohud.tsx:8-35` |
| Use GameMode (Feral Game Mode needs to be installed) | `useGameMode` | Toggle plus info | `isFlatpak` | both | L | **FP:** turning GameMode off while `eacRuntime` is on opens the dialog "EAC runtime enabled … disable the EAC Runtime and GameMode?" (Yes also disables EAC). GameMode is turned off regardless of the answer. | `SC/GameMode.tsx:8-64` |
| Prefer system libraries | `preferSystemLibs` | Toggle plus info | `false` | both | L, and the wine is not Proton, not CrossOver, and not system wine (bin starting with `/usr`) | — | `SC/PreferSystemLibs.tsx:9-46`, `SS/GamesSettings/index.tsx:211` |
| Use Steam Runtime | `steamRuntime` (`false \| 'umu-scout' \| 'umu-soldier' \| 'umu-sniper' \| 'umu-steamrt4'`) | Select: None, "Steam Linux Runtime 1.0 (scout)", "2.0 (soldier)", "3.0 (sniper)", "4.0", plus info | `false` (`umu-scout` auto-set for GOG Linux-native installs) | per-game (effectively) | only `isLinuxNative`, which is never true in Game Defaults | — | `SC/SteamRuntime.tsx:11-89` |
| Force use of NVIDIA Optimus or AMD Dynamic Switchable Graphics dGPU… | `nvidiaPrime` | Toggle plus help box with a wiki link (`openExternalUrl`) | `false` | both | L, and (≥2 GPUs detected **or** already enabled) | `systemInfo.get()` counts GPUs | `SC/NvidiaPrime.tsx:10-64` |
| BattlEye AntiCheat Runtime | `battlEyeRuntime` | Toggle plus a "Installing BattlEye Runtime..." spinner | `isLinux` | both | L, not native | On enable: `isRuntimeInstalled('battleye_runtime')`, and if missing `downloadRuntime('battleye_runtime')`. If the download fails, the toggle is not flipped. | `SC/BattlEyeRuntime.tsx:9-57` |
| EasyAntiCheat Runtime | `eacRuntime` | Toggle plus an "Installing EAC Runtime..." spinner | `isLinux` | both | L, not native | On enable: on FP without GameMode, the prompt "GameMode required" (Yes enables GameMode). Then `isRuntimeInstalled('eac_runtime')` and `downloadRuntime('eac_runtime')`. | `SC/EacRuntime.tsx:9-72` |

#### Advanced tab (per-game / defaults)

Render order: `SS/GamesSettings/index.tsx:222-241`.

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Ignore game updates | `ignoreGameUpdates` | Toggle | `false` | per-game only | not sideloaded | — | `SC/IgnoreGameUpdates.tsx:7-28` |
| Run Game Offline | `offlineMode` | Toggle, with a warning box when the game's `canRunOffline` is false ("This game does not explicitly allow offline mode…") | `false` (inherited) | per-game only | runner `legendary`, not sideloaded | `getGameInfo(appName, runner)` | `SC/OfflineMode.tsx:10-55` |
| Enable verbose logs | `verboseLogs` | Toggle | `true` | both | always | — | `SC/VerboseLogs.tsx:5-20` |
| Select an alternative EXE to run | `targetExe` | File path box (dialog starts in the install dir) | `''` | per-game only | not sideload | `openDialog` | `SC/AlternativeExe.tsx:7-29` |
| Launch Options | `lastUsedLaunchOption` (`{type:'basic', name, parameters}` etc.) | Select of the game's launch options; shows "Launch Options..." when none is selected | `{type:'basic',name:'',parameters:''}` | per-game only | more than one launch option | `useLaunchOptions({appName, runner})` | `SC/LaunchOptionSelector.tsx:10-78` |
| Game Arguments (appended to game launch command) | `launcherArgs` | Text input plus help (e.g. `-nolauncher`). Inline errors: `%command` gives "The %command% syntax from Steam is not valid as game arguments."; a match of `/[A-Z_]+=\S/` gives "Environment variables must be configured in the table below." The value is still saved. | `''` (inherited) | per-game only | — | — | `SC/LauncherArgs.tsx:7-74` |
| Scripts: run before launch | `beforeLaunchScriptPath` | File path box (dialog starts in the install dir) | `''` | both | always | — | `SC/BeforeLaunchScriptPath.tsx:7-28`, `SS/GamesSettings/index.tsx:233-237` |
| Scripts: run after exit | `afterLaunchScriptPath` | File path box | `''` | both | always | — | `SC/AfterLaunchScriptPath.tsx:7-28` |
| Wrapper command | `wrapperOptions` (`{exe, args}[]`) | Two-column table (Wrapper \| Arguments); see **Wrappers table** | `[]` | both | not W | — | `SC/WrappersTable.tsx:12-74` |
| Environment Variables | `enviromentOptions` (sic; `{key, value}[]`) | Env-var list editor plus Bulk Edit; see below | `[]` | both | always (including W) | — | `SC/EnvVariablesTable.tsx:19-245` |
| Prefered Language (Language Code) | `language` | Text input, placeholder `2-char or 4-char code (i.e.: "fr" or "pt-BR")`, help box ("Leave blank to use Heroic's language", game-dependent codes) | per-game `''` | both | not sideloaded. **Note:** in Game Defaults this edits the global `language` key, which is also the app UI language key. | — | `SC/PreferedLanguage.tsx:5-46` |

#### Cloud Saves Sync tab

Per-game only. See **Sync saves** below.

#### Gamescope tab (L only, both contexts)

`gamescope` is a single object key. Every sub-field rewrites the whole object. Default object (UI fallback; there is no factory default): `{enableUpscaling:false, enableLimiter:false, enableForceGrabCursor:false, windowType:'fullscreen', gameWidth:'', gameHeight:'', upscaleHeight:'', upscaleWidth:'', upscaleMethod:'fsr', fpsLimiter:'', fpsLimiterNoFocus:'', additionalOptions:''}` (`SC/Gamescope.tsx:18-31`).

Pre-check: `hasExecutable('gamescope')`. While checking it shows "Searching for gamescope executable." If gamescope is missing, a red message replaces the whole form. On FP: "Install Gamescope's flatpak package with runtime {{flatpakRuntimeVersion}} and restart Heroic". Otherwise: "We could not find gamescope on the PATH…" (`SC/Gamescope.tsx:39-93`).

| Field | Sub-key | Control/options | Default | Visibility | Ref |
|---|---|---|---|---|---|
| Enables Upscaling | `enableUpscaling` | Toggle | false | always | `SC/Gamescope.tsx:126-138` |
| Upscale Method | `upscaleMethod` | Select: `fsr` "AMD FidelityFX Super Resolution 1.0 (FSR)", `nis` "NVIDIA Image Scale (NIS)", `integer` "Interger Upscale" (sic), `stretch` "Stretch Image", plus info | `fsr` | when upscaling | `SC/Gamescope.tsx:95-167` |
| Game Width / Game Height | `gameWidth` / `gameHeight` | Digits only, max length 4, plus info ("16:9 assumed") | `''` | when upscaling | `SC/Gamescope.tsx:169-213` |
| Upscale Width / Upscale Height | `upscaleWidth` / `upscaleHeight` | Digits only, max length 4 | `''` | when upscaling | `SC/Gamescope.tsx:215-261` |
| Window Type | `windowType` | Select: fullscreen, borderless, windowed | `fullscreen` | when upscaling | `SC/Gamescope.tsx:108-279` |
| Enable FPS Limiter | `enableLimiter` | Toggle | false | always | `SC/Gamescope.tsx:283-295` |
| FPS Limiter / FPS Limiter (No Focus) | `fpsLimiter` / `fpsLimiterNoFocus` | Digits only, max length 3, plus info | `''` | when limiter is on | `SC/Gamescope.tsx:297-347` |
| Enable Force Grab Cursor | `enableForceGrabCursor` | Toggle plus info | false | always | `SC/Gamescope.tsx:349-370` |
| Additional Options | `additionalOptions` | Free text; **saved on blur** (all other fields save on change) | `''` | always | `SC/Gamescope.tsx:372-394` |

#### Legacy tab (L, not native)

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Limit DirectX Games FPS | `enableDXVKFpsLimit` | Toggle plus info ("Sets a frame rate cap for DirectX Games (9-12)") | `false` | both | not W, not native, bin does not contain `toolkit`, name does not end with `-DXMT` (on M this field appears in the Wine tab) | — | `SC/EnableDXVKFpsLimit.tsx:10-49` |
| ↳ FPS value | `DXVKFpsCap` (stored as string) | Text input, placeholder "Positive integer value (e.g. 30, 60, ...)"; no validation | `''` | both | when the limit is on | — | `SC/EnableDXVKFpsLimit.tsx:50-60` |
| Disable umu | `disableUMU` | Toggle | `false` for new games | per-game only | L, `wineVersion.type==='proton'` | — | `SC/DisableUMU.tsx:9-29` |

### Tools (Winecfg / Winetricks / Run EXE) — per-game Wine tab

| Action | Behaviour | IPC | Ref |
|---|---|---|---|
| Winecfg | The button shows an `active` state while running. | `callTool({tool:'winecfg', appName, runner})`. The backend runs `winecfg` in the game's prefix with the game's wine (`wait:false`). For GOG it then re-checks offline-installer changes and pushes an updated game to the library, then sends game status `done`. | `SC/Tools/index.tsx:26-46,82-88`, `src/backend/tools/ipc_handler.ts:25-62` |
| Run EXE on Prefix | Opens a file dialog titled "Select EXE to Run", starting in the install dir (none for sideloaded games), then runs the chosen file with its folder as cwd. `active` state while running. | `getGameInfo`, `openDialog`, `callTool({tool:'runExe', exe, …})` | `SC/Tools/index.tsx:48-65,96-103`, `src/backend/tools/ipc_handler.ts:41-51` |
| Winetricks | Opens the **Winetricks dialog** (below). | — | `SC/Tools/index.tsx:67-80,89-95` |

**Winetricks dialog** (`src/frontend/components/UI/Winetricks/index.tsx`). It is a `ProgressDialog` titled "Winetricks" with a log pane and a close button. The log pane is hidden unless the GUI is open, an install is running, or lists are still loading.

- **Loading on open.** It calls `winetricksListInstalled(runner, appName)` (channel `winetricksInstalled`) and `winetricksListAvailable(runner, appName)` (channel `winetricksAvailable`) in parallel. Loading text: "Loading available components ...". Error states: "No available components" and "Nothing was installed by Winetricks yet". (`:18-57,137-172`)
- **Component search.** A search bar with the placeholder "Search fonts or components". Suggestions appear after 2 characters, filter by case-sensitive substring, and exclude installed components. Each suggestion has an "Install" button that clears the search and calls `winetricksInstall(runner, appName, component)` (fire-and-forget listener). (`src/frontend/components/UI/Winetricks/WinetricksSearch/index.tsx:11-61`)
- **Open Winetricks GUI** button: `callTool({tool:'winetricks'})`. The log pane shows while it is open. (`:105-115,128-134`)
- **Progress.** `progressOfWinetricks` events `{messages[], installingComponent}` append to the logs, and `installing` stays true until a message equals `'Done'`. An `installing-winetricks-component` event with `''` triggers a re-list of installed components. The dialog restores the correct state if it is reopened mid-install. While installing it shows "Installation in progress: {{component}}" and hides the search. (`:59-103,150-158`)
- **Installed list.** "Installed components:" followed by a comma-joined list. (`:162-174`)

### Environment variables editor and Bulk Edit modal

Key: `enviromentOptions: {key, value}[]` (`SC/EnvVariablesTable.tsx`, `SC/EnvVariableRow.tsx`, `SC/BulkEditModal.tsx`).

- **Header.** Title "Environment Variables". Icon buttons: **Bulk Edit** and **Clear All**. Clear All is disabled when the list is empty and asks for confirmation ("Are you sure you want to clear all environment variables?", Cancel/Confirm). (`SC/EnvVariablesTable.tsx:106-124,159-181`)
- **Rows.** Each row shows `KEY = VALUE` (ellipsis with a full-text tooltip) and actions **Edit**, **Duplicate** and **Remove**.
  - Edit turns the row into inline NAME/VALUE inputs with Save (disabled for an empty key) and Cancel. Focus moves to the first input on edit and returns to the Edit button afterwards.
  - Duplicate appends `KEY_COPY`, then `KEY_COPY2`, `KEY_COPY3` and so on.
  - Remove has no confirmation.
  (`SC/EnvVariableRow.tsx:44-188`, `SC/EnvVariablesTable.tsx:61-104`)
- **Add New Variable.** NAME and VALUE inputs plus an Add button (disabled for an empty key). Typing `KEY=VALUE` into NAME while VALUE is empty splits it automatically. (`SC/EnvVariablesTable.tsx:197-240`)
- **Validation.**
  - Key errors: "Variable names can't be empty", "…can't contain the \"=\" sign", "…can't contain spaces". Keys are trimmed.
  - Duplicate keys: "A variable with this name already exists".
  - Values are trimmed.
  - Errors are shown inline under the row or the add form.
  (`SC/EnvVariableRow.tsx:11-35`, `SC/EnvVariablesTable.tsx:32-59,86-104`)
- **Bulk Edit modal.** A dialog with a textarea pre-filled with `KEY=VALUE` lines.
  - Save parses non-empty lines. A line without `=` gives "Some variables are incorrect. Please check that all lines follow the KEY=VALUE format."
  - Each key is validated as above. The value is everything after the first `=`.
  - Later duplicates win (Map semantics). The parsed list replaces the whole list.
  - Cancel discards changes.
  (`SC/BulkEditModal.tsx:12-86`)
- **Help box.** "Set environment variables to append to the command. Do NOT include the "=" sign…" (`SC/EnvVariablesTable.tsx:126-138`)

### Wrappers table (`TwoColTableInput`)

Key: `wrapperOptions: {exe, args}[]`. Generic component: `src/frontend/components/UI/TwoColTableInput/index.tsx:40-233`.

- **Table.** Label "Wrapper command:". Header "Wrapper \| Arguments". Each row has Edit and Remove icons.
- **Editing a row.** Edit copies the row into the bottom inputs. Re-adding with the same key **updates the value in place**, so editing the key creates a new row instead.
- **Adding.** The add row has inputs "New Wrapper" and "Wrapper Arguments" plus a **+** button. The key is required; arguments are optional (`fullFills={key:true, value:false}`). Values are trimmed on save.
- **Unsaved hint.** "Changes in this table are not saved automatically. Click the + button" appears while the inputs are dirty.
- **Warnings and help.** When arguments are non-empty: "Warning: Make sure to quote args with spaces! E.g.: "path/with spaces/"". Help: "Arguments example: --arg; --extra-file="file-path/ with/spaces"".

(`SC/WrappersTable.tsx:12-74`)

### Advanced section (`/settings/advanced`, global-only)

Render order: `SS/AdvancedSettings/index.tsx:172-364`.

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| SteamGridDB API Key | `steamGridDbApiKey` (stored encrypted by the backend) | Password-type text input. The value is never shown back. Placeholder: "Key saved — type to replace, clear to remove" when a key exists, otherwise "Enter your SteamGridDB API Key here". Details box with the URL `www.steamgriddb.com/profile/preferences/api`. Every keystroke saves. | `''` | global | always | `steamgriddb.hasApiKey`, `steamgriddb.setApiKey(value)` | `SC/SteamGridDbApiKey.tsx:5-52`, `src/backend/steamgrid/ipc_handler.ts:20,33` |
| Choose an Alternative Legendary Binary (needs restart) | `altLegendaryBin` | File path box with a clear button. Placeholder "Using built-in Legendary binary...". Below it: "Legendary Version: x". If the version is `invalid`, it shows "Invalid", then clears the setting after 3 s. | `''` | global | always | `getLegendaryVersion` (re-queried on every change) | `SC/AltLegendaryBin.tsx:6-55` |
| Choose an Alternative GOGDL Binary to use (needs restart) | `altGogdlBin` | Same pattern | `''` | global | always | `getGogdlVersion` | `SC/AltGOGdlBin.tsx:6-53` |
| Choose an Alternative Nile Binary (needs restart) | `altNileBin` | Same pattern | `''` | global | always | `getNileVersion` | `SC/AltNileBin.tsx:6-52` |
| (Comet binary) | `altCometBin` | **No UI**; the key exists in `AppSettings` only | — | — | — | — | `src/common/types.ts:91-99` |
| Download games without HTTPS (useful for CDNs e.g. LanCache) | `downloadNoHttps` | Toggle | `false` | global | always (only legendary honours it: `--no-https`) | — | `SC/DownloadNoHTTPS.tsx:5-23`, `src/backend/storeManagers/legendary/games.ts:498-512` |
| Disable Logs | `disableLogs` | Toggle plus info ("…Make sure to turn OFF this setting before reporting any issue.") | `false` | global | always | — | `SC/DisableLogs.tsx:6-27` |
| Disable GOG Presence updates | `disableGOGPresence` | Toggle | `false` | global | always | `settingChanged` triggers a presence update in the backend | `SC/DisableGOGPresence.tsx:5-20`, `src/backend/storeManagers/gog/presence.ts:96-99` |
| Allow installation of games with broken or denied anticheat | `allowInstallationBrokenAnticheat` | Toggle. Turning it on opens the confirm "Are you sure?" ("…multiplayer features won't work. Do not ask for support…", buttons "I understand"/"No"). | `false` | global | always | read by DownloadDialog | `SC/AllowInstallationBrokenAnticheat.tsx:7-56`, `src/frontend/screens/Library/components/InstallModal/DownloadDialog/index.tsx:196-202` |
| Gamepad input initial repeat delay | `gamepadInitialRepeatDelay` | Slider 50–500 ms, step 10, end labels | factory `300` / UI `250` | global | always | `updateGamepadActions()` applies it live | `SC/GamePadDelayRepeat.tsx:6-37` |
| Gamepad input repeat frequency | `gamepadRepeatDelay` | Slider 5–150 ms, step 5 | `50` | global | always | `updateGamepadActions()` | `SC/GamePadDelayRepeat.tsx:39-57` |
| Hide Heroic window when launching games from heroic:// links | `hideWindowOnProtocolLaunch` | Toggle plus info | `false` | global | always | backend `protocol.ts` | `SC/HideWindowOnProtocolLaunch.tsx:6-32`, `src/backend/protocol.ts:40,118` |
| Allow using Valve Proton builds to run games | `showValveProton` | Toggle. Turning it on opens the confirm "Are you sure?" (recommends GE-Proton/CachyOS; buttons "I understand, show them"/"Don't show them"). | `false` | global | L | adds `steamapps/common` of each Steam library to Proton discovery | `SC/ShowValveProton.tsx:7-61`, `src/backend/utils/compatibility_layers.ts:154-165` |
| EOS Overlay panel | — (legendary app `98bc04bc842e4906993fd6d6644ffb8d`) | **Status line:** "is installed", "is not installed", "is being installed...", or "is being updated...". **When installed:** "Current Version: x" and "Latest Version: y". **Buttons (conditional):** *Check for updates* when versions are equal or a check is running; *Update* when they differ; *Enable/Disable* (**W only**, global enable state); *Uninstall* when not busy; *Install* when not installed; *Cancel* while busy. | n/a | global | always (all platforms); Enable/Disable only on W | `getEosOverlayStatus`, `getLatestEosOverlayVersion`, `isEosOverlayEnabled` (W), `installEosOverlay` (install, and also update), `removeEosOverlay`, `enableEosOverlay('')`, `disableEosOverlay('')`, `updateEosOverlayInfo`, `abort(appName)`. Busy state is read from `libraryStatus` at mount. After install on W it assumes the overlay is enabled. | `SS/AdvancedSettings/index.tsx:40-170,203-315` |
| Experimental Features | `experimentalFeatures` (`{enableHelp, cometSupport, zoomPlatform}`) | Three toggles: "Help component", "Comet support", "Zoom Platform support (only Linux)". `umuSupport` exists in the type but is not shown. | `{enableHelp:false, cometSupport:true, zoomPlatform:false}` | global | always (the Zoom toggle is not hidden off L) | Also `handleExperimentalFeatures(newFeatures)` updates global state. **Bug:** it sets `zoom.enabled` to the whole object. | `SC/ExperimentalFeatures.tsx:8-62`, `src/frontend/state/GlobalState.tsx:522-527` |
| Custom CSS Style | `customCSS` | Uncontrolled textarea (5 rows) plus a warning ("…can render Heroic unusable… changed manually in `~/.config/heroic/config.json`…"). Each keystroke saves and applies live. | `''` | global | always | `window.setCustomCSS(css)` writes `#customCSS`. Loaded at startup via `getCustomCSS`. | `SC/CustomCSS.tsx:5-32`, `src/frontend/index.tsx:41-50` |
| Clear Cache | — | Danger button "Clear Heroic Cache" with a Details box (clears: third-party game info such as scores, Steam compat, HLTB, PCGW, AGW; Legendary, GOG and Amazon library info. Does **not** delete login, installed games, game settings or Heroic config). No confirmation. | — | global | always | Removes `localStorage.updates`, calls `clearCache(true)` (the backend clears the caches, sends `refreshLibrary`, and shows a "Cache Cleared" dialog), then `refreshLibrary({runInBackground:true})`. | `SC/ClearCache.tsx:7-79`, `src/backend/main.ts:670-686` |
| Reset Heroic | — | Danger button with a Details box | — | global | always | Confirm dialog "Reset Heroic" (Yes/No). `resetHeroic` deletes `GamesConfig/` and `config.json`, then relaunches after 1 s. Portable builds need a manual restart. | `SC/ResetHeroic.tsx:7-34`, `src/frontend/state/GlobalState.tsx:500-512`, `src/backend/utils.ts:404-414` |
| Copy All Settings to Clipboard | — | Footer button. The label changes to "Copied to Clipboard!" (success style) for 3 s. | — | global | always | `clipboardWriteText(JSON.stringify(config, null, 2))` | `SS/AdvancedSettings/index.tsx:56-66,337-363` |

### Per-game settings modal

- **Entry points.** All three types use `useGlobalState` actions.
  - Settings: the game card context menu "Settings" (shown when installed, not uninstalling, not a Browser game, `src/frontend/screens/Library/components/GameCard/index.tsx:363-368`), the card's hover gear icon (same conditions, `:456,548-556`), and the game page gear `SettingsButton` (hidden for Browser games, `src/frontend/screens/Game/GamePage/index.tsx:422`, `src/frontend/screens/Game/GamePage/components/SettingsButton.tsx:10-27`).
  - Logs: the card menu "Logs" (`GameCard/index.tsx:370-374`) and the game page "Report issue" (`src/frontend/screens/Game/GamePage/components/ReportIssue.tsx:13-23`).
  - Categories: the card menu "Categories" (`GameCard/index.tsx:404-408`) and the game page submenu (`src/frontend/screens/Game/GameSubMenu/index.tsx:399`).
- **State.** `settingsModalProps = {isOpen, type:'settings'|'log'|'category', gameInfo}` with `openGameSettingsModal`, `openGameLogsModal`, `openGameCategoriesModal` and `closeSettingsModal` (`src/frontend/state/GlobalStateV2.ts:22-79`). `SettingsModalWrapper` is mounted once in `src/frontend/App.tsx:95`.
- **Dialog.** Header `"<title> (Settings|Log|Categories)"` and a close button. The body wraps a per-game `SettingsContext` from `useSettingsContext({appName, gameInfo, runner})` and renders `GamesSettings`, `LogSettings` or `CategorySettings` (`SC/SettingsModal/index.tsx:25-80`). Closing re-triggers a settings reload elsewhere (the hook depends on `isOpen`, `src/frontend/screens/Game/GamePage/index.tsx:254`).
- **Differences from Game Defaults.**
  - Per-game only: Tools, Ignore updates, Offline mode, Alt EXE, Launch options, Game args, Steam Runtime (native), Disable UMU, the Cloud Saves tab, and the AppName footer link.
  - Hidden per-game (defaults only): none of the GamesSettings fields. The default-hint banner is hidden.
  - The tab set depends on the game (native, Browser, runner).

### Categories modal (per-game, `type:'category'`)

Source: `SS/CategorySettings/index.tsx:15-180`. Data lives in frontend `configStore['games.customCategories']` as `{category: ["<appName>_<runner>", …]}` (`src/frontend/state/GlobalState.tsx:422-488`).

- **New category.** A text input (max 33 characters) plus "Add new category". The button is disabled if the trimmed name is empty or already exists. Adding creates the category **and** assigns the current game to it.
- **Category grid.** A 2-column grid of all categories. Each has a toggle (assign/unassign this game) and a red delete icon.
- **Delete.** Opens the confirm "Warning: Proceeding will permanently remove this category and unassign it from all games. Continue?" with Remove Category/Cancel. If the deleted category was the only active library filter, the filter is cleared.

### Log viewer

Sources: `SS/LogSettings/index.tsx:19-312` (global `/settings/log` and per-game modal `type:'log'`), backend `src/backend/logger/ipc_handler.ts:19-37`.

| Aspect | Behaviour | Ref |
|---|---|---|
| Header | "How to report a problem?" plus instructions (join Discord, `#-support`, read pinned FAQ) | `SS/LogSettings/index.tsx:194-200` |
| Log selector (global only) | A button group. Fixed entries: **Heroic** `{}`, **Epic/Legendary** `{runner:'legendary'}`, **GOG** `{runner:'gog'}`, **Amazon/Nile** `{runner:'nile'}`, and **Zoom** `{runner:'zoom'}` (only when the Zoom experimental feature is on). Then one entry per **installed** game from epic, gog, amazon, zoom and sideloaded libraries, sorted by title, using `overrides.title` when set: `{appName, runner}`. The selected entry is highlighted. Switching shows a spinner until the first fetch. | `SS/LogSettings/index.tsx:83-103,172-227` |
| Per-game context | `showLogOf={appName, runner}` with no selector; the layout uses the `game-log` class | `SS/LogSettings/index.tsx:76-81,201-205` |
| Fetch / tail | `getLogContent(args)` runs immediately and then **every 1000 ms** (polling; a ref guard prevents overlapping fetches). The backend reads the **last 1 MiB** of the file. An empty result shows "No log file found"; an exception shows "Internal error reading log file". | `SS/LogSettings/index.tsx:105-139`, `src/backend/logger/ipc_handler.ts:19-27` |
| Rendering | Splits on `\n`. If over 1000 lines, it keeps the last 1000, prepends `...`, and shows "Log truncated, last 1000 lines are shown!". Each line is classed `log-error` if it contains `' err'`, `log-warning` if `' warn'` (case-insensitive), else `log-info`. No search, wrap toggle, auto-scroll control or level filter. | `SS/LogSettings/index.tsx:23-68` |
| Footer buttons (only when the file exists) | **Show log file in folder** (`showLogFileInFolder(args)`; backend `shell.showItemInFolder`). **Upload log file** (opens the upload dialog with a descriptive name: "General Heroic log", "Game log of {{gameTitle}}", "Epic Games / Legendary log", "GOG log", "Amazon / Nile log" or "Zoom log"). **Join our Discord** (`openDiscordLink`). | `SS/LogSettings/index.tsx:145-171,237-288` |
| Footer button (global only) | **Show uploaded log files**, which opens the uploaded-logs dialog | `SS/LogSettings/index.tsx:289-308` |

**Upload dialog** (`src/frontend/components/UI/LogFileUploadDialog/index.tsx:8-127`) is a state machine:

1. **Confirm**: "Upload log file?" / `Do you really want to upload "{{name}}"?` with Yes/No.
2. **Uploading**: a spinner.
3. **Error**: "Upload failed", with advice to upload `launch.log` manually.
4. **Done**: "Uploaded to {{url}} (URL copied to your clipboard)".

The upload uses `uploadLogFile(name, args)` and returns `[url, …]`. The URL is written to the clipboard with `navigator.clipboard`. The backend uploads to `https://dpaste.com/api/v2/` (`src/backend/logger/uploader.ts:19,56-73`).

**Uploaded logs dialog** (`SS/LogSettings/components/UploadedLogFilesList/index.tsx:70-120`, mounted in `src/frontend/App.tsx:98`):

- A table with columns Log title, Upload date (relative: "Uploaded {{n}} minutes/hours ago"), and Actions. The only action is **Open** (`openExternalUrl(url)`). Rows are sorted newest first. Empty state: "No log files were uploaded".
- Data comes from `getUploadedLogFiles` plus the `logFileUploaded`/`logFileUploadDeleted` events.
- **Delete is implemented but commented out** in the UI (`deleteUploadedLogFile(url)`, label "Request log file deletion", `:46-64`). Parity decision required.

### Sync saves (per-game "Cloud Saves Sync" tab)

Dispatcher: `SS/SyncSaves/index.tsx:11-83`. Shared pieces:

- **Manual sync modes.** Select "Manual Sync Saves" with options Download `--skip-upload`, Upload `--skip-download`, Force Download `--force-download` and Force Upload `--force-upload`. The default selection is `--skip-upload`. The sync button reads "Sync", or "Syncing" while busy.
- **Output.** The result appears in a `ProgressDialog` titled "Sync-Saves" with output lines and a close button.
- **Toggles.** **Autosync Saves** (`autoSyncSaves`, inherited default; disabled until a path is set). **Enable Quick Save-Sync Menu on game page** (`enableQuickSavesMenu`, default `false`; only shown once a path is set).
- **Help and warnings.** An `InfoBox` with help parts 1–4; part 2 (the prefix/username change caveat) is hidden on W. A yellow warning "Cloud Saves feature is in Beta, please backup your saves…".

| Store | Key(s) | Detection | Manual sync | Unsupported states | Ref |
|---|---|---|---|---|---|
| Legendary (Epic) | `savesPath` (string) | On mount, and when `winePrefix` or `isProton` changes, or on retry: if `savesPath` is empty (or on retry) it calls `getDefaultSavePath(appName,'legendary',[])`. While detecting it shows "Trying to detect the correct save folder (click to cancel)"; clicking cancels the spinner (not the request). | `syncSaves({arg, path:savesPath, appName, runner:'legendary'})` via the `frontend/helpers.syncSaves` wrapper | `!gameInfo.cloud_save_enabled` gives the warning "This game does not support Cloud Saves." | `SS/SyncSaves/legendary.tsx:31-204`, `src/frontend/helpers/index.ts:31-44` |
| GOG | `gogSaves` (`{name, location}[]`, one path box per save location) | When the feature is supported and the game is not Linux-native: always `getDefaultSavePath(appName,'gog',gogSaves)` on mount and on retry (retry first clears `gogSaves`). An empty result becomes one blank `{name:'',location:''}` entry. | `syncGOGSaves(gogSaves, appName, syncType)` returns stderr text | Linux-native: red text "Linux native games do not support GOG's Cloud Saves feature. Use the Windows version instead." Unsupported: "Cloud Saves are not supported by this game." | `SS/SyncSaves/gog.tsx:30-239` |

Path boxes (`PathSelectionBox` type directory, placeholder "Select the exact save games folder", dialog title "Choose the saves directory") are disabled while syncing. The clickable hint under each box reads "Please check twice if the path is correct (click to retry)" when a path is set, or "Save folder not found, please select it manually (click to retry)" when empty; clicking re-runs detection. Sync and autosync are disabled until every GOG location has a path (autosync additionally needs every `name` non-empty).

### System information (`/settings/systeminfo`)

Source: `SS/SystemInfo/index.tsx:89-137` plus cards.

| Item | Content | Ref |
|---|---|---|
| Data | `systemInfo.get(false)` (channel `getSystemInfo`, `cache=false`, so it is re-scanned on every visit). A spinner shows until loaded. | `SS/SystemInfo/index.tsx:92-95`, `src/backend/utils/ipc_handler.ts:22` |
| CPU card | Vendor logo (NVIDIA, AMD, Intel or Apple, matched by substring) plus "`{{cores}}x {{model}}`" | `SS/SystemInfo/cpu.tsx:10-33`, `SS/SystemInfo/vendorLogo.tsx:14-28` |
| Memory card | Determinate progress bar plus "`{{percent}}% used ({{used}} / {{total}}`" (missing closing parenthesis in the source string) | `SS/SystemInfo/memory.tsx:13-38` |
| GPU card(s) | One per GPU, titled "GPU N:" when there are several. Shows the vendor logo, `deviceString`, `DID=… VID=…, DSID=… VSID=…` and "Driver: x". | `SS/SystemInfo/gpu.tsx:18-57` |
| Steam Deck | When `steamDeckInfo.isDeck`: a "System Model: Steam Deck {{model}}" card, with the detailed specs collapsed in a `<details>` "Show detailed system specifications". | `SS/SystemInfo/index.tsx:54-87,107-113` |
| OS card | OS logo plus name (with " (inside Flatpak)" when FP) and "Version x" | `SS/SystemInfo/os.tsx:32-63` |
| Software card | Versions of Heroic, Legendary, Gogdl, Comet and Nile | `SS/SystemInfo/software.tsx:16-78` |
| Copy to clipboard | Button that calls `systemInfo.copyToClipboard()` (channel `copySystemInfoToClipboard`). The backend formats the info and writes it to the clipboard. No visual confirmation. | `SS/SystemInfo/index.tsx:126-133`, `src/backend/utils/ipc_handler.ts:23-27` |

### Shared form-control behaviours to replicate

| Control | Behaviour | Ref |
|---|---|---|
| ToggleSwitch | Hidden checkbox plus label. Props: `disabled`, `fading` (busy look), `description` (tooltip). `aria-label` is the title. RTL aware. | `src/frontend/components/UI/ToggleSwitch/index.tsx:17-53` |
| SelectField | MUI Select with an optional label, a `prompt` empty option, `afterSelect` slot (buttons or info), and `disabled`. RTL alignment. | `src/frontend/components/UI/SelectField/index.tsx:19-59` |
| SliderField | MUI Slider with explicit `marks` or optional min/max marks, value label on hover/drag | `src/frontend/components/UI/SliderField/index.tsx:21-89` |
| TextInputField | Uses a native `input` event listener (for virtual keyboard support). Props: `afterInput`, `inputIcon`, `warning` (shown when the value is non-empty). Passes through `maxLength`, `type` and `onBlur`. | `src/frontend/components/UI/TextInputField/index.tsx:21-76` |
| TextInputWithIconField | TextInputField plus a leading icon button | `src/frontend/components/UI/TextInputWithIconField/index.tsx:20-43` |
| PathSelectionBox | Editable text that commits **on blur**. The icon button is a Folder (opens `openDialog` with `openDirectory` or `openFile`, title, default path and filters) or, when a path is set and `noDeleteButton` is false, a Backspace that clears the path. Props: `canEditPath`, `disabled`. | `src/frontend/components/UI/PathSelectionBox/index.tsx:33-96` |
| InfoIcon | A circle-info icon plus inline text; the icon's `title` attribute repeats the text | `src/frontend/components/UI/InfoIcon/index.tsx:10-17` |
| FormControl | A wrapper with select caret, clear (×) button, side and left buttons, segmented and small variants. Not used by the settings screens directly. | `src/frontend/components/UI/FormControl/index.tsx:18-68` |

## Wine manager

Route `/wine-manager` (`src/frontend/App.tsx:168`). The sidebar entry is hidden on **W** (`src/frontend/components/UI/Sidebar/components/SidebarLinks/index.tsx:240-247`). Source: `src/frontend/screens/WineManager/index.tsx:23-268`.

### Tabs / repositories

| Platform | Tabs (in order) | Default tab | Ref |
|---|---|---|---|
| L | **Proton-CachyOS** (`proton-cachyos`), **GE-Proton** (`protonge`) | Proton-CachyOS | `src/frontend/screens/WineManager/index.tsx:44-66` |
| M | **Game-Porting-Toolkit** (`gpt`), **Wine-Crossover** (`winecrossover`), **Wine-Staging-macOS** (`winestagingmacos`) | Intel Mac: Wine-Crossover; Apple Silicon: Game-Porting-Toolkit | same |

- **Legacy types.** `Wine-GE`, `Proton` (Valve), `Wine-Lutris` and `Wine-Kron4ek` still exist in the `Type` union and in the downloader's `Repositorys` enum, but **no tab exposes them**. Previously installed versions of these types are kept in the cache only while their install dir exists (`src/common/types.ts:756-791`, `src/backend/wine/manager/utils.ts:143-150`). Wine-GE is therefore not a current tab.
- **Per-tab explanation.** Each tab shows an info line:
  - GE-Proton: "…created by Glorious Eggroll… meant to be used along with the umu launcher".
  - GPTK: "…developed by Apple… recommended for Apple Silicon… DX12".
  - Wine-Crossover: "…recommended for Intel Macs and for DX11 or older games".
  - Wine-Staging-macOS: "…especially when paired with the DXMT tool".
  - Proton-CachyOS: "…includes extra tools like DXVK-Sarek and D7VK".
  (`src/frontend/screens/WineManager/index.tsx:103-158`)

### List, search, refresh

| Aspect | Behaviour | Ref |
|---|---|---|
| Data source | Frontend electron-store `wine-downloader-info` → `wine-releases`, filtered by the tab's `type`. It is re-read on the `wineVersionsUpdated` event. | `src/frontend/screens/WineManager/index.tsx:72-101`, `src/frontend/helpers/electronStores.ts:121-125` |
| Header toolbar | Title "Wine Manager". A search box ("Search versions..."): case-insensitive substring match on `version`, cleared on tab change. A **Refresh** icon (spins while `refreshingWineVersions`). A **Settings** gear (L only). | `src/frontend/screens/WineManager/index.tsx:169-199,160-165` |
| Refresh | `refreshWineVersions(true)` calls `refreshWineVersionInfo(fetch=true)`. The backend fetches up to 50 releases per repository for the platform (L: PROTONGE and PROTONCACHYOS; M: WINECROSSOVER, WINESTAGINGMACOS and GPTK), merges them with the cache (keeping install state, flagging `hasUpdate` when the download URL or checksum changed), sorts by date desc, stores, and emits `wineVersionsUpdated`. On failure it shows the notification "Couldn't fetch releases from upstream, maybe because of Github API restrictions! Try again later." | `src/frontend/state/GlobalStateV2.ts:86-104`, `src/backend/wine/manager/utils.ts:143-245`, `src/backend/wine/manager/ipc_handler.ts:47-55` |
| Automatic refresh | When `releasesInfoReady` fires, the backend re-fetches only the repos whose cached `*-latest` entry is older than the upstream release or targets the wrong architecture. `getAlternativeWine` also triggers a full fetch when the cache is empty. | `src/backend/wine/manager/ipc_handler.ts:62-67`, `src/backend/wine/manager/utils.ts:87-141`, `src/backend/config.ts:191-206` |
| States | While refreshing: "Loading wine versions...". Empty: "No Wine versions found. Please click the refresh icon to try again." No search match: "No Wine versions match your search." | `src/frontend/screens/WineManager/index.tsx:225-260` |
| Columns | Wine Version (a button that opens the release notes via `openWebviewPage(release_notes_link)`) \| Release Date \| Size \| Action | `src/frontend/screens/WineManager/index.tsx:232-241`, `src/frontend/screens/WineManager/components/WineItem/index.tsx:105-171` |

### Row actions and progress (`WineItem`)

Rows without `version` or `downsize` are not rendered (`src/frontend/screens/WineManager/components/WineItem/index.tsx:51-53`).

| State | Size column | Actions | IPC | Ref |
|---|---|---|---|---|
| Not installed | `size(downsize)` | **Install** (down icon) | `installWineVersion(release)`. The backend notifies "start install" and streams progress. The install dir is `tools/wine` for Wine types, `tools/game-porting-toolkit` for GPTK, and `tools/proton` for Proton (or `<defaultSteamPath>/compatibilitytools.d` when `downloadProtonToSteam` is on and `steam.sh` exists there). Finished, error and canceled states raise notifications. | `WineItem/index.tsx:59-73,165-169`, `src/backend/wine/manager/ipc_handler.ts:15-45`, `src/backend/wine/manager/utils.ts:255-330` |
| Downloading | `size(downsize)` plus ` NN.N%`; the row background shows a progress bar via the CSS var `--progress` | **Abort** (stop icon) | `abort(version)`. Progress comes from the `progressOfWineManager` event `(version, {status:'downloading', percentage, avgSpeed, eta})` into the zustand store keyed by version. `avgSpeed` and `eta` are **not displayed**. | `WineItem/index.tsx:35-49,124-138`, `src/frontend/screens/WineManager/state.ts:4-13` |
| Unzipping | "Unzipping"; the bar is held at 100% | **Abort** | `abort(version)` | `WineItem/index.tsx:38-43,92-94` |
| Installed | `size(disksize)` | **Open Folder** (`showItemInFolder(installDir)`), **Update** (only if `hasUpdate`; re-runs `installWineVersion` with overwrite), **Uninstall** (trash icon, **no confirmation**) | `removeWineVersion(release)`; the backend sends an "uninstalled" notification | `WineItem/index.tsx:55-89,139-164`, `src/backend/wine/manager/ipc_handler.ts:57-60` |

### Wine Manager settings modal (L only)

The gear button opens a dialog "Wine Manager Settings" with a global settings context (`src/frontend/screens/WineManager/components/WineManagerSettingsModal.tsx:19-42`):

| Field | Key | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Custom Wine/Proton Paths | `customWinePaths` (`string[]`) | A select listing the paths (disabled when empty; the first path is selected by default). A red **Remove Path** icon removes the selected path. A green **Add New Path** icon opens a file dialog ("Select the Wine or Proton Binary") and appends the path if it is new. | `[]` | global | `isDefault`, not W; in practice only reachable here, i.e. L only | `openDialog`. The backend scans these in `getAlternativeWine` (`getCustomWinePaths`). | `SC/CustomWineProton.tsx:11-106`, `src/backend/utils/compatibility_layers.ts:51` |
| Download GE-Proton to Steam directory | `downloadProtonToSteam` | Toggle plus info ("…It will use the Steam path set in the 'Default Steam path' setting above.") | `false` | global | `isDefault`, L | used by `getInstallDir` for Proton types | `SC/DownloadProtonToSteam.tsx:9-42`, `src/backend/wine/manager/utils.ts:255-282` |

## Accessibility screen

Route `/accessibility` (`src/frontend/App.tsx:176`). The sidebar link is always shown (`SidebarLinks/index.tsx:258-263`). Source: `src/frontend/screens/Accessibility/index.tsx:22-269`. The page wraps itself in a global `SettingsContext`, which only `disableSmoothScrolling` uses. Every other value lives in the frontend electron-store `configStore` (`src/frontend/helpers/electronStores.ts:112-114`) and in `GlobalState`; none of them is an `AppSettings` key.

| Field | Key / storage | Control/options | Default | Scope | Visibility conditions | IPC/side effects | Ref |
|---|---|---|---|---|---|---|---|
| Zoom (`{{zoomPercent}}%`) | `configStore.zoomPercent` | Native range input 60–200, step 10, tick labels 60/80/…/200 | `100` | global (app-level) | always | Saves to the store immediately. `setZoomFactor(percent/100)` is **debounced 500 ms**. The backend converts the factor to an Electron zoom level as `(f-1)/0.2`, scaled down further when the primary display's work area is narrower than 1200 px, and re-applies it on maximize, unmaximize and restore. | `src/frontend/screens/Accessibility/index.tsx:84-141`, `src/frontend/state/GlobalState.tsx:293-303`, `src/backend/main.ts:269-307` |
| Content Font Family (Default: Cabin) | `configStore.contentFontFamily` → CSS var `--secondary-font-family` | Select of fonts: the default secondary font, the default primary font, then de-duplicated system font families from `queryLocalFonts()`. Each option is rendered in its own font. | `'Cabin', sans-serif` | global | always | Sets the CSS custom property live | `src/frontend/screens/Accessibility/index.tsx:46-107,149-160`, `src/frontend/state/GlobalState.tsx:313-319`, `src/frontend/styles/_typography.scss:17-20` |
| Actions Font Family (Default: Rubik) | `configStore.actionsFontFamily` → `--primary-font-family` | Same list | `'Rubik', sans-serif` | global | always | live | `src/frontend/screens/Accessibility/index.tsx:162-173`, `src/frontend/state/GlobalState.tsx:305-311` |
| Select Theme plus Custom Themes Path | `configStore.theme`, `customThemesPath` | Same `ThemeSelector` as the General section (duplicated) | `midnightMirage`, `''` | global | always | See the General section | `src/frontend/screens/Accessibility/index.tsx:175`, `src/frontend/components/UI/ThemeSelector/index.tsx:27-120` |
| Show all game tiles in color | `configStore.allTilesInColor` | Toggle | `false` | global | always | Library tiles of uninstalled games are no longer greyed out (`GamesList` prop) | `src/frontend/screens/Accessibility/index.tsx:177-191`, `src/frontend/screens/Library/components/GamesList/index.tsx:52,123` |
| Always show titles in library | `configStore.titlesAlwaysVisible` | Toggle | `false` | global | always | `GamesList` prop | `src/frontend/screens/Accessibility/index.tsx:193-207` |
| Disable closing dialogs by clicking outside | `configStore.disableDialogBackdropClose` | Toggle | `false` | global | always | `Dialog` ignores `backdropClick` | `src/frontend/screens/Accessibility/index.tsx:209-223`, `src/frontend/components/UI/Dialog/components/Dialog.tsx:44-64` |
| Disable smooth scrolling (requires restart) | `disableSmoothScrolling` (**AppSettings**) | Toggle | `false` | global | always | `setSetting`. At startup the backend appends the Chromium switch `disable-smooth-scrolling`. | `src/frontend/screens/Accessibility/index.tsx:49-52,225-239`, `src/backend/main.ts:369-371` |
| Disable UI animations | `configStore.disableAnimations` | Toggle | `false` | global | always | `App` adds the `disableAnimations` class to the root element | `src/frontend/screens/Accessibility/index.tsx:241-255`, `src/frontend/App.tsx:33,77` |

Theme application details to replicate:

- A custom theme is a `.css` file in `customThemesPath`, listed by `getCustomThemes` (`*.css` only).
- Selecting one injects the file's CSS and sets the body class to the filename with `.css` removed and the first `[\s.]` replaced by `_`.
- Built-in themes are body classes.
- On Windows-controls-overlay platforms, the title bar colours are read from the CSS vars `--titlebar-height`, `--titlebar-color` and `--titlebar-symbol-color`.

(`src/frontend/index.tsx:134-169`, `src/backend/main.ts:1303-1323`)

### UX pain points (Settings, Wine manager, Accessibility)

**Information architecture**
1. **Long flat lists with no grouping.** General has about 25 unrelated controls in one column: language, paths, tray, shortcuts, analytics, library layout, download workers. Advanced mixes developer overrides (alternative binaries), store-specific toggles, gamepad timing, EOS overlay management, Custom CSS and destructive actions. There are no sub-headings, search, or "restart required" grouping.
2. **Settings spread across five places with overlapping concepts.** Global, Game Defaults, the per-game modal, Accessibility, and the Wine Manager gear. The theme selector appears twice (General and Accessibility). Custom Wine paths and "Download GE-Proton to Steam" sit behind a small gear in Wine Manager, far from "Default Steam path" in General, which their help text refers to as "above".
3. **Store-specific options are not labelled by store.** Download without HTTPS only affects Legendary. Disable GOG presence and playtime sync affect only GOG. Offline mode is Legendary-only. EGL sync is Epic-only. Max workers is Legendary-only. A redesign should group these by store or tag them.
4. **The Legacy tab** holds a DXVK FPS limit that, on macOS, is instead shown in the Wine tab. Placement depends on platform for no clear reason.
5. **Hidden routes.** `/settings/sync` exists but is unreachable. The global log list only covers installed games.

**Defaults and inheritance**

6. **Copy-on-first-write inheritance is invisible.** After a game's config is first written, which happens at install, changing Game Defaults no longer affects that game. The only explanation is one hint sentence. The per-game modal does not show which values are inherited or overridden, and offers no reset per field or for the whole game.
7. **Factory defaults and UI fallbacks disagree.** For example `autoUpdateGames` (false vs true), `gamepadInitialRepeatDelay` (300 vs 250), and `enableEsync` (true vs false). Many keys have no backend default at all. The UI can therefore show a different value from what the backend uses until the user touches the control.
8. **Key collision.** "Preferred Language" in Game Defaults edits the global `language` key, which is also the app UI language. The `enviromentOptions` key is misspelled; it must be kept for config compatibility.
9. **Selecting a Wine version can write settings without user action.** `WineVersionSelector` silently writes `wineVersion` when the list has one entry or when it re-matches by binary path. DXVK, VKD3D and NVAPI toggles in Game Defaults call the backend with `appName:'default'`, mutating a prefix even though they are meant as defaults.

**Platform and conditional visibility**

10. **Controls appear and disappear with no explanation.** Many depend on platform, native status, wine type (proton, crossover, toolkit, `-DXMT` name suffix) and runner. Examples: DXVK disappears for Proton, everything disappears for CrossOver, and NVIDIA Prime only shows with two or more GPUs. Hiding instead of disabling with a reason makes options hard to find and makes support harder.
11. **Remembered tab can be invalid.** The per-game last tab is stored in `localStorage` and may point at a tab that is no longer available, which renders an empty panel.
12. **Dependencies are implicit.** VKD3D and NVAPI are disabled unless DXVK is on, and DXVK is locked while either is on. Tray toggles depend on "Hide tray icon". EAC needs GameMode on Flatpak, enforced through modal prompts. The UI does not visualise these relationships.

**Feedback, safety and validation**

13. **Inconsistent save behaviour.** Most fields save instantly per keystroke. That includes the SteamGridDB key and Custom CSS, where a half-typed CSS can break the UI live. Path boxes and Gamescope "Additional Options" save on blur. The wrappers table requires an explicit "+". There is no saved or failed feedback (`setSetting` is fire-and-forget).
14. **Weak validation.** The DXVK FPS cap is a free string. Game arguments show errors but still save. Gamescope numbers only block non-digits, with no range checks. The gamescope "Upscale Method" and "Window Type" labels are untranslated, and one option is misspelled ("Interger").
15. **Destructive actions are uneven.** Clear cache has no confirmation. Wine version uninstall has no confirmation. Env var row delete has no confirmation, while Clear All does. Reset Heroic deletes all settings and relaunches.
16. **Alternative binary path errors.** An invalid alternative binary is auto-cleared after 3 s with only a transient "Invalid" label.
17. **Bugs worth not replicating.** The always-true guard in `setSetting`. `handleExperimentalFeatures` sets `zoom.enabled` to an object. The memory label is missing its closing parenthesis. The Zoom experimental toggle is visible on non-Linux. Custom themes path uses a whole-config `writeConfig` that can clobber concurrent changes.

**Log viewer and diagnostics**

18. **Log viewer is basic.** It polls the last 1 MiB every second and shows only the last 1000 lines, coloured by naive `' err'`/`' warn'` substring matching. There is no search, level filter, pause/follow toggle, copy button or wrap toggle. The upload deletion feature exists in the backend but is disabled in the UI.
19. **System info copy has no confirmation.** The other copy buttons show a "Copied" state.

**Wine manager**

20. **Release metadata gaps.** Download speed and ETA are received but not shown. Release notes open in a webview with no in-app preview. Only the cached list is shown until a manual refresh. Uninstall has no confirmation. Nothing shows which games use a version before removing it.
21. **Settings and Valve Proton visibility are hard to find.** The settings gear is Linux-only, so macOS users cannot manage custom Wine paths in the UI. The "show Valve Proton" option lives in Global Advanced, far from the Wine version picker that it affects.

**Accessibility**

22. **Accessibility storage is fragmented.** Most settings live in a separate store from `AppSettings`, while `disableSmoothScrolling` is in `AppSettings`. Reset Heroic therefore does not reset zoom, fonts or theme.
23. **Missing controls.** Zoom is a coarse native range with no keyboard shortcut or reset-to-100 button. The font pickers have no reset-to-default button and depend on the Local Font Access API. There is no high-contrast or reduced-motion auto-detection from OS preferences.

---

## Download manager

Screen: `src/frontend/screens/DownloadManager/index.tsx`. Backend: `src/backend/downloadmanager/{downloadqueue.ts,ipc_handler.ts,utils.ts}`.

### Data model
- `DMQueueElement {type: 'install'|'update', params: InstallParams, addToQueueTime, startTime, endTime, status?: 'done'|'error'|'abort'|'paused'}` (`src/common/types.ts:631-639`).
- `DownloadManagerState` = `'idle'|'running'|'paused'|'stopped'` (`src/common/types.ts:809`).
- `getDMQueueInformation` (invoke) returns `{elements: queue, finished, state}` (`downloadqueue.ts:224-229`).
- The queue and finished list persist in the electron-store `download-manager`.

### Screen layout
1. A "Downloads" heading.
2. **ProgressHeader** (`components/ProgressHeader/index.tsx`):
   - A live area chart of the last 100 samples: download MB/s (filled) and disk MB/s (line). Below it are the current "Download" and "Disk" MB/s readouts. Samples come from `progressUpdate` (`downSpeed`, `diskSpeed`). The chart resets when idle.
   - A progress bar row, shown only when not idle and an ETA exists: `percent% [bytes]`, a determinate bar, and the ETA. The ETA text is replaced by a hard-coded, untranslated "Paused" when not running (`:108-136`).
3. **Downloading** section, only if a current element exists. Header columns: Game Title, Started at, Type, Store, Action. One row.
4. **Queued** section. Columns are the same, with "Added at". Rows hold the remaining queue elements. Empty state: "Nothing to download" (`DownloadManagerItem/index.tsx:52-58`).
5. **Completed** section, only if the finished list is non-empty. It has a "Clear List" button and the column "Finished at". Rows are sorted by `endTime` descending (`index.tsx:84-90`, `:157-184`).
6. While the first `getDMQueueInformation` is loading, a full spinner shows (`:64-66`).
7. The finished list is re-fetched whenever the queue length or current app changes (`:58-62`).

### Row content (`components/DownloadManagerItem/index.tsx:230-270`)
- Cover image (`art_cover` or `art_square`) and title, coloured by status:
  - done: success colour
  - error or aborted (and not current): danger colour
  - current: default colour
  - queued: accent colour
- `size | platform`, with `osx` shown as "Mac". "(Canceled)" is appended for errored or aborted items. The install path is the tooltip.
- Date and time: finish time, start time, or add time, depending on the section. The tooltip shows the full locale date.
- Type: "Install" or "Update". Store: "Epic Games", "GOG", "Amazon Games" or "Other" (`getStoreName`, `src/frontend/helpers/index.ts:124-135`).
- Clicking the title opens `/gamepage/<runner>/<appName>` with state `{fromDM: true, gameInfo}`. DLCs are not clickable.
- **Rows only render if the game is found in the epic, gog or amazon library** (`:60`, `:212-218`). Zoom, sideload and GOG redist queue entries are therefore invisible in the list.

### Actions
| Where | Control | Behaviour | IPC |
|---|---|---|---|
| Current row | Stop (main) | Opens the dialog "Stop Installation – Do you wish to KEEP downloaded files?" with buttons **Keep Installing** (close), **Yes** (cancel and keep files; the progress is saved to localStorage `<appName>` so a later install continues the percentage) and **No** (cancel and delete files) (`src/frontend/helpers/library.ts:102-133`) | `cancelDownload(false)` / `cancelDownload(true)` (send). The backend also removes the element's queued DLCs, aborts the process, removes it from the queue and, for No, `removeFolder(path, folder_name)` (`downloadqueue.ts:231-255`) |
| Current row | Pause / Resume (secondary) | Pause when `running`; resume when `idle` or `paused` | `pauseCurrentDownload` / `resumeCurrentDownload` (send). Pause aborts the process and sets state `paused`; resume re-runs `initQueue` (`downloadqueue.ts:257-273`) |
| Queued row | Remove | Removes from the queue immediately, with no confirmation | `removeFromDMQueue(appName)` (send). The backend also sends `gameStatusUpdate {status:'done'}` |
| Completed row (done) | Play-icon button | Navigates to the game page; it does **not** launch | — |
| Completed row (error/abort) | Stop-icon button | Removes the row from the finished list (frontend writes `download-manager.finished`) and also sends `removeFromDMQueue` | `removeFromDMQueue` |
| Completed header | Clear List | Empties the finished list (frontend writes the store directly) | — |

**Not supported** (verified): reordering or prioritising the queue, pausing or cancelling individual queued items other than removing them, "Update all" in the download manager, bandwidth limits, and retrying failed items.

### Backend queue behaviour to replicate
- **Adding** (`install`/`updateGame` invoke → `addToQueue`):
  - Sends `gameStatusUpdate: queued`.
  - Replaces an existing element with the same app and runner.
  - Computes `size` from `getInstallInfo`; it is "?? MB" for EA- or Ubisoft-managed games.
  - For Windows GOG installs, prepends a `gog-redist` element when there are dependencies or the redist is missing.
  - For legendary, each selected DLC is enqueued as its own install (`ipc_handler.ts:24-43`).
  - Starts `initQueue` if idle (`downloadqueue.ts:118-196`).
- **Processing** is sequential: one element at a time. `startTime` and `endTime` are recorded. When an element finishes and the queue is not paused, it moves to `finished` (deduplicated by appName) (`:82-116`).
- **Auto-pause on connectivity**: going offline auto-pauses a running download, and coming back online auto-resumes it (`:29-39`).
- **Install and update preconditions** (`downloadmanager/utils.ts:29-50`, `:115-135`):
  - When offline, the element errors and is skipped silently, with only a log entry.
  - For Epic during a major outage, an ERROR dialog shows: "Epic Servers are having major outage…"
- **Notifications** are listed under Notifications & dialogs.
- The queue starts 5 s after `frontendReady` and is skipped with `--no-gui` (`src/backend/main.ts:467-480`).

---

## Login

Screen: `src/frontend/screens/Login/index.tsx`. Login routes: `/loginweb/legendary`, `/loginweb/gog`, `/loginweb/nile`, `/loginweb/zoom` (`:20-23`).

### Accounts page (`/login`)
- **Header**: Heroic logo and title, plus a **language selector** with flags and a "Help Improve this translation." link that sends `openWeblate` (`:100-113`).
- Message: "Login with your platform. You can login to more than one platform at the same time."
- **macOS < 12**: a disabled message reads "Your macOS version is {{version}}. macOS 12 or newer is required to log in.", and every login button is disabled. The check uses `systemInfo.get` (`:46-60`).
- **Runner cards** (`components/Runner/index.tsx`), one per store: Epic, GOG, Amazon, and Zoom if enabled.
  - Logged out: a "<Store> Login" button navigates to the login route.
  - Logged in: shows the username (truncated to 20 chars plus "..."; Amazon falls back to "Unknown") and a **Logout** button, which shows "Logging out..." while pending.
  - Epic only, when logged out: an extra card "epic Alternative Login Method" opens the SID modal.
- A **Go to Library** button runs `refreshLibrary({runInBackground:false})` and then navigates to `/` (`:78-81`).
- The page registers the help item `login`.

### Per-store flows (all in `screens/WebView/index.tsx` except the SID modal)
| Store | Start URL | Token interception | IPC | After |
|---|---|---|---|---|
| Epic (webview) | `https://www.epicgames.com/id/login?responseType=code`. The UA is set to `… EpicGamesLauncher` on `dom-ready` (`:78`, `:185-189`) | On `dom-ready`, if the page host is `localhost` and the URL has `?code=` (`:226-238`) | `login(code)` (invoke) → `LegendaryUser.login` | Spinner "Logging In..." → `/login` |
| Epic (alternative/SID) | Modal (`components/SIDLogin/index.tsx`) with steps: (1) open `https://legendary.gl/epiclogin` in the browser (`openLoginPage`), with a "Copy" button (`clipboardWriteText`) and an "Open" button; a link explains the SID (`openSidInfoPage` → wiki "How-To: Epic Alternative Login"); (2) paste the `authorizationCode` | Manual text input. Middle-click pastes via `clipboardReadText`. The Login button needs ≥30 chars | `login(code)`, then `getUserInfo` | On error the button reads "Error, try a different Code" for 2.5 s. Clicking the backdrop closes the modal |
| GOG | `https://auth.gog.com/auth?client_id=46899977096215655&redirect_uri=…embed.gog.com/on_login_success?origin=client&response_type=code&layout=galaxy` (`:87-88`) | On `dom-ready`, if the URL matches `embed.gog.com/on_login_success`, read `code` (`:204-216`) | `authGOG(code)` (invoke) | → `/login` |
| Amazon (nile) | `getAmazonLoginData` (invoke) returns `{client_id, code_verifier, serial, url}`, and the webview loads `url` ("Preparing Login..." meanwhile) (`:125-140`) | On `dom-ready`, read the query param `openid.oa2.authorization_code` (`:217-225`) | `authAmazon({client_id, code, code_verifier, serial})` (invoke). This is a device-registration (PKCE) flow | → `/login` |
| Zoom | `https://www.zoom-platform.com/login?li=heroic&return_li_token=true` | On `did-navigate`, if the URL has `li_token` (`:295-307`) | `authZoom(fullUrl)` (invoke; the backend also fetches user details), then `getZoomUserInfo` (invoke) | → `/login` |

After a successful login (`GlobalState.tsx:529-535`), the app sets localStorage `category='all'` and runs a foreground `refreshLibrary` for that runner.

**Logout** (`GlobalState.tsx:555-665`):
- Epic: `logoutLegendary` (invoke), then the whole window reloads.
- GOG: `logoutGOG` (send), then reload.
- Amazon: `logoutAmazon` (invoke), then reload.
- Zoom: `logoutZoom` (send), then reload.
- There is no confirmation for any store.

Webview partitions are persistent per store: `persist:epic|gog|amazon|zoom`, with the runner mapped to the store (`WebView/index.tsx:457`). Cookies therefore persist between sessions.

At startup, the backend drops the Epic user if `LegendaryUser.isLoggedIn()` is false once online, and refreshes GOG user details (`src/backend/main.ts:346-361`).

---

## WebView

Screen: `src/frontend/screens/WebView/index.tsx`. Controls: `src/frontend/components/UI/WebviewControls/index.tsx`. Guest preload: `src/webviewPreload/index.ts`.

### Pages
| Route | URL |
|---|---|
| `/store/epic` | `https://www.epicgames.com/store/<lang>/`. `pt` maps to `pt-BR` |
| `/store/gog` | `https://af.gog.com?as=1838482841` (affiliate) |
| `/store/amazon` | `https://gaming.amazon.com` |
| `/store/zoom` | `https://www.zoom-platform.com` |
| `/wiki` | `https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/wiki` |
| `/store-page?store-url=…` | any URL (from Deals and game pages) |
| `/loginweb/*` | see Login |

- **Last-visited URL per store**: when a navigation stays on the store's domain, the URL is saved to `sessionStorage last-url-<store>` and restored on the next visit; `last-store` is saved too (`:28-41`, `:109-115`, `:284-324`).
- **Webview element**: `allowpopups`, with preload from `getWebviewPreloadPath` (invoke). Nothing renders until the preload path is known (`:168-176`, `:438-440`).
- **Loading overlay**: "Loading Website" until `dom-ready`.
- **Connectivity shortcut**: on `page-title-updated`, if the app thinks it is offline, it sends `set-connectivity-online` (`:268-273`).
- **Not-logged-in warning** (`components/LoginWarning`): opening a store page while logged out of that store shows "You are NOT logged in… Don't use the store page to login, click the following button instead" with a **Log in** button pointing to the matching `/loginweb/*` (`:336-356`).
- **Adtraction blocked dialog**: if `track.adtraction.com` fails to load, the app loads the target GOG URL without a port. It warns once, "Adtraction is blocked…", with a "Don't show this warning again" toggle stored in localStorage `adtraction-warning` (`:242-262`, `:468-506`).
- **Mouse back/forward buttons** (buttons 3 and 4) navigate the webview (`:409-436`). The guest preload handles them too.

### Controls bar (`WebviewControls/index.tsx`)
- **Back**: goes back in the webview, or, if it can't, falls back to app `history.back()`.
- **Forward**: disabled when the webview can't go forward.
- **Reload** and **Home** (reloads the initial URL).
- A **read-only URL field** with warning styling when the URL is not `https://`.
- **Open in browser**: `openWebviewPage(url)` (send) → OS browser. The guard `!startUrl.startsWith('login')` is always true, so this is effectively always enabled.

### Gamepad inside webviews
- The host keeps focus. The guest shows a "virtual cursor" (orange outline) and receives commands over `webview.send('heroic-webview', cmd)`: `enter`, `exit`, `click`, `goBack`, `goForward`, `focusSearch`, `navigate(dir)` and `scroll(dir)`. See Gamepad navigation (`src/frontend/helpers/gamepad.ts:40-172`, `src/webviewPreload/index.ts:221-255`).
- B (back) leaves the webview and focuses the sidebar Stores item (`WebView/index.tsx:385-400`).

---

## Discounts

Screen: `src/frontend/screens/Discounts/index.tsx` ("Deals", route `/discounts`). Helpers: `screens/Discounts/helpers.ts`. Types: `src/common/types/discounts.ts`.

### Sources (fetched in parallel; one feed failing does not block the others, `:212-267`)
| Store | IPC | Backend source |
|---|---|---|
| GOG | `getGogDiscounts(localeSettings, hideOwned)` (invoke) | `https://catalog.gog.com/v1/catalog`. `hideOwned` requires GOG login (`src/backend/discounts/index.ts:20`, `:176`) |
| GMG (Green Man Gaming) | `getGmgDiscounts(currency)` (invoke) | Heroic-hosted JSON feed per currency on raw.githubusercontent (`src/backend/discounts/gmg.ts:10`, cached) |
| Humble | `getHumbleDiscounts()` (invoke) | Heroic-hosted feed (`src/backend/discounts/humble.ts:10`, cached) |
| GOG wishlist | `getGogWishlist()` (invoke) → product ids | `embed.gog.com/user/wishlist.json`. Only when logged into GOG, applied client-side (`:262-294`) |
| GOG region | `getGogDealsRegion()` (invoke) → `{countryCode, currencyCode}` from the GOG account | `embed.gog.com/userData.json` |

### Region and currency
- **Store region**, for the GOG and "all" tabs: a select with "Auto (CC · CUR)" plus 30 countries, sorted by localised name.
  - The override is stored in `discounts.regionOverride`.
  - Resolution order: override, then GOG account region, then a language→country/currency map, then US/USD (`helpers.ts:195-226`).
  - Changing the region **resets all filters** (`index.tsx:97-110`).
- **Currency**, for the GMG tab: "Auto" or one of AUD, BRL, CAD, CNY, EUR, GBP, KRW, TRL, USD. The override is stored in `discounts.gmgCurrencyOverride` (`helpers.ts:128-169`).
- The Humble tab has no selector; prices are in USD.

### Store tabs
"All stores" plus one tab per store that has items. Tabs only show when more than one store is present. A persisted tab for a store with no items falls back to "all" (`:390-400`, `:671-694`).

### Filters (`components/DiscountFilters/index.tsx`)
- A **filter bar** with a "More/Less filters" toggle (with a count of active hidden filters), **Reset**, a **view mode** switch (grid or list), and **search** by title.
- **GOG-only filters**, shown and applied only on the GOG tab or when only GOG items are present:
  - Wishlist Only and Hide Owned (both require GOG login; ignored when logged out, `:209-210`)
  - Hide DLCs
  - Genres (multi-select with chips, OR semantics)
  - Age rating "Suitable for age": All, or PEGI ≤ 3/7/12/16/18
  - Rating range 0–10 (GOG 0–50 scaled /5)
  - Release-year range
- **Always available**:
  - Features (multi-select, AND semantics)
  - Platform chips: Windows, Linux, macOS, Xbox (OR); only options present in the tab are shown
  - **Sort by**: Trending (server order), Biggest discount, Price low→high, Price high→low, Release new→old, Release old→new, Rating high→low, Rating low→high. Missing dates and unrated items sink to the bottom (`:504-566`)
  - **Per page**: 20, 50 (default) or 100
  - **Price range** slider, hidden on the mixed "all" tab when non-GOG stores are present (`:380`)
- **Persistence**: filters (except price and year ranges, which are re-seeded from the data) are saved to localStorage `discounts.filters` (`:169-200`).
- Any filter change resets to page 1.

### Cards (`components/DiscountCard/index.tsx`)
- Each card shows the cover (Humble uses a landscape layout), a −NN% badge, a ★ rating (one decimal), the title, a struck-through base price and the final price.
- A store badge carries a tooltip. GMG and Humble say the key is "for an external store, not installable through Heroic", with the DRM name when known.
- **Clicking**:
  - GOG opens the in-app webview `/store-page?store-url=<af.gog.com affiliate link>`.
  - GMG and Humble open the OS browser via `openExternalUrl`.
  - Affiliate rewriting: GOG gets `af.gog.com` plus `?as=`; Humble is wrapped in `humblebundleinc.sjv.io/heroic?u=`; GMG is already deeplinked (`helpers.ts:228-271`).

### Pagination (`components/DiscountPagination/index.tsx`)
Previous and Next buttons with page numbers and ellipses (all pages if ≤ 7). Hidden when there is a single page. Changing page scrolls to the top.

### States
- Loading: "Loading discounted games..."
- Error: "Could not load discounts. Please try again later."
- Empty: "No discounted games available right now."
- No matches: "No games match the current filters."

---

## Console Mode

Screen: `src/frontend/screens/ConsoleMode/index.tsx` (route `/console`). It is a "big picture" grid for controllers and keyboards.

### Entering and leaving
- **Enter**:
  - Sidebar item "Console Mode"
  - The gamepad **Guide/PS button** toggles `/console` and `/` from anywhere, except while a console launch is in progress (`helpers/gamepad.ts:290-295`)
  - The setting **Start in Console Mode** (`startInConsoleMode`, General settings; `screens/Settings/components/StartInConsoleMode.tsx`)
  - The CLI flag `--console`. The backend loads the start hash `/console` (`src/backend/main.ts:229-252`, `src/backend/constants/environment.ts:36`)
- **On mount** it sends `setFullscreen(true)`, and kicks off a background library refresh if all libraries are empty. On unmount it sends `setFullscreen(false)` (`:101-116`). This exits fullscreen even if the app was started fullscreen.
- **Leave**:
  - Esc when idle
  - The "Quit Console" button → `/`
  - Gamepad B (global `back` → `webContents.goBack()`)
  - The Guide button
  - "Quit App" (danger button) sends `quit` and exits Heroic (`:442-465`)
- The shell hides the sidebar, banner, help, dialogs and controller hints in console mode (`App.tsx:84-87`).

### Layout
1. **Top bar**:
   - Logo
   - Filter chips: "Installed" toggle, then store chips (All, Epic, GOG, Amazon, Other = sideload, ZOOM), only for stores that have games
   - Sort chip "A → Z" / "Z → A"
   - Quit Console and Quit App buttons
2. **Title bar**: the focused game's title.
3. **Grid**: square-art cards. Each card shows a "Needs update" badge, or an active-status overlay with label and progress bar for installing, updating, queued, launching, playing, uninstalling, moving, repairing, syncing-saves, extracting, redist and winetricks (`components/ConsoleCard/index.tsx:16-29`).
4. **Footer**: controller hints, only when a gamepad is connected and no launch is in progress: Launch, Quit, L1/R1 Change store, R2 Sort, D-pad/stick Navigate (`components/ControllerHints/index.tsx`).

Which games appear: all libraries plus sideloaded, excluding DLC, third-party-managed apps and **hidden games** (`:118-142`). They are sorted by title.

Empty grid text: "Loading your library…" while refreshing. Otherwise "No installed games to show. Install something first.", which shows even when the Installed filter is off.

### Navigation model
- **Keyboard**:
  - Grid: arrows move focus, using a column count computed from card positions (`hooks.ts:105-136`). Up from the first row goes to the top bar. Enter or Space activates.
  - Top bar: Left and Right cycle buttons; Down returns to the grid.
  - Esc quits console mode.
  - Mouse hover focuses a card; clicking a focused card activates it.
- **Gamepad**:
  - The global gamepad handler turns D-pad and stick into arrow keys and A into a click (see Gamepad navigation).
  - Console-specific polling hooks: L1 and R1 cycle store, R2 toggles sort (`:404-406`, `hooks.ts:16-46`).
  - Layout detection for button glyphs uses `controller.ts:26-34`. The **PS4 regex is tested before PS5, so PS5 is never detected**; Nintendo swaps A and B.

### Activation logic (`:247-276`)
| Game state | Result |
|---|---|
| queued | ConfirmDialog "Remove from queue? This game is queued for download…": Yes sends `removeFromDMQueue` and clears the localStorage progress; No closes |
| installing / updating | ConfirmDialog "Cancel installation?" / "Cancel update?": Yes sends `kill(appName, runner)` (invoke); No closes |
| not installed | **InstallOverlay** |
| installed and has an update | ConfirmDialog (game page strings "update" title and message): **Yes** runs `updateGame`, **No** launches without updating, **Cancel** dismisses |
| installed | **LaunchOverlay** |

### InstallOverlay (`InstallOverlay/index.tsx`)
- **Fields**:
  - Platform selector, only when more than one platform is available. Linux appears if native or sideload, macOS if native or sideload, Windows always. The default is native, else Windows.
  - Wine selector, when installing Windows on non-Windows. It uses `getAlternativeWine` (invoke) and is disabled if there is ≤ 1 option.
  - Read-only install path: the default from `requestAppSettings`. The path cannot be changed, and there are no DLC, language or SDL options.
- **Buttons**: Cancel and Install. Install is disabled if Wine is needed and none is chosen.
- **Install** writes the chosen `wineVersion` into the game config (`requestGameSettings` invoke + `writeConfig` invoke), then calls `install(...)` → `install` (invoke) (`:138-161`).
- **Input**: Up and Down move between rows; Left and Right cycle values or switch between the two buttons; Enter, Space or A activates; Esc, Backspace or B closes. The body class `console-modal-open` blocks the global back action.

### LaunchOverlay (`components/LaunchOverlay/index.tsx`)
- Fires `launch(...)` → `launch` (invoke) once on mount and closes itself when that promise settles (`:71-82`).
- Shows a spinner, a status label ("Syncing Saves", "Installing Redistributables (x)", "Applying Winetricks fixes", "Launching" or "Playing") and the game title.
- **Hold Esc or B for 3 s to cancel** (`kill` invoke). The hint "Hold [B/◯/Esc] for 3s to cancel" highlights while held (`:22`, `:39-67`, `components/BackHint`).
- The body class `console-launching` blocks Guide and back.

### ConfirmDialog (`components/ConfirmDialog/index.tsx`)
- Two or three buttons: confirm, cancel and an optional dismiss. Each carries a key hint (A/✕ or Enter, B/◯ or Esc).
- Left and Right move between buttons; Enter, Space or A activates; Esc, Backspace or B triggers the dismiss handler if present, otherwise cancel.

### Missing compared with normal mode
Console Mode has no search, game details page, settings, uninstall, verify/repair, move, categories or favourites, download manager view, login, stores, Deals, notifications UI, offline banner or log access. Launch options and alternate executables are also missing.

---

## Gamepad navigation

Implementation: `src/frontend/helpers/gamepad.ts`. Layouts: `src/frontend/helpers/gamepad_layouts/*`. Backend key synthesis: `src/backend/main.ts:1167-1293`. Spatial navigation is enabled with `app.commandLine.appendSwitch('enable-spatial-navigation')` (`src/backend/main.ts:196`).

### Polling and enablement
- Polling uses `requestAnimationFrame` while at least one controller is connected (`:700-734`, `:755-786`). The Logitech G29 wheel (`046d…c24f`) is ignored.
- Input is ignored when the setting `disableController` is on, or when the Heroic window is not focused, to avoid acting on in-game input (`:220-248`).
- **Steam Input de-duplication**: for a non-Valve pad whose timestamp is within 10 of a Valve (`28de`) virtual pad, the non-Valve pad is ignored (`:653-697`).
- **Active controller**: the first input emits a `controller-changed` window event with the controller id. The body class `controllerLayout` switches on and the on-screen ControllerHints appear. The first mouse movement resets to "no controller" (`:788-829`).

### Repeat timing (`:174-218`, `:250-283`)
- Navigation (D-pad, left stick): first repeat after `gamepadInitialRepeatDelay` + `gamepadRepeatDelay`, then every `gamepadRepeatDelay` ms. The defaults are 300 and 50 ms (`src/backend/config.ts:365-366`, configurable in Settings).
- Right-stick scroll repeats every 50 ms.
- Buttons don't repeat.

### Actions (`ValidGamepadAction`)
`mainAction`, `back`, `altAction`, `rightClick`, `leftClick`, `prevPage`, `nextPage`, `esc`, `tab`, `shiftTab`, `keyboardClick`, `guide`, `pad{Up,Down,Left,Right}`, `leftStick{Up,Down,Left,Right}`, `rightStick{Up,Down,Left,Right}`.

### Button mapping
| Physical (standard layout, `gamepad_layouts/standard.ts:14-56`) | Action |
|---|---|
| A / Cross (0) | `mainAction` |
| B / Circle (1) | `back` |
| X / Square (2) | `rightClick` (context menu) |
| Y / Triangle (3) | `altAction` |
| LB/L1 (4), RB/R1 (5) | `prevPage`, `nextPage`. These are only used inside webviews; the backend has no case for them otherwise |
| D-pad (12–15) | `padUp/Down/Left/Right` |
| Left stick (axes 0/1, ±0.5) | `leftStick*` |
| Right stick (axes 2/3) | `rightStick*` (scroll; left/right unused) |
| Guide (16) | `guide` (toggle Console Mode) |

Controller-specific layouts (`gamepad.ts:712-727`):
- GameCube (`gamecube|0337`)
- PS3 clone (`2563…0523`): the D-pad is on axes 4/5
- N64 clone (`0079…0006`)
- Genius MaxFire G-12U (`0583…a009`)
- Nintendo Switch Pro / Joy-Con (`057e…2006|2007|2009`): A and B are swapped by position (`nintendo.ts:55-96`)
- Everything else falls back to the standard layout.

### Context-sensitive behaviour (`gamepad.ts:325-424`)
- **mainAction**:
  - On a `<select>` or MUI Select: a simulated left click at the element centre (`leftClick` with metadata).
  - On a game card: `.click()`.
  - On a virtual keyboard key: types it.
  - On a text input: **opens the virtual keyboard**.
  - On a MUI slider: Tab, which toggles slider focus.
  - Otherwise: `.click()` on the focused element (the parent if the target is an SVG).
- **back**, in priority order:
  1. Close the virtual keyboard.
  2. Inside a MUI dialog: click its `[aria-label=close]` button. This is why every Dialog renders a hidden close button.
  3. Inside a `.dropdown`: close it.
  4. On a select: blur and refocus.
  5. In a MUI popover: Tab.
  6. In a context menu: right-click to close.
  7. Inside a slider: Shift+Tab.
  8. Otherwise: backend `webContents.goBack()`, which is browser history back.
  - Blocked while a console launch or console modal is active.
- **altAction (Y)**:
  - On a game card: play if installed (clicks `.playIcon`), otherwise install (clicks `.downIcon`).
  - In the virtual keyboard: space.
- **rightClick (X)**:
  - In the virtual keyboard: backspace.
  - Otherwise: a synthesized right mouse click at the element centre, which opens game card context menus.
- **Directional**:
  - The backend sends `keyDown`/`keyUp` arrow keys for Chromium spatial navigation.
  - Down on a MUI Select, the dialog close button or a slider becomes Tab; Up becomes Shift+Tab.
  - With nothing focused, focus moves to `<body>`.
- **rightStickUp/Down**: the backend sends mouse-wheel events of ±50 at the window centre.

### Inside store webviews (`gamepad.ts:82-172`, `:307-322`)
- **Arming**: a nav or A press when nothing host-side is focused, or A on the sidebar Stores link, arms forwarding. It sends `enter` after 150 ms.
- **While armed**:

  | Input | Effect |
  |---|---|
  | A | click |
  | Y | focus the page's search input |
  | X | focus and select the host URL bar, and disarm |
  | B | exit to the sidebar |
  | L1 / R1 | history back / forward |
  | D-pad / left stick | spatial focus in the guest |
  | Right stick | scroll ±60 px |
  | Guide | stays host-side |

- The WebView screen auto-arms on `dom-ready` if a controller is in use (`WebView/index.tsx:197-200`).

### Virtual keyboard (`src/frontend/helpers/virtualKeyboard.ts`)
- Uses `simple-keyboard` rendered in a top-layer `<dialog>`. Focus starts on the "h" key.
- Typing appends to the target input and dispatches an `input` event. Only append, `{bksp}` and space are supported; there is no cursor movement.
- B closes the keyboard and refocuses the input.

### Controller hints bar (normal mode, `src/frontend/components/UI/ControllerHints/index.tsx`)
- Shown only while a controller is active.
- **Glyph sets**: ps5 (`054c…0ce6`), ps4 (`054c|PS3|09cc|0268|2563…0523`), steam-deck (`28de…11ff`, also the default) and xbox (`microsoft|xbox`).
- **Contextual labels** for Main, Back, Alt and Alt2, chosen by the focused element:
  - Game card: Play game, Install game, Update game, Game settings, Game details, Options
  - Search: Open virtual keyboard
  - Context menu: Select, Back, Close options
  - Virtual keyboard: Select, Close keyboard, Backspace, Space
  - Install dialog: Close dialog
  - MUI lists
- The Back fallback is "Back" unless history is at index 0.
- **In webviews**: Select, Exit, URL bar, Search, Previous page, Next page, Move focus, Scroll.
- Always shown: "Move cursor" and "Scroll".

### Steam Deck specifics
- `isSteamDeckGameMode` is `XDG_CURRENT_DESKTOP === 'gamescope'`. The Deck desktop is detected from env variables (`src/backend/constants/environment.ts:27-33`).
- In Game Mode:
  - The window goes fullscreen at start (`src/backend/main.ts:153-164`).
  - `isFullscreen` reports true (`:647`).
  - Window bounds are not saved (`:209-215`).
  - **OS notifications are suppressed** (`src/backend/dialog/dialog.ts:62`).
- Controller hint glyphs default to Steam Deck.
- `window.isSteamDeck` and `window.isSteamDeckGameMode` are exposed to the renderer (`src/preload/index.ts`).

---

## Keyboard shortcuts

### Renderer (`src/frontend/helpers/shortcuts.ts`) and backend dispatcher (`src/backend/main.ts:1365-1393`)
`Ctrl` means Ctrl on Linux and Windows and Cmd on macOS (`e.ctrlKey || e.metaKey`).

| Keys | Action | IPC |
|---|---|---|
| Ctrl+F | Focus the library search box (`#search`) | none |
| Ctrl+R | Reload the renderer (`mainWindow.reload()`) | `processShortcut('ctrl+r')` |
| Ctrl+L | Go to the library. It sends `openScreen('/library')`, which the catch-all redirects to `/` | `processShortcut('ctrl+l')` |
| Ctrl+J | Open Downloads (`/download-manager`) | `processShortcut('ctrl+j')` |
| Ctrl+K | Open Settings (`/settings/general`) | `processShortcut('ctrl+k')` |
| Ctrl+Q | Quit (`handleExit`, with a pending-operations confirm) | `processShortcut('ctrl+q')` |
| Ctrl+Shift+I | Open DevTools | `processShortcut('ctrl+shift+i')` |

Implicit and other keyboard behaviour:
- **Arrow keys** move focus spatially in normal mode (Chromium spatial navigation switch).
- Esc closes MUI dialogs. Backdrop click closes them too, unless "Disable closing dialogs by clicking outside" is on (`components/UI/Dialog/components/Dialog.tsx:63-66`).
- Console Mode: arrows, Enter or Space, Esc, and Backspace in overlays (see Console Mode).
- SID login input: middle-click pastes from the clipboard.
- Mouse back and forward buttons work in webviews.

What is **not** implemented:
- F11 fullscreen.
- Zoom keys (Ctrl +, -, 0). `Menu.setApplicationMenu(null)` removes the default menu roles in packaged builds (`src/backend/main.ts:245`), and zoom is only adjustable from Accessibility.
- `globalShortcut` and `before-input-event` are not used.
- The tray menu shows the accelerators Ctrl/Cmd+R, Ctrl+Shift+I / Alt+Cmd+I and Ctrl/Cmd+Q. These are only labels in the context menu.
- In development builds DevTools open automatically.

---

## Tray

Implementation: `src/backend/tray_icon/tray_icon.ts`. On macOS the same menu is also set as the Dock menu (`:30`).

- **Created at startup** unless the setting `noTrayIcon` is on (`:19-20`). Tooltip: "Heroic".
- **Icon**: `icon-dark.png` if `darkTrayIcon`, else `icon-light.png`. Sizes are 20×20 on macOS and 32×32 on Linux and Windows. It updates on the `changeTrayColor` signal (send), with a 500 ms delay (`:51-57`, `:86-93`).
- **Left click**: if `exitToTray` is on and the window is visible, it hides the window; otherwise it shows it (`:37-45`).
- **Context menu** (`:96-147`):
  1. **Recent games**: up to `maxRecentGames` (default 5, `src/backend/recent_games/recent_games.ts:7-10`), labelled by title. A click launches the game via `heroic://launch?appName=…` (protocol handler).
  2. separator
  3. **Show Heroic** (`tray.show`)
  4. **About**: native about panel with "Heroic Games Launcher", the version, "GPL V3", the icon and the website (`src/backend/utils.ts:230-239`)
  5. **Reload** (Ctrl/Cmd+R)
  6. **Debug**: opens DevTools (Ctrl+Shift+I / Alt+Cmd+I)
  7. **Quit**: `handleExit` (Ctrl/Cmd+Q)
- **Rebuilt** on language change (backend event `languageChanged`) and on recent-games change (backend `recentGamesChanged`, trimmed to the limit) (`:47-65`).
- **Related settings**:
  - `exitToTray`: closing the window hides it (`src/backend/main.ts:217-222`)
  - `startInTray`: start hidden (`:417-421`)
  - `minimizeOnLaunch`: hide the window when a game launches, only if a tray icon exists (`src/backend/launcher.ts:136`, `:170-172`). Nothing re-shows the window when the game exits.

---

## Notifications & dialogs

### OS notifications
- Backend `notify({title, body})` (`src/backend/dialog/dialog.ts:61-72`). It is skipped if notifications are unsupported or in Steam Deck Game Mode. **Clicking a notification shows the main window.**
- The frontend can trigger one with `notify` (send) (`src/frontend/helpers/index.ts:16-17`).

| Trigger | Title | Body key / text | Source |
|---|---|---|---|
| Install starts | game title | `notify.install.startInstall` "Installation Started" | `downloadmanager/utils.ts:66-69` |
| Update starts | game title | `notify.update.started` "Update Started" | `downloadmanager/utils.ts:144-147` |
| Install/update finished | game title | `notify.{install,update}.finished` | `downloadqueue.ts:320-326` |
| Install/update failed | game title | `notify.{install,update}.failed` | `downloadqueue.ts:312-319` |
| Install/update canceled | game title | `notify.{install,update}.canceled` | `downloadqueue.ts:303-311` |
| Install/update paused | game title | `notify.{install,update}.paused` | `downloadqueue.ts:294-302` |
| (GOG redist elements are skipped) | | | `downloadqueue.ts:284-289` |
| Repair finished / error | game title | `notify.finished.reparing` / `notify.error.reparing` | `src/backend/main.ts:918-924` |
| Move started / error / done | game title | `notify.moving` / `notify.error.move` / `notify.moved` | `src/backend/main.ts:946-990` |
| Import done / failed | game title | `notify.install.imported` / `notify.import.failed` | `src/backend/main.ts:1041`, `:1076` |
| Uninstalled / uninstall error | game title | `notify.uninstalled` / `notify.uninstalled.error` | `src/backend/utils/uninstaller.ts:104-119`, `storeManagers/sideload/games.ts:128` |
| Desktop/start-menu shortcuts added / removed | "Shortcuts" | per-OS body | `src/backend/shortcuts/ipc_handler.ts:27`, `:54` |
| Added to / removed from Steam | game title | message | `shortcuts/nonesteamgame/nonesteamgame.ts:110` |
| Wine version install start / done / error, uninstall | version | `notify.install.startInstall`, … | `src/backend/wine/manager/ipc_handler.ts:20-59` |
| Wine Manager refresh failed (frontend) | "Wine-Manager" | `notify.refresh.error` | `src/frontend/state/GlobalStateV2.ts:95-101` |
| New Heroic version on GitHub | "Update Available!" | `notify.new-heroic-version` | `src/backend/utils.ts:758-766` |
| Epic service offline check | "Epic Games Store offline" | "Heroic will maybe not work probably!" Uses `Notification` directly. It **also fires when the component isn't found** (bug) | `src/backend/utils.ts:186-224` |

### In-app message box (`DialogHandler`)
- **Sources**:
  - Frontend `showDialogModal({title, message (string|JSX), buttons[{text,onClick}], type 'MESSAGE'|'ERROR', className, onClose})`.
  - Backend `showDialog` event via `showDialogBoxModalAuto` (`src/backend/dialog/dialog.ts:8-54`). If sending fails, it falls back to native `dialog.showErrorBox` or `showMessageBox`.
- **Rendering** (`components/UI/DialogHandler/components/MessageBoxModal/index.tsx`):
  - Title, then content. String messages are HTML-decoded and split on `\n`.
  - ERROR type adds an "Error:" header and an error box.
  - Each button first closes the dialog, then runs its `onClick`.
  - A close (X) button is shown.
- Only **one** dialog can show at a time: a new call replaces the current one.
- **Known backend dialogs**:
  - Epic outage warnings on install, update and import
  - Launch aborted (per store)
  - Zoom executable not found
  - Error moving game
  - Winetricks error
  - Cache cleared ("Heroic Cache Was Cleared!" with Ok)
  - Uncaught exception (ERROR: "A uncaught exception occured… Report the exception on our Github repository", `src/backend/main.ts:483-504`)
  - Update error (`src/backend/updater.ts:77-87`)
- **Frontend-originated dialogs** in the shell:
  - Quit confirm
  - Stop installation
  - Reset Heroic confirm ("This will remove all Settings and Caching but won't remove your Installed games or your Epic credentials…", Yes sends `resetHeroic`, `GlobalState.tsx:500-512`)
  - Analytics opt-in

### Native OS dialogs (backend `dialog.showMessageBox`)
- Quit with pending operations: "There are pending operations, are you sure?" (No/Yes). Confirming kills legendary, gogdl and nile processes and aborts children (`src/backend/utils.ts:241-280`).
- Protocol launch of an uninstalled game: "<title> Is Not Installed, do you wish to Install it?" (Yes/No) (`src/backend/protocol.ts:153-178`).
- Heroic update available, and update downloaded (`src/backend/updater.ts`).
- VC Runtime missing on Windows: Download now / Ok / Don't show again (`src/backend/utils.ts:690-725`).
- Wine not found, "continue with <found>?" (`src/backend/utils.ts:870-890`).
- Rosetta missing on macOS (`src/backend/utils.ts:1285-1300`).
- Game folder missing, "remove from installed list?" (`src/backend/utils.ts:282-298`).
- EOS overlay prompts (`storeManagers/legendary/eos_overlay/eos_overlay.ts:156`, `:189`).
- File and folder pickers: `openDialog` (invoke) → `dialog.showOpenDialog` (`src/backend/main.ts:881-893`).

### Other shell dialogs and components
- **ExternalLinkDialog** (`components/UI/ExternalLinkDialog/index.tsx`): "You are about to open an external link." with a "Don't ask again" toggle and Continue/Cancel. The preference is stored in localStorage `show_external_link_dialog`. It is only used by the sidebar community links.
- **OfflineMessage** banner: see Connectivity / offline.
- **ProgressDialog** (`components/UI/ProgressDialog/index.tsx`): title plus a scrolling log. Lines containing " err" or " warn" are coloured. It auto-scrolls unless the user scrolls up, shows an indeterminate progress bar and has a close button (used for Winetricks and similar).
- **ChangelogModal**: see Heroic self-update & changelog.
- **Shared Dialog primitive** (`components/UI/Dialog/components/Dialog.tsx`):
  - Max width `min(700px, 85vw)`.
  - Always renders a close button, hidden when not wanted, so gamepad B can close it.
  - Simulates Tab presses on open to move focus into the dialog (`gamepadAction {action:'tab'}`).
  - Respects `disableDialogBackdropClose`.
- **ErrorComponent** (`components/UI/ErrorComponent/index.tsx`): a broken-heart icon with a message and three buttons: **Refresh Library** (foreground refresh with update check), **Clear Heroic Cache** (`clearCache(true)` shows the "Cache cleared" dialog and triggers `refreshLibrary`), and **Reset Heroic** (confirm dialog).
- **UpdateComponent**: a generic spinner with an optional message (default `loading.default`).
- Small primitives used across screens:
  - `PopoverComponent`: click-toggled, closes on outside click
  - `Dropdown`: optional hover pop-up; B closes it
  - `TabPanel`
  - `Header`: library search, category filter and library filters bar (Library section)

---

## First-run / onboarding / tour

- **First launch, not logged in**:
  - The sidebar shows **Login** at the top instead of Manage Accounts.
  - The library shows the empty-state text: "Your library is empty. Click here to log in with your Epic, GOG.com, Amazon, or Zoom accounts… To use games or apps from other sources, click [Add Game] to add them manually." (`screens/Library/components/EmptyLibrary/index.tsx:13-26`).
  - If libraries exist but filters remove everything: "The current filters produced no results."
  - There is no forced redirect to `/login` and no welcome wizard.
- **Analytics opt-in** (`screens/Settings/components/AnalyticsDialog.tsx`):
  - Shown once, through `showDialogModal`, when localStorage `analytics-modal-shown` is unset. It is skipped in E2E.
  - It explains Plausible self-hosted anonymous analytics: app version, OS, architecture, distro, connected stores and country.
  - **Enable** or **Disable** sends `setSetting {analyticsOptIn}` and marks the dialog as shown. Closing via X or the backdrop leaves it unset, so the dialog **comes back next launch**.
  - When opted in, the backend runs `startPlausible()` at boot (`src/backend/main.ts:365-367`).
- **Changelog on first run**: `lastChangelogShown` is null on a fresh profile, so the changelog modal also auto-opens. It can stack with the analytics dialog (see Heroic self-update & changelog).
- **Guided tours** (intro.js `Steps`; `components/Tour/Tour.tsx`, `components/Tour/TourButton.tsx`, `state/TourContext.tsx`):
  - State: `{activeTour, tourProgress, completedTours}` in localStorage `heroic-tour-state`.
  - **Tours never auto-start.** They start from a "?" `TourButton`: the sidebar version area starts `sidebar-tour`, and the Library ActionIcons start `library-tour`. Starting a completed tour resets it first.
  - Options: Next, Back, Skip and Done labels; bullets; no step numbers; overlay opacity 0.7; exit on overlay click. Completing marks the tour complete; skipping does not.
  - **Sidebar tour steps** (`components/UI/Sidebar/components/SidebarTour.tsx`), with the tooltip on the right (left in RTL):
    1. Welcome / Sidebar Navigation
    2. Library
    3. Stores
    4. Settings
    5. Downloads
    6. Wine Manager (not Windows)
    7. Manage Accounts (if logged in) or Login
    8. Accessibility
    9. Documentation
    10. Community
    11. Quit
    12. Version ("access tours from here")
  - **Library tour steps** (`screens/Library/components/LibraryTour.tsx`): game card, search, categories, filters, view toggle, sort A–Z, sort installed, refresh, add game (Library section).
- **Help panel** (experimental, `experimentalFeatures.enableHelp`; `components/UI/Help/index.tsx`): a floating "?" button toggles a popover. The popover lists `<details>` entries registered by the current screen through `hasHelp(id, title, content)`. The registered help items are login, downloadManager, library, gamePage, settings, wineManager, accessibility, defaultInstallPath, defaultSteamPath and customThemesPath.

---

## Heroic self-update & changelog

- **GitHub release check** (`getLatestReleases`, `src/backend/utils.ts:732-776`):
  - Queries the GitHub releases API (`src/backend/constants/urls.ts` `GITHUB_API`) for the newest stable and prerelease newer than the current version (semver).
  - When either exists, it fires the OS notification "A new Heroic version was released!" and returns them.
  - Skipped (returns []) if `checkForUpdatesOnStartup` is off. That setting defaults to on everywhere except Flatpak (`src/backend/config.ts:338`).
  - The sidebar shows "Update Available!" with Stable/Beta links to the release pages.
- **Auto-updater** (`src/backend/updater.ts`; packaged builds when `checkForUpdatesOnStartup`, `src/backend/main.ts:250-252`):
  - `autoDownload` and `autoInstallOnAppQuit` are off.
  - On `update-available`, a native dialog "There is a new Version available!" appears.
    - Where auto-update is supported (Windows, macOS, AppImage): buttons **Update** (download in the background), **Postpone** and **Changelog**. Changelog opens GitHub releases and then re-shows the dialog.
    - Otherwise: "Automatic updates are not supported for your packaging format. Please use your package manager to update." with Ok and Changelog.
  - When the download finishes: "Do you want to restart Heroic now?" No/Yes. Yes calls `quitAndInstall`; No installs on quit.
  - Errors show the in-app ERROR dialog "Update Error".
  - The setting toggle visibility is decided by `showUpdateSetting` (invoke), which is false in Flatpak.
- **Changelog modal** (`components/UI/ChangelogModal/index.tsx`):
  - Fetches the release for tag `v<current>` (`getCurrentChangelog` invoke → GitHub).
  - Renders the title and markdown body (raw HTML allowed; links have `target=_blank`).
  - **Auto-shows on startup** when `!hideChangelogsOnStartup` (a setting) and the version differs from localStorage `last_changelog`. Closing it records the version (`components/UI/Sidebar/components/HeroicVersion/index.tsx:65-77`). Clicking the version text re-opens it.
  - If the fetch fails, nothing shows.
- **Post-upgrade cache clear**: when `last_version` differs, the app runs `clearCache(false, true)` silently (`HeroicVersion/index.tsx:33-42`).
- **Navigation guard**: in-app navigation to non-app URLs, such as changelog links, is blocked and the URL is opened externally (`src/backend/main.ts:255-267`).

---

## Connectivity / offline

- **Backend monitor** (`src/backend/online_monitor.ts`):
  - Status is one of `online`, `offline` or `check-online`.
  - At start it uses `net.isOnline()`: if online, the status is `check-online` and the monitor HEAD-pings github.com, store.epicgames.com, gog.com and cloudflare-dns.com with a 10 s timeout. Any success means online.
  - If all pings fail, it counts down `retryIn` seconds (5, 10, 15, … increasing by 5 each time) and pushes `connectivity-changed {status:'check-online', retryIn}` every second before retrying.
  - Every status change pushes the `connectivity-changed` event and emits an internal backend event.
- **Frontend hooks** (`src/frontend/helpers/onlineMonitor.ts`): browser `online` sends `connectivity-changed('check-online')`, and `offline` sends `connectivity-changed('offline')` (send).
- **Manual override**: the banner's "(Ignore)" sends `set-connectivity-online`, forcing online. Store webviews also force online when a page title loads.
- **Banner** (`components/UI/OfflineMessage/index.tsx`): hidden when online. Otherwise it reads "Offline", "Offline. Retrying in {{seconds}} seconds." or "Retrying...", with an (Ignore) button and a "?" hover hint: "We are checking the connectivity against: github.com, gog.com, store.epicgames.com and cloudflare-dns.com".
- **Effects of offline**:
  - Downloads auto-pause, then auto-resume when back online.
  - New install or update queue items error out silently.
  - Games launch in offline mode (`src/backend/launcher.ts:492`).
  - Save sync is skipped.
  - Several backend tasks are deferred with `runOnceWhenOnline`: user validation, GOG presence, playtime sync, and the releases-info fetch.
  - Game page, game card and download-size UI change (Library/GamePage sections).

---

## Window management

- **Main window** (`src/backend/main_window.ts`):
  - Default 1200×690, shrunk to 80% of the work area on small screens. Minimum 600×345.
  - Bounds and the maximized flag are restored from `configStore.window-props`, and saved on close (except in CLI fullscreen or Steam Deck Game Mode, `src/backend/main.ts:206-216`).
  - Shown on `ready-to-show`. On Wayland it is shown on `did-finish-load` instead, then maximized if it was saved that way (`src/backend/main.ts:421-437`).
- **Frameless / title bar**: the setting `framelessWindow` (read at window creation, so it needs a restart).
  - **macOS and Windows** use `titleBarStyle: 'hidden'` plus a native `titleBarOverlay`. Its colours come from the theme CSS vars `--titlebar-*` via `setTitleBarOverlay` (`src/frontend/index.tsx:153-164`, `src/backend/main.ts:1331-1337`).
  - **Linux** uses `frame: false`, and the app draws **WindowControls** (Minimize, Maximize/Restore, Close) with `minimizeWindow`, `maximizeWindow`, `unmaximizeWindow` and `closeWindow` (send). The initial state comes from `isMaximized` (invoke), kept current by the `maximized`/`unmaximized` events (`components/UI/WindowControls/index.tsx`).
  - `isFrameless` (invoke) adds the `frameless` class.
- **Fullscreen**:
  - Forced with `--fullscreen` or Steam Deck Game Mode (`src/backend/main.ts:153-164`).
  - Console Mode sends `setFullscreen(true/false)`.
  - The `fullscreen` event keeps the `fullscreen` class in sync.
  - No user shortcut toggles fullscreen.
- **Close behaviour**:
  - The close event is always intercepted. With `exitToTray` and a tray icon, the window hides. Otherwise `handleExit` runs, with a pending-ops confirm; it hides the window, deletes GOG presence and calls `app.exit()` (`src/backend/main.ts:206-225`, `src/backend/utils.ts:241-280`).
  - On non-macOS, `window-all-closed` quits.
- **Start hidden**: with `--no-gui`, a protocol launch with `gui=false` or `hideWindowOnProtocolLaunch`, or `startInTray` (with a tray icon) (`src/backend/main.ts:413-421`).
- **Single instance**: a second launch focuses and shows the existing window (unless the protocol args say to hide) and forwards its argv to the protocol handler (`src/backend/main.ts:296-321`).
- **`heroic://` protocol** (`src/backend/protocol.ts`): registered as the default OS handler (`src/backend/main.ts:399-411`). macOS `open-url` is handled too (`:576-585`).
  - `heroic://ping`: logs only.
  - `heroic://launch/<appName>`, `heroic://launch/<runner>/<appName>`, or `heroic://launch?appName=&runner=&arg=…&altExe=…&gui=false`.
  - Runners accepted: legendary, gog, nile, sideload. **Zoom is not accepted.**
  - If the game is installed, it launches; the window hides if requested. If not installed, a native "install?" prompt appears and Yes sends the `installGame` event, which currently goes nowhere (see Global state).
- **Other CLI flags**: `--console`, `--spoof-windows` (sets Windows `sec-ch-ua-platform` request headers), `--no-gui` and `--fullscreen` (`src/backend/constants/environment.ts:34-36`, `src/backend/main.ts:332-339`).
- **Zoom**: `setZoomFactor` maps a percent to a Chromium zoom level, with extra scale-down on screens narrower than 1200 px (`src/backend/main.ts:269-307`). It is re-applied on maximize, restore, fullscreen and navigation. Pinch zoom is disabled.
- **Power**: see the power-save lock under Global state.
- **External windows**: `createNewWindow(url)` (send) opens a plain 1200×700 BrowserWindow (`src/backend/main.ts:698-700`).

---

## i18n

- **Library**: i18next with the HTTP backend in the renderer and the fs backend in the main process (`src/frontend/index.tsx:52-115`, `src/backend/main.ts:383-395`).
- **Namespaces**: `translation`, `gamepage` and `login` (`public/locales/<lng>/*.json`). The fallback is `en`, and `returnEmptyString` / `returnNull` are false, so a missing key shows the English default given in code.
- **Language counts**:
  - **50** locale folders exist in `public/locales`.
  - **43 are selectable** (`src/common/languages.ts`): ar, az, be, bg, bs, ca, cs, de, el, en, es, et, eu, fa, fi, fr, ga, gl, he, hr, hu, ja, ko, id, it, lt, ml, nb_NO, nl, pl, pt, pt_BR, ro, ru, sr, sk, sv, ta, tr, uk, vi, zh_Hans, zh_Hant.
  - The folders br, da, hup, ka, sl, th and uz exist but are not selectable.
- **Selection**:
  - At first run, the language is `configStore.language`, then localStorage `language`, then `'en'`. There is **no OS-locale detection** (`src/frontend/index.tsx:36-39`).
  - The language selector lists native labels with optional flags (`components/UI/LanguageSelector/index.tsx:21-111`). It appears on the Login page (with flags and a "Help Improve this translation." Weblate link) and in General settings.
  - Changing it updates localStorage, `configStore` and the i18n instance, and calls the context `setLanguage`, which also sets `<html lang>`. It also sends `changeLanguage`. The backend then switches i18next, clears the legendary `gameInfoStore` cache (re-localised metadata), saves `settings.language` and rebuilds the tray menu (`src/backend/main.ts:449-455`).
- **RTL**:
  - Only `fa` and `ar` are treated as RTL (`src/frontend/state/GlobalState.tsx:48`). **Hebrew (he) is missing.**
  - RTL sets the body and app class `isRTL`, the MUI `direction: 'rtl'`, mirrored sidebar resize and flyouts, and tour tooltip placement.
- **Region-dependent features**: Deals uses the language to pick its default region and currency. The Epic store URL uses `/store/<lang>/`. Dates in the download manager use `Intl` with the OS locale, not the app language.

---

## UX pain points (shell & other screens)

### Navigation and shell
1. The sidebar has no explicit collapse toggle. Collapse is an undiscoverable side effect of dragging below 120 px. `sidebarCollapsed` state exists but is dead code, and resizing is impossible by keyboard or gamepad.
2. Contextual submenus (Stores, Settings) only appear once you are already in that section, so you can't jump directly to the GOG store or the Log settings from elsewhere.
3. "Login" and "Manage Accounts" are the same page shown under different labels. Logging out of any store reloads the whole app, with no confirmation.
4. Ctrl+L sends `/library`, which isn't a route and only works through the catch-all redirect. There are no zoom keys, no F11 and no shortcut discoverability (cheat sheet). The tray accelerator labels suggest global shortcuts that don't exist.
5. Community and donation links (four entries) and Quit take permanent sidebar space. Only these links use the external-link confirmation; other external opens, such as Deals (GMG/Humble) and webview "open in browser", don't.
6. Global singletons:
   - Only one message dialog can exist; a new one silently replaces an open one. For example, analytics opt-in and an error can clobber each other.
   - Backend dialogs cannot carry button actions, so backend prompts are either native OS boxes (inconsistent style) or inert in-app boxes.

### Download manager
7. The cancel dialog is confusing: "Stop Installation — Do you wish to KEEP downloaded files?" with "Keep Installing / Yes / No". Users can easily delete files by accident.
8. The queue has no reordering, no "move to top" and no per-item pause. Removing a queued item has no confirmation or undo.
9. Zoom, sideload and redistributable items are invisible in the list because rows only render for Epic, GOG or Amazon games. There are no per-item progress, ETA or size-remaining figures in queued rows.
10. In Completed, the "Play" icon only navigates to the game page; it doesn't launch. Failed items have no retry and no error reason. The paused label is hard-coded English ("Paused"), and the sidebar widget always says "Installing", even for updates.
11. Offline installs fail silently (log only). Auto-pause on offline gives no in-app explanation.

### Login and webviews
12. Login state detection relies on scraping URLs inside the webview on `dom-ready` and `did-navigate`. Failures leave the user on a spinner with no error. Epic's alternative SID flow expects users to understand JSON or authorization codes.
13. The webview URL bar is read-only (you can't type a URL), and "Open in browser" is enabled on login pages. Webview session partitions for `/wiki` and `/store-page` resolve to `persist:undefined`.
14. Clicking a GOG deal opens an in-app webview while GMG and Humble open the external browser, with no visual cue beyond a tooltip on the store badge.

### Deals
15. The filter set changes depending on the tab (GOG-only filters vanish), and changing the region silently wipes all filters. The price filter disappears on the "All" tab. Pagination resets on any change, and there is no infinite scroll.

### Console Mode
16. PS5 controllers are never detected (regex order), so PS4 glyphs show.
17. Leaving Console Mode always exits fullscreen, even when Heroic was started fullscreen (Steam Deck, `--fullscreen`).
18. The empty-state text is wrong when the Installed filter is off. There is no search, no game details, no way to choose an install path, DLC or language, and no access to downloads, settings or accounts. Users must leave Console Mode for nearly everything.
19. The launch overlay closes when the launch promise settles, and cancelling requires a non-obvious 3-second hold.

### Gamepad and input
20. Gamepad support works by synthesising DOM keys and mouse clicks (Tab, Shift+Tab, arrow keys, simulated clicks at element centres) and by checking CSS class names. It is fragile, and it explains odd behaviours such as Down on a select turning into Tab and B triggering browser history back.
21. The virtual keyboard only appends and backspaces, with no cursor or IME, and opens only for `type=text` inputs. Hints depend on focus heuristics.
22. Controller input is ignored whenever the window isn't focused, which is correct for games but confusing after alt-tab without a click.

### Onboarding, help and updates
23. There is no first-run wizard. A fresh user can see the changelog modal and the analytics dialog stacked at once. Dismissing the analytics dialog via X re-prompts on every launch.
24. Tours never auto-start and are hidden behind a small "?" next to the version string. The Help panel is experimental and off by default.
25. Update notice is split across three places: a GitHub-release notification, a sidebar "Update Available!" link, and native electron-updater dialogs. They follow different rules (Flatpak, AppImage, `checkForUpdatesOnStartup`).
26. The changelog auto-opens on every version change unless the user finds the setting to hide it.

### Connectivity, window and i18n
27. Connectivity starts as "offline" in the frontend until the first status arrives, so the offline banner can flash at startup. The "Ignore" override is permanent until the next status change and is easy to trigger by accident.
28. The Epic status notifier shows "Epic Games Store offline" even when the status component isn't found (false positive), with a garbled body ("Heroic will maybe not work probably!").
29. `minimizeOnLaunch` hides the window but never restores it after the game exits. Frameless mode needs a restart, and on Linux Heroic draws its own minimize/maximize/close controls instead of using the system ones.
30. Protocol install requests (`heroic://launch` for an uninstalled game, then "Yes") go nowhere because the `installGame` event updates state nothing renders. The protocol ignores Zoom games.
31. i18n:
    - RTL covers only Arabic and Persian; Hebrew is missing.
    - Seven translated locales exist on disk but aren't selectable.
    - There is no OS language detection.
    - Dates use the OS locale instead of the app language.
    - Some strings are hard-coded English ("Paused", "Debug" in the tray, "Previous/Next" aria labels).
32. Accessibility zoom depends on screen width (extra scale-down below 1200 px) and is debounced. Font selection needs the browser Local Font Access API. Smooth-scrolling changes need a restart.

---

## Cross-cutting redesign priorities

These are distilled from the per-area pain point lists above. Each one is a direction for the redesign; none of them reduces scope.

1. **One coherent model for game identity and state.** Use a typed `(runner, appName)` key everywhere, with one state machine per game.
   - Heroic keys status, favourites, hidden, recent, overrides and the non-available list by `appName` alone, but keys categories by `appName_runner`.
   - Its status reducer accepts only some transitions.
   - Each library card re-fetches `getGameInfo` and `isGameAvailable` on its own.
   - The native client should hold one authoritative library model, updated from backend events.
2. **Make every action discoverable and reachable from every input method.** Heroic hides most per-game actions:
   - behind a right-click context menu (library) or a hover-only dots menu (game page);
   - the alt-play arrow and quick saves menu also open only on hover;
   - the filters and categories dropdowns open on hover.
   - The redesign needs a visible "more" affordance, grouped actions with destructive ones separated, and full keyboard and gamepad parity. Library-only actions (hide, favourite, remove from recent) must also be available on the game page.
3. **A unified filter, sort and search model.**
   - Today these are spread over four places, with two different search algorithms (fuzzy for the grid, substring for the dropdown).
   - Sort uses the original title while display uses the override title.
   - "Favourites only" silently bypasses the other filters.
   - The empty state does not say which filter excluded everything.
   - Needed: an active-filter summary with a one-click reset, sorting by relevance, last played, playtime, size and date added, and a virtualised list.
4. **Clear operation semantics for downloads.**
   - Separate *Pause* from *Cancel (keep files)* and *Cancel (delete files)*, and give them unambiguous labels.
   - Allow cancelling updates.
   - Add queue reordering, per-item ETA and size, retry for failed items with an error reason, and confirm-or-undo when removing an item.
   - Show Zoom, sideload and redist items, which are invisible today.
   - Make "Update all" a real action; the backend already has `updateAll`.
5. **Visible settings inheritance.**
   - Per-game settings are copy-on-first-write snapshots of Game Defaults. The UI does not show this, and there is no per-field reset.
   - The redesign should show inherited vs overridden values, offer per-field and per-game reset, and explain why a control is unavailable instead of hiding it.
   - Group settings by topic and by store, and add settings search.
   - Pick one source of truth for defaults; the backend and UI fallbacks disagree today.
6. **No silent failures.**
   - These operations currently fail without telling the user: repair while offline, save sync (offline or Epic down), installs while offline, EOS overlay download, `setSetting` writes, protocol install requests and `getInstallInfo` returning null (infinite spinner).
   - Every backend operation needs a user-visible result (toast, inline error or retry).
   - A failed metadata fetch must not replace the whole game page.
7. **Safety on destructive actions.** Confirmation (or undo) must be consistent across uninstall, Wine version removal, cache clearing, environment variable deletion, store logout and category deletion. Separate "Move files" clearly from "Re-point install location".
8. **First-run onboarding.**
   - Today, a fresh user can get the changelog and the analytics dialog stacked at once, and tours never start on their own.
   - Replace this with a single first-run flow: language, theme and accessibility, store logins, default install and prefix paths, Wine/Proton download, analytics consent, and controller and Console Mode hints.
9. **Console Mode as a first-class shell.** Today it can only launch, install with default options, and remove or cancel queue items. A native big-picture mode should at least add search, game details, install options (path, DLC, language), the download queue, uninstall/repair, and basic settings. It also needs gamepad glyph detection that works, including PS5.
10. **Real input handling.**
    - Replace Heroic's synthesised-DOM-event gamepad layer (Tab and arrow key injection, simulated clicks, CSS-class sniffing) with native focus navigation.
    - Add a proper on-screen keyboard (cursor, IME), zoom shortcuts, F11 fullscreen and a shortcut cheat sheet.
11. **Accessibility done properly.**
    - Respect the OS reduced-motion, contrast and language preferences.
    - Use the app locale for dates.
    - Support RTL for Hebrew as well as Arabic and Persian.
    - Add zoom reset and shortcuts, and give every control proper semantics (no clickable divs).
    - Keep accessibility values in the main settings so that "Reset" covers them.
12. **Richer diagnostics.** The log viewer needs search, level filtering, follow/pause, copy and wrap. Add an uploaded-logs manager with delete (the backend supports it). Show download speed and ETA in the Wine manager, and list which games use a Wine version before it is removed.
13. **Window and tray behaviour.** Restore the window after a game exits when `minimizeOnLaunch` hid it. Leaving Console Mode should not force windowed mode. Use native window decorations by default.

## Known bugs and dead code not to replicate

Found while reading the source. Each item is described in more detail, with refs, in its area section.

| # | Area | Issue | Ref |
|---|---|---|---|
| 1 | Protocol / install | `installGame` event (`heroic://` install confirmation) stores `showInstallModal` in GlobalState, which nothing renders, so the install dialog never opens | `src/frontend/state/GlobalState.tsx:892-910`, `src/backend/protocol.ts:171` |
| 2 | Install dialog | Linux-native games default to the **Windows** build on Linux | `src/frontend/screens/Library/components/InstallModal/index.tsx:57-158` |
| 3 | IPC contract | `getInstallInfo` positional args: the type says `branch, build`, but the frontend and backend use `build, branch` | `src/common/types/ipc.ts:192-198`, `src/backend/main.ts:756-778` |
| 4 | Categories | Adding an existing category name wipes its members; renaming onto an existing name overwrites it; deleting a category with no active filter switches the view to "Uncategorized only" | `src/frontend/state/GlobalState.tsx:407-458` |
| 5 | Library | `pushGameToLibrary` drops metadata overrides until the next refresh; `refreshLibrary` silently drops concurrent requests | `src/frontend/state/GlobalState.tsx:779-797, 928-965` |
| 6 | Library | Resume-progress key `${appName}_${runner}_progress` is read but never written; the real key is bare `<appName>` | `src/frontend/hooks/hasProgress.ts:8-11`, `src/frontend/helpers/library.ts:102-134` |
| 7 | Library | Cancelling an *update* from a card calls `kill` instead of the download-cancel flow | `src/frontend/screens/Library/components/GameCard/index.tsx:183-274` |
| 8 | Library | The non-available filter only knows games whose cards have scrolled into view | `src/frontend/hooks/constants.ts:51-79` |
| 9 | Game page | IGDB score link uses the Metacritic id | `src/frontend/components/UI/WikiGameInfo/components/GameScore/index.tsx` |
| 10 | Game page | Known-fixes wiki link is an in-app router `<Link>` for an external URL | `src/frontend/screens/Game/GamePage/index.tsx:375-388` |
| 11 | Game page | Status `extracting` is never emitted by the backend; `canceled` is unused | `src/common/types.ts:274-294` |
| 12 | Game page | Zoom "Move" and "Verify/Repair" are shown in the UI but stubbed in the backend | `src/backend/storeManagers/zoom/games.ts:744-757` |
| 13 | Game page | The alt-play arrow permanently flips `verboseLogs` instead of doing a one-off launch with logs | `src/frontend/screens/Game/GamePage/components/MainButton.tsx:99-119` |
| 14 | Game page | Release date is hidden when the PCGamingWiki date list is empty, even though the store has a date | `src/frontend/screens/Game/GamePage/components/ReleaseDate.tsx:34-75` |
| 15 | Settings | "Preferred Language" in Game Defaults writes the global `language` key, which is also the UI language | see the Settings section |
| 16 | Settings | Factory defaults and UI fallbacks disagree (`autoUpdateGames`, `enableEsync`, `gamepadInitialRepeatDelay`, …) | `src/backend/config.ts:323-382` vs `src/frontend/screens/Settings/components/*` |
| 17 | Settings | `setSetting` guard is always true; `handleExperimentalFeatures` writes an object into `zoom.enabled`; the custom themes path uses a whole-config `writeConfig` that can clobber concurrent changes | `src/frontend/hooks/useSettingsContext.ts:43-51` |
| 18 | Settings | A stored per-game "last tab" can point at a tab that no longer exists, which renders an empty panel | `src/frontend/screens/Settings/sections/GamesSettings/index.tsx:57-73` |
| 19 | Wine manager | The settings modal (custom Wine paths) is Linux-only, so macOS users cannot add custom Wine in the UI | `src/frontend/screens/WineManager/index.tsx` |
| 20 | Download manager | Rows render only for Epic, GOG and Amazon games, so Zoom, sideload and GOG-redist items are invisible | `src/frontend/screens/DownloadManager/components/DownloadManagerItem/index.tsx:60,212-218` |
| 21 | Console Mode | PS4 regex is tested before PS5, so PS5 pads are never detected | `src/frontend/screens/ConsoleMode/controller.ts:26-34` |
| 22 | Console Mode | Leaving Console Mode always sends `setFullscreen(false)` | `src/frontend/screens/ConsoleMode/index.tsx:101-116` |
| 23 | Notifications | The Epic-offline notification fires even when the status lookup finds nothing | see Notifications & dialogs |
| 24 | Shell | `launchGame` event and sidebar `sidebarCollapsed` state are dead code; Ctrl+L navigates to a non-existent `/library` route, which only works through the catch-all redirect | see App shell / Keyboard shortcuts |
| 25 | i18n | RTL is limited to Arabic and Persian (Hebrew is missing); 7 of the 50 locale folders are not selectable; several strings are hard-coded in English | see i18n |
| 26 | Connectivity | Frontend connectivity starts as `offline` until the first status query resolves, which can trigger spurious offline warnings and banners | `src/frontend/state/GlobalState.tsx:248` |

## Appendix A: IPC channel catalogue

This table was generated from `src/common/types/ipc.ts`, the preload alias definitions in `src/preload/api/*.ts`, and a grep of `src/frontend`. "Used in" lists at most six files and is a heuristic. Channels marked **not used by frontend** are exposed in preload (or only declared) but never called by the renderer. They still matter for parity where the backend or tray uses them; for example `updateAll` would enable an "Update all" button.

Preload also exposes non-IPC helpers: direct electron-store access (`storeNew/storeGet/storeSet/storeHas/storeDelete`), namespaced `wine.*`, `steamgriddb.*` and `systemInfo.*` wrappers, and the platform flags listed in Conventions.

| Channel | Kind | Frontend alias(es) | Used in (frontend files) |
|---|---|---|---|
| `setZoomFactor` | send | — | state/GlobalState.tsx |
| `changeLanguage` | send | — | components/UI/LanguageSelector/index.tsx |
| `notify` | send | — | helpers/index.ts, state/GlobalStateV2.ts |
| `frontendReady` | send | — | state/GlobalState.tsx |
| `lock` | send | — | state/GlobalState.tsx |
| `unlock` | send | — | state/GlobalState.tsx |
| `quit` | send | — | helpers/index.ts, screens/ConsoleMode/index.tsx |
| `openExternalUrl` | send | — | components/UI/Sidebar/components/HeroicVersion/index.tsx, screens/Discounts/components/DiscountCard/index.tsx, screens/Settings/components/NvidiaPrime.tsx, screens/Settings/sections/LogSettings/components/UploadedLogFilesList/index.tsx |
| `openFolder` | send | — | screens/Game/GamePage/components/CloudSavesSync.tsx, screens/Game/GamePage/components/InstalledInfo.tsx, screens/Game/GameSubMenu/index.tsx |
| `openSupportPage` | send | — | **not used by frontend** |
| `openReleases` | send | — | **not used by frontend** |
| `openWeblate` | send | — | components/UI/LanguageSelector/index.tsx |
| `showAboutWindow` | send | — | **not used by frontend** |
| `openLoginPage` | send | — | helpers/index.ts |
| `openDiscordLink` | send | — | helpers/index.ts, screens/Settings/sections/LogSettings/index.tsx |
| `openPatreonPage` | send | — | components/UI/Sidebar/components/SidebarLinks/index.tsx |
| `openKofiPage` | send | — | components/UI/Sidebar/components/SidebarLinks/index.tsx |
| `openGithubSponsorsPage` | send | — | components/UI/Sidebar/components/SidebarLinks/index.tsx |
| `openWinePrefixFAQ` | send | — | screens/Settings/components/WinePrefix.tsx |
| `openWebviewPage` | send | — | components/UI/WebviewControls/index.tsx, screens/WineManager/components/WineItem/index.tsx |
| `openWikiLink` | send | — | **not used by frontend** |
| `openSidInfoPage` | send | — | helpers/index.ts |
| `openCustomThemesWiki` | send | — | components/UI/ThemeSelector/index.tsx |
| `showConfigFileInFolder` | send | — | screens/Settings/index.tsx, screens/Settings/sections/FooterInfo/index.tsx |
| `removeFolder` | send | — | **not used by frontend** |
| `clearCache` | send | — | components/UI/ErrorComponent/index.tsx, components/UI/Sidebar/components/HeroicVersion/index.tsx, screens/Settings/components/ClearCache.tsx |
| `clearAchievementCache` | send | — | screens/Game/GamePage/index.tsx |
| `resetHeroic` | send | — | state/GlobalState.tsx |
| `createNewWindow` | send | — | helpers/index.ts, screens/Game/GameSubMenu/index.tsx |
| `logoutGOG` | send | — | state/GlobalState.tsx |
| `logError` | send | — | index.tsx, screens/ConsoleMode/InstallOverlay/index.tsx, screens/Discounts/index.tsx, screens/Game/GamePage/components/CloudSavesSync.tsx, screens/Game/GamePage/index.tsx, screens/Library/components/InstallModal/SideloadDialog/index.tsx (+2 more) |
| `logInfo` | send | — | components/UI/Sidebar/components/HeroicVersion/index.tsx, screens/Login/components/SIDLogin/index.tsx, state/GlobalState.tsx, state/GlobalStateV2.ts |
| `showItemInFolder` | send | — | screens/WineManager/components/WineItem/index.tsx |
| `clipboardWriteText` | send | — | screens/Login/components/SIDLogin/index.tsx, screens/Settings/index.tsx, screens/Settings/sections/AdvancedSettings/index.tsx |
| `processShortcut` | send | — | helpers/shortcuts.ts |
| `addNewApp` | send | — | screens/Library/components/InstallModal/SideloadDialog/index.tsx |
| `showLogFileInFolder` | send | — | screens/Settings/sections/LogSettings/index.tsx |
| `addShortcut` | send | — | screens/Game/GameSubMenu/index.tsx |
| `removeShortcut` | send | — | screens/Game/GameSubMenu/index.tsx |
| `removeFromDMQueue` | send | — | screens/ConsoleMode/index.tsx, screens/DownloadManager/components/DownloadManagerItem/index.tsx, screens/Game/GamePage/index.tsx, screens/Library/components/GameCard/index.tsx |
| `clearDMFinished` | send | — | **not used by frontend** |
| `abort` | send | — | screens/Settings/sections/AdvancedSettings/index.tsx, screens/WineManager/components/WineItem/index.tsx |
| `connectivity-changed` | send | `connectivityChanged`, `onConnectivityChanged` | helpers/onlineMonitor.ts, state/GlobalState.tsx |
| `set-connectivity-online` | send | `setConnectivityOnline` | components/UI/OfflineMessage/index.tsx, screens/WebView/index.tsx |
| `changeTrayColor` | send | — | screens/Settings/components/UseDarkTrayIcon.tsx |
| `setSetting` | send | — | helpers/library.ts, hooks/useSettingsContext.ts, screens/Settings/components/AnalyticsDialog.tsx |
| `resumeCurrentDownload` | send | — | screens/DownloadManager/components/DownloadManagerItem/index.tsx |
| `pauseCurrentDownload` | send | — | screens/DownloadManager/components/DownloadManagerItem/index.tsx |
| `cancelDownload` | send | — | helpers/library.ts |
| `copySystemInfoToClipboard` | send | `copyToClipboard` | screens/Settings/sections/SystemInfo/index.tsx |
| `minimizeWindow` | send | — | components/UI/WindowControls/index.tsx |
| `maximizeWindow` | send | — | components/UI/WindowControls/index.tsx |
| `unmaximizeWindow` | send | — | components/UI/WindowControls/index.tsx |
| `closeWindow` | send | — | components/UI/WindowControls/index.tsx |
| `setFullscreen` | send | — | screens/ConsoleMode/index.tsx |
| `setTitleBarOverlay` | send | — | index.tsx |
| `winetricksInstall` | send | — | components/UI/Winetricks/index.tsx |
| `changeGameVersionPinnedStatus` | send | — | screens/Game/ModifyInstallModal/GOG/index.tsx |
| `logoutZoom` | send | — | state/GlobalState.tsx |
| `setGameMetadataOverride` | send | — | components/UI/EditGameDialog/index.tsx |
| `kill` | invoke | — | helpers/index.ts |
| `checkDiskSpace` | invoke | — | screens/Library/components/InstallModal/DownloadDialog/index.tsx |
| `callTool` | invoke | — | components/UI/Winetricks/index.tsx, screens/Settings/components/Tools/index.tsx |
| `runWineCommand` | invoke | — | screens/Library/components/InstallModal/SideloadDialog/index.tsx |
| `winetricksInstalled` | invoke | `winetricksListInstalled` | components/UI/Winetricks/index.tsx |
| `winetricksAvailable` | invoke | `winetricksListAvailable` | components/UI/Winetricks/index.tsx |
| `checkGameUpdates` | invoke | — | state/GlobalState.tsx |
| `getEpicGamesStatus` | invoke | — | **not used by frontend** |
| `updateAll` | invoke | — | **not used by frontend** |
| `getMaxCpus` | invoke | — | screens/Settings/components/MaxWorkers.tsx |
| `getHeroicVersion` | invoke | — | components/UI/ChangelogModal/index.tsx, components/UI/Sidebar/components/HeroicVersion/index.tsx |
| `getLegendaryVersion` | invoke | — | screens/Settings/components/AltLegendaryBin.tsx |
| `getGogdlVersion` | invoke | — | screens/Settings/components/AltGOGdlBin.tsx |
| `getCometVersion` | invoke | — | **not used by frontend** |
| `getNileVersion` | invoke | — | screens/Settings/components/AltNileBin.tsx |
| `isFullscreen` | invoke | — | state/GlobalState.tsx |
| `isFrameless` | invoke | — | state/GlobalState.tsx |
| `isMaximized` | invoke | — | components/UI/WindowControls/index.tsx |
| `isMinimized` | invoke | — | **not used by frontend** |
| `showUpdateSetting` | invoke | — | screens/Settings/components/CheckUpdatesOnStartup.tsx |
| `getLatestReleases` | invoke | — | components/UI/Sidebar/components/HeroicVersion/index.tsx |
| `getCurrentChangelog` | invoke | — | components/UI/ChangelogModal/index.tsx |
| `getGameInfo` | invoke | — | components/UI/UninstallModal/index.tsx, helpers/index.ts, hooks/hasStatus.ts, state/GlobalState.tsx |
| `getAchievements` | invoke | — | screens/Game/GamePage/index.tsx |
| `getExtraInfo` | invoke | — | screens/Game/GamePage/index.tsx |
| `getGameSettings` | invoke | — | components/UI/UninstallModal/index.tsx, helpers/index.ts |
| `getGOGLinuxInstallersLangs` | invoke | — | **not used by frontend** |
| `getInstallInfo` | invoke | — | helpers/index.ts |
| `getUserInfo` | invoke | — | screens/Login/components/SIDLogin/index.tsx, state/GlobalState.tsx |
| `getAmazonUserInfo` | invoke | — | state/GlobalState.tsx |
| `getZoomUserInfo` | invoke | — | state/GlobalState.tsx |
| `isLoggedIn` | invoke | — | state/GlobalState.tsx |
| `login` | invoke | — | state/GlobalState.tsx |
| `authGOG` | invoke | — | state/GlobalState.tsx |
| `authAmazon` | invoke | — | state/GlobalState.tsx |
| `authZoom` | invoke | — | state/GlobalState.tsx |
| `logoutLegendary` | invoke | — | state/GlobalState.tsx |
| `logoutAmazon` | invoke | — | state/GlobalState.tsx |
| `getAlternativeWine` | invoke | — | screens/ConsoleMode/InstallOverlay/index.tsx, screens/Library/components/InstallModal/index.tsx, screens/Settings/components/WineVersionSelector.tsx |
| `readConfig` | invoke | — | helpers/index.ts |
| `requestAppSettings` | invoke | — | components/UI/ThemeSelector/index.tsx, helpers/gamepad.ts, helpers/library.ts, hooks/useSettingsContext.ts, screens/ConsoleMode/InstallOverlay/index.tsx, screens/Game/GameSubMenu/index.tsx (+5 more) |
| `requestGameSettings` | invoke | — | components/UI/UninstallModal/index.tsx, helpers/library.ts, hooks/useSettingsContext.ts, screens/ConsoleMode/InstallOverlay/index.tsx, screens/Game/GamePage/index.tsx, screens/Library/components/InstallModal/DownloadDialog/index.tsx (+1 more) |
| `writeConfig` | invoke | — | helpers/index.ts, screens/Library/components/InstallModal/SideloadDialog/index.tsx |
| `refreshLibrary` | invoke | `handleRefreshLibrary`, `refreshLibrary` | state/GlobalState.tsx |
| `launch` | invoke | — | helpers/library.ts |
| `openDialog` | invoke | — | components/UI/PathSelectionBox/index.tsx, screens/Game/GameSubMenu/index.tsx, screens/Library/components/InstallModal/SideloadDialog/index.tsx, screens/Settings/components/CustomWineProton.tsx, screens/Settings/components/Tools/index.tsx |
| `install` | invoke | — | components/UI/Winetricks/index.tsx, helpers/library.ts, screens/Game/GameSubMenu/index.tsx, screens/Settings/sections/AdvancedSettings/index.tsx, screens/WineManager/components/WineItem/index.tsx, state/GlobalState.tsx |
| `uninstall` | invoke | — | components/UI/UninstallModal/index.tsx, screens/Game/GameSubMenu/index.tsx |
| `repair` | invoke | — | helpers/library.ts, screens/Game/GameSubMenu/index.tsx |
| `moveInstall` | invoke | — | screens/Game/GameSubMenu/index.tsx |
| `importGame` | invoke | — | screens/Library/components/InstallModal/ImportDialog/index.tsx |
| `updateGame` | invoke | — | helpers/library.ts, screens/Game/ModifyInstallModal/GOG/index.tsx |
| `changeInstallPath` | invoke | — | screens/Game/GameSubMenu/index.tsx |
| `egsSync` | invoke | — | screens/Settings/components/EgsSettings.tsx |
| `syncGOGSaves` | invoke | — | screens/Game/GamePage/components/CloudSavesSync.tsx, screens/Settings/sections/SyncSaves/gog.tsx |
| `syncSaves` | invoke | — | helpers/index.ts, screens/Game/GamePage/components/CloudSavesSync.tsx |
| `gamepadAction` | invoke | — | components/UI/Dialog/components/Dialog.tsx, components/UI/Dropdown/index.tsx, helpers/gamepad.ts |
| `runWineCommandForGame` | invoke | — | **not used by frontend** |
| `getShellPath` | invoke | — | **not used by frontend** |
| `getWebviewPreloadPath` | invoke | — | screens/WebView/index.tsx |
| `clipboardReadText` | invoke | — | screens/Login/components/SIDLogin/index.tsx |
| `getCustomThemes` | invoke | — | components/UI/ThemeSelector/index.tsx |
| `getThemeCSS` | invoke | — | index.tsx |
| `isNative` | invoke | — | components/UI/UninstallModal/index.tsx, screens/Settings/sections/GamesSettings/index.tsx |
| `getLogContent` | invoke | — | screens/Settings/sections/LogSettings/index.tsx |
| `installWineVersion` | invoke | — | screens/WineManager/components/WineItem/index.tsx |
| `refreshWineVersionInfo` | invoke | — | state/GlobalStateV2.ts |
| `removeWineVersion` | invoke | — | screens/WineManager/components/WineItem/index.tsx |
| `wine.isValidVersion` | invoke | `isValidVersion` | screens/Settings/components/WineVersionSelector.tsx |
| `shortcutsExists` | invoke | — | screens/Game/GameSubMenu/index.tsx |
| `addToSteam` | invoke | — | screens/Game/GameSubMenu/index.tsx |
| `removeFromSteam` | invoke | — | screens/Game/GameSubMenu/index.tsx |
| `isAddedToSteam` | invoke | — | screens/Game/GameSubMenu/index.tsx |
| `getAnticheatInfo` | invoke | — | hooks/hasAnticheatInfo.ts |
| `getKnownFixes` | invoke | — | hooks/hasKnownFixes.ts |
| `getGameMetadataOverride` | invoke | — | **not used by frontend** |
| `getAllGameOverrides` | invoke | — | **not used by frontend** |
| `getEosOverlayStatus` | invoke | — | screens/Settings/sections/AdvancedSettings/index.tsx |
| `getLatestEosOverlayVersion` | invoke | — | screens/Settings/sections/AdvancedSettings/index.tsx |
| `updateEosOverlayInfo` | invoke | — | screens/Settings/sections/AdvancedSettings/index.tsx |
| `installEosOverlay` | invoke | — | screens/Game/GameSubMenu/index.tsx, screens/Settings/sections/AdvancedSettings/index.tsx |
| `removeEosOverlay` | invoke | — | screens/Settings/sections/AdvancedSettings/index.tsx |
| `enableEosOverlay` | invoke | — | screens/Game/GameSubMenu/index.tsx, screens/Settings/sections/AdvancedSettings/index.tsx |
| `disableEosOverlay` | invoke | — | screens/Game/GameSubMenu/index.tsx, screens/Settings/sections/AdvancedSettings/index.tsx |
| `isEosOverlayEnabled` | invoke | — | screens/Game/GameSubMenu/index.tsx, screens/Settings/sections/AdvancedSettings/index.tsx |
| `downloadRuntime` | invoke | — | screens/Settings/components/BattlEyeRuntime.tsx, screens/Settings/components/EacRuntime.tsx |
| `isRuntimeInstalled` | invoke | — | screens/Settings/components/BattlEyeRuntime.tsx, screens/Settings/components/EacRuntime.tsx |
| `getDMQueueInformation` | invoke | — | components/UI/Sidebar/index.tsx, screens/DownloadManager/index.tsx |
| `get-connectivity-status` | invoke | `getConnectivityStatus` | state/GlobalState.tsx |
| `getSystemInfo` | invoke | `get` | components/UI/Sidebar/components/HeroicVersion/index.tsx, components/UI/Sidebar/index.tsx, index.tsx, screens/Discounts/index.tsx, screens/Game/GamePage/index.tsx, screens/Game/ModifyInstallModal/GOG/index.tsx (+6 more) |
| `removeRecent` | invoke | `removeRecentGame` | screens/Library/components/GameCard/index.tsx |
| `getWikiGameInfo` | invoke | — | screens/Game/GamePage/index.tsx, screens/Game/GameSubMenu/index.tsx |
| `getDefaultSavePath` | invoke | — | screens/Game/GamePage/components/CloudSavesSync.tsx, screens/Settings/sections/SyncSaves/gog.tsx, screens/Settings/sections/SyncSaves/legendary.tsx |
| `isGameAvailable` | invoke | — | hooks/constants.ts |
| `toggleDXVK` | invoke | — | screens/Settings/components/AutoDXVK.tsx |
| `toggleVKD3D` | invoke | — | screens/Settings/components/AutoVKD3D.tsx |
| `toggleDXVKNVAPI` | invoke | — | screens/Settings/components/AutoDXVKNVAPI.tsx |
| `pathExists` | invoke | — | screens/Settings/components/WineVersionSelector.tsx |
| `getLaunchOptions` | invoke | — | helpers/library.ts, hooks/useLaunchOptions.ts |
| `getGameOverride` | invoke | — | screens/Library/components/InstallModal/DownloadDialog/index.tsx |
| `getGameSdl` | invoke | — | screens/Library/components/InstallModal/DownloadDialog/index.tsx |
| `getPlaytimeFromRunner` | invoke | `fetchPlaytimeFromServer` | screens/Game/TimeContainer/index.tsx |
| `getAmazonLoginData` | invoke | — | state/GlobalState.tsx |
| `hasExecutable` | invoke | — | screens/Settings/components/Gamescope.tsx |
| `setPrivateBranchPassword` | invoke | — | screens/Library/components/InstallModal/DownloadDialog/BranchSelector.tsx |
| `getPrivateBranchPassword` | invoke | — | screens/Game/ModifyInstallModal/GOG/index.tsx, screens/Library/components/InstallModal/DownloadDialog/index.tsx |
| `getAvailableCyberpunkMods` | invoke | — | screens/Game/ModifyInstallModal/GOG/index.tsx |
| `setCyberpunkModConfig` | invoke | `setCyberpunModConfig` | screens/Game/ModifyInstallModal/GOG/index.tsx |
| `uploadLogFile` | invoke | — | components/UI/LogFileUploadDialog/index.tsx |
| `deleteUploadedLogFile` | invoke | — | screens/Settings/sections/LogSettings/components/UploadedLogFilesList/index.tsx |
| `getUploadedLogFiles` | invoke | — | state/UploadedLogFiles.ts |
| `getCustomCSS` | invoke | — | index.tsx |
| `isIntelMac` | invoke | — | state/GlobalState.tsx |
| `getGogDealsRegion` | invoke | — | screens/Discounts/index.tsx |
| `getGogDiscounts` | invoke | — | screens/Discounts/index.tsx |
| `getGogWishlist` | invoke | — | screens/Discounts/index.tsx |
| `getGmgDiscounts` | invoke | — | screens/Discounts/index.tsx |
| `getHumbleDiscounts` | invoke | — | screens/Discounts/index.tsx |
| `steamgriddb.hasApiKey` | invoke | `hasApiKey` | components/UI/EditGameDialog/index.tsx, screens/Library/components/InstallModal/SideloadDialog/index.tsx, screens/Settings/components/SteamGridDbApiKey.tsx |
| `steamgriddb.setApiKey` | invoke | `setApiKey` | screens/Settings/components/SteamGridDbApiKey.tsx |
| `steamgriddb.searchGame` | invoke | `searchGame` | components/UI/SteamGridDBPicker/index.tsx |
| `steamgriddb.getGrids` | invoke | `getGrids` | components/UI/SteamGridDBPicker/index.tsx |
| `steamgriddb.getHeroes` | invoke | `getHeroes` | components/UI/SteamGridDBPicker/index.tsx |
| `gameStatusUpdate` | event | `handleGameStatus` | state/GlobalState.tsx |
| `wineVersionsUpdated` | event | `handleWineVersionsUpdated` | screens/Settings/components/WineVersionSelector.tsx, screens/WineManager/index.tsx |
| `showDialog` | event | `handleShowDialog` | components/UI/DialogHandler/index.tsx |
| `changedDMQueueInformation` | event | `handleDMQueueInformation` | components/UI/Sidebar/index.tsx, screens/DownloadManager/index.tsx |
| `maximized` | event | `handleMaximized` | components/UI/WindowControls/index.tsx |
| `unmaximized` | event | `handleUnmaximized` | components/UI/WindowControls/index.tsx |
| `fullscreen` | event | `handleFullscreen` | state/GlobalState.tsx |
| `refreshLibrary` | event | `handleRefreshLibrary`, `refreshLibrary` | state/GlobalState.tsx |
| `openScreen` | event | `handleGoToScreen` | components/UI/Sidebar/index.tsx |
| `connectivity-changed` | event | `connectivityChanged`, `onConnectivityChanged` | helpers/onlineMonitor.ts, state/GlobalState.tsx |
| `launchGame` | event | — | **not used by frontend** |
| `installGame` | event | `handleInstallGame` | state/GlobalState.tsx |
| `recentGamesChanged` | event | `handleRecentGamesChanged` | screens/Library/components/RecentlyPlayed/index.tsx |
| `pushGameToLibrary` | event | `handleGamePush` | state/GlobalState.tsx |
| `progressOfWinetricks` | event | `handleProgressOfWinetricks` | components/UI/Winetricks/index.tsx |
| `progressOfWineManager` | event | `handleProgressOfWineManager` | screens/WineManager/state.ts |
| `installing-winetricks-component` | event | `handleWinetricksInstalling` | components/UI/Winetricks/index.tsx |
| `logFileUploaded` | event | `logFileUploadedSlot` | state/UploadedLogFiles.ts |
| `logFileUploadDeleted` | event | `logFileUploadDeletedSlot` | state/UploadedLogFiles.ts |
| `progressUpdate` | event | `onProgressUpdate` | state/InstallProgress.ts |
| `metadataChanged` | event | `handleMetadataChanged` | state/GlobalState.tsx |
| `message` | event | — | index.tsx, screens/Discounts/index.tsx, screens/Game/GamePage/components/CloudSavesSync.tsx, screens/Game/GameSubMenu/index.tsx, screens/Settings/components/Gamescope.tsx, state/GlobalState.tsx |
