//! Help after a game failed to start: likely causes read from the end of its
//! log, the community's known fix for it, and ways to try again, including
//! with another Wine or Proton build.

use std::sync::Arc;

use ember_core::{GameId, HostPlatform, WineInstallation};
use ember_extras::known_fixes::{self, KnownFix};
use ember_ui::backend;
use ember_ui::i18n::{t, t_args};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::link::Link;
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, rems,
};

use crate::choice::{Choice, ChoiceSelect, choice_select, set_choices};
use crate::load::Load;
use crate::logs::{LineLevel, LogTail, open_log_sheet, read_tail};
use crate::ops;

/// Log lines shown in the sheet.
const SHOWN_LINES: usize = 30;

/// A likely cause of a failed start, recognised from the game's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaunchHint {
    AntiCheat,
    MissingLibrary,
    DotNet,
    Vulkan,
    BrokenPrefix,
    PermissionDenied,
}

impl LaunchHint {
    fn text(self) -> SharedString {
        t(match self {
            LaunchHint::AntiCheat => "game.launch_problem.hint.anticheat",
            LaunchHint::MissingLibrary => "game.launch_problem.hint.missing_library",
            LaunchHint::DotNet => "game.launch_problem.hint.dotnet",
            LaunchHint::Vulkan => "game.launch_problem.hint.vulkan",
            LaunchHint::BrokenPrefix => "game.launch_problem.hint.broken_prefix",
            LaunchHint::PermissionDenied => "game.launch_problem.hint.permission",
        })
    }

    fn matches(self, lower: &str) -> bool {
        let any = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
        match self {
            LaunchHint::AntiCheat => any(&["easyanticheat", "battleye", "eac_launcher"]),
            LaunchHint::MissingLibrary => any(&[
                "err:module:import_dll",
                "err:module:load_dll",
                "error while loading shared libraries",
            ]),
            LaunchHint::DotNet => any(&["err:mscoree", "mscoree.dll", ".net framework"]),
            LaunchHint::Vulkan => any(&[
                "vk_error_incompatible_driver",
                "vk_error_initialization_failed",
                "failed to load vulkan",
                "no vulkan",
                "dxvk: no adapters",
            ]),
            LaunchHint::BrokenPrefix => any(&[
                "could not load kernel32.dll",
                "wine client error",
                "version mismatch",
                "wrong elf class",
            ]),
            LaunchHint::PermissionDenied => any(&["permission denied"]),
        }
    }

    const ALL: [LaunchHint; 6] = [
        LaunchHint::AntiCheat,
        LaunchHint::MissingLibrary,
        LaunchHint::DotNet,
        LaunchHint::Vulkan,
        LaunchHint::BrokenPrefix,
        LaunchHint::PermissionDenied,
    ];
}

/// The likely causes found in `lines`, most telling first.
pub fn launch_hints<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<LaunchHint> {
    let mut found = Vec::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        for hint in LaunchHint::ALL {
            if !found.contains(&hint) && hint.matches(&lower) {
                found.push(hint);
            }
        }
    }
    found.sort();
    found
}

/// Open the help for a game that just failed to start.
pub fn open_launch_problem_sheet(
    id: GameId,
    exit_code: Option<i32>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(game) = backend(cx).game(&id) else {
        return;
    };
    let title = t_args("game.launch_problem.title", &[("title", &game.title)]);
    let view = cx.new(|cx| LaunchProblem::new(id, exit_code, window, cx));
    window.open_sheet(cx, move |sheet, _, _| {
        sheet
            .title(title.clone())
            .size(rems(40.))
            .child(view.clone())
    });
}

/// Contents of the launch-problem sheet.
pub struct LaunchProblem {
    id: GameId,
    exit_code: Option<i32>,
    needs_wine: bool,
    tail: Load<Option<Arc<LogTail>>>,
    hints: Vec<LaunchHint>,
    fix: Option<KnownFix>,
    wines: Load<Vec<WineInstallation>>,
    wine: Option<WineInstallation>,
    wine_select: Entity<ChoiceSelect>,
    _subscriptions: Vec<Subscription>,
}

