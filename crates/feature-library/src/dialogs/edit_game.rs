//! Custom title and art for a store game (kept by Irori, never sent to the
//! store). Sideloaded apps are edited in their own dialog instead.

use std::cell::Cell;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, Render, SharedString, Styled as _, StyledImage as _,
    Subscription, Window, div, rems,
};
use irori_core::{GameId, GameInfo, GameOverrides, Runner};
use irori_ui::components::{art_img, physical_width};
use irori_ui::i18n::t;
use irori_ui::{backend, models};

use super::steamgrid::{ArtKind, ArtPicker, ArtPickerEvent};
use super::{dialog_width, pick_path, text};

/// The art a store game's details can replace, in the order the form lists it.
const ART_KINDS: [ArtKind; 4] = [
    ArtKind::Cover,
    ArtKind::Square,
    ArtKind::Background,
    ArtKind::Logo,
];

/// What the user entered for each kind of art, in [`ART_KINDS`] order.
pub type ArtValues<'a> = [&'a str; 4];

/// Overrides that turn `original` into what the user entered: unchanged or
/// empty fields are left out, so the store's value keeps applying.
pub fn overrides_for(original: &GameInfo, title: &str, art: ArtValues<'_>) -> GameOverrides {
    let title = title.trim();
    let keep = |s: &str, orig: &Option<String>| {
        let s = s.trim();
        (!s.is_empty() && Some(s) != orig.as_deref()).then(|| s.to_string())
    };
    let [cover, square, background, logo] = art;
    GameOverrides {
        title: (!title.is_empty() && title != original.title).then(|| title.to_string()),
        art_cover: keep(cover, &original.art_cover),
        art_square: keep(square, &original.art_square),
        art_background: keep(background, &original.art_background),
        art_logo: keep(logo, &original.art_logo),
    }
}

/// The override field holding `kind`.
fn override_of(o: &GameOverrides, kind: ArtKind) -> Option<String> {
    match kind {
        ArtKind::Cover => o.art_cover.clone(),
        ArtKind::Square => o.art_square.clone(),
        ArtKind::Background => o.art_background.clone(),
        ArtKind::Logo => o.art_logo.clone(),
    }
}

/// Placeholder of an empty art field: the store's art keeps applying.
fn store_placeholder(kind: ArtKind) -> SharedString {
    t(match kind {
        ArtKind::Cover => "library.edit.store_cover",
        ArtKind::Square => "library.edit.store_artwork",
        ArtKind::Background => "library.edit.store_background",
        ArtKind::Logo => "library.edit.store_logo",
    })
}

/// Label and description of an art field.
fn field_text(kind: ArtKind) -> (SharedString, SharedString) {
    let (label, hint) = match kind {
        ArtKind::Cover => ("library.common.cover_art", "library.edit.cover_hint"),
        ArtKind::Square => ("library.edit.square_art", "library.edit.square_hint"),
        ArtKind::Background => (
            "library.edit.background_art",
            "library.edit.background_hint",
        ),
        ArtKind::Logo => ("library.edit.logo_art", "library.edit.logo_hint"),
    };
    (t(label), t(hint))
}

/// Stable name of a kind for element ids.
fn kind_name(kind: ArtKind) -> &'static str {
    match kind {
        ArtKind::Cover => "cover",
        ArtKind::Square => "square",
        ArtKind::Background => "background",
        ArtKind::Logo => "logo",
    }
}

