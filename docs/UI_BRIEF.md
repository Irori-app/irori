# UI brief for Irori feature crates

Read `docs/AGENT_BRIEF.md` first (general rules still apply), then this file.

## Required reading before writing UI code

1. gpui-kit Design Guides (normative):
   `/tmp/claude-1000/-home-radud--config-Claude-scratch-workspaces-be9c0f01-1ca9-426c-9323-c4f5fd7497b3-bb73d751-aec5-4f35-9234-f6306f8360c4-scratch-2026-10-22-dd28a5/7cad61c2-7751-448b-add9-7e546ed39814/scratchpad/ref/gpui-component/skills/gpui-kit-design-guides/references/design-guides.md`
2. gpui-kit Coding Guides (normative) and skill references (component catalog, recipes, GPUI mechanics):
   `.../scratchpad/ref/gpui-component/skills/gpui-kit/SKILL.md`, `references/coding-guides.md`,
   `references/usage.md`, `references/conventions.md`, `references/recipes.md`, `references/gpui/*.md`
   (same scratchpad root as above).
3. **Never invent an API.** The exact sources we compile against are in
   `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` (`gpui-kit-0.8.0`,
   `gpui-component-0.8.0`, `gpui-base-0.8.0`, `gpui-pre-0.3.9`). Grep them for signatures.
   Examples: `.../scratchpad/ref/gpui-component/examples/` and `crates/story/src` in that repo.
4. `docs/parity/frontend-features.md` — every capability Heroic has, by screen, plus its
   pain points. **Parity is mandatory; Heroic's layout is not.** Fix its listed pain points.
5. `crates/ui/src` (irori-ui): `state` (backend access, `run` bridge), `models` (observable
   `LibraryModel`, `StatusModel`, `DownloadsModel`, `AccountsModel`, `SettingsModel`,
   `NoticeModel`), `nav` (`Route`, `navigate`), `art` (thumbnail pipeline, `LruImageCache`),
   `components` (`GameCover`, `art_img`, `StoreBadge`, `ProgressSummary`, `EmptyState`,
   `PageHeader`, `SectionTitle`), `theme`.
6. `crates/services/src` — the `Backend` API you call (library, settings, userdata, statuses,
   downloads, launch/stop/uninstall/move/repair/import/sync_saves/run_in_prefix/winetricks).
   Store-specific extras are on the store crates (`irori_sideload::SideloadStore`, etc.); reach a
   concrete store via `backend.store(runner)` and the public API documented in its crate.

## Architecture contract

- Applications depend on `gpui-kit` only: `use gpui_kit::*;` for GPUI, `gpui_kit::component::*`
  for components, `gpui_kit::assets::IconName` for the full Lucide icon set
  (`Icon::new(IconName::Gamepad2)`).
- Your crate exposes view entities (`impl Render`) with `pub fn new(..., window, cx) -> Self`
  constructors plus any `pub fn open_*_dialog(window, cx)` entry points other features need. The
  shell (`crates/irori`) owns the window, `Root`, title bar, sidebar and routing; you own what
  renders inside the content area for your routes.
- Backend calls: `let task = irori_ui::run(cx, async move { backend.launch(&id, None, vec![]).await });`
  then `cx.spawn_in(window, async move |this, cx| { let r = task.await; this.update_in(cx, |..| ...) })`.
  Never block the UI thread; never call backend `async fn`s outside `irori_ui::run` (they need
  the Tokio runtime).
- Observe only the models you render (`cx.observe(&models(cx).library, |_, _, cx| cx.notify())`)
  and keep the returned `Subscription`s in the view.
- Report errors to the user: `window.push_notification(Notification::error(...)...)` (check the
  real API) or inline `Alert`. Never only log.
- Confirm only destructive actions (uninstall, delete prefix, cancel download with file
  deletion) with `AlertDialog`-style copy naming the object: `Uninstall “Hades”?` / `Uninstall`.

## Product direction: the best launcher on Linux

Fast, calm, information-dense where it matters, one click to play.

- **Every action is visible.** No hover-only menus as the only path. Each game action is
  reachable from the game page's visible buttons, a right-click context menu, the command
  palette, and keyboard shortcuts — all dispatching the same `Action`/method.
- **One-click play** from library tiles and the "Continue playing" row.
- **Live state everywhere**: install progress on tiles (thin bar + percent), "Playing" state,
  update-available dot, queued badge. Status comes from `StatusModel`.
- **Keyboard and gamepad first-class**: focusable tiles, arrow-key grid navigation, `Enter`
  opens, `Space`/`Ctrl+Enter` plays, `/` or `Ctrl+F` focuses search, `Esc` dismisses.
  Visible focus rings (`focus_visible`).
- **Install as a side sheet**, not a modal wall: disk-space bar (free vs required), path picker
  (`cx.prompt_for_paths`), platform, language, DLC checklist with sizes, optional components,
  Wine/prefix for Windows games, shortcut toggles, and a primary button stating the size:
  `Install · 12.3 GiB`.
- **Per-game settings inherit global defaults**; each overridden field shows an "Overridden"
  marker with a one-click reset (backed by `SettingsService::set_game_value(id, key, None)`).
  Settings are searchable.
- **Honest feedback**: every async action has loading, success and failure states; offline
  state is explained, not silent.
- **Performance**: virtualize every list that can exceed ~50 items (`v_virtual_list` from
  gpui-base / `List`/`DataTable` from gpui-component); wrap art-heavy regions in
  `div().image_cache(LruImageCache)`; never clone the whole library per frame (hold
  `Arc<GameInfo>`); derive filtered lists only when inputs change (cache by revision), not every render.
- **Visual language**: theme tokens only (`cx.theme().*`), no raw colors or px values in
  application code (use rem helpers `p_4()`, `gap_3()`, `w_64()`…). Cover art carries the
  color; chrome stays neutral; primary color is reserved for the main action of a region.
  Respect `SettingsModel.app.ui.reduce_motion` / `disable_animations`.
- Copy: sentence case, verbs on buttons ("Install", "Play", "Verify files"), no "Are you sure?".

## Quality bar

- `cargo fmt`, `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo test -p <crate>`
  pass with zero warnings, using `CARGO_TARGET_DIR=target/agents/<crate>`.
- Unit-test pure logic (filtering, sorting, view-state transitions, formatting). Add
  `#[gpui_kit::test]` UI tests where practical (see `references/gpui/test.md`).
- Run the Design review checklist from the design guide against your screens before finishing.
- You cannot see the screen. Be meticulous with layout: flex ownership, `min_w_0`/`min_h_0`,
  one scroll owner per region, `items_stretch` for full-height columns.