impl LaunchProblem {
    fn new(
        id: GameId,
        exit_code: Option<i32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let b = backend(cx);
        let needs_wine = b
            .game(&id)
            .and_then(|g| g.installed_platform())
            .is_some_and(|p| HostPlatform::current().needs_wine(p));
        let wine_select = choice_select(Vec::new(), None, window, cx);
        let subscription = cx.subscribe_in(
            &wine_select,
            window,
            |this, _, e: &SelectEvent<Vec<Choice>>, _, cx| {
                let SelectEvent::Confirm(v) = e;
                if let (Some(bin), Some(list)) = (v, this.wines.ready()) {
                    this.wine = list.iter().find(|w| w.bin == bin.as_ref()).cloned();
                }
                cx.notify();
            },
        );
        let mut this = Self {
            id,
            exit_code,
            needs_wine,
            tail: Load::Loading,
            hints: Vec::new(),
            fix: None,
            wines: Load::Idle,
            wine: None,
            wine_select,
            _subscriptions: vec![subscription],
        };
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let path = b.game_log(&self.id);
        let tail = ember_ui::run(cx, read_tail(path));
        let (runner, app_name) = (self.id.runner, self.id.app_name.clone());
        let fix = ember_ui::run(cx, async move {
            Ok(known_fixes::lookup(runner, &app_name).await)
        });
        cx.spawn_in(window, async move |this, cx| {
            let tail = tail.await;
            let fix = fix.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.tail = Load::from_result(tail.map(|t| t.map(Arc::new)));
                if let Some(Some(tail)) = this.tail.ready() {
                    this.hints = launch_hints(tail.lines.iter().map(|(_, l)| l.as_ref()));
                }
                this.fix = fix;
                cx.notify();
            });
        })
        .detach();

        if !self.needs_wine {
            return;
        }
        self.wines = Load::Loading;
        let app = b.settings.app();
        let current = b.settings.game(&self.id).wine_version;
        let task = ember_ui::run(cx, async move {
            Ok(ember_runtime::wine_installations(&app).await)
        });
        cx.spawn_in(window, async move |this, cx| {
            let list = task.await.unwrap_or_default();
            let _ = this.update_in(cx, |this, window, cx| {
                // The current build just failed; offer the others.
                let list: Vec<WineInstallation> =
                    list.into_iter().filter(|w| w.bin != current.bin).collect();
                this.wine = list.first().cloned();
                let items = list
                    .iter()
                    .map(|w| Choice::new(w.bin.clone(), w.name.clone()))
                    .collect();
                let selected = this.wine.as_ref().map(|w| w.bin.clone());
                set_choices(&this.wine_select, items, selected.as_deref(), window, cx);
                this.wines = Load::Ready(list);
                cx.notify();
            });
        })
        .detach();
    }

    fn try_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_sheet(cx);
        ops::play(&self.id, None, window, cx).detach();
    }

    fn try_with_wine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wine) = self.wine.clone() else {
            return;
        };
        if let Ok(value) = serde_json::to_value(&wine) {
            backend(cx)
                .settings
                .set_game_value(&self.id, "wineVersion", Some(value));
        }
        self.try_again(window, cx);
    }

    fn section(label: SharedString, cx: &App) -> gpui_kit::Div {
        v_flex().gap_1().child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
    }

    fn render_hints(&self, cx: &App) -> AnyElement {
        let body: Vec<AnyElement> = if self.tail.is_loading() {
            vec![div().child(t("game.logs.reading")).into_any_element()]
        } else if self.hints.is_empty() {
            vec![
                div()
                    .child(t("game.launch_problem.no_hints"))
                    .into_any_element(),
            ]
        } else {
            self.hints
                .iter()
                .map(|h| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(div().flex_shrink_0().child("•"))
                        .child(div().flex_1().min_w_0().child(h.text()))
                        .into_any_element()
                })
                .collect()
        };
        Self::section(t("game.launch_problem.causes"), cx)
            .text_sm()
            .children(body)
            .into_any_element()
    }

    fn render_fix(&self, cx: &App) -> Option<AnyElement> {
        let fix = self.fix.as_ref()?;
        let notes = fix.notes.values().map(|n| {
            div()
                .text_sm()
                .min_w_0()
                .child(SharedString::from(n.clone()))
        });
        Some(
            Self::section(t("game.launch_problem.known_fix"), cx)
                .child(
                    div()
                        .text_sm()
                        .child(t("game.launch_problem.known_fix_applied")),
                )
                .children(notes)
                .when_some(fix.wiki_link.clone(), |col, link| {
                    col.child(
                        Link::new("launch-problem-wiki")
                            .href(link)
                            .child(t("game.launch_problem.open_wiki")),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_log(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let body = match &self.tail {
            Load::Ready(Some(tail)) if !tail.lines.is_empty() => {
                let start = tail.lines.len().saturating_sub(SHOWN_LINES);
                v_flex()
                    .children(tail.lines[start..].iter().map(|(level, text)| {
                        let color = match level {
                            LineLevel::Error => theme.danger,
                            LineLevel::Warning => theme.warning,
                            LineLevel::Info => theme.foreground,
                        };
                        div()
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_color(color)
                            .child(text.clone())
                    }))
                    .into_any_element()
            }
            Load::Failed(e) => div()
                .text_color(theme.danger)
                .child(t_args("game.logs.read_failed", &[("error", e.as_ref())]))
                .into_any_element(),
            Load::Loading | Load::Idle => div().child(t("game.logs.reading")).into_any_element(),
            Load::Ready(_) => div()
                .text_color(theme.muted_foreground)
                .child(t("game.launch_problem.log_empty"))
                .into_any_element(),
        };
        Self::section(t("game.launch_problem.last_lines"), cx)
            .child(
                div()
                    .id("launch-problem-log")
                    .w_full()
                    .max_h(rems(16.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(body),
            )
            .into_any_element()
    }

    fn render_wine(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.needs_wine {
            return None;
        }
        let empty = self.wines.ready().is_some_and(|l| l.is_empty());
        let row = if empty {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t("game.launch_problem.no_other_wine"))
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(div().flex_1().min_w(rems(12.)).child(
                    Select::new(&self.wine_select).placeholder(t("game.install.wine_version")),
                ))
                .child(
                    Button::new("launch-problem-try-wine")
                        .outline()
                        .label(t("game.launch_problem.try_with_wine"))
                        .disabled(self.wine.is_none())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.try_with_wine(window, cx)),
                        ),
                )
                .into_any_element()
        };
        Some(
            Self::section(t("game.launch_problem.other_wine"), cx)
                .child(row)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("game.launch_problem.other_wine_help")),
                )
                .into_any_element(),
        )
    }
}

impl Render for LaunchProblem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = match self.exit_code {
            Some(code) => t_args(
                "game.launch_problem.summary_code",
                &[("code", &code.to_string())],
            ),
            None => t("game.launch_problem.summary"),
        };
        let (log_id, settings_id) = (self.id.clone(), self.id.clone());
        v_flex()
            .id("launch-problem")
            .w_full()
            .gap_4()
            .py_2()
            .child(div().text_sm().child(summary))
            .child(self.render_hints(cx))
            .children(self.render_fix(cx))
            .children(self.render_wine(cx))
            .child(self.render_log(cx))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("launch-problem-retry")
                            .primary()
                            .icon(IconName::Play)
                            .label(t("game.launch_problem.try_again"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.try_again(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("launch-problem-log-open")
                            .outline()
                            .label(t("game.launch_problem.show_log"))
                            .on_click(move |_, window, cx| {
                                open_log_sheet(log_id.clone(), window, cx)
                            }),
                    )
                    .child(
                        Button::new("launch-problem-settings")
                            .ghost()
                            .icon(IconName::Settings)
                            .label(t("game.launch_problem.settings"))
                            .on_click(move |_, window, cx| {
                                window.close_sheet(cx);
                                ops::open_settings(&settings_id, "general", cx);
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_causes_in_logs() {
        let log = [
            "wine: Read access denied for device L\"\\\\??\\\\Z:\\\\\"",
            "0024:err:module:import_dll Library VCRUNTIME140.dll (which is needed by L\"game.exe\") not found",
            "EasyAntiCheat: launcher error",
            "err:module:import_dll Library MSVCP140.dll not found",
        ];
        assert_eq!(
            launch_hints(log),
            vec![LaunchHint::AntiCheat, LaunchHint::MissingLibrary]
        );
        assert_eq!(
            launch_hints(["err:vulkan:wine_vk_init Failed to load Vulkan"]),
            vec![LaunchHint::Vulkan]
        );
        assert_eq!(
            launch_hints(["sh: ./game.x86_64: Permission denied"]),
            vec![LaunchHint::PermissionDenied]
        );
        assert!(launch_hints(["Started", "fixme:d3d:stub"]).is_empty());
    }
}