pub struct EditGameDialog {
    id: GameId,
    original: Arc<GameInfo>,
    has_overrides: bool,
    title: Entity<InputState>,
    /// One field per kind of art, in [`ART_KINDS`] order.
    art: [Entity<InputState>; 4],
    /// The SteamGridDB picker, shown instead of the form while open.
    picker: Option<Entity<ArtPicker>>,
    _picker_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EditGameDialog {
    fn new(
        id: GameId,
        original: Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = models(cx)
            .library
            .read(cx)
            .user
            .overrides
            .get(&id.key())
            .cloned()
            .unwrap_or_default();
        let has_overrides = !current.is_empty();
        let art = ART_KINDS.map(|kind| {
            let value = override_of(&current, kind).unwrap_or_default();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(store_placeholder(kind))
                    .default_value(value)
            })
        });
        let title_value = current.title.unwrap_or_else(|| original.title.clone());
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(original.title.clone())
                .default_value(title_value)
        });
        let store = irori_ui::art::art_store(cx);
        let mut subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        for input in std::iter::once(&title).chain(&art) {
            subscriptions.push(
                cx.subscribe_in(input, window, |this, _, event, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.save(window, cx),
                    _ => {}
                }),
            );
        }
        Self {
            id,
            original,
            has_overrides,
            title,
            art,
            picker: None,
            _picker_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    /// The field holding `kind`.
    fn input(&self, kind: ArtKind) -> &Entity<InputState> {
        let ix = ART_KINDS.iter().position(|k| *k == kind).unwrap_or(0);
        &self.art[ix]
    }

    /// Everything the user entered, as overrides of the store's details.
    fn entered(&self, cx: &App) -> GameOverrides {
        let art = self.art.each_ref().map(|input| text(input, cx));
        overrides_for(
            &self.original,
            &text(&self.title, cx),
            art.each_ref().map(String::as_str),
        )
    }

    /// Title of the dialog: the picker's while it is open.
    fn dialog_title(&self, cx: &App) -> SharedString {
        match &self.picker {
            Some(p) => p.read(cx).kind().title(),
            None => t("library.edit.title"),
        }
    }

    fn find_art(&mut self, kind: ArtKind, window: &mut Window, cx: &mut Context<Self>) {
        let title = Some(text(&self.title, cx))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.original.title.clone());
        let picker = cx.new(|cx| ArtPicker::new(kind, &title, window, cx));
        picker.update(cx, |p, cx| p.focus(window, cx));
        self._picker_subscription = Some(cx.subscribe_in(
            &picker,
            window,
            move |this, _, event: &ArtPickerEvent, window, cx| {
                if let ArtPickerEvent::Picked(url) = event {
                    let url = url.clone();
                    this.input(kind)
                        .clone()
                        .update(cx, |s, cx| s.set_value(url, window, cx));
                }
                this.picker = None;
                this._picker_subscription = None;
                this.title.focus_handle(cx).focus(window, cx);
                cx.notify();
            },
        ));
        self.picker = Some(picker);
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let overrides = self.entered(cx);
        let b = backend(cx);
        let id = self.id.clone();
        irori_ui::state::run_infallible(
            cx,
            async move { b.userdata.set_overrides(&id, overrides) },
        )
        .detach();
        window.close_dialog(cx);
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.original.title.clone();
        self.title
            .update(cx, |s, cx| s.set_value(title, window, cx));
        for input in &self.art {
            input.update(cx, |s, cx| s.set_value(SharedString::default(), window, cx));
        }
        cx.notify();
    }

    fn browse(&mut self, kind: ArtKind, window: &mut Window, cx: &mut Context<Self>) {
        let picked = pick_path(false, t("library.edit.select_image"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            this.update_in(cx, |this, window, cx| {
                let value = path.to_string_lossy().into_owned();
                this.input(kind)
                    .clone()
                    .update(cx, |s, cx| s.set_value(value, window, cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn art_row(&self, kind: ArtKind, cx: &mut Context<Self>) -> impl IntoElement {
        let name = kind_name(kind);
        h_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(self.input(kind)).cleanable(true)),
            )
            .child(
                Button::new(SharedString::from(format!("browse-{name}")))
                    .outline()
                    .icon(IconName::FolderOpen)
                    .label(t("library.common.browse"))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.browse(kind, window, cx)),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("find-{name}")))
                    .outline()
                    .icon(IconName::Globe)
                    .label(t("library.common.find_online"))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.find_art(kind, window, cx)),
                    ),
            )
    }

    fn preview(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = rems(7.5);
        let url = Some(text(self.input(ArtKind::Cover), cx))
            .filter(|s| !s.is_empty())
            .or_else(|| self.original.cover_art().map(str::to_string));
        let image = url.and_then(|u| art_img(&u, physical_width(width, window), false, cx));
        let theme = cx.theme();
        let frame = div()
            .w(width)
            .h(rems(width.0 * 1.5))
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .bg(theme.muted);
        match image {
            Some(img) => frame.child(
                img.size_full()
                    .rounded(theme.radius_lg)
                    .object_fit(ObjectFit::Cover),
            ),
            None => frame.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Image).large()),
            ),
        }
    }
}

