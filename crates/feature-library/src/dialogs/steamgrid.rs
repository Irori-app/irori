//! "Find art online": search SteamGridDB for a game and pick a cover,
//! square or background image. Embedded in the "Add game", "Edit app" and
//! "Edit details" dialogs in place of their form while open.
//!
//! Without an API key the picker explains how to get one and where to enter
//! it. Nothing is searched until the user opens the picker (Heroic looked up
//! art on a third-party server whenever the title field lost focus).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, EventEmitter, Focusable as _,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Rems, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription,
    Task, Window, div, rems,
};
use irori_extras::steamgrid::{self, ApiKey, ImageFilter, SgdbGame, SgdbImage};
use irori_ui::components::{art_img, physical_width};
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, models, navigate};

/// Where SteamGridDB users create an API key.
const API_KEY_PAGE: &str = "https://www.steamgriddb.com/profile/preferences/api";
/// Images shown per game; the rest are rarely useful and cost downloads.
const MAX_IMAGES: usize = 60;

/// Which artwork to pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtKind {
    /// Vertical 2:3 library cover (SteamGridDB "grids").
    Cover,
    /// Square artwork (square "grids").
    Square,
    /// Wide background (SteamGridDB "heroes").
    Background,
    /// The game's name as artwork, usually transparent (SteamGridDB "logos").
    Logo,
}

impl ArtKind {
    /// Listing filter for this kind.
    pub fn filter(self) -> ImageFilter {
        let dimensions: &[&str] = match self {
            ArtKind::Cover => &["600x900", "342x482", "660x930"],
            ArtKind::Square => &["512x512", "1024x1024"],
            ArtKind::Background | ArtKind::Logo => &[],
        };
        ImageFilter {
            dimensions: dimensions.iter().map(|d| d.to_string()).collect(),
            styles: Vec::new(),
        }
    }

    /// "No covers found for “Hades”."
    pub fn none_found(self, game: &str) -> SharedString {
        let key = match self {
            ArtKind::Cover => "library.art.none_found.cover",
            ArtKind::Square => "library.art.none_found.square",
            ArtKind::Background => "library.art.none_found.background",
            ArtKind::Logo => "library.art.none_found.logo",
        };
        t_args(key, &[("name", game)])
    }

    /// Dialog title while picking.
    pub fn title(self) -> SharedString {
        t(match self {
            ArtKind::Cover => "library.art.title.cover",
            ArtKind::Square => "library.art.title.square",
            ArtKind::Background => "library.art.title.background",
            ArtKind::Logo => "library.art.title.logo",
        })
    }

    /// Thumbnail width and height, in rem.
    fn thumb_rem(self) -> (f32, f32) {
        match self {
            ArtKind::Cover => (6.0, 9.0),
            ArtKind::Square => (6.0, 6.0),
            ArtKind::Background => (12.5, 4.0),
            ArtKind::Logo => (9.0, 4.0),
        }
    }
}

/// What to do after a search returned `games`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    NoMatch,
    /// Exactly one match: show its images right away.
    Single(SgdbGame),
    Choose(Vec<SgdbGame>),
}

pub fn search_outcome(mut games: Vec<SgdbGame>) -> SearchOutcome {
    match games.len() {
        0 => SearchOutcome::NoMatch,
        1 => SearchOutcome::Single(games.remove(0)),
        _ => SearchOutcome::Choose(games),
    }
}

/// Picker results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtPickerEvent {
    /// The full-size image URL the user picked.
    Picked(String),
    Cancelled,
}

enum Phase {
    /// No SteamGridDB API key in settings.
    NoKey,
    Idle,
    Searching,
    Games,
    Loading(SharedString),
    Images(SharedString, Vec<SgdbImage>),
    Message(SharedString),
}

