//! The game page (`Route::Game`): a header with the cover beside the title,
//! facts and the primary action over a blurred backdrop, then tabs. A slim bar
//! keeps the primary action in reach once the header scrolls away.

mod achievements;
mod compat;
mod extras;
mod hero;
mod info;
mod menu;
mod overlay;
mod screenshots;

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use ember_core::{Achievement, ExtraInfo, GameId, GameInfo, LaunchOption};
use ember_services::LaunchParams;
use ember_ui::art::{LruImageCache, art_store};
use ember_ui::components::EmptyState;
use ember_ui::i18n::t;
use ember_ui::{Route, backend, models, navigate};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::SelectEvent;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Pixels, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, px,
};

use crate::choice::{Choice, ChoiceSelect, choice_select, set_choices};
use crate::hooks::{self, CompatibilityInfo};
use crate::load::Load;
use crate::state::{ActionInputs, PrimaryAction};
use crate::{OpenGameLog, OpenGameSettingsAction, RunPrimaryAction, ToggleFavourite, ops};

pub use achievements::AchievementFilter;
pub use menu::game_menu;
use overlay::EosOverlayPanel;

/// Key context of the game page; bindings are registered by [`crate::init`].
pub const GAME_PAGE_CONTEXT: &str = "GamePage";

/// Events the shell may react to.
#[derive(Debug, Clone, PartialEq)]
pub enum GamePageEvent {
    /// The game's (possibly overridden) title became known or changed;
    /// use it for the window title or breadcrumb.
    TitleChanged(SharedString),
    /// "Edit title and art…" was chosen: open the metadata editor for the
    /// game (`ember_library::open_edit_game_dialog`).
    RequestEdit(GameId),
    /// "Categories…" was chosen: open category assignment for the page's
    /// game ([`GamePage::game_id`]) through `ember_library`'s categories dialog.
    RequestCategories,
}

/// Section of the info area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoTab {
    About,
    Details,
    Achievements,
    Screenshots,
    Compatibility,
    Requirements,
    Changelog,
    Extras,
}