impl Render for EditGameDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = &self.picker {
            return v_flex().id("edit-game-art").child(picker.clone());
        }
        v_flex()
            .id("edit-game-form")
            .gap_4()
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .child(self.preview(window, cx))
                    .child(
                        div().flex_1().min_w_0().child(
                            v_form()
                                .child(
                                    field()
                                        .label(t("library.edit.field.title"))
                                        .child(Input::new(&self.title)),
                                )
                                .children(ART_KINDS.map(|kind| {
                                    let (label, hint) = field_text(kind);
                                    field()
                                        .label(label)
                                        .description(hint)
                                        .child(self.art_row(kind, cx))
                                })),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("edit-game-reset")
                            .ghost()
                            .icon(IconName::RotateCcw)
                            .label(t("library.edit.reset"))
                            .disabled(!self.has_overrides && !self.is_changed(cx))
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("edit-game-cancel")
                                    .ghost()
                                    .label(t("library.common.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("edit-game-save")
                                    .primary()
                                    .label(t("library.common.save"))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.save(window, cx)),
                                    ),
                            ),
                    ),
            )
    }
}

impl EditGameDialog {
    fn is_changed(&self, cx: &App) -> bool {
        !self.entered(cx).is_empty()
    }
}

/// Open the title and art editor of a game. Sideloaded apps open their own
/// edit dialog.
pub fn open_edit_game_dialog(id: &GameId, window: &mut Window, cx: &mut App) {
    if id.runner == Runner::Sideload {
        super::sideload::open_edit_app_dialog(id.app_name.to_string(), window, cx);
        return;
    }
    let b = backend(cx);
    // The library applies overrides; the store keeps the original.
    let Some(original) = b
        .store(id.runner)
        .and_then(|s| s.game(&id.app_name))
        .or_else(|| b.game(id))
    else {
        return;
    };
    let id = id.clone();
    let view = cx.new(|cx| EditGameDialog::new(id, original, window, cx));
    show(view, window, cx);
}

fn show(view: Entity<EditGameDialog>, window: &mut Window, cx: &mut App) {
    let width = dialog_width(window, 40.);
    let focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            let input = view.read(cx).title.clone();
            input.focus_handle(cx).focus(window, cx);
        }
        let title = view.read(cx).dialog_title(cx);
        dialog.title(title).w(width).child(view.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_skip_unchanged_fields() {
        let original = GameInfo {
            title: "Hades".into(),
            art_cover: Some("https://cdn/cover.jpg".into()),
            ..Default::default()
        };
        let o = overrides_for(&original, " Hades ", ["https://cdn/cover.jpg", "", "", ""]);
        assert!(o.is_empty());
        let o = overrides_for(&original, "Hades II", [" /home/me/cover.png ", "", "", ""]);
        assert_eq!(o.title.as_deref(), Some("Hades II"));
        assert_eq!(o.art_cover.as_deref(), Some("/home/me/cover.png"));
        // An emptied title falls back to the store title.
        assert_eq!(overrides_for(&original, "  ", ["", "", "", ""]).title, None);
    }

    #[test]
    fn overrides_cover_background_and_logo() {
        let original = GameInfo {
            title: "Hades".into(),
            art_logo: Some("https://cdn/logo.png".into()),
            ..Default::default()
        };
        let art = ["", "", "/home/me/hero.jpg", "https://cdn/logo.png"];
        let o = overrides_for(&original, "Hades", art);
        assert_eq!(o.art_background.as_deref(), Some("/home/me/hero.jpg"));
        // The store's own logo is not an override.
        assert_eq!(o.art_logo, None);
    }
}