/// The SteamGridDB search and image grid.
pub struct ArtPicker {
    kind: ArtKind,
    key: Option<ApiKey>,
    query: Entity<InputState>,
    phase: Phase,
    games: Vec<SgdbGame>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ArtPickerEvent> for ArtPicker {}

impl ArtPicker {
    /// A picker for `kind`, searching for `title` right away when a key is set.
    pub fn new(kind: ArtKind, title: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = ApiKey::from_settings(&models(cx).settings.read(cx).app);
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("library.art.placeholder"))
                .default_value(title.trim().to_string())
        });
        let art = irori_ui::art::art_store(cx);
        let subscriptions = vec![
            cx.observe(&art, |_, _, cx| cx.notify()),
            cx.subscribe_in(&query, window, |this, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.search(cx);
                }
            }),
        ];
        let mut this = Self {
            phase: if key.is_some() {
                Phase::Idle
            } else {
                Phase::NoKey
            },
            kind,
            key,
            query,
            games: Vec::new(),
            _task: None,
            _subscriptions: subscriptions,
        };
        if !title.trim().is_empty() {
            this.search(cx);
        }
        this
    }

    pub fn kind(&self) -> ArtKind {
        self.kind
    }

    /// Focus the search field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.focus_handle(cx).focus(window, cx);
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            return;
        }
        self.phase = Phase::Searching;
        cx.notify();
        let task = irori_ui::run(
            cx,
            async move { steamgrid::search_games(&key, &query).await },
        );
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result.map(search_outcome) {
                    Ok(SearchOutcome::NoMatch) => {
                        this.games.clear();
                        this.phase = Phase::Message(t("library.art.no_games"));
                    }
                    Ok(SearchOutcome::Single(game)) => {
                        this.games = vec![game.clone()];
                        this.load_images(game, cx);
                    }
                    Ok(SearchOutcome::Choose(games)) => {
                        this.games = games;
                        this.phase = Phase::Games;
                    }
                    Err(e) => this.phase = Phase::Message(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_images(&mut self, game: SgdbGame, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let name: SharedString = game.name.clone().into();
        self.phase = Phase::Loading(name.clone());
        cx.notify();
        let kind = self.kind;
        let task = irori_ui::run(cx, async move {
            let filter = kind.filter();
            match kind {
                ArtKind::Background => steamgrid::heroes(&key, game.id, &filter).await,
                ArtKind::Logo => steamgrid::logos(&key, game.id, &filter).await,
                ArtKind::Cover | ArtKind::Square => steamgrid::grids(&key, game.id, &filter).await,
            }
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.phase = match result {
                    Ok(images) if images.is_empty() => Phase::Message(kind.none_found(&name)),
                    Ok(mut images) => {
                        images.truncate(MAX_IMAGES);
                        Phase::Images(name, images)
                    }
                    Err(e) => Phase::Message(format!("{e:#}").into()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self._task = None;
        self.phase = if self.games.len() > 1 {
            Phase::Games
        } else {
            Phase::Idle
        };
        cx.notify();
    }

    fn render_no_key(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .text_sm()
            .child(t("library.art.no_key.intro"))
            .child(
                v_flex()
                    .gap_1()
                    .text_color(muted)
                    .child(t("library.art.no_key.step_copy"))
                    .child(
                        Link::new("sgdb-api-page")
                            .href(API_KEY_PAGE)
                            .child(t("library.art.no_key.open_page")),
                    )
                    .child(t("library.art.no_key.step_paste")),
            )
            .child(
                h_flex().justify_end().gap_2().child(
                    Button::new("sgdb-open-settings")
                        .outline()
                        .icon(IconName::Settings)
                        .label(t("library.art.no_key.open_settings"))
                        .on_click(|_, window, cx| {
                            window.close_dialog(cx);
                            navigate(Route::Settings("advanced".into()), cx);
                        }),
                ),
            )
            .into_any_element()
    }

    fn render_games(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.games.iter().enumerate().map(|(ix, game)| {
            let chosen = game.clone();
            ListItem::new(ElementId::Name(format!("sgdb-game-{ix}").into()))
                .py_2()
                .accessibility_label(game.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.load_images(chosen.clone(), cx)))
                .child(div().text_sm().truncate().child(game.name.clone()))
        });
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("library.art.choose_game")),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_images(
        &self,
        images: &[SgdbImage],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (w, h) = self.kind.thumb_rem();
        let width: Rems = rems(w);
        let pixels = physical_width(width, window);
        let theme = cx.theme();
        let (radius, muted_bg, muted) = (theme.radius, theme.muted, theme.muted_foreground);
        let mut tiles = Vec::with_capacity(images.len());
        for (ix, image) in images.iter().enumerate() {
            // Logos keep their transparency and are shown whole.
            let logo = self.kind == ArtKind::Logo;
            let thumb = art_img(&image.thumb, pixels, logo, cx);
            let url = image.url.clone();
            let frame = div()
                .w(width)
                .h(rems(h))
                .overflow_hidden()
                .rounded(radius)
                .bg(muted_bg)
                .map(|el| match thumb {
                    Some(img) => el.child(img.size_full().rounded(radius).object_fit(if logo {
                        ObjectFit::Contain
                    } else {
                        ObjectFit::Cover
                    })),
                    None => el.child(
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .text_color(muted)
                            .child(Icon::new(IconName::Image)),
                    ),
                });
            tiles.push(
                Button::new(ElementId::Name(format!("sgdb-image-{ix}").into()))
                    .ghost()
                    .p_1()
                    .h_auto()
                    .accessibility_label(t_args(
                        "library.art.image_label",
                        &[("number", &(ix + 1).to_string())],
                    ))
                    .child(frame)
                    .on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(ArtPickerEvent::Picked(url.clone()))
                        }),
                    ),
            );
        }
        h_flex()
            .flex_wrap()
            .gap_1()
            .children(tiles)
            .into_any_element()
    }
}

impl Render for ArtPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let has_key = self.key.is_some();
        let busy = matches!(self.phase, Phase::Searching | Phase::Loading(_));
        let can_go_back =
            matches!(self.phase, Phase::Images(..) | Phase::Loading(_)) && self.games.len() > 1;

        let content: AnyElement = match &self.phase {
            Phase::NoKey => self.render_no_key(cx),
            Phase::Idle => div()
                .text_sm()
                .text_color(muted)
                .child(t("library.art.idle"))
                .into_any_element(),
            Phase::Searching | Phase::Loading(_) => h_flex()
                .gap_2()
                .py_4()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new())
                .child(match &self.phase {
                    Phase::Loading(name) => t_args("library.art.loading", &[("name", name)]),
                    _ => t("library.art.searching"),
                })
                .into_any_element(),
            Phase::Games => self.render_games(cx),
            Phase::Images(name, images) => {
                let images = images.clone();
                let name = name.clone();
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t_args("library.art.choose_image", &[("name", &name)])),
                    )
                    .child(self.render_images(&images, window, cx))
                    .into_any_element()
            }
            Phase::Message(text) => div()
                .text_sm()
                .text_color(muted)
                .child(text.clone())
                .into_any_element(),
        };

        v_flex()
            .id("art-picker")
            .gap_3()
            .when(has_key, |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .when(can_go_back, |el| {
                            el.child(
                                Button::new("sgdb-back")
                                    .ghost()
                                    .icon(IconName::ArrowLeft)
                                    .tooltip(t("library.art.back_to_games"))
                                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                            )
                        })
                        .child(div().flex_1().min_w_0().child(
                            Input::new(&self.query).prefix(Icon::new(IconName::Search).small()),
                        ))
                        .child(
                            Button::new("sgdb-search")
                                .outline()
                                .label(t("library.art.search"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.search(cx))),
                        ),
                )
            })
            .child(
                div()
                    .id("art-picker-results")
                    .max_h(rems(24.))
                    .overflow_y_scroll()
                    .child(content),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .min_w_0()
                            .when(has_key, |el| el.child(t("library.art.attribution"))),
                    )
                    .child(
                        Button::new("sgdb-cancel")
                            .ghost()
                            .label(t("library.art.back_to_form"))
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ArtPickerEvent::Cancelled)),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: u64, name: &str) -> SgdbGame {
        SgdbGame {
            id,
            name: name.into(),
        }
    }

    #[test]
    fn search_outcomes() {
        assert_eq!(search_outcome(Vec::new()), SearchOutcome::NoMatch);
        assert_eq!(
            search_outcome(vec![game(1, "Hades")]),
            SearchOutcome::Single(game(1, "Hades"))
        );
        assert_eq!(
            search_outcome(vec![game(1, "Hades"), game(2, "Hades II")]),
            SearchOutcome::Choose(vec![game(1, "Hades"), game(2, "Hades II")])
        );
    }

    #[test]
    fn filters_per_kind() {
        assert_eq!(
            ArtKind::Cover.filter().dimensions,
            vec!["600x900", "342x482", "660x930"]
        );
        assert_eq!(ArtKind::Square.filter().dimensions.len(), 2);
        assert_eq!(ArtKind::Background.filter(), ImageFilter::default());
    }
}
