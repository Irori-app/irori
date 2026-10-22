//! The library screen view.

mod bulk;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ember_core::settings::LibraryLayout;
use ember_core::{
    GameId, GameInfo, GameStatus, InstallPlatform, Runner, StatusKind, UpdateRequest,
};
use ember_services::{LibrarySort, StoreFilter};
use ember_ui::art::LruImageCache;
use ember_ui::components::{
    ArtImage, EmptyState, GameCover, PageHeader, SectionTitle, StoreBadge, art_image, art_img,
    art_loading, art_missing, physical_width,
};
use ember_ui::i18n::{t, t_args};
use ember_ui::motion::{HOVER_SPRING, gliding_progress};
use ember_ui::style::{KeyboardFocusable as _, art_tint};
use ember_ui::{Route, backend, models, navigate};
use gpui_kit::AnimationExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, ThemeStyled as _,
    VirtualListScrollHandle, WindowExt as _, h_flex, v_flex, v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, BoxShadow, ClickEvent, Context, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent,
    ObjectFit, ParentElement as _, Pixels, Rems, Render, ScrollStrategy, SharedString, Size,
    SpringAnimation, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription,
    WeakEntity, Window, canvas, div, linear_color_stop, linear_gradient, point, px, rems, size,
};

use crate::availability::{self, Availability};
use crate::commands::{self, GameCommand, GameState};
use crate::dialogs;
use crate::filter::{self, FilteredGames, HiddenMode, ViewFilter};
use crate::filters_panel::{FilterModel, FiltersPanel, set_active_filters};
use crate::grid::{self, GridMetrics, Move};
use crate::launch::{self, LaunchChoice};
use crate::top::{TopCache, TopKind};
use crate::{
    ActivateSelected, EndSelection, FocusSearch, GRID_CONTEXT, LIBRARY_CONTEXT, LibraryEvent,
    OpenSelected, SelectAllGames, SelectDown, SelectFirst, SelectLast, SelectLeft, SelectPageDown,
    SelectPageUp, SelectRight, SelectUp,
};

/// Width of a "Continue playing" card, in rem (16:9 art).
const CARD_REM: f32 = 24.0;
/// How far a hovered or focused cover rises, in pixels. Rows leave
/// `grid::ROW_INSET_REM` above them, so this plus the focus ring must fit.
const TILE_LIFT_PX: f32 = 3.0;
/// Height of the logo art on a "Continue playing" card, in rem.
const CARD_LOGO_REM: f32 = 3.25;
/// Width of a favourite tile in the top row, in rem.
const FAVOURITE_REM: f32 = 8.0;
/// Decoded images kept by the library's image cache.
const IMAGE_CACHE_CAPACITY: usize = 240;
/// How long a checked shortcut state is trusted before it is checked again.
const SHORTCUT_TTL: Duration = Duration::from_secs(30);

/// One entry of the virtual list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Top,
    Heading,
    ListHeader,
    GridRow(usize),
    ListRow(usize),
}

#[derive(Debug, Clone, PartialEq)]
struct FrameKey {
    count: usize,
    top: Option<(TopKind, usize)>,
    layout: LibraryLayout,
    metrics: GridMetrics,
    rem: Pixels,
    width: Pixels,
}

/// Everything derived for the current frame. Rebuilt only when its inputs
/// change (the game list itself is cached in [`FilteredGames`]).
struct Frame {
    games: Arc<[Arc<GameInfo>]>,
    top: Option<(TopKind, Arc<[Arc<GameInfo>]>)>,
    stores: Vec<Runner>,
    /// Title of the main section under the top row.
    heading: SharedString,
    metrics: GridMetrics,
    /// Which of `grid::LIST_COLUMNS` fit the width.
    list_columns: [bool; 6],
    items: Rc<Vec<Item>>,
    sizes: Rc<Vec<Size<Pixels>>>,
    key: Option<FrameKey>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            games: Arc::from(Vec::new()),
            top: None,
            stores: Vec::new(),
            heading: SharedString::default(),
            metrics: grid::grid_metrics(60.0, 11.0),
            list_columns: [true; 6],
            items: Rc::default(),
            sizes: Rc::default(),
            key: None,
        }
    }
}