/// Game page for one game. Owns the page's async data; live state (status,
/// queue, library entry) comes from the shared models.
pub struct GamePage {
    id: GameId,
    focus: FocusHandle,
    game: Option<Arc<GameInfo>>,
    extra: Load<ExtraInfo>,
    achievements: Load<Arc<Vec<Achievement>>>,
    /// Offline installers and goodies, fetched when the Extras tab opens.
    extras: Load<Arc<Vec<ember_core::ExtraFile>>>,
    achievement_filter: AchievementFilter,
    achievement_view: achievements::RowsCache,
    launch_options: Vec<LaunchOption>,
    launch_select: Entity<ChoiceSelect>,
    selected_option: Option<LaunchOption>,
    compat: Load<CompatibilityInfo>,
    /// EOS overlay controls (Epic Windows installs with a store handle).
    overlay: Option<Entity<EosOverlayPanel>>,
    files_available: bool,
    shortcuts_exist: Option<bool>,
    tab: InfoTab,
    launch_pending: bool,
    /// Commands in flight, keyed by command name (for loading states).
    pending: HashSet<&'static str>,
    image_cache: Entity<LruImageCache>,
    was_installed: bool,
    was_playing: bool,
    scroll: ScrollHandle,
    /// Height of the header, measured each frame, past which the sticky bar shows.
    header_height: Rc<Cell<Pixels>>,
    /// Width of the page, measured each frame; layouts that adapt to the
    /// window (header, screenshot grid) read it.
    page_width: Rc<Cell<Pixels>>,
    /// Whether the sticky bar showed at the last check.
    sticky: bool,
    /// The whole changelog is shown, not just its latest entries.
    changelog_expanded: bool,
    /// Index of the screenshot open in the viewer.
    viewer: Option<usize>,
    viewer_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GamePageEvent> for GamePage {}

impl Focusable for GamePage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GamePage {
    /// Create the page for `id` and start loading store details,
    /// achievements, launch options and compatibility data.
    pub fn new(id: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let m = models(cx).clone();
        let game = m
            .library
            .read(cx)
            .snapshot
            .get(&id)
            .cloned()
            .or_else(|| backend(cx).game(&id));
        let launch_select = choice_select(Vec::new(), None, window, cx);
        let subscriptions = vec![
            cx.observe_in(&m.library, window, |this, _, window, cx| {
                this.on_library_changed(window, cx)
            }),
            cx.observe_in(&m.statuses, window, |this, _, window, cx| {
                this.on_status_changed(window, cx)
            }),
            cx.observe(&m.downloads, |_, _, cx| cx.notify()),
            cx.observe(&m.accounts, |_, _, cx| cx.notify()),
            cx.observe(&m.settings, |_, _, cx| cx.notify()),
            cx.observe(&art_store(cx), |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &launch_select,
                window,
                |this, _, e: &SelectEvent<Vec<Choice>>, _, cx| {
                    let SelectEvent::Confirm(Some(v)) = e else {
                        return;
                    };
                    this.choose_launch_option(v.as_ref(), cx);
                },
            ),
        ];
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let was_installed = game.as_ref().is_some_and(|g| g.is_installed);
        let mut this = Self {
            id,
            focus,
            game,
            extra: Load::Idle,
            achievements: Load::Idle,
            extras: Load::Idle,
            achievement_filter: AchievementFilter::All,
            achievement_view: achievements::RowsCache::default(),
            launch_options: Vec::new(),
            launch_select,
            selected_option: None,
            compat: Load::Idle,
            overlay: None,
            files_available: true,
            shortcuts_exist: None,
            tab: InfoTab::About,
            launch_pending: false,
            pending: HashSet::new(),
            image_cache: LruImageCache::new(48, cx),
            was_installed,
            was_playing: false,
            scroll: ScrollHandle::new(),
            header_height: Rc::new(Cell::new(px(0.))),
            page_width: Rc::new(Cell::new(px(0.))),
            sticky: false,
            changelog_expanded: false,
            viewer: None,
            viewer_focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.was_playing = models(cx).statuses.read(cx).is_playing(&this.id);
        this.load_all(window, cx);
        this
    }

    /// Width of the page in rem, or `None` before the first layout.
    pub(super) fn page_rem(&self, window: &Window) -> Option<f32> {
        let width = self.page_width.get();
        (width > px(0.)).then(|| width / window.rem_size())
    }

    /// Whether the header has scrolled far enough for the sticky bar.
    fn sticky_wanted(&self) -> bool {
        let header_height = self.header_height.get();
        header_height > px(0.) && -self.scroll.offset().y > header_height - px(48.)
    }

    /// The game shown.
    pub fn game_id(&self) -> &GameId {
        &self.id
    }

    /// Current title (override applied), if the game is known.
    pub fn title(&self) -> Option<SharedString> {
        self.game.as_ref().map(|g| g.title.clone().into())
    }

    /// The launch option Play uses.
    pub fn selected_launch_option(&self) -> Option<&LaunchOption> {
        self.selected_option.as_ref()
    }

    fn load_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(game) = self.game.clone() else {
            return;
        };
        cx.emit(GamePageEvent::TitleChanged(game.title.clone().into()));
        self.load_extra(window, cx);
        self.load_achievements(window, cx);
        self.load_compat(window, cx);
        self.load_install_state(window, cx);
    }

