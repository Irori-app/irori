//! First-run onboarding: welcome (with the interface language), import from
//! Heroic, connect stores, choose the install folder.

use std::path::PathBuf;

use ember_core::{Paths, json_store};
use ember_ui::hooks::{self, ImportSummary};
use ember_ui::i18n::{LANGUAGES, Language, t, t_args};
use ember_ui::{Route, models, navigate};
use gpui_kit::assets::IconName;
use gpui_kit::component::IndexPath;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _,
    Subscription, Window, div, rems,
};

use crate::accounts::{StoreCard, store_cards, store_icon};
use crate::common::{backend_op, card};
use crate::login::open_login_dialog;

fn marker_file() -> PathBuf {
    Paths::get().state_dir().join("onboarding-done")
}

/// Whether the shell should show [`OnboardingView`] instead of the library:
/// no store is logged in and onboarding was never completed or skipped.
pub fn should_show_onboarding(cx: &App) -> bool {
    !models(cx).accounts.read(cx).any_logged_in() && !marker_file().exists()
}

/// Record that onboarding was completed or skipped, so it is not shown again.
pub fn mark_onboarding_done() -> anyhow::Result<()> {
    json_store::write_bytes(&marker_file(), b"done\n")
}

/// Emitted by [`OnboardingView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingEvent {
    /// The user finished or skipped onboarding; the marker is written and the
    /// navigator points at the library.
    Finished,
}

/// One page of the onboarding flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnboardingStep {
    Welcome,
    Import,
    Stores,
    InstallFolder,
    Done,
}

/// Pages in order; the Heroic import page only when Heroic data exists.
pub(crate) fn onboarding_steps(heroic_available: bool) -> Vec<OnboardingStep> {
    let mut steps = vec![OnboardingStep::Welcome];
    if heroic_available {
        steps.push(OnboardingStep::Import);
    }
    steps.extend([
        OnboardingStep::Stores,
        OnboardingStep::InstallFolder,
        OnboardingStep::Done,
    ]);
    steps
}

enum ImportState {
    Idle,
    Running,
    Done(ImportSummary),
    Failed(SharedString),
}

enum LutrisState {
    Idle,
    Running,
    Done(ember_ui::hooks::LutrisSummary),
    Failed(SharedString),
}

/// One line per kind of data the import brought over; empty when nothing
/// needed importing.
fn import_summary_lines(s: &ImportSummary) -> Vec<SharedString> {
    let mut lines = Vec::new();
    if !s.stores.is_empty() {
        lines.push(t_args(
            "manage.onboarding.import.summary.stores",
            &[("stores", &s.stores.join(", "))],
        ));
    }
    let count = |n: usize| n.to_string();
    match s.games_with_settings {
        0 => {}
        1 => lines.push(t("manage.onboarding.import.summary.settings_one")),
        n => lines.push(t_args(
            "manage.onboarding.import.summary.settings_other",
            &[("count", &count(n))],
        )),
    }
    match s.favourites {
        0 => {}
        1 => lines.push(t("manage.onboarding.import.summary.favourites_one")),
        n => lines.push(t_args(
            "manage.onboarding.import.summary.favourites_other",
            &[("count", &count(n))],
        )),
    }
    match s.categories {
        0 => {}
        1 => lines.push(t("manage.onboarding.import.summary.categories_one")),
        n => lines.push(t_args(
            "manage.onboarding.import.summary.categories_other",
            &[("count", &count(n))],
        )),
    }
    lines
}

/// An interface language in the welcome step's picker.
#[derive(Debug, Clone, Copy)]
struct LanguageChoice(&'static Language);

impl SelectItem for LanguageChoice {
    type Value = &'static str;

    fn title(&self) -> SharedString {
        self.0.native_name.into()
    }

    fn value(&self) -> &&'static str {
        &self.0.code
    }
}

type LanguageSelect = SelectState<Vec<LanguageChoice>>;

/// Index of `code` in [`LANGUAGES`]; unknown codes select English.
fn language_index(code: &str) -> usize {
    let base = code.split(['_', '-']).next().unwrap_or(code);
    LANGUAGES
        .iter()
        .position(|l| l.code == code)
        .or_else(|| LANGUAGES.iter().position(|l| l.code == base))
        .unwrap_or(0)
}

