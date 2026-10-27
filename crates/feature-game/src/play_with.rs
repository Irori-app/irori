//! "Play with…" for an emulated game: which emulator (and RetroArch core)
//! plays it, and which installed version. Choices are saved in the game's
//! settings (`iroriEngine`, `iroriEngineVersion`); "Default" follows the
//! system's default emulator.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};
use irori_core::GameId;
use irori_engines::{EngineRef, Installation, Origin};
use irori_services::EngineStatus;
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, backend, models, navigate};
use serde_json::Value;

/// Open "Play with…" for `id`.
pub fn open_play_with_dialog(id: GameId, window: &mut Window, cx: &mut App) {
    crate::i18n::register();
    let Some(game) = backend(cx).game(&id) else {
        return;
    };
    let dialog = cx.new(|cx| PlayWith::new(id, window, cx));
    let width = window.rem_size() * 30.;
    let title = t_args("game.play_with.title", &[("title", &game.title)]);
    window.open_dialog(cx, move |d, _, _| {
        d.title(title.clone()).width(width).child(dialog.clone())
    });
}

struct PlayWith {
    id: GameId,
    system: String,
    statuses: Option<Vec<EngineStatus>>,
    _subscriptions: Vec<Subscription>,
}

impl PlayWith {
    fn new(id: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let system = backend(cx)
            .game(&id)
            .and_then(|g| g.system.clone())
            .unwrap_or_default();
        let settings = models(cx).settings.clone();
        let mut this = Self {
            id,
            system,
            statuses: None,
            _subscriptions: vec![cx.observe(&settings, |_, _, cx| cx.notify())],
        };
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task = irori_ui::run(cx, async move { Ok(b.engine_statuses().await) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(statuses) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.statuses = Some(statuses);
                cx.notify();
            });
        })
        .detach();
    }

    fn set(&self, key: &'static str, value: Option<String>, cx: &mut App) {
        let (b, id) = (backend(cx), self.id.clone());
        drop(b.clone().spawn_blocking(move || {
            b.settings
                .set_game_value(&id, key, value.map(Value::String));
        }));
    }

    fn choose(&self, choice: Option<&EngineRef>, cx: &mut App) {
        self.set("iroriEngine", choice.map(ToString::to_string), cx);
        // A version belongs to one emulator.
        self.set("iroriEngineVersion", None, cx);
    }

    fn render_versions(
        &self,
        status: &EngineStatus,
        pinned: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if status.installations.len() < 2 {
            return None;
        }
        let label = |i: &Installation| -> SharedString {
            match (i.origin, &i.version) {
                (Origin::Managed, Some(v)) => {
                    t_args("game.play_with.version", &[("version", v.as_str())])
                }
                (Origin::Flatpak, _) => t("game.play_with.flatpak"),
                (Origin::Custom, _) => t("game.play_with.custom"),
                _ => t("game.play_with.system"),
            }
        };
        let default_name = status.default_installation().map(label).unwrap_or_default();
        let mut rows = vec![
            Button::new("version-default")
                .small()
                .outline()
                .label(t_args(
                    "game.play_with.version_default",
                    &[("version", &default_name)],
                ))
                .selected(pinned.is_empty())
                .on_click(cx.listener(|this, _, _, cx| this.set("iroriEngineVersion", None, cx)))
                .into_any_element(),
        ];
        for (ix, i) in status.installations.iter().enumerate() {
            let id = i.id();
            rows.push(
                Button::new(SharedString::from(format!("version-{ix}")))
                    .small()
                    .outline()
                    .label(label(i))
                    .selected(pinned == id)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set("iroriEngineVersion", Some(id.clone()), cx)
                    }))
                    .into_any_element(),
            );
        }
        Some(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .child(t("game.play_with.versions")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("game.play_with.versions_help")),
                )
                .child(h_flex().flex_wrap().gap_1().children(rows))
                .into_any_element(),
        )
    }
}

impl Render for PlayWith {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(statuses) = self.statuses.clone() else {
            return v_flex().child(Spinner::new()).into_any_element();
        };
        let settings = backend(cx).settings.game(&self.id);
        let chosen = EngineRef::parse(&settings.engine);
        let default = backend(cx).system_engine(&self.system, &statuses);
        let name = |c: &EngineRef| -> String {
            let Some(m) = irori_engines::engine(&c.engine) else {
                return c.to_string();
            };
            match c.core.as_deref().and_then(|core| m.core(core)) {
                Some(core) => format!("{} · {}", m.name, core.name),
                None => m.name.clone(),
            }
        };
        let plays = |c: &EngineRef| {
            statuses
                .iter()
                .any(|s| s.manifest.id == c.engine && s.plays(c))
        };
        let row = |label: String, detail: Option<SharedString>, ready: bool, selected: bool| {
            h_flex()
                .gap_3()
                .w_full()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .child(div().text_sm().font_medium().truncate().child(label))
                        .when_some(detail, |d, detail| {
                            d.child(div().text_xs().text_color(muted).truncate().child(detail))
                        }),
                )
                .when(!ready, |d| {
                    d.child(
                        Tag::secondary()
                            .xsmall()
                            .child(t("game.play_with.not_installed")),
                    )
                })
                .when(selected, |d| {
                    d.child(Tag::info().xsmall().child(t("game.play_with.chosen")))
                })
        };
        let mut items: Vec<AnyElement> = vec![
            Button::new("choice-default")
                .ghost()
                .w_full()
                .selected(chosen.is_none())
                .child(row(
                    t("game.play_with.default").to_string(),
                    default.as_ref().map(|d| SharedString::from(name(d))),
                    default.as_ref().is_none_or(plays),
                    chosen.is_none(),
                ))
                .on_click(cx.listener(|this, _, _, cx| this.choose(None, cx)))
                .into_any_element(),
        ];
        for (ix, c) in irori_engines::choices_for(&self.system)
            .into_iter()
            .enumerate()
        {
            let selected = chosen.as_ref() == Some(&c);
            let ready = plays(&c);
            let choice = c.clone();
            items.push(
                Button::new(SharedString::from(format!("choice-{ix}")))
                    .ghost()
                    .w_full()
                    .selected(selected)
                    .child(row(name(&c), None, ready, selected))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(Some(&choice), cx)))
                    .into_any_element(),
            );
        }
        let versions = chosen
            .as_ref()
            .or(default.as_ref())
            .and_then(|c| statuses.iter().find(|s| s.manifest.id == c.engine))
            .cloned()
            .and_then(|s| self.render_versions(&s, &settings.engine_version, cx));
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t("game.play_with.description")),
            )
            .child(v_flex().gap_1().children(items))
            .children(versions)
            .child(
                h_flex().justify_end().child(
                    Button::new("open-emulators")
                        .ghost()
                        .small()
                        .icon(IconName::Joystick)
                        .label(t("game.play_with.manage"))
                        .on_click(|_, window, cx| {
                            window.close_dialog(cx);
                            navigate(Route::Emulators, cx);
                        }),
                ),
            )
            .into_any_element()
    }
}
