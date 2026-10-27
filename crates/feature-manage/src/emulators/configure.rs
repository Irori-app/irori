//! The settings dialog of one emulator: its options from the manifest, with
//! plain help under each, advanced ones collapsed, and the default for
//! every game played with it. Values are saved as they change
//! (`emberEngineOptions` of the game defaults); a game can override them in
//! its own settings.

use std::collections::BTreeMap;

use ember_engines::{EngineManifest, EngineOption, OptionKind, Os};
use ember_ui::i18n::{t, t_args};
use ember_ui::models;
use gpui_kit::component::IndexPath;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};
use std::sync::Arc;

use serde_json::Value;

/// Translation key of an option's label.
pub(crate) fn label_key(option: &str) -> String {
    format!("manage.engines.option.{option}.label")
}

/// Translation key of an option's help.
pub(crate) fn help_key(option: &str) -> String {
    format!("manage.engines.option.{option}.help")
}

/// Translation key of a choice's name (`""` is "the emulator's default").
pub(crate) fn choice_key(option: &str, value: &str) -> String {
    let value = if value.is_empty() { "default" } else { value };
    format!("manage.engines.option.{option}.{value}")
}

#[derive(Debug, Clone, PartialEq)]
struct ValueChoice {
    value: SharedString,
    title: SharedString,
}

impl SelectItem for ValueChoice {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.title.clone()
    }
    fn value(&self) -> &SharedString {
        &self.value
    }
}

/// Open the settings of `engine`.
pub fn open_engine_settings(engine: &str, window: &mut Window, cx: &mut App) {
    crate::i18n::register();
    let Some(manifest) = ember_engines::engine(engine) else {
        return;
    };
    let dialog = cx.new(|cx| EngineSettings::new(manifest.clone(), window, cx));
    let title = t_args(
        "manage.emulators.configure.title",
        &[("engine", &manifest.name)],
    );
    let width = window.rem_size() * 36.;
    window.open_dialog(cx, move |d, _, _| {
        d.title(title.clone()).width(width).child(dialog.clone())
    });
}

pub(crate) struct EngineSettings {
    manifest: Arc<EngineManifest>,
    show_advanced: bool,
    selects: BTreeMap<String, Entity<SelectState<Vec<ValueChoice>>>>,
    _subscriptions: Vec<Subscription>,
}

impl EngineSettings {
    fn new(manifest: Arc<EngineManifest>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = models(cx).settings.clone();
        let mut subscriptions = vec![cx.observe(&settings, |_, _, cx| cx.notify())];
        let values = Self::values_of(&manifest, cx);
        let mut selects = BTreeMap::new();
        for option in manifest
            .options
            .iter()
            .filter(|o| o.kind == OptionKind::Choice)
        {
            let items: Vec<ValueChoice> = option
                .choices
                .iter()
                .map(|c| ValueChoice {
                    value: c.clone().into(),
                    title: t(&choice_key(&option.id, c)),
                })
                .collect();
            let current = values.get(&option.id).and_then(Value::as_str).unwrap_or("");
            let ix = option
                .choices
                .iter()
                .position(|c| c == current)
                .map(|row| IndexPath::default().row(row));
            let select = cx.new(|cx| SelectState::new(items, ix, window, cx));
            let (engine, id) = (manifest.id.clone(), option.id.clone());
            subscriptions.push(cx.subscribe_in(
                &select,
                window,
                move |_, _, e: &SelectEvent<Vec<ValueChoice>>, _, cx| {
                    let SelectEvent::Confirm(Some(v)) = e else {
                        return;
                    };
                    set_option(&engine, &id, Some(Value::String(v.to_string())), cx);
                },
            ));
            selects.insert(option.id.clone(), select);
        }
        Self {
            manifest,
            show_advanced: false,
            selects,
            _subscriptions: subscriptions,
        }
    }

    /// The current values: the saved defaults over the manifest's.
    fn values_of(manifest: &EngineManifest, cx: &App) -> BTreeMap<String, Value> {
        let app = &models(cx).settings.read(cx).app;
        let set: BTreeMap<String, Value> = app
            .game
            .engine_options
            .get(&manifest.id)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        manifest.option_values(&set)
    }