    /// Data that depends on the game being installed.
    fn load_install_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(game) = self.game.clone() else {
            return;
        };
        self.overlay = overlay::applies(&game, cx).then(|| {
            let game = game.clone();
            cx.new(|cx| EosOverlayPanel::new(game, window, cx))
        });
        if !game.is_installed {
            self.launch_options.clear();
            self.selected_option = None;
            self.files_available = true;
            self.shortcuts_exist = None;
            return;
        }
        let b = backend(cx);
        let id = self.id.clone();
        let browser = game.browser_url.is_some();
        let task = ember_ui::run(cx, {
            let game = game.clone();
            async move {
                let store = b
                    .store(id.runner)
                    .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
                let options = store.launch_options(&id.app_name).await.unwrap_or_default();
                let available = browser || store.is_available(&id.app_name).await;
                let shortcuts = ember_extras::shortcuts::exists(&game).await;
                Ok((options, available, shortcuts))
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok((options, available, shortcuts)) = task.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.files_available = available;
                this.shortcuts_exist = Some(shortcuts);
                this.set_launch_options(options, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_launch_options(
        &mut self,
        options: Vec<LaunchOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = backend(cx).settings.game(&self.id).last_used_launch_option;
        self.selected_option = last
            .filter(|l| options.contains(l))
            .or_else(|| options.first().cloned());
        let items = options
            .iter()
            .enumerate()
            .map(|(ix, o)| Choice::new(ix.to_string(), o.label()))
            .collect();
        let selected = self
            .selected_option
            .as_ref()
            .and_then(|s| options.iter().position(|o| o == s))
            .map(|ix| ix.to_string());
        set_choices(&self.launch_select, items, selected.as_deref(), window, cx);
        self.launch_options = options;
    }

    fn choose_launch_option(&mut self, value: &str, cx: &mut Context<Self>) {
        let Some(option) = value
            .parse::<usize>()
            .ok()
            .and_then(|ix| self.launch_options.get(ix).cloned())
        else {
            return;
        };
        if let Ok(v) = serde_json::to_value(&option) {
            backend(cx)
                .settings
                .set_game_value(&self.id, "lastUsedLaunchOption", Some(v));
        }
        self.selected_option = Some(option);
        cx.notify();
    }

    fn load_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.id.runner == ember_core::Runner::Sideload {
            return;
        }
        self.extra = Load::Loading;
        let b = backend(cx);
        let id = self.id.clone();
        let task = ember_ui::run(cx, async move {
            let store = b
                .store(id.runner)
                .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
            store.extra_info(&id.app_name).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.extra = Load::from_result(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_achievements(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.id.runner == ember_core::Runner::Sideload {
            return;
        }
        if !matches!(self.achievements, Load::Ready(_)) {
            self.achievements = Load::Loading;
        }
        let b = backend(cx);
        let id = self.id.clone();
        let language = b.settings.app().ui_language.clone();
        let task = ember_ui::run(cx, async move {
            let store = b
                .store(id.runner)
                .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
            store.achievements(&id.app_name, &language).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.achievements = Load::from_result(result.map(Arc::new));
                if this.tab == InfoTab::Achievements && !this.has_achievements() {
                    this.tab = InfoTab::About;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_compat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(game) = self.game.clone() else {
            return;
        };
        // PC compatibility data (ProtonDB, Steam Deck) says nothing about
        // emulated games, and a title match would be another game.
        if matches!(
            game.runner(),
            ember_core::Runner::Sideload | ember_core::Runner::Emulation
        ) {
            return;
        }
        let Some(task) = hooks::fetch_compatibility(game, cx) else {
            return;
        };
        self.compat = Load::Loading;
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.compat = Load::from_result(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn on_library_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = models(cx)
            .library
            .read(cx)
            .snapshot
            .get(&self.id)
            .cloned()
            .or_else(|| backend(cx).game(&self.id));
        let first = self.game.is_none() && next.is_some();
        let title_changed = match (&self.game, &next) {
            (Some(a), Some(b)) => a.title != b.title,
            _ => false,
        };
        self.game = next;
        if first {
            self.load_all(window, cx);
        } else if title_changed && let Some(t) = self.title() {
            cx.emit(GamePageEvent::TitleChanged(t));
        }
        let installed = self.game.as_ref().is_some_and(|g| g.is_installed);
        if installed != self.was_installed {
            self.was_installed = installed;
            self.load_install_state(window, cx);
        }
        cx.notify();
    }

    fn on_status_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let playing = models(cx).statuses.read(cx).is_playing(&self.id);
        if self.was_playing && !playing {
            // Unlocks happen while playing; refresh after the session.
            self.load_achievements(window, cx);
        }
        self.was_playing = playing;
        cx.notify();
    }

    pub(crate) fn has_achievements(&self) -> bool {
        self.achievements.ready().is_some_and(|a| !a.is_empty())
    }

    /// Primary action as currently shown.
    pub fn primary_action(&self, cx: &App) -> Option<PrimaryAction> {
        let game = self.game.as_ref()?;
        let m = models(cx);
        let status = m.statuses.read(cx).kind(&self.id);
        let queue = &m.downloads.read(cx).queue;
        let has_update = m.library.read(cx).has_update(&self.id);
        let mut inputs = ActionInputs::from_game(game, status, queue, has_update);
        inputs.files_available = self.files_available;
        inputs.launch_pending = self.launch_pending;
        Some(PrimaryAction::from_inputs(&inputs))
    }

    /// Run the primary action (the same command as the main button).
    pub fn run_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.primary_action(cx) else {
            return;
        };
        match action {
            PrimaryAction::Install => crate::open_install_sheet(self.id.clone(), window, cx),
            PrimaryAction::Play => self.play(self.selected_option.clone(), window, cx),
            PrimaryAction::Update => ops::queue_update(&self.id, cx),
            PrimaryAction::Stop => self.run_pending("stop", ops::stop(&self.id, window, cx), cx),
            PrimaryAction::Resume => ops::resume_downloads(cx),
            PrimaryAction::Queued => ops::cancel_download(&self.id, true, cx),
            PrimaryAction::Missing => ops::change_install_location(&self.id, window, cx),
            PrimaryAction::Downloading { .. }
            | PrimaryAction::Launching
            | PrimaryAction::Busy(_)
            | PrimaryAction::NotInstallable
            | PrimaryAction::Unsupported => {}
        }
    }

    pub(crate) fn play(
        &mut self,
        option: Option<LaunchOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.play_with(
            LaunchParams {
                option,
                ..Default::default()
            },
            window,
            cx,
        );
    }

    /// Launch with one-off choices (verbose logs, no update check).
    pub(crate) fn play_with(
        &mut self,
        params: LaunchParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.launch_pending {
            return;
        }
        let online = models(cx).accounts.read(cx).online;
        let offline_capable = self.game.as_ref().is_some_and(|g| g.can_run_offline);
        if !online && !offline_capable {
            window.push_notification(
                gpui_kit::component::notification::Notification::warning(t(
                    "game.page.offline_warning",
                )),
                cx,
            );
        }
        self.launch_pending = true;
        cx.notify();
        let task = ops::play_with(&self.id, params, window, cx);
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |this, cx| {
                this.launch_pending = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Track a command for its loading state.
    pub(crate) fn run_pending(
        &mut self,
        key: &'static str,
        task: gpui_kit::Task<bool>,
        cx: &mut Context<Self>,
    ) {
        self.pending.insert(key);
        cx.notify();
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |this, cx| {
                this.pending.remove(key);
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_favourite(&mut self, cx: &mut Context<Self>) {
        let fav = models(cx).library.read(cx).is_favourite(&self.id);
        ops::set_favourite(&self.id, !fav, cx);
    }

    fn on_run_primary(
        &mut self,
        _: &RunPrimaryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_primary(window, cx);
    }

    fn on_toggle_favourite(&mut self, _: &ToggleFavourite, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_favourite(cx);
    }

    fn on_open_log(&mut self, _: &OpenGameLog, window: &mut Window, cx: &mut Context<Self>) {
        if self.id.runner.manages_installs() {
            crate::open_log_sheet(self.id.clone(), window, cx);
        }
    }

    fn on_open_settings(
        &mut self,
        _: &OpenGameSettingsAction,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.id.runner.manages_installs() {
            ops::open_settings(&self.id, "general", cx);
        }
    }
}

impl Render for GamePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = div()
            .id("game-page")
            .key_context(GAME_PAGE_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_run_primary))
            .on_action(cx.listener(Self::on_toggle_favourite))
            .on_action(cx.listener(Self::on_open_log))
            .on_action(cx.listener(Self::on_open_settings))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        let Some(game) = self.game.clone() else {
            return base.child(
                EmptyState::new(IconName::Gamepad2, t("game.page.not_found_title"))
                    .description(t("game.page.not_found_description"))
                    .action(
                        Button::new("back-to-library")
                            .label(t("game.page.back_to_library"))
                            .on_click(|_, _, cx| navigate(Route::Library, cx)),
                    ),
            );
        };
        let backdrop = self.render_backdrop(&game, cx);
        let header = self.render_header(&game, window, cx);
        let body = self.render_body(&game, window, cx);
        self.sticky = self.sticky_wanted();
        let sticky = self.sticky.then(|| self.render_sticky_bar(&game, cx));
        let viewer = self.render_viewer(window, cx);
        let measure = self.header_height.clone();
        let page_width = self.page_width.clone();
        base.relative()
            .child(backdrop)
            .child(
                div()
                    .id("game-page-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    // The sticky bar depends on how far the page scrolled. Redraw only
                    // when it should appear or go, once the scroll has been applied.
                    .on_scroll_wheel(cx.listener(|_, _, window, cx| {
                        cx.defer_in(window, |this, _, cx| {
                            if this.sticky_wanted() != this.sticky {
                                cx.notify();
                            }
                        });
                    }))
                    .child(
                        v_flex()
                            .w_full()
                            .child(
                                div().relative().w_full().child(header).child(
                                    canvas(
                                        move |bounds, window, _| {
                                            measure.set(bounds.size.height);
                                            // Layouts read the width while rendering, so
                                            // render again once it changed.
                                            if page_width.replace(bounds.size.width)
                                                != bounds.size.width
                                            {
                                                window.refresh();
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                                ),
                            )
                            .child(body),
                    ),
            )
            .vertical_scrollbar(&self.scroll)
            .children(sticky)
            .children(viewer)
    }
}