/// The first-run flow. Emits [`OnboardingEvent::Finished`] when done.
pub struct OnboardingView {
    steps: Vec<OnboardingStep>,
    ix: usize,
    import: ImportState,
    /// Heroic data exists (its import section shows).
    heroic: bool,
    /// A Lutris library exists (its import section shows).
    lutris: bool,
    lutris_import: LutrisState,
    stores: Vec<StoreCard>,
    install_path: String,
    language: Entity<LanguageSelect>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OnboardingEvent> for OnboardingView {}

impl OnboardingView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::with_heroic(hooks::heroic_detected(), window, cx);
        this.set_lutris(hooks::lutris_detected());
        this
    }

    /// Show the Lutris import section (and the import step) or not.
    pub(crate) fn set_lutris(&mut self, lutris: bool) {
        self.lutris = lutris;
        self.steps = onboarding_steps(self.heroic || lutris);
    }

    fn run_lutris_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.lutris_import, LutrisState::Running) {
            return;
        }
        self.lutris_import = LutrisState::Running;
        cx.notify();
        let task = hooks::import_from_lutris(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.lutris_import = match result {
                    Ok(summary) => {
                        ember_ui::models::refresh_library(false, cx);
                        ember_ui::lutris_summary::open_lutris_summary(summary.clone(), window, cx);
                        LutrisState::Done(summary)
                    }
                    Err(e) => LutrisState::Failed(format!("{e:#}").into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn render_lutris(&self, cx: &mut Context<Self>) -> AnyElement {
        let status: AnyElement = match &self.lutris_import {
            LutrisState::Idle | LutrisState::Running => Button::new("lutris-import")
                .primary()
                .icon(IconName::Import)
                .label(t("manage.onboarding.lutris.run"))
                .loading(matches!(self.lutris_import, LutrisState::Running))
                .on_click(cx.listener(|this, _, window, cx| this.run_lutris_import(window, cx)))
                .into_any_element(),
            LutrisState::Done(summary) => {
                let summary = summary.clone();
                v_flex()
                    .gap_2()
                    .child(
                        Alert::success(
                            "lutris-done",
                            ember_ui::lutris_summary::summary_lines(&summary).join("\n"),
                        )
                        .title(t("manage.onboarding.lutris.done")),
                    )
                    .child(
                        div().child(
                            Button::new("lutris-details")
                                .label(t("manage.onboarding.lutris.details"))
                                .on_click(move |_, window, cx| {
                                    ember_ui::lutris_summary::open_lutris_summary(
                                        summary.clone(),
                                        window,
                                        cx,
                                    )
                                }),
                        ),
                    )
                    .into_any_element()
            }
            LutrisState::Failed(message) => Alert::error("lutris-failed", message.clone())
                .title(t("manage.onboarding.lutris.failed"))
                .into_any_element(),
        };
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_semibold()
                    .child(t("manage.onboarding.lutris.title")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("manage.onboarding.lutris.description")),
            )
            .child(div().child(status))
            .into_any_element()
    }

    /// Construct with an explicit Heroic availability (tests).
    pub(crate) fn with_heroic(heroic: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::i18n::register();
        let m = models(cx).clone();
        let current = language_index(&m.settings.read(cx).app.ui_language);
        let choices: Vec<LanguageChoice> = LANGUAGES.iter().map(LanguageChoice).collect();
        let language = cx.new(|cx| {
            SelectState::new(choices, Some(IndexPath::default().row(current)), window, cx)
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &language,
                window,
                |_, _, e: &SelectEvent<Vec<LanguageChoice>>, _, cx| {
                    let SelectEvent::Confirm(Some(code)) = e else {
                        return;
                    };
                    let code = code.to_string();
                    // The shell applies `ui_language` live when settings change.
                    backend_op(cx, move |b| b.settings.update_app(|s| s.ui_language = code));
                },
            ),
            cx.observe(&m.accounts, |this, _, cx| {
                this.stores = store_cards(cx);
                cx.notify();
            }),
            cx.observe(&m.library, |this, _, cx| {
                this.stores = store_cards(cx);
                cx.notify();
            }),
            cx.observe(&m.settings, |this, settings, cx| {
                this.install_path = settings.read(cx).app.default_install_path.clone();
                cx.notify();
            }),
        ];
        Self {
            steps: onboarding_steps(heroic),
            ix: 0,
            import: ImportState::Idle,
            heroic,
            lutris: false,
            lutris_import: LutrisState::Idle,
            stores: store_cards(cx),
            install_path: m.settings.read(cx).app.default_install_path.clone(),
            language,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn step(&self) -> OnboardingStep {
        self.steps[self.ix]
    }

    pub(crate) fn next(&mut self, cx: &mut Context<Self>) {
        if self.ix + 1 < self.steps.len() {
            self.ix += 1;
            cx.notify();
        } else {
            self.finish(cx);
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.ix > 0 {
            self.ix -= 1;
            cx.notify();
        }
    }

    pub(crate) fn finish(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = mark_onboarding_done() {
            tracing::warn!("saving onboarding state: {e:#}");
        }
        cx.emit(OnboardingEvent::Finished);
        navigate(Route::Library, cx);
    }

    fn run_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.import, ImportState::Running) {
            return;
        }
        self.import = ImportState::Running;
        cx.notify();
        let task = hooks::import_from_heroic(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.import = match result {
                    Ok(summary) => {
                        ember_ui::models::refresh_library(false, cx);
                        ImportState::Done(summary)
                    }
                    Err(e) => ImportState::Failed(format!("{e:#}").into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t("manage.onboarding.folder.prompt")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            let _ = this.update_in(cx, |this, _, cx| {
                this.install_path = path.clone();
                cx.notify();
                backend_op(cx, move |b| {
                    b.settings.update_app(|s| s.default_install_path = path)
                });
            });
        })
        .detach();
    }

    fn render_welcome(&self, cx: &App) -> AnyElement {
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_2xl()
                    .font_bold()
                    .child(t("manage.onboarding.welcome.title")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("manage.onboarding.welcome.description")),
            )
            .child(
                v_flex()
                    .pt_2()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(t("manage.onboarding.welcome.language")),
                    )
                    .child(
                        div()
                            .w_full()
                            .max_w(rems(16.))
                            .child(Select::new(&self.language)),
                    ),
            )
            .into_any_element()
    }

    fn render_import(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let status: AnyElement = match &self.import {
            ImportState::Idle | ImportState::Running => Button::new("import")
                .primary()
                .icon(IconName::Import)
                .label(t("manage.onboarding.import.run"))
                .loading(matches!(self.import, ImportState::Running))
                .on_click(cx.listener(|this, _, window, cx| this.run_import(window, cx)))
                .into_any_element(),
            ImportState::Done(summary) => {
                let lines = import_summary_lines(summary);
                if lines.is_empty() {
                    Alert::info("import-done", t("manage.onboarding.import.nothing"))
                        .into_any_element()
                } else {
                    Alert::success("import-done", lines.join("\n"))
                        .title(t("manage.onboarding.import.done"))
                        .into_any_element()
                }
            }
            ImportState::Failed(message) => v_flex()
                .gap_2()
                .child(
                    Alert::error("import-failed", message.clone())
                        .title(t("manage.onboarding.import.failed")),
                )
                .child(
                    div().child(
                        Button::new("import-retry")
                            .label(t("manage.onboarding.import.retry"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.run_import(window, cx)),
                            ),
                    ),
                )
                .into_any_element(),
        };
        let _ = window;
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_semibold()
                    .child(t("manage.onboarding.import.title")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("manage.onboarding.import.description")),
            )
            .child(div().child(status))
            .into_any_element()
    }

    fn render_stores(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let online = models(cx).accounts.read(cx).online;
        let rows: Vec<AnyElement> = self
            .stores
            .iter()
            .map(|s| {
                let runner = s.runner;
                h_flex()
                    .gap_3()
                    .py_2()
                    .child(Icon::new(store_icon(runner)).text_color(muted))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_medium().child(runner.store_name()))
                            .when_some(s.user.clone(), |this, u| {
                                this.child(div().text_xs().text_color(muted).truncate().child(u))
                            }),
                    )
                    .child(if s.user.is_some() {
                        Tag::success()
                            .xsmall()
                            .child(t("manage.onboarding.stores.signed_in"))
                            .into_any_element()
                    } else if s.disabled {
                        Tag::secondary()
                            .xsmall()
                            .child(t("manage.onboarding.stores.turned_off"))
                            .into_any_element()
                    } else {
                        Button::new(SharedString::from(format!("login-{}", runner.id())))
                            .small()
                            .label(t("manage.onboarding.stores.login"))
                            .disabled(!online)
                            .on_click(move |_, window, cx| open_login_dialog(runner, window, cx))
                            .into_any_element()
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_semibold()
                    .child(t("manage.onboarding.stores.title")),
            )
            .child(
                div()
                    .text_color(muted)
                    .child(t("manage.onboarding.stores.description")),
            )
            .when(!online, |this| {
                this.child(Alert::warning(
                    "onboarding-offline",
                    t("manage.onboarding.stores.offline"),
                ))
            })
            .child(v_flex().children(rows))
            .into_any_element()
    }

    fn render_folder(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_semibold()
                    .child(t("manage.onboarding.folder.title")),
            )
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(t("manage.onboarding.folder.description")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::FolderOpen).text_color(theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(self.install_path.clone()),
                    )
                    .child(
                        Button::new("choose-folder")
                            .small()
                            .label(t("manage.onboarding.folder.choose"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_done(&self, cx: &App) -> AnyElement {
        let signed_in: Vec<&str> = self
            .stores
            .iter()
            .filter(|s| s.user.is_some())
            .map(|s| s.runner.store_name())
            .collect();
        let text = if signed_in.is_empty() {
            t("manage.onboarding.done.no_stores")
        } else {
            t_args(
                "manage.onboarding.done.stores",
                &[("stores", &signed_in.join(", "))],
            )
        };
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_semibold()
                    .child(t("manage.onboarding.done.title")),
            )
            .child(div().text_color(cx.theme().muted_foreground).child(text))
            .into_any_element()
    }
}

impl Render for OnboardingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.step();
        let content = match step {
            OnboardingStep::Welcome => self.render_welcome(cx),
            OnboardingStep::Import => v_flex()
                .gap_8()
                .when(self.heroic, |d| d.child(self.render_import(window, cx)))
                .when(self.lutris, |d| d.child(self.render_lutris(cx)))
                .into_any_element(),
            OnboardingStep::Stores => self.render_stores(cx),
            OnboardingStep::InstallFolder => self.render_folder(cx),
            OnboardingStep::Done => self.render_done(cx),
        };
        let last = self.ix + 1 == self.steps.len();
        let next_label = match step {
            OnboardingStep::Welcome => t("manage.onboarding.get_started"),
            OnboardingStep::Done => t("manage.onboarding.finish"),
            _ => t("manage.onboarding.continue"),
        };
        div()
            .id("onboarding")
            .size_full()
            .overflow_y_scrollbar()
            .flex()
            .justify_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(36.))
                    .px_6()
                    .py_12()
                    .gap_6()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t_args(
                                "manage.onboarding.step",
                                &[
                                    ("current", &(self.ix + 1).to_string()),
                                    ("total", &self.steps.len().to_string()),
                                ],
                            )),
                    )
                    .child(card(cx).p_6().gap_4().child(content))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("onboarding-skip")
                                    .ghost()
                                    .label(t("manage.onboarding.skip"))
                                    .when(last, |b| b.disabled(true))
                                    .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                            )
                            .child(div().flex_1())
                            .when(self.ix > 0, |this| {
                                this.child(
                                    Button::new("onboarding-back")
                                        .label(t("manage.onboarding.back"))
                                        .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                                )
                            })
                            .child(
                                Button::new("onboarding-next")
                                    .debug_selector(|| "onboarding-next".into())
                                    .primary()
                                    .label(next_label)
                                    .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_include_import_only_with_heroic() {
        assert_eq!(onboarding_steps(false).len(), 4);
        let with = onboarding_steps(true);
        assert_eq!(with[1], OnboardingStep::Import);
        assert_eq!(*with.last().unwrap(), OnboardingStep::Done);
    }

    #[test]
    fn import_summary_wording() {
        crate::i18n::register();
        let s = ImportSummary {
            stores: vec!["GOG".into(), "Epic Games".into()],
            games_with_settings: 3,
            favourites: 0,
            categories: 1,
        };
        assert_eq!(
            import_summary_lines(&s),
            vec![
                SharedString::from("Accounts and installed games: GOG, Epic Games"),
                SharedString::from("Settings for 3 games"),
                SharedString::from("1 category"),
            ]
        );
        assert!(import_summary_lines(&ImportSummary::default()).is_empty());
    }

    #[test]
    fn language_index_falls_back() {
        assert_eq!(LANGUAGES[language_index("pt_BR")].code, "pt_BR");
        assert_eq!(LANGUAGES[language_index("de_AT")].code, "de");
        assert_eq!(language_index("xx"), 0);
    }
}