    fn render_option(
        &self,
        option: &EngineOption,
        values: &BTreeMap<String, Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let label = t(&label_key(&option.id));
        let help = t(&help_key(&option.id));
        let control: AnyElement = match option.kind {
            OptionKind::Toggle => {
                let (engine, id) = (self.manifest.id.clone(), option.id.clone());
                let checked = values
                    .get(&option.id)
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Switch::new(SharedString::from(format!("opt-{}", option.id)))
                    .checked(checked)
                    .on_change(move |v: &bool, _, cx| {
                        set_option(&engine, &id, Some(Value::Bool(*v)), cx)
                    })
                    .into_any_element()
            }
            OptionKind::Choice => match self.selects.get(&option.id) {
                Some(select) => div()
                    .w(gpui_kit::rems(14.))
                    .child(Select::new(select).small())
                    .into_any_element(),
                None => div().into_any_element(),
            },
            // No built-in engine has text options yet; they show their value.
            OptionKind::Text => div()
                .text_sm()
                .child(
                    values
                        .get(&option.id)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
                .into_any_element(),
        };
        h_flex()
            .gap_4()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().font_medium().child(label))
                    .child(div().text_xs().text_color(muted).child(help)),
            )
            .child(div().flex_shrink_0().child(control))
            .into_any_element()
    }
}

/// Save one option for every game (`None` restores Ember's default).
fn set_option(engine: &str, option: &str, value: Option<Value>, cx: &mut App) {
    let (engine, option) = (engine.to_string(), option.to_string());
    crate::common::backend_op(cx, move |b| {
        b.settings.update_app(|s| {
            let options = s.game.engine_options.entry(engine.clone()).or_default();
            match value {
                Some(v) => {
                    options.insert(option, v);
                }
                None => {
                    options.remove(&option);
                }
            }
            if options.is_empty() {
                s.game.engine_options.remove(&engine);
            }
        });
    });
}

impl Render for EngineSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let os = Os::current();
        let values = Self::values_of(&self.manifest, cx);
        let visible: Vec<&EngineOption> = self
            .manifest
            .options
            .iter()
            .filter(|o| o.visible_when.visible(os, &values))
            .collect();
        let (basic, advanced): (Vec<&EngineOption>, Vec<&EngineOption>) =
            visible.into_iter().partition(|o| !o.advanced);
        let basic: Vec<AnyElement> = basic
            .into_iter()
            .map(|o| self.render_option(o, &values, cx))
            .collect();
        let advanced_rows: Vec<AnyElement> = if self.show_advanced {
            advanced
                .iter()
                .map(|o| self.render_option(o, &values, cx))
                .collect()
        } else {
            Vec::new()
        };
        let own = models(cx).settings.read(cx).app.game.engine_own_settings;
        let name = self.manifest.name.clone();
        let engine = self.manifest.id.clone();
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(muted).child(t_args(
                "manage.emulators.configure.description",
                &[("engine", &name)],
            )))
            .children(basic)
            .when(!advanced.is_empty(), |d| {
                d.child(
                    Button::new("toggle-advanced")
                        .ghost()
                        .small()
                        .label(if self.show_advanced {
                            t("manage.emulators.configure.hide_advanced")
                        } else {
                            t("manage.emulators.configure.advanced")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_advanced = !this.show_advanced;
                            cx.notify();
                        })),
                )
                .children(advanced_rows)
            })
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child(t("manage.emulators.configure.own_settings")),
                            )
                            .child(div().text_xs().text_color(muted).child(t_args(
                                "manage.emulators.configure.own_settings_help",
                                &[("engine", &name)],
                            ))),
                    )
                    .child(Switch::new("own-settings").checked(own).on_change(
                        |v: &bool, _, cx| {
                            let v = *v;
                            crate::common::backend_op(cx, move |b| {
                                b.settings.update_app(|s| s.game.engine_own_settings = v)
                            });
                        },
                    )),
            )
            .child(
                h_flex().justify_end().child(
                    Button::new("reset-engine")
                        .ghost()
                        .small()
                        .label(t("manage.emulators.configure.reset"))
                        .on_click(move |_, window, cx| {
                            let engine = engine.clone();
                            crate::common::backend_op(cx, move |b| {
                                b.settings.update_app(|s| {
                                    s.game.engine_options.remove(&engine);
                                })
                            });
                            window.close_dialog(cx);
                        }),
                ),
            )
    }
}

/// Every option key the built-in manifests need.
#[cfg(test)]
pub fn required_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for (_, json) in ember_engines::BUILT_IN {
        let m = EngineManifest::parse(json).expect("built-in manifest");
        for o in &m.options {
            keys.push(label_key(&o.id));
            keys.push(help_key(&o.id));
            for c in &o.choices {
                keys.push(choice_key(&o.id, c));
            }
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every option of the built-in emulators has a label, help and choice
    /// names in English (the catalog tests then cover the other languages).
    #[test]
    fn every_option_has_text() {
        let en: std::collections::HashMap<String, String> =
            serde_json::from_str(crate::i18n::CATALOGS[0].1).unwrap();
        for key in required_keys() {
            assert!(en.contains_key(&key), "en.json lacks {key}");
        }
    }
}