/// The library screen. Create one per window with [`LibraryView::new`] and
/// render it for [`Route::Library`]. Emits [`LibraryEvent`]s for actions owned
/// by the game feature.
pub struct LibraryView {
    focus_handle: FocusHandle,
    grid_focus: FocusHandle,
    search: Entity<InputState>,
    filters: Entity<FilterModel>,
    filters_panel: Entity<FiltersPanel>,
    tile_slider: Entity<SliderState>,
    scroll: VirtualListScrollHandle,
    images: Entity<LruImageCache>,
    games: FilteredGames,
    top: TopCache,
    layout: LibraryLayout,
    tile_rem: f32,
    /// Measured width of the scroll region (zero until the first paint).
    viewport: Pixels,
    frame: Frame,
    focused: Option<GameId>,
    hovered: Option<GameId>,
    menu_open: Option<GameId>,
    /// Games picked for bulk actions.
    selection: crate::selection::Selection,
    launching: HashSet<GameId>,
    /// Games whose next launch skips the store's update check ("Play now").
    skip_version_check: HashSet<GameId>,
    availability: Availability,
    /// Whether a game has desktop/app-menu shortcuts, with when it was
    /// checked. Checked when a game is hovered or focused, so its menu can
    /// offer the one shortcut command that applies.
    shortcuts: HashMap<GameId, (bool, Instant)>,
    shortcut_checks: HashSet<GameId>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LibraryEvent> for LibraryView {}

impl Focusable for LibraryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl LibraryView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::init(cx);
        let m = models(cx).clone();
        let ui = m.settings.read(cx).app.ui.clone();

        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("library.search.placeholder")));
        let filters = cx.new(|_| FilterModel::new(filter::load()));
        set_active_filters(&filters, cx);
        let filters_panel = cx.new(|cx| FiltersPanel::new(filters.clone(), cx));
        let tile_rem = ui.tile_size.clamp(grid::MIN_TILE_REM, grid::MAX_TILE_REM);
        let tile_slider = cx.new(|_| {
            SliderState::new()
                .min(grid::MIN_TILE_REM)
                .max(grid::MAX_TILE_REM)
                .step(0.5)
                .default_value(tile_rem)
        });
        let art = ember_ui::art::art_store(cx);

        let subscriptions = vec![
            cx.observe(&m.library, |_, _, cx| cx.notify()),
            cx.observe(&m.statuses, |_, _, cx| cx.notify()),
            cx.observe(&m.accounts, |_, _, cx| cx.notify()),
            cx.observe(&art, |_, _, cx| cx.notify()),
            cx.observe(&filters, |_, _, cx| cx.notify()),
            cx.observe_in(&m.settings, window, |this, settings, window, cx| {
                let ui = settings.read(cx).app.ui.clone();
                this.layout = ui.library_layout;
                let size = ui.tile_size.clamp(grid::MIN_TILE_REM, grid::MAX_TILE_REM);
                if (size - this.tile_rem).abs() > f32::EPSILON {
                    this.tile_rem = size;
                    this.tile_slider
                        .update(cx, |s, cx| s.set_value(size, window, cx));
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &search,
                window,
                |this, state, event, window, cx| match event {
                    InputEvent::Change => {
                        let query = state.read(cx).value().to_string();
                        this.filters
                            .update(cx, |m, cx| m.update(|f| f.query = query, cx));
                    }
                    InputEvent::PressEnter { .. } => this.enter_results(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe(
                &tile_slider,
                |this, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(v) => {
                        this.tile_rem = slider_value(v);
                        cx.notify();
                    }
                    SliderEvent::Release(v) => {
                        let size = slider_value(v);
                        this.tile_rem = size;
                        persist_ui(cx, move |ui| ui.tile_size = size);
                        cx.notify();
                    }
                },
            ),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            grid_focus: cx.focus_handle().tab_stop(true),
            search,
            filters,
            filters_panel,
            tile_slider,
            scroll: VirtualListScrollHandle::new(),
            images: LruImageCache::new(IMAGE_CACHE_CAPACITY, cx),
            games: FilteredGames::default(),
            top: TopCache::default(),
            layout: ui.library_layout,
            tile_rem,
            viewport: px(0.),
            frame: Frame::default(),
            focused: None,
            hovered: None,
            menu_open: None,
            selection: crate::selection::Selection::default(),
            launching: HashSet::new(),
            skip_version_check: HashSet::new(),
            availability: Availability::default(),
            shortcuts: HashMap::new(),
            shortcut_checks: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Current view criteria.
    pub fn filter(&self, cx: &App) -> ViewFilter {
        self.filters.read(cx).filter().clone()
    }

    /// Change the view criteria programmatically (e.g. "show updates" from a
    /// notification).
    pub fn update_filter(&mut self, f: impl FnOnce(&mut ViewFilter), cx: &mut Context<Self>) {
        self.filters.update(cx, |m, cx| m.update(f, cx));
    }

    /// Games currently shown, in display order.
    pub fn visible_games(&self) -> Arc<[Arc<GameInfo>]> {
        self.frame.games.clone()
    }

    /// The game with keyboard focus in the grid or list.
    pub fn focused_game(&self) -> Option<&GameId> {
        self.focused.as_ref()
    }

    /// Picked games in display order, and whether selection mode is on.
    #[cfg(test)]
    pub(crate) fn selection_state(&self) -> (Vec<String>, bool) {
        let picked = self.selection.games(&self.frame.games);
        (
            picked.iter().map(|g| g.title.clone()).collect(),
            self.selection.is_active(),
        )
    }

    #[cfg(test)]
    pub(crate) fn click_for_test(
        &mut self,
        id: &GameId,
        modifiers: gpui_kit::Modifiers,
        cx: &mut Context<Self>,
    ) {
        self.click_game(id, modifiers, cx)
    }

    #[cfg(test)]
    pub(crate) fn grid_focus_handle(&self) -> FocusHandle {
        self.grid_focus.clone()
    }

    #[cfg(test)]
    pub(crate) fn search_focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }

    /// Move keyboard focus to the search field.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    // ---- frame derivation ------------------------------------------------

    fn prepare_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let m = models(cx).clone();
        let (snapshot, user, updates) = {
            let lib = m.library.read(cx);
            (lib.snapshot.clone(), lib.user.clone(), lib.updates.clone())
        };
        let stores = {
            let accounts = m.accounts.read(cx);
            filter::offered_stores(&snapshot, &|r| accounts.is_logged_in(r))
        };
        let app = m.settings.read(cx).app.clone();
        let filter = self.filters.read(cx).filter().clone();
        self.check_availability(&snapshot, cx);
        let missing = self.availability.missing().clone();

        let games = self
            .games
            .get(&filter, &stores, &snapshot, &user, &updates, &missing);
        let top = if filter.query.trim().is_empty() && filter.hidden != HiddenMode::Only {
            self.top.get(
                app.library_top_section,
                app.max_recent_games as usize,
                filter.hidden == HiddenMode::Show,
                &snapshot,
                &user,
            )
        } else {
            None
        };

        let rem = window.rem_size();
        let width = if self.viewport > px(0.) {
            self.viewport
        } else {
            window.viewport_size().width
        };
        let available = f32::from(width) / f32::from(rem) - 2.0 * grid::EDGE_REM;
        let metrics = grid::grid_metrics(available, self.tile_rem);

        let key = FrameKey {
            count: games.len(),
            top: top.as_ref().map(|(k, g)| (*k, g.len())),
            layout: self.layout,
            metrics,
            rem,
            width,
        };
        if self.frame.key.as_ref() != Some(&key) {
            let mut items = Vec::new();
            if top.is_some() {
                items.push(Item::Top);
                items.push(Item::Heading);
            }
            match self.layout {
                LibraryLayout::Grid => items
                    .extend((0..grid::row_count(games.len(), metrics.columns)).map(Item::GridRow)),
                LibraryLayout::List => {
                    items.push(Item::ListHeader);
                    items.extend((0..games.len()).map(Item::ListRow));
                }
            }
            let top_kind = top.as_ref().map(|(k, _)| *k);
            let sizes = items
                .iter()
                .map(|item| size(width, rem * item_rem(*item, top_kind, &metrics)))
                .collect();
            self.frame.items = Rc::new(items);
            self.frame.sizes = Rc::new(sizes);
            self.frame.key = Some(key);
        }
        // Picks a filter or search now hides are dropped: bulk actions only
        // ever touch games on screen.
        self.selection.keep_shown(&games);
        self.frame.games = games;
        self.frame.top = top;
        self.frame.stores = stores;
        self.frame.heading = if !filter.categories.is_empty() {
            filter.category_heading().into()
        } else if filter.favourites_only {
            t("library.heading.favourites")
        } else {
            t("library.heading.all")
        };
        self.frame.metrics = metrics;
        self.frame.list_columns = grid::list_columns(available);
    }

    fn columns(&self) -> usize {
        match self.layout {
            LibraryLayout::Grid => self.frame.metrics.columns,
            LibraryLayout::List => 1,
        }
    }

    /// Virtual-list index of the row holding the game at `game_ix`.
    fn item_of_game(&self, game_ix: usize) -> Option<usize> {
        let row = game_ix / self.columns();
        self.frame.items.iter().position(|item| match item {
            Item::GridRow(r) => *r == row,
            Item::ListRow(i) => *i == game_ix,
            _ => false,
        })
    }

    // ---- state and commands ---------------------------------------------

    fn state_for(&self, game: &GameInfo, in_recent: bool, cx: &App) -> GameState {
        let m = models(cx);
        let lib = m.library.read(cx);
        let id = game.id();
        GameState {
            installed: game.is_installed,
            installable: game.installable,
            sideload: id.runner == Runner::Sideload,
            browser: game.browser_url.is_some()
                || game.installed_platform() == Some(InstallPlatform::Browser),
            status: m.statuses.read(cx).kind(&id),
            has_update: lib.has_update(&id),
            favourite: lib.is_favourite(&id),
            hidden: lib.is_hidden(&id),
            has_install_path: game.install_path().is_some(),
            in_recent,
            launching: self.launching.contains(&id),
            shortcut: self.shortcuts.get(&id).map(|(exists, _)| *exists),
            unavailable: game.is_installed && self.availability.missing().contains(&id),
        }
    }

    /// Start a background availability check when the library changed.
    fn check_availability(
        &mut self,
        snapshot: &ember_services::LibrarySnapshot,
        cx: &mut Context<Self>,
    ) {
        let revision = snapshot.revision;
        if !self.availability.start(revision) {
            return;
        }
        let ids = availability::candidates(snapshot);
        let b = backend(cx);
        let task = ember_ui::state::run_infallible(cx, async move {
            let mut missing = HashSet::new();
            for id in ids {
                if let Some(store) = b.store(id.runner)
                    && !store.is_available(&id.app_name).await
                {
                    missing.insert(id);
                }
            }
            missing
        });
        cx.spawn(async move |this, cx| {
            let missing = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                if this.availability.finish(revision, missing) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Check whether a game has shortcuts, unless a recent answer is known.
    fn check_shortcut(&mut self, id: &GameId, cx: &mut Context<Self>) {
        if self
            .shortcuts
            .get(id)
            .is_some_and(|(_, at)| at.elapsed() < SHORTCUT_TTL)
        {
            return;
        }
        let Some(game) = self.game(id, cx).filter(|g| g.is_installed) else {
            return;
        };
        if !self.shortcut_checks.insert(id.clone()) {
            return;
        }
        let task = ember_ui::state::run_infallible(cx, async move {
            ember_extras::shortcuts::exists(&game).await
        });
        let id = id.clone();
        cx.spawn(async move |this, cx| {
            let exists = task.await;
            this.update(cx, |this, _| {
                this.shortcut_checks.remove(&id);
                if let Some(exists) = exists {
                    this.shortcuts.insert(id, (exists, Instant::now()));
                }
            })
            .ok();
        })
        .detach();
    }

    fn game(&self, id: &GameId, cx: &App) -> Option<Arc<GameInfo>> {
        models(cx).library.read(cx).snapshot.get(id).cloned()
    }

    /// Run a command on a game. Commands owned by other features are emitted
    /// as [`LibraryEvent`]s.
    pub fn run_command(
        &mut self,
        id: &GameId,
        command: GameCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(game) = self.game(id, cx) else {
            return;
        };
        let b = backend(cx);
        let id = id.clone();
        let title = game.title.clone();
        match command {
            GameCommand::Play => self.play(&game, window, cx),
            GameCommand::Stop => {
                let task = ember_ui::run(cx, async move { b.stop(&id).await });
                let failed = t_args("library.error.stop", &[("title", &title)]);
                report(task, failed, window, cx);
            }
            GameCommand::Install => cx.emit(LibraryEvent::RequestInstall(id)),
            GameCommand::Update => {
                ember_ui::state::run_infallible(cx, async move {
                    b.enqueue_update(id, title, UpdateRequest::default())
                })
                .detach();
            }
            GameCommand::RemoveFromQueue => {
                ember_ui::state::run_infallible(cx, async move { b.cancel_download(&id, true) })
                    .detach();
            }
            GameCommand::OpenPage => navigate(Route::Game(id), cx),
            GameCommand::Settings => navigate(Route::GameSettings(id, "general".into()), cx),
            GameCommand::Logs => cx.emit(LibraryEvent::RequestLogs(id)),
            GameCommand::ToggleFavourite => {
                let on = !models(cx).library.read(cx).is_favourite(&id);
                ember_ui::state::run_infallible(
                    cx,
                    async move { b.userdata.set_favourite(&id, on) },
                )
                .detach();
            }
            GameCommand::ToggleHidden => {
                let on = !models(cx).library.read(cx).is_hidden(&id);
                ember_ui::state::run_infallible(cx, async move { b.userdata.set_hidden(&id, on) })
                    .detach();
            }
            GameCommand::ToggleCategory(name) => {
                let member = models(cx)
                    .library
                    .read(cx)
                    .user
                    .categories
                    .get(&name)
                    .is_some_and(|set| set.contains(&id.key()));
                ember_ui::state::run_infallible(cx, async move {
                    b.userdata.set_category(&id, &name, !member)
                })
                .detach();
            }
            GameCommand::NewCategory => {
                dialogs::categories::open_new_category_dialog(id, window, cx)
            }
            GameCommand::Edit => dialogs::edit_game::open_edit_game_dialog(&id, window, cx),
            GameCommand::RemoveFromRecent => {
                ember_ui::state::run_infallible(cx, async move { b.userdata.remove_recent(&id) })
                    .detach();
            }
            GameCommand::AddShortcut | GameCommand::RemoveShortcut => {
                let add = command == GameCommand::AddShortcut;
                let app = b.settings.app();
                let task = ember_ui::run(cx, async move {
                    use ember_extras::shortcuts;
                    if add {
                        let targets = shortcuts::ShortcutTargets {
                            desktop: true,
                            app_menu: true,
                            steam: false,
                        };
                        shortcuts::add(&game, &app, targets).await
                    } else {
                        shortcuts::remove(&game, &app).await
                    }
                });
                let failed = if add {
                    t_args("library.error.add_shortcut", &[("title", &title)])
                } else {
                    t_args("library.error.remove_shortcut", &[("title", &title)])
                };
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    this.update_in(cx, |this, window, cx| match result {
                        Ok(()) => {
                            this.shortcuts.insert(id, (add, Instant::now()));
                        }
                        Err(e) => {
                            this.shortcuts.remove(&id);
                            crate::notify_error(failed, &e, window, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            GameCommand::OpenFolder => {
                if let Some(path) = game.install_path() {
                    cx.open_with_system(&path);
                }
            }
            GameCommand::Verify => {
                let task = ember_ui::run(cx, async move { b.repair(&id).await });
                let failed = t_args("library.error.verify", &[("title", &title)]);
                report(task, failed, window, cx);
            }
            GameCommand::Uninstall => cx.emit(LibraryEvent::RequestUninstall(id)),
        }
    }

    fn play(&mut self, game: &Arc<GameInfo>, window: &mut Window, cx: &mut Context<Self>) {
        let id = game.id();
        let online = models(cx).accounts.read(cx).online;
        let has_update = models(cx).library.read(cx).has_update(&id);
        let view = cx.entity().downgrade();
        let title = game.title.clone();
        if !online && !game.can_run_offline {
            window.open_alert_dialog(cx, move |alert, _, _| {
                let view = view.clone();
                let id = id.clone();
                alert
                    .title(t_args("library.offline.title", &[("title", &title)]))
                    .description(t("library.offline.description"))
                    .confirm()
                    .ok_text(t("library.offline.play_anyway"))
                    .on_ok(move |_, window, cx| {
                        let id = id.clone();
                        view.update(cx, |this, cx| this.launch(&id, window, cx))
                            .ok();
                        true
                    })
            });
        } else if has_update && game.runner() != Runner::Sideload {
            window.open_alert_dialog(cx, move |alert, _, _| {
                let (update_view, play_view) = (view.clone(), view.clone());
                let (update_id, play_id) = (id.clone(), id.clone());
                alert
                    .title(t_args("library.update_first.title", &[("title", &title)]))
                    .description(t("library.update_first.description"))
                    .confirm()
                    .ok_text(t("library.update_first.update"))
                    .ok_variant(ButtonVariant::Primary)
                    .cancel_text(t("library.update_first.play_now"))
                    .on_ok(move |_, window, cx| {
                        let id = update_id.clone();
                        update_view
                            .update(cx, |this, cx| {
                                this.run_command(&id, GameCommand::Update, window, cx)
                            })
                            .ok();
                        true
                    })
                    .on_cancel(move |_, window, cx| {
                        let id = play_id.clone();
                        play_view
                            .update(cx, |this, cx| {
                                // The user chose the installed version.
                                this.skip_version_check.insert(id.clone());
                                this.launch(&id, window, cx)
                            })
                            .ok();
                        true
                    })
            });
        } else {
            self.launch(&id, window, cx);
        }
    }

    /// Start a game: directly when it has one launch option or none,
    /// otherwise after asking which option to use.
    fn launch(&mut self, id: &GameId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.launching.insert(id.clone()) {
            return;
        }
        cx.notify();
        let b = backend(cx);
        let lookup = id.clone();
        let task = ember_ui::run(cx, async move {
            let options = match b.store(lookup.runner) {
                Some(store) => store
                    .launch_options(&lookup.app_name)
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let last = b.settings.game(&lookup).last_used_launch_option;
            Ok((options, last))
        });
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (options, last) = task.await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                match launch::decide(options, last.as_ref()) {
                    LaunchChoice::Direct(option) => this.start_launch(&id, option, window, cx),
                    LaunchChoice::Ask {
                        options,
                        preselected,
                        remembered,
                    } => {
                        this.launching.remove(&id);
                        cx.notify();
                        let title = this
                            .game(&id, cx)
                            .map(|g| g.title.clone())
                            .unwrap_or_default();
                        let view = cx.entity().downgrade();
                        launch::open_launch_picker(
                            &title,
                            options,
                            preselected,
                            remembered,
                            move |option, window, cx| {
                                view.update(cx, |this, cx| {
                                    if this.launching.insert(id.clone()) {
                                        this.start_launch(&id, Some(option), window, cx);
                                    }
                                })
                                .ok();
                            },
                            window,
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Ask the backend to start a game (already marked as launching). The
    /// backend remembers the chosen option for next time.
    fn start_launch(
        &mut self,
        id: &GameId,
        option: Option<ember_core::LaunchOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
        let b = backend(cx);
        let launch_id = id.clone();
        let params = ember_services::LaunchParams {
            option,
            skip_version_check: self.skip_version_check.remove(id),
            ..Default::default()
        };
        let task = ember_ui::run(cx, async move { b.launch_with(&launch_id, params).await });
        let id = id.clone();
        let title = self
            .game(&id, cx)
            .map(|g| g.title.clone())
            .unwrap_or_default();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.launching.remove(&id);
                if let Err(e) = result {
                    let failed = t_args("library.error.start", &[("title", &title)]);
                    crate::notify_error(failed, &e, window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- keyboard --------------------------------------------------------

    fn move_focus(&mut self, step: Move, window: &mut Window, cx: &mut Context<Self>) {
        let games = self.frame.games.clone();
        let current = self.focused.as_ref().and_then(|id| self.games.position(id));
        let rem = window.rem_size();
        let row_rem = match self.layout {
            LibraryLayout::Grid => self.frame.metrics.row_rem,
            LibraryLayout::List => grid::LIST_ROW_REM,
        };
        let page_rows = (f32::from(self.scroll.bounds().size.height) / f32::from(rem * row_rem))
            .floor()
            .max(1.0) as usize;
        let step = match step {
            Move::PageUp(_) => Move::PageUp(page_rows),
            Move::PageDown(_) => Move::PageDown(page_rows),
            other => other,
        };
        if let Some(ix) = grid::move_focus(current, games.len(), self.columns(), step) {
            let id = games[ix].id();
            self.check_shortcut(&id, cx);
            self.focused = Some(id);
            if let Some(item) = self.item_of_game(ix) {
                self.scroll.scroll_to_item(item, ScrollStrategy::Nearest);
            }
            cx.notify();
        }
    }

    fn focused_in_view(&self) -> Option<GameId> {
        let id = self.focused.as_ref()?;
        self.games.position(id).map(|_| id.clone())
    }

    fn open_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Enter on a button inside a tile activates that button instead.
        if !self.grid_focus.is_focused(window) {
            cx.propagate();
            return;
        }
        if let Some(id) = self.focused_in_view() {
            if self.selection.is_active() {
                self.selection.toggle(&id);
                cx.notify();
            } else {
                navigate(Route::Game(id), cx);
            }
        }
    }

    fn activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.grid_focus.is_focused(window) {
            cx.propagate();
            return;
        }
        let Some(id) = self.focused_in_view() else {
            return;
        };
        // In selection mode Space picks the focused game.
        if self.selection.is_active() {
            self.selection.toggle(&id);
            cx.notify();
            return;
        }
        let Some(game) = self.game(&id, cx) else {
            return;
        };
        let state = self.state_for(&game, false, cx);
        if let Some(cmd) = commands::primary(&state) {
            self.run_command(&id, cmd, window, cx);
        }
    }

    /// Enter from the search field: move into the results with the first one
    /// focused.
    fn enter_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(first) = self.frame.games.first() {
            self.focused = Some(first.id());
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.grid_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Escape in the search field clears it; a second Escape leaves it.
    fn escape_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.read(cx).value().is_empty() {
            self.grid_focus.focus(window, cx);
        } else {
            self.search
                .update(cx, |s, cx| s.set_value(SharedString::default(), window, cx));
            self.filters
                .update(cx, |m, cx| m.update(|f| f.query.clear(), cx));
        }
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |s, cx| s.set_value(SharedString::default(), window, cx));
        self.filters
            .update(cx, |m, cx| m.update(ViewFilter::clear_all, cx));
    }

    fn set_layout(&mut self, layout: LibraryLayout, cx: &mut Context<Self>) {
        if self.layout == layout {
            return;
        }
        self.layout = layout;
        persist_ui(cx, move |ui| ui.library_layout = layout);
        cx.notify();
    }

    fn set_hovered(&mut self, id: &GameId, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered.as_ref() != Some(id) {
                self.check_shortcut(id, cx);
                self.hovered = Some(id.clone());
                cx.notify();
            }
        } else if self.hovered.as_ref() == Some(id) {
            self.hovered = None;
            cx.notify();
        }
    }
}

fn slider_value(v: &SliderValue) -> f32 {
    v.start().clamp(grid::MIN_TILE_REM, grid::MAX_TILE_REM)
}

/// Height of a virtual-list item, in rem.
fn item_rem(item: Item, top: Option<TopKind>, metrics: &GridMetrics) -> f32 {
    match item {
        Item::Top => {
            // Favourites carry their title below the cover; continue-playing
            // cards carry it on the art.
            let (art, caption) = match top {
                Some(TopKind::Favourites) => (FAVOURITE_REM * grid::COVER_RATIO, grid::CAPTION_REM),
                _ => (CARD_REM * 9.0 / 16.0, 0.0),
            };
            grid::HEADING_REM + grid::ROW_INSET_REM + art + caption + grid::GAP_REM
        }
        Item::Heading => grid::HEADING_REM,
        Item::ListHeader => 2.25,
        Item::GridRow(_) => metrics.row_rem,
        Item::ListRow(_) => grid::LIST_ROW_REM,
    }
}

/// Persist a UI preference off the UI thread.
fn persist_ui(cx: &App, f: impl FnOnce(&mut ember_core::settings::UiPrefs) + Send + 'static) {
    let b = backend(cx);
    ember_ui::state::run_infallible(cx, async move { b.settings.update_app(|s| f(&mut s.ui)) })
        .detach();
}

/// Await a backend task and report a failure as a notification.
fn report<T: 'static>(
    task: gpui_kit::Task<anyhow::Result<T>>,
    title: SharedString,
    window: &mut Window,
    cx: &mut Context<LibraryView>,
) {
    cx.spawn_in(window, async move |_, cx| {
        if let Err(e) = task.await {
            cx.update(|window, cx| crate::notify_error(title, &e, window, cx))
                .ok();
        }
    })
    .detach();
}

/// Label of a running operation: "Installing 42%".
fn status_text(status: &GameStatus) -> String {
    let label = crate::i18n::status_label(status.kind);
    match status.progress.and_then(|p| p.percent) {
        Some(pc) if status.kind != StatusKind::Playing => t_args(
            "library.status.progress",
            &[("status", &label), ("percent", &format!("{pc:.0}"))],
        )
        .to_string(),
        _ => label,
    }
}

/// Secondary line under a game's title.
fn caption(game: &GameInfo, state: &GameState, status: Option<&GameStatus>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let text = |key: &str| t(key).to_string();
    if state.hidden {
        parts.push(text("library.caption.hidden"));
    }
    match status {
        Some(s) if s.kind.is_busy() => parts.push(status_text(s)),
        _ if state.unavailable => parts.push(text("library.caption.files_missing")),
        // Installed games stand out by their full-colour cover; the caption
        // only names what needs attention.
        _ if state.installed => {
            if state.has_update && !state.sideload {
                parts.push(text("library.caption.update_available"));
            }
        }
        _ => match &game.third_party_managed_app {
            Some(app) => {
                parts.push(t_args("library.caption.needs_launcher", &[("app", app)]).to_string())
            }
            None if !game.installable && !state.sideload => {
                parts.push(text("library.caption.not_installable"))
            }
            None => {}
        },
    }
    parts.join(" · ")
}

/// Build a game's menu (used by the context menu and the "…" button).
fn game_menu(
    menu: PopupMenu,
    view: &WeakEntity<LibraryView>,
    game: &Arc<GameInfo>,
    in_recent: bool,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(state) = view.upgrade().map(|v| {
        let id = game.id();
        v.update(cx, |this, cx| this.check_shortcut(&id, cx));
        v.read(cx).state_for(game, in_recent, cx)
    }) else {
        return menu;
    };
    let id = game.id();
    let mut menu = menu;
    for (ix, group) in commands::menu(&state).into_iter().enumerate() {
        if ix > 0 {
            menu = menu.separator();
        }
        for command in group {
            let after_hidden = command == GameCommand::ToggleHidden;
            menu = menu.item(command_item(view, &id, command, &state));
            if after_hidden {
                let view = view.clone();
                let id = id.clone();
                menu = menu.submenu_with_icon(
                    Some(Icon::new(IconName::Tags)),
                    t("library.menu.categories"),
                    window,
                    cx,
                    move |sub, _, cx| categories_menu(sub, &view, &id, cx),
                );
            }
        }
    }
    menu
}

fn categories_menu(
    menu: PopupMenu,
    view: &WeakEntity<LibraryView>,
    id: &GameId,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let key = id.key();
    let entries: Vec<(String, bool)> = models(cx)
        .library
        .read(cx)
        .user
        .categories
        .iter()
        .map(|(name, set)| (name.clone(), set.contains(&key)))
        .collect();
    let mut menu = menu;
    if entries.is_empty() {
        menu = menu.label(t("library.menu.no_categories"));
    }
    let state = GameState::default();
    for (name, member) in entries {
        menu = menu.item(
            command_item(view, id, GameCommand::ToggleCategory(name), &state).checked(member),
        );
    }
    menu.separator()
        .item(command_item(view, id, GameCommand::NewCategory, &state))
}

fn command_item(
    view: &WeakEntity<LibraryView>,
    id: &GameId,
    command: GameCommand,
    state: &GameState,
) -> PopupMenuItem {
    let view = view.clone();
    let id = id.clone();
    let item = PopupMenuItem::new(command.label(state));
    let item = if matches!(command, GameCommand::ToggleCategory(_)) {
        item
    } else {
        item.icon(Icon::new(command.icon(state)))
    };
    item.on_click(move |_, window, cx| {
        let command = command.clone();
        view.update(cx, |this, cx| this.run_command(&id, command, window, cx))
            .ok();
    })
}

// ---- rendering --------------------------------------------------------------

impl LibraryView {
    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (total, refreshing) = {
            let lib = models(cx).library.read(cx);
            (lib.snapshot.len(), lib.refreshing)
        };
        let shown = self.frame.games.len();
        let filter = self.filters.read(cx).filter().clone();
        let (shown, count) = (shown.to_string(), total.to_string());
        // Installing from GOG offline installers needs the GOG library.
        let gog_signed_in = models(cx)
            .accounts
            .read(cx)
            .users
            .get(&Runner::Gog)
            .is_some_and(Option::is_some);
        let description = match (filter.is_narrowed(), total) {
            (_, 0) => t("library.header.no_games"),
            (true, 1) => t_args(
                "library.header.shown_of_total_one",
                &[("shown", &shown), ("total", &count)],
            ),
            (true, _) => t_args(
                "library.header.shown_of_total_other",
                &[("shown", &shown), ("total", &count)],
            ),
            (false, 1) => t("library.common.games_one"),
            (false, _) => t_args("library.common.games_other", &[("count", &count)]),
        };

        v_flex()
            .gap_4()
            .px(rems(grid::EDGE_REM))
            .pt_5()
            .pb_3()
            .child(
                PageHeader::new(t("library.header.title"))
                    .description(description)
                    .action(
                        Button::new("library-refresh")
                            .outline()
                            .icon(IconName::RefreshCw)
                            .tooltip(if refreshing {
                                t("library.header.refreshing")
                            } else {
                                t("library.header.refresh")
                            })
                            .loading(refreshing)
                            .on_click(|_, _, cx| ember_ui::models::refresh_library(false, cx)),
                    )
                    .when(gog_signed_in, |header| {
                        header.action(
                            Button::new("library-import-installers")
                                .outline()
                                .icon(IconName::FolderInput)
                                .label(t("library.header.import_installers"))
                                .tooltip(t("library.header.import_installers_tooltip"))
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(LibraryEvent::RequestImportInstallers)
                                })),
                        )
                    })
                    .action(
                        Button::new("library-add-game")
                            .icon(IconName::Plus)
                            .label(t("library.header.add_game"))
                            .on_click(|_, window, cx| {
                                dialogs::sideload::open_add_game_dialog(window, cx)
                            }),
                    ),
            )
            .child(self.render_toolbar(&filter, cx))
    }

    fn render_toolbar(&mut self, filter: &ViewFilter, cx: &mut Context<Self>) -> impl IntoElement {
        let stores = self.frame.stores.clone();
        let current_store = match filter.store {
            StoreFilter::Store(r) if stores.contains(&r) => Some(r),
            _ => None,
        };
        let active = filter.active_count();
        let model = self.filters.clone();
        let sort = filter.sort;
        let descending = filter.descending;
        let searching = !filter.query.trim().is_empty();
        let panel = self.filters_panel.clone();
        let layout = self.layout;

        let store_switcher = (stores.len() > 1).then(|| {
            let options: Vec<Option<Runner>> = std::iter::once(None)
                .chain(stores.iter().copied().map(Some))
                .collect();
            let clicks = options.clone();
            ButtonGroup::new("library-store")
                .small()
                .outline()
                .children(options.iter().map(|store| {
                    let (id, label): (&str, SharedString) = match store {
                        Some(r) => (r.short_name(), r.short_name().into()),
                        None => ("all", t("library.toolbar.all_stores")),
                    };
                    Button::new(SharedString::from(format!("store-{id}")))
                        .label(label)
                        .selected(*store == current_store)
                }))
                .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                    if let Some(&ix) = clicked.first() {
                        let store = match clicks[ix] {
                            Some(r) => StoreFilter::Store(r),
                            None => StoreFilter::All,
                        };
                        this.update_filter(|f| f.store = store, cx);
                    }
                }))
        });

        let order = crate::i18n::sort_label(sort);
        let sort_label = if searching {
            t("library.toolbar.sort_best_match")
        } else if descending {
            t_args("library.toolbar.sort_reversed", &[("order", &order)])
        } else {
            t_args("library.toolbar.sort", &[("order", &order)])
        };

        h_flex()
            .w_full()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .id("library-search")
                    .w(rems(18.))
                    .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                        if e.keystroke.key == "escape" {
                            this.escape_search(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        Input::new(&self.search)
                            .small()
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    ),
            )
            .children(store_switcher)
            .child(
                Popover::new("library-filters-popover")
                    .trigger(
                        Button::new("library-filters")
                            .small()
                            .outline()
                            .icon(IconName::ListFilter)
                            .label(if active > 0 {
                                t_args(
                                    "library.toolbar.filters_active",
                                    &[("count", &active.to_string())],
                                )
                            } else {
                                t("library.toolbar.filters")
                            }),
                    )
                    .content(move |_, _, _| panel.clone()),
            )
            .child(
                Button::new("library-sort")
                    .small()
                    .outline()
                    .icon(IconName::ArrowDownUp)
                    .label(sort_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for option in LibrarySort::ALL {
                            let model = model.clone();
                            menu = menu.item(
                                PopupMenuItem::new(crate::i18n::sort_label(option))
                                    .checked(option == sort)
                                    .on_click(move |_, _, cx| {
                                        model.update(cx, |m, cx| m.update(|f| f.sort = option, cx))
                                    }),
                            );
                        }
                        let model = model.clone();
                        menu.separator().item(
                            PopupMenuItem::new(t("library.toolbar.reverse"))
                                .checked(descending)
                                .on_click(move |_, _, cx| {
                                    model.update(cx, |m, cx| {
                                        m.update(|f| f.descending = !f.descending, cx)
                                    })
                                }),
                        )
                    }),
            )
            .child(div().flex_1())
            .when(layout == LibraryLayout::Grid, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Image)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            Slider::new(&self.tile_slider)
                                .accessibility_label(t("library.toolbar.tile_size"))
                                .w(rems(7.)),
                        ),
                )
            })
            .child(
                Button::new("library-select")
                    .small()
                    .outline()
                    .icon(IconName::ListChecks)
                    .label(if self.selection.is_active() {
                        t("library.bulk.done")
                    } else {
                        t("library.toolbar.select")
                    })
                    .selected(self.selection.is_active())
                    .tooltip(t("library.toolbar.select_tooltip"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_selection_mode(cx))),
            )
            .child(
                ButtonGroup::new("library-layout")
                    .small()
                    .outline()
                    .child(
                        Button::new("layout-grid")
                            .icon(IconName::LayoutGrid)
                            .tooltip(t("library.toolbar.grid"))
                            .selected(layout == LibraryLayout::Grid),
                    )
                    .child(
                        Button::new("layout-list")
                            .icon(IconName::List)
                            .tooltip(t("library.toolbar.list"))
                            .selected(layout == LibraryLayout::List),
                    )
                    .on_click(cx.listener(
                        |this, clicked: &Vec<usize>, _, cx| match clicked.first() {
                            Some(0) => this.set_layout(LibraryLayout::Grid, cx),
                            Some(1) => this.set_layout(LibraryLayout::List, cx),
                            _ => {}
                        },
                    )),
            )
    }

    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (total, refreshing) = {
            let lib = models(cx).library.read(cx);
            (lib.snapshot.len(), lib.refreshing)
        };
        let any_logged_in = models(cx).accounts.read(cx).any_logged_in();

        if total == 0 {
            if refreshing {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(Spinner::new().large())
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t("library.body.loading")),
                    )
                    .into_any_element();
            }
            let state = if any_logged_in {
                EmptyState::new(IconName::Gamepad2, t("library.empty.title"))
                    .description(t("library.empty.description"))
                    .action(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("empty-refresh")
                                    .icon(IconName::RefreshCw)
                                    .label(t("library.header.refresh"))
                                    .on_click(|_, _, cx| {
                                        ember_ui::models::refresh_library(false, cx)
                                    }),
                            )
                            .child(
                                Button::new("empty-add")
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label(t("library.header.add_game"))
                                    .on_click(|_, window, cx| {
                                        dialogs::sideload::open_add_game_dialog(window, cx)
                                    }),
                            ),
                    )
            } else {
                EmptyState::new(IconName::LogIn, t("library.empty.no_stores_title"))
                    .description(t("library.empty.no_stores_description"))
                    .action(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("empty-accounts")
                                    .primary()
                                    .icon(IconName::LogIn)
                                    .label(t("library.empty.log_in"))
                                    .on_click(|_, _, cx| navigate(Route::Accounts, cx)),
                            )
                            .child(
                                Button::new("empty-add")
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label(t("library.header.add_game"))
                                    .on_click(|_, window, cx| {
                                        dialogs::sideload::open_add_game_dialog(window, cx)
                                    }),
                            ),
                    )
            };
            return state.into_any_element();
        }

        if self.frame.games.is_empty() {
            let filter = self.filters.read(cx).filter().clone();
            let description = t_args(
                "library.no_match.description",
                &[("filters", &filter.describe().join(" · "))],
            );
            return EmptyState::new(IconName::SearchX, t("library.no_match.title"))
                .description(description)
                .action(
                    Button::new("clear-filters")
                        .icon(IconName::X)
                        .label(t("library.no_match.clear"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.clear_filters(window, cx)),
                        ),
                )
                .into_any_element();
        }

        let view = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, _, cx| {
                let width = bounds.size.width;
                view.update(cx, |this, cx| {
                    if (this.viewport - width).abs() > px(0.5) {
                        this.viewport = width;
                        cx.notify();
                    }
                })
                .ok();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let list = v_virtual_list(
            cx.entity(),
            "library-items",
            self.frame.sizes.clone(),
            |this, range, window, cx| this.render_items(range, window, cx),
        )
        .track_scroll(&self.scroll)
        .size_full();

        div()
            .image_cache(self.images.clone())
            .id("library-content")
            .key_context(GRID_CONTEXT)
            .track_focus(&self.grid_focus)
            .on_action(
                cx.listener(|this, _: &SelectLeft, w, cx| this.move_focus(Move::Left, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectRight, w, cx| this.move_focus(Move::Right, w, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectUp, w, cx| this.move_focus(Move::Up, w, cx)))
            .on_action(
                cx.listener(|this, _: &SelectDown, w, cx| this.move_focus(Move::Down, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectFirst, w, cx| this.move_focus(Move::First, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, w, cx| this.move_focus(Move::Last, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPageUp, w, cx| {
                    this.move_focus(Move::PageUp(1), w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectPageDown, w, cx| {
                this.move_focus(Move::PageDown(1), w, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, w, cx| this.open_focused(w, cx)))
            .on_action(cx.listener(|this, _: &SelectAllGames, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &EndSelection, _, cx| this.end_selection(cx)))
            .on_action(
                cx.listener(|this, _: &ActivateSelected, w, cx| this.activate_focused(w, cx)),
            )
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(measure)
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    fn render_items(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let items = self.frame.items.clone();
        range
            .filter_map(|ix| items.get(ix).copied())
            .map(|item| match item {
                Item::Top => self.render_top(window, cx),
                Item::Heading => div()
                    .h(rems(grid::HEADING_REM))
                    .px(rems(grid::EDGE_REM))
                    .flex()
                    .items_end()
                    .pb_2()
                    .child(SectionTitle::new(self.frame.heading.clone()))
                    .into_any_element(),
                Item::ListHeader => self.render_list_header(cx),
                Item::GridRow(row) => self.render_grid_row(row, window, cx),
                Item::ListRow(ix) => self.render_list_row(ix, window, cx),
            })
            .collect()
    }

    fn render_grid_row(
        &mut self,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metrics = self.frame.metrics;
        let start = row * metrics.columns;
        let end = (start + metrics.columns).min(self.frame.games.len());
        let games: Vec<_> = self.frame.games[start..end].to_vec();
        h_flex()
            .items_start()
            .h(rems(metrics.row_rem))
            .pt(rems(grid::ROW_INSET_REM))
            .px(rems(grid::EDGE_REM))
            .gap(rems(grid::GAP_REM))
            .children(
                games
                    .iter()
                    .map(|g| self.render_tile(g, rems(metrics.tile_rem), false, window, cx)),
            )
            .into_any_element()
    }

    /// A cover tile with live status, markers, and Play/Install plus "…" on
    /// hover or keyboard focus.
    fn render_tile(
        &mut self,
        game: &Arc<GameInfo>,
        width: Rems,
        in_recent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = game.id();
        let key = id.key();
        let state = self.state_for(game, in_recent, cx);
        let status = models(cx).statuses.read(cx).get(&id).cloned();
        let show_badges = models(cx).settings.read(cx).app.ui.show_store_badges;
        let keyboard =
            self.focused.as_ref() == Some(&id) && self.grid_focus.contains_focused(window, cx);
        let active =
            keyboard || self.hovered.as_ref() == Some(&id) || self.menu_open.as_ref() == Some(&id);
        let busy = status.as_ref().filter(|s| {
            s.kind.is_busy() && !matches!(s.kind, StatusKind::Playing | StatusKind::Queued)
        });
        let theme = cx.theme();
        let scrim = theme.background.opacity(0.88);
        let muted = theme.muted_foreground;
        let radius = theme.radius_lg;

        let markers = h_flex()
            .absolute()
            .top_2()
            .right_2()
            .gap_1()
            .when(
                state.has_update && state.installed && !state.sideload,
                |el| {
                    el.child(
                        div()
                            .p_1()
                            .rounded_full()
                            .bg(scrim)
                            .child(Icon::new(IconName::CircleArrowDown).xsmall()),
                    )
                },
            )
            .when(state.favourite, |el| {
                el.child(
                    div()
                        .p_1()
                        .rounded_full()
                        .bg(scrim)
                        .child(Icon::new(IconName::Star).xsmall()),
                )
            });

        let busy_more = (active && busy.is_some()).then(|| self.more_button(game, in_recent, cx));
        let status_overlay = status.as_ref().and_then(|s| match s.kind {
            StatusKind::Playing => Some(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .child(Tag::success().xsmall().child(t("library.status.playing")))
                    .into_any_element(),
            ),
            StatusKind::Queued => Some(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .child(Tag::secondary().xsmall().child(t("library.status.queued")))
                    .into_any_element(),
            ),
            kind if kind.is_busy() => {
                let pct = s.progress.and_then(|p| p.percent);
                Some(
                    v_flex()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .p_2()
                        .gap_1()
                        .bg(scrim)
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(crate::i18n::status_label(kind)),
                                )
                                .children(pct.map(|p| {
                                    t_args(
                                        "library.status.percent",
                                        &[("percent", &format!("{p:.0}"))],
                                    )
                                }))
                                .children(busy_more),
                        )
                        .child(gliding_progress(
                            ElementId::Name(format!("progress:{key}").into()),
                            pct,
                        ))
                        .into_any_element(),
                )
            }
            _ => None,
        });

        let selecting = self.selection.is_active();
        let picked = self.selection.contains(&id);
        let actions = (active && busy.is_none() && !selecting).then(|| {
            let primary = commands::primary(&state).map(|command| {
                let label = command.label(&state);
                let id = id.clone();
                let button = Button::new(ElementId::Name(format!("tile-primary:{key}").into()))
                    .small()
                    .icon(command.icon(&state))
                    .label(label)
                    .loading(state.launching)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.run_command(&id, command.clone(), window, cx)
                    }));
                if matches!(state.status, Some(StatusKind::Playing)) {
                    button
                } else {
                    button.primary()
                }
            });
            h_flex()
                .absolute()
                .bottom_2()
                .left_2()
                .right_2()
                .gap_2()
                .justify_end()
                .children(primary.map(|b| div().flex_1().min_w_0().child(b.w_full())))
                .child(self.more_button(game, in_recent, cx))
        });

        // Uninstalled games are dimmed until pointed at, so installed ones stand out.
        let dim = !state.installed && !active && status.is_none();
        let primary_color = cx.theme().primary;
        let cover = div()
            .relative()
            .rounded(radius)
            .when(active, |el| el.shadow_lg())
            .when(keyboard, |el| el.focus_ring_style(window, cx))
            .child(
                div()
                    .when(dim, |el| el.opacity(0.45))
                    .child(GameCover::new(game.clone()).width(width)),
            )
            .when(active, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(radius)
                        .border_2()
                        .border_color(primary_color.opacity(0.85)),
                )
            })
            .when(show_badges && active && !selecting, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_2()
                        .left_2()
                        .rounded_md()
                        .bg(scrim)
                        .child(StoreBadge::new(game.runner())),
                )
            })
            .child(markers)
            .when(actions.is_none(), |el| el.children(status_overlay))
            .children(actions)
            // Selection mode: a ring and a tick on picked covers, an empty
            // circle on the others.
            .when(picked, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(radius)
                        .border_3()
                        .border_color(primary_color),
                )
            })
            .when(selecting, |el| el.child(pick_mark(picked, cx)));

        let view = cx.entity().downgrade();
        let menu_game = game.clone();
        let hover_id = id.clone();
        let click_id = id.clone();
        let caption_text = caption(game, &state, status.as_ref());
        v_flex()
            .id(ElementId::Name(format!("tile:{key}").into()))
            .w(width)
            .gap_2()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered(&hover_id, *hovered, cx)
            }))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                this.click_game(&click_id, e.modifiers(), cx)
            }))
            // Pointed-at and focused covers rise a little off the grid.
            .child(cover.with_spring(
                SharedString::from(format!("tile-lift:{key}")),
                SpringAnimation::new(HOVER_SPRING).to(if active { 1.0 } else { 0.0 }),
                |el, lift: f32| el.top(px(-TILE_LIFT_PX * lift)),
            ))
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .truncate()
                            .when(dim, |el| el.text_color(muted))
                            .child(game.title.clone()),
                    )
                    .when(!caption_text.is_empty(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(caption_text),
                        )
                    }),
            )
            .context_menu(move |menu, window, cx| {
                game_menu(menu, &view, &menu_game, in_recent, window, cx)
            })
            .into_any_element()
    }

    /// The visible "…" button opening the same menu as a right-click.
    fn more_button(
        &self,
        game: &Arc<GameInfo>,
        in_recent: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = game.id().key();
        let view = cx.entity().downgrade();
        let open_view = view.clone();
        let menu_game = game.clone();
        let open_id = game.id();
        div()
            .id(ElementId::Name(format!("tile-more-wrap:{key}").into()))
            .flex_shrink_0()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Button::new(ElementId::Name(format!("tile-more:{key}").into()))
                    .small()
                    .icon(IconName::Ellipsis)
                    .tooltip(t("library.tile.more_actions"))
                    .dropdown_menu(move |menu, window, cx| {
                        game_menu(menu, &view, &menu_game, in_recent, window, cx)
                    })
                    .on_open_change(move |open, _, cx| {
                        let id = open_id.clone();
                        let open = *open;
                        open_view
                            .update(cx, |this, cx| {
                                this.menu_open = open.then_some(id);
                                cx.notify();
                            })
                            .ok();
                    }),
            )
    }

    fn render_top(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((kind, games)) = self.frame.top.clone() else {
            return div().into_any_element();
        };
        let cards: Vec<AnyElement> = match kind {
            TopKind::ContinuePlaying => games
                .iter()
                .map(|g| self.render_card(g, window, cx))
                .collect(),
            TopKind::Favourites => games
                .iter()
                .map(|g| self.render_tile(g, rems(FAVOURITE_REM), false, window, cx))
                .collect(),
        };
        v_flex()
            .h(rems(item_rem(Item::Top, Some(kind), &self.frame.metrics)))
            .child(
                div()
                    .h(rems(grid::HEADING_REM))
                    .px(rems(grid::EDGE_REM))
                    .flex()
                    .items_end()
                    .pb_2()
                    .child(SectionTitle::new(kind.title())),
            )
            .child(
                h_flex()
                    .id("library-top-row")
                    .items_start()
                    .overflow_x_scroll()
                    .pt(rems(grid::ROW_INSET_REM))
                    .px(rems(grid::EDGE_REM))
                    .gap(rems(grid::GAP_REM))
                    .children(cards),
            )
            .into_any_element()
    }

    /// A large landscape "Continue playing" card with one-click Play.
    fn render_card(
        &mut self,
        game: &Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = game.id();
        let key = id.key();
        let state = self.state_for(game, true, cx);
        let status = models(cx).statuses.read(cx).get(&id).cloned();
        let (minutes, last) = {
            let lib = models(cx).library.read(cx);
            (
                lib.playtime_minutes(&id),
                lib.last_played(&id).map(str::to_string),
            )
        };
        let width = rems(CARD_REM);
        let height = rems(CARD_REM * 9.0 / 16.0);
        let image = match game.hero_art() {
            Some(url) => art_image(url, physical_width(width, window), false, cx),
            None => ArtImage::Missing,
        };
        let logo = game
            .art_logo
            .clone()
            .filter(|l| !l.is_empty())
            .and_then(|url| {
                let px = physical_width(rems(CARD_REM * 0.6), window);
                art_img(&url, px, true, cx)
            });
        // Hovered cards glow in their art's colour.
        let glow = game
            .hero_art()
            .map(str::to_owned)
            .and_then(|url| ember_ui::art::ambient(&url, cx))
            .map(|a| BoxShadow {
                color: art_tint(a.rgb, cx).opacity(0.5),
                offset: point(px(0.), px(8.)),
                blur_radius: px(32.),
                spread_radius: px(-4.),
                inset: false,
            });
        let theme = cx.theme();
        let radius = theme.radius_lg;
        let muted_bg = theme.muted;
        let muted = theme.muted_foreground;

        let now = crate::format::now();
        let meta: Vec<String> = [
            status
                .as_ref()
                .filter(|s| s.kind.is_busy())
                .map(status_text),
            crate::format::last_played(last.as_deref(), now)
                .map(|when| t_args("library.card.last_played", &[("when", &when)]).to_string()),
            crate::format::playtime(minutes),
        ]
        .into_iter()
        .flatten()
        .collect();

        let bg = theme.background;
        let primary_color = theme.primary;
        let fade = linear_gradient(
            180.,
            linear_color_stop(bg.opacity(0.), 0.35),
            linear_color_stop(bg.opacity(0.92), 1.),
        );

        let primary = commands::primary(&state).map(|command| {
            let id = id.clone();
            let playing = command == GameCommand::Stop;
            let button = Button::new(ElementId::Name(format!("card-primary:{key}").into()))
                .icon(command.icon(&state))
                .label(command.label(&state))
                .loading(state.launching)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.run_command(&id, command.clone(), window, cx)
                }));
            if playing { button } else { button.primary() }
        });

        let group = SharedString::from(format!("card-group:{key}"));
        let art = div()
            .relative()
            .w(width)
            .h(height)
            .rounded(radius)
            .overflow_hidden()
            .bg(muted_bg)
            .child(match image {
                ArtImage::Ready(img) => img
                    .size_full()
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                ArtImage::Loading => art_loading(),
                ArtImage::Missing => art_missing(cx),
            })
            .child(div().absolute().top_0().left_0().size_full().bg(fade))
            .child(
                h_flex()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .p_3()
                    .gap_3()
                    .items_end()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(match logo {
                                // The game's logo says its name in its own type.
                                Some(logo) => div()
                                    .h(rems(CARD_LOGO_REM))
                                    .max_w(rems(CARD_REM * 0.6))
                                    .mb_1()
                                    .child(logo.h_full().object_fit(ObjectFit::Contain))
                                    .into_any_element(),
                                None => div()
                                    .text_lg()
                                    .font_extrabold()
                                    .truncate()
                                    .child(game.title.clone())
                                    .into_any_element(),
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(muted)
                                    .truncate()
                                    .child(meta.join(" · ")),
                            ),
                    )
                    .children(primary)
                    .child(self.more_button(game, true, cx)),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(radius)
                    .border_2()
                    .border_color(primary_color.opacity(0.))
                    .group_hover(group.clone(), |s| {
                        s.border_color(primary_color.opacity(0.85))
                    }),
            );

        let view = cx.entity().downgrade();
        let menu_game = game.clone();
        let click_id = id.clone();
        div()
            .id(ElementId::Name(format!("card:{key}").into()))
            .keyboard_focusable(cx)
            .group(group)
            .w(width)
            .flex_shrink_0()
            .rounded(radius)
            .hover(move |s| match glow {
                Some(glow) => s.shadow(vec![glow]),
                None => s.shadow_lg(),
            })
            .on_click(move |_, _, cx| navigate(Route::Game(click_id.clone()), cx))
            .child(art)
            .context_menu(move |menu, window, cx| {
                game_menu(menu, &view, &menu_game, true, window, cx)
            })
            .into_any_element()
    }

    fn render_list_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let shown = self.frame.list_columns;
        let column = |w: f32, key: &str, right: bool| {
            div()
                .w(rems(w))
                .flex_shrink_0()
                .truncate()
                .when(right, |el| el.text_right())
                .child(t(key))
        };
        h_flex()
            .w_full()
            .h(rems(2.25))
            .px(rems(grid::EDGE_REM))
            .gap_4()
            .text_xs()
            .font_bold()
            .text_color(muted)
            .border_b_1()
            .border_color(border)
            .child(div().w(rems(grid::LIST_THUMB_REM)).flex_shrink_0())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(t("library.list.title")),
            )
            .when(shown[0], |el| {
                el.child(column(grid::LIST_COLUMNS[0], "library.list.store", false))
            })
            .when(shown[1], |el| {
                el.child(column(grid::LIST_COLUMNS[1], "library.list.size", true))
            })
            .when(shown[2], |el| {
                el.child(column(grid::LIST_COLUMNS[2], "library.list.playtime", true))
            })
            .when(shown[3], |el| {
                el.child(column(
                    grid::LIST_COLUMNS[3],
                    "library.list.last_played",
                    false,
                ))
            })
            .child(column(grid::LIST_COLUMNS[4], "library.list.status", false))
            .child(div().w(rems(grid::LIST_COLUMNS[5])).flex_shrink_0())
            .into_any_element()
    }

    fn render_list_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(game) = self.frame.games.get(ix).cloned() else {
            return div().into_any_element();
        };
        let id = game.id();
        let key = id.key();
        let state = self.state_for(&game, false, cx);
        let status = models(cx).statuses.read(cx).get(&id).cloned();
        let (minutes, last) = {
            let lib = models(cx).library.read(cx);
            (
                lib.playtime_minutes(&id),
                lib.last_played(&id).map(str::to_string),
            )
        };
        let selecting = self.selection.is_active();
        let picked = self.selection.contains(&id);
        let keyboard =
            self.focused.as_ref() == Some(&id) && self.grid_focus.contains_focused(window, cx);
        let active =
            keyboard || self.hovered.as_ref() == Some(&id) || self.menu_open.as_ref() == Some(&id);
        let theme = cx.theme();
        let (muted, border, hover_bg, active_bg, ring) = (
            theme.muted_foreground,
            theme.border,
            theme.list_hover,
            theme.list_active,
            theme.ring,
        );
        let now = crate::format::now();
        let dash = || "—".to_string();

        let status_cell: AnyElement = match status.as_ref() {
            Some(s)
                if s.kind.is_busy()
                    && !matches!(s.kind, StatusKind::Playing | StatusKind::Queued) =>
            {
                let pct = s.progress.and_then(|p| p.percent);
                v_flex()
                    .gap_1()
                    .child(div().text_xs().truncate().child(status_text(s)))
                    .child(gliding_progress(
                        ElementId::Name(format!("row-progress:{key}").into()),
                        pct,
                    ))
                    .into_any_element()
            }
            Some(s) if s.kind == StatusKind::Playing => Tag::success()
                .xsmall()
                .child(t("library.status.playing"))
                .into_any_element(),
            Some(s) if s.kind == StatusKind::Queued => Tag::secondary()
                .xsmall()
                .child(t("library.status.queued"))
                .into_any_element(),
            _ => div()
                .text_xs()
                .text_color(muted)
                .truncate()
                .child(t(if state.unavailable {
                    "library.caption.files_missing"
                } else if state.installed {
                    if state.has_update && !state.sideload {
                        "library.caption.update_available"
                    } else {
                        "library.caption.installed"
                    }
                } else {
                    "library.caption.not_installed"
                }))
                .into_any_element(),
        };

        let primary = commands::primary(&state).map(|command| {
            let id = id.clone();
            Button::new(ElementId::Name(format!("row-primary:{key}").into()))
                .xsmall()
                .icon(command.icon(&state))
                .label(command.label(&state))
                .loading(state.launching)
                .when(command != GameCommand::Stop, |b| b.primary())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.run_command(&id, command.clone(), window, cx)
                }))
        });

        let view = cx.entity().downgrade();
        let menu_game = game.clone();
        let hover_id = id.clone();
        let click_id = id.clone();
        let shown = self.frame.list_columns;
        let cell = |w: f32, right: bool, text: String| {
            div()
                .w(rems(w))
                .flex_shrink_0()
                .text_sm()
                .truncate()
                .when(right, |el| el.text_right())
                .child(text)
        };
        h_flex()
            .id(ElementId::Name(format!("row:{key}").into()))
            .w_full()
            .h(rems(grid::LIST_ROW_REM))
            .px(rems(grid::EDGE_REM))
            .gap_4()
            .border_b_1()
            .border_color(border)
            .relative()
            .hover(|s| s.bg(hover_bg))
            // Rows span the list, where a ring around them would be clipped; the
            // focused one gets a bar in the focus colour on its leading edge.
            .when(keyboard, |el| {
                el.bg(active_bg).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(ring),
                )
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered(&hover_id, *hovered, cx)
            }))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                this.click_game(&click_id, e.modifiers(), cx)
            }))
            .when(picked, |el| el.bg(active_bg))
            .when(selecting, |el| {
                el.child(div().flex_shrink_0().child(pick_mark(picked, cx)))
            })
            .child(
                GameCover::new(game.clone())
                    .width(rems(grid::LIST_THUMB_REM))
                    .radius(cx.theme().radius / 2.),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .child(game.title.clone()),
                            )
                            .when(state.favourite, |el| {
                                el.child(Icon::new(IconName::Star).xsmall().text_color(muted))
                            }),
                    )
                    .when(state.hidden, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(t("library.caption.hidden")),
                        )
                    }),
            )
            .when(shown[0], |el| {
                el.child(cell(
                    grid::LIST_COLUMNS[0],
                    false,
                    game.runner().short_name().to_string(),
                ))
            })
            .when(shown[1], |el| {
                el.child(cell(
                    grid::LIST_COLUMNS[1],
                    true,
                    crate::format::install_size(&game).unwrap_or_else(dash),
                ))
            })
            .when(shown[2], |el| {
                el.child(cell(
                    grid::LIST_COLUMNS[2],
                    true,
                    crate::format::playtime(minutes).unwrap_or_else(dash),
                ))
            })
            .when(shown[3], |el| {
                el.child(cell(
                    grid::LIST_COLUMNS[3],
                    false,
                    crate::format::last_played(last.as_deref(), now).unwrap_or_else(dash),
                ))
            })
            .child(
                div()
                    .w(rems(grid::LIST_COLUMNS[4]))
                    .flex_shrink_0()
                    .child(status_cell),
            )
            .child(
                h_flex()
                    .w(rems(grid::LIST_COLUMNS[5]))
                    .flex_shrink_0()
                    .justify_end()
                    .gap_1()
                    .when(active, |el| {
                        el.children(primary)
                            .child(self.more_button(&game, false, cx))
                    }),
            )
            .context_menu(move |menu, window, cx| {
                game_menu(menu, &view, &menu_game, false, window, cx)
            })
            .into_any_element()
    }
}

/// The circle marking a game in selection mode: filled with a tick when
/// picked, empty otherwise. Sits at the top left of a cover, or leads a row.
fn pick_mark(picked: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let mark = div()
        .size(rems(1.5))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2();
    if picked {
        mark.bg(theme.primary)
            .border_color(theme.primary)
            .text_color(theme.primary_foreground)
            .child(Icon::new(IconName::Check).xsmall())
            .into_any_element()
    } else {
        mark.bg(theme.background.opacity(0.6))
            .border_color(theme.foreground.opacity(0.8))
            .into_any_element()
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_frame(window, cx);
        let bulk_bar = self.render_bulk_bar(cx);
        v_flex()
            .id("library")
            .relative()
            .key_context(LIBRARY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(
                cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)),
            )
            .size_full()
            .min_h_0()
            .text_color(cx.theme().foreground)
            .child(self.render_header(cx))
            .child(self.render_body(cx))
            .children(bulk_bar)
    }
}
