//! The Big Picture screen: a fullscreen, controller-first library.
//!
//! Keyboard focus stays on the view's root; tiles and overlay entries are a
//! virtual focus (`aria-activedescendant` style) driven by the gamepad
//! actions, which the keyboard reaches through the `BigPicture` key context.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use chrono::Timelike as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
    VirtualListScrollHandle, WindowExt as _, h_flex, h_virtual_list, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnimationExt as _, AnyElement, App, BoxShadow, Context, Entity, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Pixels, Render, Role,
    ScrollHandle, ScrollStrategy, SharedString, Size, SpringAnimation, SpringConfig,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, Window,
    div, linear_color_stop, linear_gradient, point, px, rems, size, transparent_black,
};
use irori_core::{GameId, GameInfo, InstallInfoQuery, RemoveArgs, StatusKind, UpdateRequest};
use irori_ui::art::{LruImageCache, art_store};
use irori_ui::components::{
    ArtImage, BrandMark, EmptyState, art_image, art_img, art_loading, missing_art_fill,
    physical_width,
};
use irori_ui::i18n::{t, t_args};
use irori_ui::models;
use irori_ui::style::art_tint;

use crate::gamepad::{self, GamepadInput};
use crate::grid::{FocusGrid, GridMove};
use crate::model::{
    GameAction, GameState, MenuItem, Row, RowKind, build_rows, default_install_request,
    default_platform, disabled_reason, game_actions, menu_items, next_scale,
};
use crate::text::{self, status_label, status_with_percent, store_name};

/// Cover tile width in rem at rest.
const TILE_W: f32 = 12.0;
/// Cover aspect ratio (height / width).
const TILE_RATIO: f32 = 1.5;
/// Scale of the focused tile.
const FOCUS_SCALE: f32 = 1.1;
/// Space between tile slots in rem.
const TILE_GAP: f32 = 1.5;
/// Tiles moved by the shoulder buttons.
const PAGE: isize = 5;
/// Focus must rest this long before the backdrop art follows it, so fast
/// scrolling does not fetch a hero image per tile passed.
const HERO_DELAY: Duration = Duration::from_millis(250);
/// Height and widest extent of the focused game's logo, in rem.
const LOGO_H: f32 = 5.0;
const LOGO_MAX_W: f32 = 28.0;
/// Decoded cover images kept in memory.
const IMAGE_CACHE: usize = 96;
/// Critically damped spring (≈180 ms response) for the focus scale.
const FOCUS_SPRING: SpringConfig = SpringConfig::new(1200.0, 69.3, 1.0);

/// What covers the shelves.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    /// Commands for a game. `confirm` is set while asking to uninstall
    /// (0 = Cancel focused, 1 = Uninstall focused).
    Game {
        id: GameId,
        ix: usize,
        confirm: Option<usize>,
    },
    Menu {
        ix: usize,
    },
}

#[derive(Clone, Copy)]
struct Colors {
    background: Hsla,
    foreground: Hsla,
    muted: Hsla,
    muted_foreground: Hsla,
    ring: Hsla,
    border: Hsla,
    popover: Hsla,
    overlay: Hsla,
    radius: Pixels,
    radius_lg: Pixels,
}

impl Colors {
    fn of(cx: &App) -> Self {
        let t = cx.theme();
        Self {
            background: t.background,
            foreground: t.foreground,
            muted: t.muted,
            muted_foreground: t.muted_foreground,
            ring: t.ring,
            border: t.border,
            popover: t.popover,
            overlay: t.overlay,
            radius: t.radius,
            radius_lg: t.radius_lg,
        }
    }
}

/// The Big Picture view (route [`irori_ui::Route::BigPicture`]). Owns its
/// shelves (derived from the library model when its revision changes), the
/// focus position, the open overlay, and the window's fullscreen state while
/// it lives: it enters fullscreen when created and restores the previous
/// state when released.
pub struct BigPictureView {
    focus: FocusHandle,
    rows: Vec<Row>,
    rows_key: Option<(u64, usize, u32)>,
    item_sizes: Vec<Rc<Vec<Size<Pixels>>>>,
    sizes_rem: Option<Pixels>,
    grid: FocusGrid,
    row_scrolls: HashMap<RowKind, VirtualListScrollHandle>,
    page_scroll: ScrollHandle,
    /// The rows changed; scroll the focused tile into view at the next render.
    reveal_pending: bool,
    overlay: Option<Overlay>,
    pending: HashSet<GameId>,
    image_cache: Entity<LruImageCache>,
    hero: Option<Arc<GameInfo>>,
    hero_task: Option<Task<()>>,
    _clock_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// The current time in the interface language's usual clock format.
fn clock_text() -> SharedString {
    let now = chrono::Local::now();
    text::clock(now.hour(), now.minute())
}

impl BigPictureView {
    /// Build the view, enter fullscreen (remembering the previous state) and
    /// take keyboard focus.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let m = models(cx).clone();
        let mut subscriptions = vec![
            cx.observe(&m.library, |this, _, cx| {
                this.refresh_rows(cx);
                cx.notify();
            }),
            cx.observe(&m.settings, |this, settings, cx| {
                let zoom = settings.read(cx).app.ui.big_picture_zoom();
                irori_ui::theme::set_zoom(zoom, cx);
                this.refresh_rows(cx);
                cx.notify();
            }),
            cx.observe(&m.statuses, |_, _, cx| cx.notify()),
            cx.observe(&m.downloads, |_, _, cx| cx.notify()),
            cx.observe(&art_store(cx), |_, _, cx| cx.notify()),
            cx.observe_global::<crate::ShowGame>(|this, cx| this.show_requested_game(cx)),
            cx.on_focus_lost(window, |this, window, cx| this.focus.focus(window, cx)),
        ];
        if let Some(pad) = gamepad::model(cx) {
            subscriptions.push(cx.observe(&pad, |_, _, cx| cx.notify()));
        }

        // Big Picture's own interface size, for TVs, while it shows.
        irori_ui::theme::set_zoom(m.settings.read(cx).app.ui.big_picture_zoom(), cx);
        cx.on_release(|_, cx| irori_ui::theme::set_zoom(1.0, cx))
            .detach();

        let restore_fullscreen = window.is_fullscreen();
        if !restore_fullscreen {
            window.toggle_fullscreen();
        }
        cx.on_release_in(window, move |_, window, _| {
            if window.is_fullscreen() != restore_fullscreen {
                window.toggle_fullscreen();
            }
        })
        .detach();

        let clock_task = cx.spawn(async move |this, cx| {
            loop {
                let second = chrono::Local::now().second() as u64;
                cx.background_executor()
                    .timer(Duration::from_secs(60 - second.min(59)))
                    .await;
                let updated = this.update(cx, |_, cx| cx.notify());
                if updated.is_err() {
                    break;
                }
            }
        });

        focus.focus(window, cx);
        let mut this = Self {
            focus,
            rows: Vec::new(),
            rows_key: None,
            item_sizes: Vec::new(),
            sizes_rem: None,
            grid: FocusGrid::default(),
            row_scrolls: HashMap::new(),
            page_scroll: ScrollHandle::new(),
            reveal_pending: true,
            overlay: None,
            pending: HashSet::new(),
            image_cache: LruImageCache::new(IMAGE_CACHE, cx),
            hero: None,
            hero_task: None,
            _clock_task: clock_task,
            _subscriptions: subscriptions,
        };
        this.refresh_rows(cx);
        this.show_requested_game(cx);
        this.hero = this.focused_game().cloned();
        this
    }

    /// Focus the game asked for through [`crate::show_game`] and open its
    /// commands.
    fn show_requested_game(&mut self, cx: &mut Context<Self>) {
        let Some(id) = crate::take_shown_game(cx) else {
            return;
        };
        let found = self.rows.iter().enumerate().find_map(|(row, r)| {
            r.games
                .iter()
                .position(|g| g.id() == id)
                .map(|col| (row, col))
        });
        if let Some((row, col)) = found {
            let lens = self.lens();
            self.grid.set(row, col, &lens);
            self.after_focus_change(cx);
        }
        self.overlay = Some(Overlay::Game {
            id,
            ix: 0,
            confirm: None,
        });
        cx.notify();
    }

    /// The focused game, if any shelf has games.
    pub fn focused_game(&self) -> Option<&Arc<GameInfo>> {
        let (row, col) = self.grid.focused(&self.lens())?;
        self.rows.get(row)?.games.get(col)
    }

    /// Whether a game or menu overlay is open.
    pub fn is_overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    fn lens(&self) -> Vec<usize> {
        self.rows.iter().map(|r| r.games.len()).collect()
    }

    fn reduce_motion(cx: &App) -> bool {
        let app = &models(cx).settings.read(cx).app;
        app.ui.reduce_motion || app.disable_animations
    }

    /// Rebuild the shelves when the library, user data or recent-games limit
    /// changed, keeping focus on the same game when it still exists.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let m = models(cx);
        let (snapshot, user) = {
            let lib = m.library.read(cx);
            (lib.snapshot.clone(), lib.user.clone())
        };
        let max_recent = m.settings.read(cx).app.max_recent_games;
        let key = (snapshot.revision, Arc::as_ptr(&user) as usize, max_recent);
        if self.rows_key == Some(key) {
            return;
        }
        let previous = self.grid.focused(&self.lens()).and_then(|(r, c)| {
            Some((self.rows.get(r)?.kind, self.rows.get(r)?.games.get(c)?.id()))
        });
        self.rows = build_rows(&snapshot, &user, max_recent as usize);
        self.rows_key = Some(key);
        self.item_sizes.clear();
        let lens = self.lens();
        self.grid.clamp(&lens);
        if let Some((kind, id)) = previous
            && let Some(row) = self.rows.iter().position(|r| r.kind == kind)
            && let Some(col) = self.rows[row].games.iter().position(|g| g.id() == id)
        {
            self.grid.set(row, col, &lens);
        }
        if self.hero.is_none() {
            self.hero = self.focused_game().cloned();
        }
        self.reveal_pending = true;
    }

    fn game_state(game: &GameInfo, cx: &App) -> GameState {
        let m = models(cx);
        let id = game.id();
        let lib = m.library.read(cx);
        let statuses = m.statuses.read(cx);
        GameState {
            installed: game.is_installed,
            installable: game.installable,
            playing: statuses.is_playing(&id),
            busy: statuses.kind(&id).filter(|k| k.is_busy()),
            in_queue: m.downloads.read(cx).queue.active.iter().any(|i| i.id == id),
            has_update: lib.has_update(&id),
            favourite: lib.is_favourite(&id),
            external: !id.runner.manages_installs(),
        }
    }

    fn overlay_game(&self, cx: &App) -> Option<Arc<GameInfo>> {
        let Some(Overlay::Game { id, .. }) = &self.overlay else {
            return None;
        };
        models(cx)
            .library
            .read(cx)
            .snapshot
            .get(id)
            .cloned()
            .or_else(|| {
                self.rows
                    .iter()
                    .flat_map(|r| r.games.iter())
                    .find(|g| g.id() == *id)
                    .cloned()
            })
    }

    // ---- input ---------------------------------------------------------

    fn on_input(&mut self, input: GamepadInput, window: &mut Window, cx: &mut Context<Self>) {
        match self.overlay.clone() {
            Some(overlay) => self.overlay_input(overlay, input, window, cx),
            None => self.grid_input(input, window, cx),
        }
    }

    fn grid_input(&mut self, input: GamepadInput, window: &mut Window, cx: &mut Context<Self>) {
        let mv = match input {
            GamepadInput::Up => GridMove::Up,
            GamepadInput::Down => GridMove::Down,
            GamepadInput::Left => GridMove::Left,
            GamepadInput::Right => GridMove::Right,
            GamepadInput::PrevPage => GridMove::Page(-PAGE),
            GamepadInput::NextPage => GridMove::Page(PAGE),
            GamepadInput::Confirm => {
                if let Some(game) = self.focused_game() {
                    self.overlay = Some(Overlay::Game {
                        id: game.id(),
                        ix: 0,
                        confirm: None,
                    });
                    cx.notify();
                }
                return;
            }
            GamepadInput::Alt => {
                self.quick_action(window, cx);
                return;
            }
            GamepadInput::Menu => {
                self.overlay = Some(Overlay::Menu { ix: 0 });
                cx.notify();
                return;
            }
            GamepadInput::Back | GamepadInput::Guide => {
                crate::exit_big_picture(cx);
                return;
            }
        };
        let lens = self.lens();
        if self.grid.apply(mv, &lens) {
            self.after_focus_change(cx);
        }
    }

    /// Alt on a tile: the game's main action without opening the overlay
    /// (play, or install with defaults). Anything else opens the overlay.
    fn quick_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(game) = self.focused_game().cloned() else {
            return;
        };
        let state = Self::game_state(&game, cx);
        let main = game_actions(state)[0];
        if matches!(main, GameAction::Play | GameAction::Install)
            && disabled_reason(main, state).is_none()
        {
            self.run_game_action(main, game, window, cx);
        } else {
            self.overlay = Some(Overlay::Game {
                id: game.id(),
                ix: 0,
                confirm: None,
            });
            cx.notify();
        }
    }

    fn overlay_input(
        &mut self,
        overlay: Overlay,
        input: GamepadInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match overlay {
            Overlay::Game {
                id,
                ix,
                confirm: Some(choice),
            } => match input {
                GamepadInput::Up
                | GamepadInput::Down
                | GamepadInput::Left
                | GamepadInput::Right => {
                    self.overlay = Some(Overlay::Game {
                        id,
                        ix,
                        confirm: Some(1 - choice.min(1)),
                    });
                    cx.notify();
                }
                GamepadInput::Confirm | GamepadInput::Alt => {
                    if choice == 1 {
                        self.uninstall(window, cx);
                    } else {
                        self.overlay = Some(Overlay::Game {
                            id,
                            ix,
                            confirm: None,
                        });
                        cx.notify();
                    }
                }
                GamepadInput::Back | GamepadInput::Menu => {
                    self.overlay = Some(Overlay::Game {
                        id,
                        ix,
                        confirm: None,
                    });
                    cx.notify();
                }
                _ => {}
            },
            Overlay::Game {
                ix, confirm: None, ..
            } => {
                let Some(game) = self.overlay_game(cx) else {
                    self.close_overlay(cx);
                    return;
                };
                let actions = game_actions(Self::game_state(&game, cx));
                match input {
                    GamepadInput::Up | GamepadInput::Left => {
                        self.set_overlay_ix(ix.saturating_sub(1), cx)
                    }
                    GamepadInput::Down | GamepadInput::Right => {
                        self.set_overlay_ix((ix + 1).min(actions.len().saturating_sub(1)), cx)
                    }
                    GamepadInput::Confirm | GamepadInput::Alt => {
                        if let Some(action) = actions.get(ix.min(actions.len().saturating_sub(1))) {
                            self.activate_game_action(*action, game, window, cx);
                        }
                    }
                    GamepadInput::Back => self.close_overlay(cx),
                    GamepadInput::Menu => {
                        self.overlay = Some(Overlay::Menu { ix: 0 });
                        cx.notify();
                    }
                    GamepadInput::Guide => {
                        self.close_overlay(cx);
                        crate::exit_big_picture(cx);
                    }
                    GamepadInput::PrevPage | GamepadInput::NextPage => {}
                }
            }
            Overlay::Menu { ix } => {
                let items = self.menu(cx);
                match input {
                    GamepadInput::Up | GamepadInput::Left => {
                        self.set_overlay_ix(ix.saturating_sub(1), cx)
                    }
                    GamepadInput::Down | GamepadInput::Right => {
                        self.set_overlay_ix((ix + 1).min(items.len().saturating_sub(1)), cx)
                    }
                    GamepadInput::Confirm | GamepadInput::Alt => {
                        if let Some(item) = items.get(ix) {
                            self.activate_menu_item(*item, window, cx);
                        }
                    }
                    GamepadInput::Back | GamepadInput::Menu => self.close_overlay(cx),
                    GamepadInput::Guide => {
                        self.close_overlay(cx);
                        crate::exit_big_picture(cx);
                    }
                    GamepadInput::PrevPage | GamepadInput::NextPage => {}
                }
            }
        }
    }

    fn set_overlay_ix(&mut self, new_ix: usize, cx: &mut Context<Self>) {
        match &mut self.overlay {
            Some(Overlay::Game { ix, .. }) | Some(Overlay::Menu { ix }) => *ix = new_ix,
            None => return,
        }
        cx.notify();
    }

    fn close_overlay(&mut self, cx: &mut Context<Self>) {
        if self.overlay.take().is_some() {
            cx.notify();
        }
    }

    fn after_focus_change(&mut self, cx: &mut Context<Self>) {
        if let Some((row, col)) = self.grid.focused(&self.lens()) {
            let kind = self.rows[row].kind;
            self.row_scroll(kind)
                .scroll_to_item(col, ScrollStrategy::Center);
            self.page_scroll.scroll_to_item(row);
        }
        self.schedule_hero(cx);
        cx.notify();
    }

    fn row_scroll(&mut self, kind: RowKind) -> VirtualListScrollHandle {
        self.row_scrolls
            .entry(kind)
            .or_insert_with(VirtualListScrollHandle::new)
            .clone()
    }

    fn schedule_hero(&mut self, cx: &mut Context<Self>) {
        let target = self.focused_game().map(|g| g.id());
        if self.hero.as_ref().map(|g| g.id()) == target {
            self.hero_task = None;
            return;
        }
        self.hero_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HERO_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.hero = this.focused_game().cloned();
                cx.notify();
            });
        }));
    }

    fn activate_tile(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lens = self.lens();
        self.grid.set(row, col, &lens);
        self.after_focus_change(cx);
        self.focus.focus(window, cx);
        self.grid_input(GamepadInput::Confirm, window, cx);
    }

    // ---- commands ------------------------------------------------------

    fn activate_game_action(
        &mut self,
        action: GameAction,
        game: Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = Self::game_state(&game, cx);
        if let Some(reason) = disabled_reason(action, state) {
            window.push_notification(Notification::warning(reason).title(game.title.clone()), cx);
            return;
        }
        if action == GameAction::Uninstall {
            if let Some(Overlay::Game { confirm, .. }) = &mut self.overlay {
                *confirm = Some(0);
                cx.notify();
            }
            return;
        }
        self.run_game_action(action, game, window, cx);
    }

    fn run_game_action(
        &mut self,
        action: GameAction,
        game: Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let b = irori_ui::backend(cx);
        let id = game.id();
        let title = game.title.clone();
        let error = |key: &str| t_args(key, &[("title", title.as_str())]);
        let (task, error_title, close): (Task<Result<()>>, SharedString, bool) = match action {
            GameAction::Play => {
                let id = id.clone();
                (
                    irori_ui::run(cx, async move { b.launch(&id, None, Vec::new()).await }),
                    error("bigpicture.error.launch"),
                    true,
                )
            }
            GameAction::Stop => {
                let id = id.clone();
                (
                    irori_ui::run(cx, async move { b.stop(&id).await }),
                    error("bigpicture.error.stop"),
                    false,
                )
            }
            GameAction::Install => {
                let app = models(cx).settings.read(cx).app.clone();
                let dir = irori_core::paths::expand_home(&app.default_install_path);
                let platform = default_platform(&game);
                let id = id.clone();
                let game = game.clone();
                (
                    irori_ui::run(cx, async move {
                        if let Some(launcher) = &game.third_party_managed_app {
                            bail!(
                                "{}",
                                t_args(
                                    "bigpicture.error.third_party",
                                    &[
                                        ("title", game.title.as_str()),
                                        ("launcher", launcher.as_str())
                                    ],
                                )
                            );
                        }
                        let store = b.store(id.runner).with_context(|| {
                            t_args(
                                "bigpicture.error.store_unavailable",
                                &[("store", store_name(id.runner).as_str())],
                            )
                        })?;
                        let info = store
                            .install_info(&id.app_name, platform, &InstallInfoQuery::default())
                            .await
                            .with_context(|| t("bigpicture.error.install_options"))?;
                        if let Some(launcher) = &info.third_party_managed_app {
                            bail!(
                                "{}",
                                t_args(
                                    "bigpicture.error.third_party",
                                    &[
                                        ("title", game.title.as_str()),
                                        ("launcher", launcher.as_str())
                                    ],
                                )
                            );
                        }
                        let request = default_install_request(&info, &dir, platform);
                        b.enqueue_install(id, game.title.clone(), request, info.download_size);
                        Ok(())
                    }),
                    error("bigpicture.error.install"),
                    true,
                )
            }
            GameAction::CancelDownload => {
                let id = id.clone();
                (
                    irori_ui::run(cx, async move {
                        b.cancel_download(&id, true);
                        Ok(())
                    }),
                    error("bigpicture.error.cancel_download"),
                    false,
                )
            }
            GameAction::Update => {
                let id = id.clone();
                let queued_title = title.clone();
                (
                    irori_ui::run(cx, async move {
                        b.enqueue_update(id, queued_title, UpdateRequest::default());
                        Ok(())
                    }),
                    error("bigpicture.error.update"),
                    true,
                )
            }
            GameAction::AddFavourite | GameAction::RemoveFavourite => {
                let on = action == GameAction::AddFavourite;
                let id = id.clone();
                (
                    irori_ui::run(cx, async move {
                        b.userdata.set_favourite(&id, on);
                        Ok(())
                    }),
                    t("bigpicture.error.favourites"),
                    false,
                )
            }
            GameAction::Uninstall => return,
        };
        self.track(id, task, error_title, close, window, cx);
    }

    fn uninstall(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(game) = self.overlay_game(cx) else {
            self.close_overlay(cx);
            return;
        };
        let b = irori_ui::backend(cx);
        let id = game.id();
        let task_id = id.clone();
        let task = irori_ui::run(cx, async move {
            b.uninstall(
                &task_id,
                RemoveArgs {
                    delete_files: true,
                    ..Default::default()
                },
            )
            .await
        });
        self.track(
            id,
            task,
            t_args(
                "bigpicture.error.uninstall",
                &[("title", game.title.as_str())],
            ),
            true,
            window,
            cx,
        );
    }

    /// Mark `id` busy until `task` finishes, then report a failure or close
    /// the overlay on success.
    fn track(
        &mut self,
        id: GameId,
        task: Task<Result<()>>,
        error_title: SharedString,
        close_on_success: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending.insert(id.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending.remove(&id);
                match result {
                    Ok(()) => {
                        if close_on_success
                            && matches!(&this.overlay, Some(Overlay::Game { id: open, .. }) if *open == id)
                        {
                            this.overlay = None;
                        }
                    }
                    // Only Play can hit saves that changed in both places.
                    Err(e)
                        if irori_ui::save_conflict::resolve_launch_error(
                            &id,
                            Default::default(),
                            &e,
                            window,
                            cx,
                        ) => {}
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}")).title(error_title),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn menu(&self, cx: &App) -> Vec<MenuItem> {
        let m = models(cx);
        let queue = &m.downloads.read(cx).queue;
        let scale = m.settings.read(cx).app.ui.big_picture_scale;
        menu_items(!queue.active.is_empty(), queue.paused, scale)
    }

    fn activate_menu_item(&mut self, item: MenuItem, window: &mut Window, cx: &mut Context<Self>) {
        let b = irori_ui::backend(cx);
        match item {
            MenuItem::PauseDownloads | MenuItem::ResumeDownloads => {
                let pause = item == MenuItem::PauseDownloads;
                let task = irori_ui::run(cx, async move {
                    if pause {
                        b.pause_downloads();
                    } else {
                        b.resume_downloads();
                    }
                    Ok(())
                });
                cx.spawn_in(window, async move |_, cx| {
                    if let Err(e) = task.await {
                        let _ = cx.update(|window, cx| {
                            window.push_notification(
                                Notification::error(format!("{e:#}"))
                                    .title(t("bigpicture.error.downloads")),
                                cx,
                            )
                        });
                    }
                })
                .detach();
            }
            MenuItem::InterfaceSize(current) => {
                let next = next_scale(current);
                let task = irori_ui::run(cx, async move {
                    b.settings.update_app(|app| app.ui.big_picture_scale = next);
                    Ok::<_, anyhow::Error>(())
                });
                task.detach();
            }
            MenuItem::ToggleFullscreen => window.toggle_fullscreen(),
            MenuItem::SetUpEmulation => {
                self.close_overlay(cx);
                crate::exit_big_picture(cx);
                irori_ui::hooks::open_emulation_setup(window, cx);
            }
            MenuItem::ExitBigPicture => {
                self.close_overlay(cx);
                crate::exit_big_picture(cx);
            }
            MenuItem::QuitIrori => cx.quit(),
        }
    }

    // ---- rendering -----------------------------------------------------

    fn ensure_item_sizes(&mut self, window: &Window) {
        let rem = window.rem_size();
        if self.sizes_rem == Some(rem) && self.item_sizes.len() == self.rows.len() {
            return;
        }
        let slot = size(
            rem * (TILE_W * FOCUS_SCALE + TILE_GAP),
            rem * (TILE_W * TILE_RATIO * FOCUS_SCALE + TILE_GAP),
        );
        self.item_sizes = self
            .rows
            .iter()
            .map(|r| Rc::new(vec![slot; r.games.len()]))
            .collect();
        self.sizes_rem = Some(rem);
    }

    fn render_tiles(
        &mut self,
        row_ix: usize,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(row) = self.rows.get(row_ix) else {
            return Vec::new();
        };
        let kind = row.kind;
        let focused = self.grid.focused(&self.lens());
        let animate = !Self::reduce_motion(cx);
        let games: Vec<(usize, Arc<GameInfo>)> = range
            .filter_map(|ix| row.games.get(ix).map(|g| (ix, g.clone())))
            .collect();
        games
            .into_iter()
            .map(|(ix, game)| {
                let is_focused = focused == Some((row_ix, ix)) && self.overlay.is_none();
                self.render_tile(kind, row_ix, ix, game, is_focused, animate, window, cx)
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tile(
        &self,
        kind: RowKind,
        row_ix: usize,
        ix: usize,
        game: Arc<GameInfo>,
        focused: bool,
        animate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = Colors::of(cx);
        let key = game.id().key();
        let state = Self::game_state(&game, cx);
        let status = models(cx).statuses.read(cx).get(&game.id()).cloned();
        let width = physical_width(rems(TILE_W * FOCUS_SCALE), window);
        let image = match game.cover_art() {
            Some(url) => art_image(url, width, false, cx),
            None => ArtImage::Missing,
        };

        let mut badges: Vec<AnyElement> = Vec::new();
        if state.playing {
            badges.push(
                Tag::primary()
                    .small()
                    .child(t("bigpicture.badge.playing"))
                    .into_any_element(),
            );
        } else if state.has_update {
            badges.push(
                Tag::secondary()
                    .small()
                    .child(t("bigpicture.badge.update"))
                    .into_any_element(),
            );
        }
        // Uninstalled games are dimmed instead of badged, so installed ones stand out.
        let dim = !state.installed && !state.in_queue && !focused && state.busy.is_none();
        let progress = status
            .filter(|s| s.kind.is_busy() && s.kind != StatusKind::Playing)
            .map(|s| {
                let pct = s.progress.and_then(|p| p.percent);
                (status_with_percent(s.kind, pct), pct)
            });

        let label = if state.installed {
            SharedString::from(game.title.clone())
        } else {
            t_args(
                "bigpicture.tile.not_installed",
                &[("title", game.title.as_str())],
            )
        };
        let tile = div()
            .id(SharedString::from(format!("bp-tile-{}-{key}", kind.id())))
            .role(Role::Button)
            .aria_label(label)
            .aria_selected(focused)
            .when(focused, |d| d.aria_active_descendant())
            .relative()
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(c.radius_lg)
            .bg(c.muted)
            .border_2()
            .border_color(if focused { c.ring } else { transparent_black() })
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: c.ring.opacity(0.45),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(28.),
                    spread_radius: px(2.),
                    inset: false,
                }])
            })
            .map(|d| match image {
                ArtImage::Ready(img) => d.child(
                    img.size_full()
                        .rounded(c.radius_lg)
                        .object_fit(ObjectFit::Cover)
                        .when(dim, |img| img.opacity(0.45)),
                ),
                ArtImage::Loading => d.child(art_loading()),
                ArtImage::Missing => d.child(
                    v_flex()
                        .size_full()
                        .justify_between()
                        .p_3()
                        .bg(missing_art_fill(cx))
                        .child(Icon::new(IconName::Gamepad2).text_color(c.muted_foreground))
                        .child(
                            div()
                                .text_lg()
                                .font_bold()
                                .line_clamp(4)
                                .text_color(c.foreground)
                                .child(game.title.clone()),
                        ),
                ),
            })
            .when(!badges.is_empty(), |d| {
                d.child(
                    h_flex()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .p_2()
                        .gap_1()
                        .min_w_0()
                        .overflow_hidden()
                        .justify_between()
                        .children(badges),
                )
            })
            .when_some(progress, |d, (label, pct)| {
                d.child(
                    v_flex()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .p_2()
                        .gap_1()
                        .bg(c.background.opacity(0.85))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(c.foreground)
                                .child(label),
                        )
                        .child(irori_ui::motion::gliding_progress(
                            SharedString::from(format!("bp-progress-{}-{key}", kind.id())),
                            pct,
                        )),
                )
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.activate_tile(row_ix, ix, window, cx)),
            );

        let target = if focused { FOCUS_SCALE } else { 1.0 };
        let sized: AnyElement = if animate {
            tile.with_spring(
                SharedString::from(format!("bp-scale-{}-{key}", kind.id())),
                SpringAnimation::new(FOCUS_SPRING).to(target),
                |el, scale: f32| {
                    el.w(rems(TILE_W * scale))
                        .h(rems(TILE_W * TILE_RATIO * scale))
                },
            )
            .into_any_element()
        } else {
            tile.w(rems(TILE_W * target))
                .h(rems(TILE_W * TILE_RATIO * target))
                .into_any_element()
        };
        div()
            .w(rems(TILE_W * FOCUS_SCALE + TILE_GAP))
            .h(rems(TILE_W * TILE_RATIO * FOCUS_SCALE + TILE_GAP))
            .flex()
            .items_center()
            .justify_center()
            .child(sized)
            .into_any_element()
    }

    fn render_backdrop(&self, c: Colors, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) * window.scale_factor()).ceil() as u32;
        let url = self
            .hero
            .as_ref()
            .and_then(|g| g.hero_art().map(str::to_owned));
        let tint = url
            .as_deref()
            .and_then(|url| irori_ui::art::ambient(url, cx))
            .map(|a| art_tint(a.rgb, cx));
        let image = url.and_then(|url| art_img(&url, width.min(1920), false, cx));
        div()
            .absolute()
            .inset_0()
            .size_full()
            .when_some(image, |d, img| {
                d.child(img.size_full().object_fit(ObjectFit::Cover).opacity(0.5))
            })
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(c.background.opacity(0.4), 0.),
                linear_color_stop(c.background, 0.7),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(c.background.opacity(0.9), 0.),
                linear_color_stop(c.background.opacity(0.), 0.65),
            )))
            // The focused game's colour rising behind the shelves.
            .when_some(tint, |d, tint| {
                d.child(div().absolute().inset_0().bg(linear_gradient(
                    0.,
                    linear_color_stop(tint.opacity(0.2), 0.),
                    linear_color_stop(tint.opacity(0.), 0.6),
                )))
            })
    }

    fn downloads_summary(cx: &App) -> Option<SharedString> {
        let m = models(cx);
        let queue = &m.downloads.read(cx).queue;
        if queue.paused && !queue.active.is_empty() {
            return Some(t_args(
                "bigpicture.downloads.paused",
                &[("count", queue.active.len().to_string().as_str())],
            ));
        }
        let current = queue.current()?;
        let waiting = queue.active.len().saturating_sub(1);
        let pct = m
            .statuses
            .read(cx)
            .get(&current.id)
            .and_then(|s| s.progress)
            .and_then(|p| p.percent)
            .map(|p| format!("{p:.0}"));
        let title = current.title.as_str();
        let count = waiting.to_string();
        Some(match (pct, waiting) {
            (None, 0) => SharedString::from(current.title.clone()),
            (Some(p), 0) => t_args(
                "bigpicture.downloads.progress",
                &[("title", title), ("percent", p.as_str())],
            ),
            (None, n) => t_args(
                if n == 1 {
                    "bigpicture.downloads.queued_one"
                } else {
                    "bigpicture.downloads.queued_other"
                },
                &[("title", title), ("count", count.as_str())],
            ),
            (Some(p), n) => t_args(
                if n == 1 {
                    "bigpicture.downloads.progress_queued_one"
                } else {
                    "bigpicture.downloads.progress_queued_other"
                },
                &[
                    ("title", title),
                    ("percent", p.as_str()),
                    ("count", count.as_str()),
                ],
            ),
        })
    }

    fn render_top_bar(c: Colors, cx: &App) -> impl IntoElement {
        let pad = gamepad::model(cx);
        let controller = pad.as_ref().and_then(|p| p.read(cx).primary().cloned());
        h_flex()
            .w_full()
            .px_12()
            .pt_8()
            .gap_6()
            .justify_between()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_3()
                    .child(BrandMark::new(rems(2.5)))
                    .child(div().text_2xl().font_extrabold().child("Irori"))
                    .child(
                        div()
                            .text_sm()
                            .font_extrabold()
                            .text_color(c.muted_foreground)
                            .child(SharedString::from(t("bigpicture.title").to_uppercase())),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .gap_8()
                    .text_lg()
                    .when_some(Self::downloads_summary(cx), |d, s| {
                        d.child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(Icon::new(IconName::Download))
                                .child(div().min_w_0().truncate().child(s)),
                        )
                    })
                    .child(match controller {
                        Some(info) => {
                            let battery_icon = match info.battery {
                                gamepad::Battery::Charging(_) => Some(IconName::BatteryCharging),
                                gamepad::Battery::Full => Some(IconName::BatteryFull),
                                b if b.is_low() => Some(IconName::BatteryLow),
                                gamepad::Battery::Discharging(_) => Some(IconName::BatteryMedium),
                                _ => None,
                            };
                            let name = info.kind.label();
                            let aria = match info.battery.label() {
                                Some(battery) => t_args(
                                    "bigpicture.controller.connected_battery",
                                    &[("controller", name.as_str()), ("battery", battery.as_str())],
                                ),
                                None => t_args(
                                    "bigpicture.controller.connected",
                                    &[("controller", name.as_str())],
                                ),
                            };
                            h_flex()
                                .id("bp-controller")
                                .role(Role::Status)
                                .aria_label(aria)
                                .flex_shrink_0()
                                .gap_2()
                                .child(Icon::new(IconName::Gamepad2))
                                .child(name)
                                .when_some(battery_icon, |d, icon| d.child(Icon::new(icon)))
                                .when_some(info.battery.label(), |d, l| d.child(l))
                                .into_any_element()
                        }
                        None => h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .text_color(c.muted_foreground)
                            .child(Icon::new(IconName::Gamepad2))
                            .child(t("bigpicture.controller.none"))
                            .into_any_element(),
                    })
                    .child({
                        let clock = clock_text();
                        div()
                            .id("bp-clock")
                            .role(Role::Timer)
                            .aria_label(clock.clone())
                            .flex_shrink_0()
                            .text_2xl()
                            .font_extrabold()
                            .child(clock)
                    }),
            )
    }

    fn status_text(game: &GameInfo, state: GameState, cx: &App) -> SharedString {
        if state.playing {
            return status_label(StatusKind::Playing);
        }
        if let Some(kind) = state.busy {
            let pct = models(cx)
                .statuses
                .read(cx)
                .get(&game.id())
                .and_then(|s| s.progress)
                .and_then(|p| p.percent);
            return status_with_percent(kind, pct);
        }
        if state.has_update {
            return t("bigpicture.state.update_available");
        }
        if state.installed {
            match game.install.as_ref().map(|i| i.install_size.as_str()) {
                Some(size) if !size.is_empty() => {
                    t_args("bigpicture.state.installed_size", &[("size", size)])
                }
                _ => t("bigpicture.state.installed"),
            }
        } else {
            t("bigpicture.state.not_installed")
        }
    }

    fn render_info(&self, c: Colors, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let game = self.focused_game();
        // The logo follows the backdrop, which waits for focus to rest, so
        // scrolling past tiles does not fetch a logo for each.
        let logo = game
            .filter(|g| self.hero.as_ref().is_some_and(|h| h.id() == g.id()))
            .and_then(|g| g.art_logo.clone())
            .filter(|url| !url.is_empty())
            .and_then(|url| {
                let px = physical_width(rems(LOGO_MAX_W), window);
                art_img(&url, px, true, cx)
            });
        v_flex()
            .w_full()
            .px_12()
            .pt_10()
            .pb_6()
            .gap_2()
            .when_some(game, |d, game| {
                let state = Self::game_state(game, cx);
                let library = models(cx).library.read(cx);
                let minutes = library.playtime_minutes(&game.id());
                let played = minutes > 0 || library.last_played(&game.id()).is_some();
                let playtime = if played {
                    t_args(
                        "bigpicture.info.played",
                        &[("playtime", text::playtime(minutes).as_str())],
                    )
                } else {
                    t("bigpicture.info.never_played")
                };
                d.child(
                    div()
                        .text_sm()
                        .font_extrabold()
                        .text_color(c.ring)
                        .child(SharedString::from(store_name(game.runner()).to_uppercase())),
                )
                .child(
                    div()
                        .id("bp-title")
                        .role(Role::Heading)
                        .aria_label(SharedString::from(game.title.clone()))
                        .map(|d| match logo {
                            Some(logo) => d.h(rems(LOGO_H)).max_w(rems(LOGO_MAX_W)).child(
                                logo.image_cache(&self.image_cache)
                                    .h_full()
                                    .object_fit(ObjectFit::Contain),
                            ),
                            None => d
                                .text_size(rems(3.))
                                .line_height(rems(3.4))
                                .font_extrabold()
                                .line_clamp(1)
                                .child(game.title.clone()),
                        }),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_4()
                        .text_lg()
                        .text_color(c.muted_foreground)
                        .child(div().flex_shrink_0().child(playtime))
                        .child("·")
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(Self::status_text(game, state, cx)),
                        ),
                )
            })
    }

    fn render_rows(
        &mut self,
        c: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_item_sizes(window);
        if std::mem::take(&mut self.reveal_pending)
            && let Some((row, col)) = self.grid.focused(&self.lens())
        {
            let kind = self.rows[row].kind;
            self.row_scroll(kind)
                .scroll_to_item(col, ScrollStrategy::Center);
            self.page_scroll.scroll_to_item(row);
        }
        let view = cx.entity();
        let rows: Vec<(usize, RowKind)> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.kind))
            .collect();
        let slot_h = TILE_W * TILE_RATIO * FOCUS_SCALE + TILE_GAP;
        div()
            .id("bp-rows")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.page_scroll)
            // Rows are the scroll area's direct children, so `scroll_to_item(row)`
            // brings that row into view.
            .flex()
            .flex_col()
            .gap_6()
            .pb_8()
            .children(rows.into_iter().map(|(row_ix, kind)| {
                let sizes = self.item_sizes[row_ix].clone();
                let scroll = self.row_scroll(kind);
                let view = view.clone();
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .px_12()
                            .min_w_0()
                            .truncate()
                            .text_base()
                            .font_extrabold()
                            .text_color(c.muted_foreground)
                            .child(SharedString::from(kind.title().to_uppercase())),
                    )
                    .child(
                        div().w_full().h(rems(slot_h)).child(
                            h_virtual_list(
                                view,
                                SharedString::from(format!("bp-row-{}", kind.id())),
                                sizes,
                                move |this, range, window, cx| {
                                    this.render_tiles(row_ix, range, window, cx)
                                },
                            )
                            .track_scroll(&scroll)
                            .px_12(),
                        ),
                    )
            }))
    }

    fn hints(&self, cx: &App) -> Vec<(GamepadInput, SharedString)> {
        match &self.overlay {
            Some(Overlay::Game {
                confirm: Some(_), ..
            }) => vec![
                (GamepadInput::Confirm, t("bigpicture.hint.select")),
                (GamepadInput::Back, t("bigpicture.hint.cancel")),
            ],
            Some(_) => vec![
                (GamepadInput::Confirm, t("bigpicture.hint.select")),
                (GamepadInput::Back, t("bigpicture.hint.close")),
            ],
            None => {
                let mut out = vec![(GamepadInput::Confirm, t("bigpicture.hint.details"))];
                if let Some(game) = self.focused_game() {
                    let state = Self::game_state(game, cx);
                    match game_actions(state)[0] {
                        GameAction::Play => {
                            out.push((GamepadInput::Alt, t("bigpicture.hint.play")))
                        }
                        GameAction::Install if state.installable => {
                            out.push((GamepadInput::Alt, t("bigpicture.hint.install")))
                        }
                        _ => {}
                    }
                }
                out.push((GamepadInput::Menu, t("bigpicture.hint.menu")));
                out.push((GamepadInput::Back, t("bigpicture.hint.exit")));
                out
            }
        }
    }

    fn render_hints(&self, c: Colors, cx: &App) -> impl IntoElement {
        let kind = gamepad::model(cx).and_then(|m| m.read(cx).active_kind());
        let glyph = |input: GamepadInput| -> &'static str {
            match kind {
                Some(kind) => kind.glyph(input),
                None => match input {
                    GamepadInput::Confirm => "Enter",
                    GamepadInput::Back => "Esc",
                    GamepadInput::Alt => "Space",
                    GamepadInput::Menu => "M",
                    GamepadInput::PrevPage => "PgUp",
                    GamepadInput::NextPage => "PgDn",
                    _ => "Arrows",
                },
            }
        };
        h_flex()
            .w_full()
            .px_12()
            .py_6()
            .gap_6()
            .flex_wrap()
            .justify_end()
            .text_base()
            .text_color(c.muted_foreground)
            .children(self.hints(cx).into_iter().map(|(input, label)| {
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .child(
                        div()
                            .min_w_8()
                            .px_2()
                            .py_0p5()
                            .rounded(c.radius)
                            .bg(c.muted)
                            .border_1()
                            .border_color(c.border)
                            .text_center()
                            .font_extrabold()
                            .text_color(c.foreground)
                            .child(glyph(input)),
                    )
                    .child(label)
            }))
    }

    #[allow(clippy::too_many_arguments)]
    fn overlay_button(
        id: &'static str,
        icon: IconName,
        label: SharedString,
        focused: bool,
        primary: bool,
        danger: bool,
        disabled: bool,
        loading: bool,
        c: Colors,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .p_1()
            .rounded(c.radius_lg)
            .border_2()
            .border_color(if focused { c.ring } else { transparent_black() })
            .child(
                Button::new(id)
                    .large()
                    .w_full()
                    .icon(icon)
                    .label(label)
                    .selected(focused)
                    .disabled(disabled)
                    .loading(loading)
                    .map(|b| {
                        if danger {
                            b.danger()
                        } else if primary {
                            b.primary()
                        } else {
                            b.outline()
                        }
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        on_click(this, window, cx);
                        this.focus.focus(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_overlay(&self, c: Colors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlay = self.overlay.clone()?;
        let panel = v_flex()
            .id("bp-overlay")
            .role(Role::Dialog)
            .w(rems(36.))
            .max_w_full()
            .p_8()
            .gap_6()
            .rounded(c.radius_lg)
            .bg(c.popover)
            .border_1()
            .border_color(c.border)
            .shadow_lg();
        let panel =
            match overlay {
                Overlay::Menu { ix } => {
                    let items = self.menu(cx);
                    let ix = ix.min(items.len().saturating_sub(1));
                    panel
                        .aria_label(t("bigpicture.menu.title"))
                        .child(
                            div()
                                .text_2xl()
                                .font_bold()
                                .child(t("bigpicture.menu.title")),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_lg()
                                .text_color(c.muted_foreground)
                                .child(
                                    Self::downloads_summary(cx)
                                        .unwrap_or_else(|| t("bigpicture.menu.no_downloads")),
                                ),
                        )
                        .child(v_flex().gap_2().children(items.into_iter().enumerate().map(
                            |(i, item)| {
                                Self::overlay_button(
                                    item.id(),
                                    item.icon(),
                                    item.label(),
                                    i == ix,
                                    false,
                                    false,
                                    false,
                                    false,
                                    c,
                                    move |this, window, cx| {
                                        this.set_overlay_ix(i, cx);
                                        this.activate_menu_item(item, window, cx);
                                    },
                                    cx,
                                )
                            },
                        )))
                        .into_any_element()
                }
                Overlay::Game { ix, confirm, .. } => {
                    let game = self.overlay_game(cx)?;
                    let state = Self::game_state(&game, cx);
                    let status = Self::status_text(&game, state, cx);
                    let header =
                        v_flex()
                            .gap_1()
                            .child(div().text_2xl().font_bold().line_clamp(2).child(
                                match confirm {
                                    Some(_) => t_args(
                                        "bigpicture.uninstall.title",
                                        &[("title", game.title.as_str())],
                                    ),
                                    None => SharedString::from(game.title.clone()),
                                },
                            ))
                            .child(div().text_lg().text_color(c.muted_foreground).child(
                                match confirm {
                                    Some(_) => t("bigpicture.uninstall.description"),
                                    None => SharedString::from(format!(
                                        "{} · {status}",
                                        store_name(game.runner())
                                    )),
                                },
                            ));
                    let panel = panel
                        .aria_label(SharedString::from(game.title.clone()))
                        .child(header);
                    match confirm {
                        Some(choice) => panel
                            .child(
                                h_flex()
                                    .gap_4()
                                    .child(div().flex_1().child(Self::overlay_button(
                                        "bp-confirm-cancel",
                                        IconName::X,
                                        t("bigpicture.uninstall.cancel"),
                                        choice == 0,
                                        false,
                                        false,
                                        false,
                                        false,
                                        c,
                                        |this, window, cx| {
                                            this.overlay_input_confirm(0, window, cx)
                                        },
                                        cx,
                                    )))
                                    .child(div().flex_1().child(Self::overlay_button(
                                        "bp-confirm-uninstall",
                                        IconName::Trash,
                                        t("bigpicture.uninstall.confirm"),
                                        choice == 1,
                                        false,
                                        true,
                                        false,
                                        self.pending.contains(&game.id()),
                                        c,
                                        |this, window, cx| {
                                            this.overlay_input_confirm(1, window, cx)
                                        },
                                        cx,
                                    ))),
                            )
                            .into_any_element(),
                        None => {
                            let actions = game_actions(state);
                            let ix = ix.min(actions.len().saturating_sub(1));
                            let loading = self.pending.contains(&game.id());
                            let reason = actions.get(ix).and_then(|a| disabled_reason(*a, state));
                            panel
                                .child(v_flex().gap_2().children(
                                    actions.into_iter().enumerate().map(|(i, action)| {
                                        let main = i == 0;
                                        Self::overlay_button(
                                            action.id(),
                                            action.icon(),
                                            action.label(),
                                            i == ix,
                                            main && action != GameAction::Stop,
                                            action == GameAction::Uninstall
                                                || action == GameAction::Stop,
                                            disabled_reason(action, state).is_some(),
                                            main && loading,
                                            c,
                                            move |this, window, cx| {
                                                this.set_overlay_ix(i, cx);
                                                this.overlay_input(
                                                    this.overlay
                                                        .clone()
                                                        .unwrap_or(Overlay::Menu { ix: 0 }),
                                                    GamepadInput::Confirm,
                                                    window,
                                                    cx,
                                                );
                                            },
                                            cx,
                                        )
                                    }),
                                ))
                                .when_some(reason, |d, r| {
                                    d.child(
                                        div().text_base().text_color(c.muted_foreground).child(r),
                                    )
                                })
                                .into_any_element()
                        }
                    }
                }
            };
        Some(
            div()
                .absolute()
                .inset_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(c.overlay)
                .child(panel)
                .into_any_element(),
        )
    }

    fn overlay_input_confirm(
        &mut self,
        choice: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Overlay::Game { confirm, .. }) = &mut self.overlay {
            *confirm = Some(choice);
        }
        if let Some(overlay) = self.overlay.clone() {
            self.overlay_input(overlay, GamepadInput::Confirm, window, cx);
        }
    }
}

impl Focusable for BigPictureView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for BigPictureView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let empty = self.rows.is_empty();
        let refreshing = models(cx).library.read(cx).refreshing;
        div()
            .image_cache(self.image_cache.clone())
            .id("big-picture")
            .key_context(crate::CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(c.background)
            .text_color(c.foreground)
            .on_action(
                cx.listener(|this, _: &gamepad::Up, w, cx| this.on_input(GamepadInput::Up, w, cx)),
            )
            .on_action(cx.listener(|this, _: &gamepad::Down, w, cx| {
                this.on_input(GamepadInput::Down, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::Left, w, cx| {
                this.on_input(GamepadInput::Left, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::Right, w, cx| {
                this.on_input(GamepadInput::Right, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::Confirm, w, cx| {
                this.on_input(GamepadInput::Confirm, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::Back, w, cx| {
                this.on_input(GamepadInput::Back, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &gamepad::Alt, w, cx| {
                    this.on_input(GamepadInput::Alt, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &gamepad::Menu, w, cx| {
                this.on_input(GamepadInput::Menu, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::PrevPage, w, cx| {
                this.on_input(GamepadInput::PrevPage, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::NextPage, w, cx| {
                this.on_input(GamepadInput::NextPage, w, cx)
            }))
            .on_action(cx.listener(|this, _: &gamepad::Guide, w, cx| {
                this.on_input(GamepadInput::Guide, w, cx)
            }))
            .child(self.render_backdrop(c, window, cx))
            .child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .child(Self::render_top_bar(c, cx))
                    .map(|d| {
                        if empty {
                            d.child(
                                div().flex_1().min_h_0().child(
                                    EmptyState::new(
                                        IconName::Gamepad2,
                                        if refreshing {
                                            t("bigpicture.empty.loading")
                                        } else {
                                            t("bigpicture.empty.title")
                                        },
                                    )
                                    .description(t("bigpicture.empty.description"))
                                    .action(
                                        Button::new("bp-empty-exit")
                                            .large()
                                            .label(t("bigpicture.exit"))
                                            .on_click(|_, _, cx| crate::exit_big_picture(cx)),
                                    ),
                                ),
                            )
                        } else {
                            d.child(self.render_info(c, window, cx))
                                .child(self.render_rows(c, window, cx))
                        }
                    })
                    .child(self.render_hints(c, cx)),
            )
            .when_some(self.render_overlay(c, cx), |d, overlay| d.child(overlay))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui_kit::{
        AppContext as _, Bounds, TestAppContext, WindowBounds, WindowOptions, px, size,
    };
    use irori_core::{
        DynStore, ExtraInfo, InstallInfo, InstallPlatform, InstallRequest, LaunchPlan,
        LaunchRequest, LoginFlow, OpContext, Outcome, Paths, Runner, Store, UserInfo, async_trait,
    };
    use irori_services::Backend;

    use super::*;

    struct FakeStore {
        runner: Runner,
        games: Vec<Arc<GameInfo>>,
    }

    #[async_trait]
    impl Store for FakeStore {
        fn runner(&self) -> Runner {
            self.runner
        }
        async fn init(&self) -> Result<()> {
            Ok(())
        }
        fn user(&self) -> Option<UserInfo> {
            None
        }
        fn login_flow(&self) -> LoginFlow {
            LoginFlow::None
        }
        async fn login(&self, _: &str) -> Result<UserInfo> {
            bail!("unsupported")
        }
        async fn logout(&self) -> Result<()> {
            Ok(())
        }
        async fn refresh(&self, _: bool) -> Result<()> {
            Ok(())
        }
        fn games(&self) -> Vec<Arc<GameInfo>> {
            self.games.clone()
        }
        fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
            self.games.iter().find(|g| g.app_name == app_name).cloned()
        }
        async fn extra_info(&self, _: &str) -> Result<ExtraInfo> {
            Ok(ExtraInfo::default())
        }
        async fn install_info(
            &self,
            _: &str,
            _: InstallPlatform,
            _: &InstallInfoQuery,
        ) -> Result<InstallInfo> {
            Ok(InstallInfo::default())
        }
        async fn updatable_games(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
            bail!("unsupported")
        }
        async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
            bail!("unsupported")
        }
        async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
            bail!("unsupported")
        }
        async fn uninstall(&self, _: &str, _: RemoveArgs) -> Result<()> {
            bail!("unsupported")
        }
        async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
            bail!("unsupported")
        }
        async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
            Ok(())
        }
        async fn launch(&self, _: &str, _: &LaunchRequest, _: OpContext) -> Result<LaunchPlan> {
            bail!("unsupported")
        }
    }

    fn game(runner: Runner, app: &str, title: &str, installed: bool) -> Arc<GameInfo> {
        Arc::new(
            GameInfo {
                runner: Some(runner),
                app_name: app.into(),
                title: title.into(),
                is_installed: installed,
                installable: true,
                ..Default::default()
            }
            .finalize(),
        )
    }

    fn library() -> Vec<Arc<GameInfo>> {
        vec![
            game(Runner::Gog, "a", "Alpha", true),
            game(Runner::Gog, "b", "Bravo", false),
            game(Runner::Legendary, "c", "Charlie", true),
        ]
    }

    fn start_backend() -> Backend {
        let root =
            std::env::temp_dir().join(format!("irori-bigpicture-test-{}", std::process::id()));
        Paths::init(Paths::under(&root));
        let backend = Backend::start_headless(|_, _| {
            let games = library();
            let store = |runner: Runner| -> DynStore {
                Arc::new(FakeStore {
                    runner,
                    games: games
                        .iter()
                        .filter(|g| g.runner() == runner)
                        .cloned()
                        .collect(),
                })
            };
            vec![store(Runner::Gog), store(Runner::Legendary)]
        })
        .expect("backend starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while backend.library().len() != 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "test library did not load"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        backend
    }

    fn focused_title(view: &Entity<BigPictureView>, cx: &TestAppContext) -> String {
        cx.read(|cx| {
            view.read(cx)
                .focused_game()
                .map(|g| g.title.clone())
                .unwrap_or_default()
        })
    }

    #[gpui_kit::test]
    fn navigates_shelves_and_overlays_with_keyboard_and_gamepad(cx: &mut TestAppContext) {
        // This integration test receives events from the real Tokio backend.
        cx.executor().allow_parking();
        let backend = start_backend();
        cx.update(|cx| {
            gpui_kit::init(cx);
            irori_ui::init(backend, cx);
            crate::bind_keys(cx);
        });
        let (handle, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1280.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| BigPictureView::new(window, cx)),
            )
            .expect("open test window")
        });
        cx.run_until_parked();

        let fullscreen = cx
            .update_window(handle, |_, window, _| window.is_fullscreen())
            .unwrap();
        assert!(fullscreen, "Big Picture enters fullscreen");
        cx.read(|cx| {
            let kinds: Vec<RowKind> = view.read(cx).rows.iter().map(|r| r.kind).collect();
            assert_eq!(
                kinds,
                vec![
                    RowKind::Installed,
                    RowKind::All,
                    RowKind::Store(Runner::Legendary),
                    RowKind::Store(Runner::Gog)
                ]
            );
        });
        assert_eq!(focused_title(&view, cx), "Alpha");

        // Gamepad actions and keyboard bindings drive the same focus.
        cx.dispatch_action(handle, gamepad::Right);
        assert_eq!(focused_title(&view, cx), "Charlie");
        cx.dispatch_action(handle, gamepad::Right);
        assert_eq!(
            focused_title(&view, cx),
            "Charlie",
            "no wrap at the row end"
        );
        cx.simulate_keystrokes(handle, "down");
        assert_eq!(
            focused_title(&view, cx),
            "Alpha",
            "the next shelf keeps its own column"
        );
        cx.simulate_keystrokes(handle, "right");
        assert_eq!(focused_title(&view, cx), "Bravo");
        cx.simulate_keystrokes(handle, "up");
        assert_eq!(
            focused_title(&view, cx),
            "Charlie",
            "the shelf above remembered its column"
        );
        cx.simulate_keystrokes(handle, "up");
        assert_eq!(
            focused_title(&view, cx),
            "Charlie",
            "nothing above the first shelf"
        );
        cx.simulate_keystrokes(handle, "down");

        // Confirm opens the game overlay on the focused game.
        cx.simulate_keystrokes(handle, "enter");
        cx.read(|cx| {
            let v = view.read(cx);
            assert_eq!(
                v.overlay,
                Some(Overlay::Game {
                    id: GameId::new(Runner::Gog, "b"),
                    ix: 0,
                    confirm: None
                })
            );
        });
        // Directions now move inside the overlay, not the shelves.
        cx.dispatch_action(handle, gamepad::Down);
        cx.dispatch_action(handle, gamepad::Down);
        cx.read(|cx| {
            let v = view.read(cx);
            assert!(
                matches!(v.overlay, Some(Overlay::Game { ix: 1, .. })),
                "clamped to the last action"
            );
            assert_eq!(v.focused_game().map(|g| g.title.as_str()), Some("Bravo"));
        });
        cx.simulate_keystrokes(handle, "escape");
        cx.read(|cx| assert!(!view.read(cx).is_overlay_open()));

        // Uninstall asks first and Back cancels the question, not the overlay.
        cx.simulate_keystrokes(handle, "left");
        assert_eq!(focused_title(&view, cx), "Alpha");
        cx.dispatch_action(handle, gamepad::Confirm);
        cx.dispatch_action(handle, gamepad::Down);
        cx.dispatch_action(handle, gamepad::Down);
        cx.dispatch_action(handle, gamepad::Confirm);
        cx.read(|cx| {
            assert!(matches!(
                view.read(cx).overlay,
                Some(Overlay::Game {
                    confirm: Some(0),
                    ..
                })
            ));
        });
        cx.dispatch_action(handle, gamepad::Back);
        cx.read(|cx| {
            assert!(matches!(
                view.read(cx).overlay,
                Some(Overlay::Game {
                    confirm: None,
                    ix: 2,
                    ..
                })
            ));
        });
        cx.dispatch_action(handle, gamepad::Back);

        // Menu opens the quick menu; Back closes it.
        cx.simulate_keystrokes(handle, "m");
        cx.read(|cx| assert_eq!(view.read(cx).overlay, Some(Overlay::Menu { ix: 0 })));
        cx.dispatch_action(handle, gamepad::Back);
        cx.read(|cx| assert!(!view.read(cx).is_overlay_open()));
    }
}
