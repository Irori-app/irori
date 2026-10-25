//! Settings of one game (`Route::GameSettings`).

use std::collections::HashSet;

use ember_core::{GameId, InstallPlatform, OpContext};
use ember_runtime::{DxTool, Target};
use ember_ui::components::{EmptyState, PageHeader};
use ember_ui::i18n::{t, t_args};
use ember_ui::{Route, backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window,
    div, rems,
};

use crate::form::{FormTarget, SettingsForm, info_alert};
use crate::i18n::LanguageStamp;
use crate::logs::LogViewer;
use crate::registry::FieldSection;
use crate::widgets::{RowState, field_row, muted, nav_button};
use crate::winetricks::WinetricksPanel;

/// Sections of a game's settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameSection {
    Wine,
    Performance,
    Launch,
    CloudSaves,
    Tools,
    Log,
}

impl GameSection {
    pub const ALL: [GameSection; 6] = [
        GameSection::Wine,
        GameSection::Performance,
        GameSection::Launch,
        GameSection::CloudSaves,
        GameSection::Tools,
        GameSection::Log,
    ];

    /// Route id of the section.
    pub fn id(self) -> &'static str {
        match self {
            GameSection::Wine => "wine",
            GameSection::Performance => "performance",
            GameSection::Launch => "launch",
            GameSection::CloudSaves => "saves",
            GameSection::Tools => "tools",
            GameSection::Log => "log",
        }
    }

    /// Parse a route id. Unknown ids open the first section that applies.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "wine" | "proton" => GameSection::Wine,
            "performance" | "other" => GameSection::Performance,
            "launch" | "advanced" => GameSection::Launch,
            "saves" | "sync" | "cloud" => GameSection::CloudSaves,
            "tools" => GameSection::Tools,
            "log" | "logs" => GameSection::Log,
            _ => return None,
        })
    }

    /// Name of the section in the interface language.
    pub fn label(self) -> SharedString {
        t(match self {
            GameSection::Wine => "settings.game.section.wine",
            GameSection::Performance => "settings.game.section.performance",
            GameSection::Launch => "settings.game.section.launch",
            GameSection::CloudSaves => "settings.game.section.cloud_saves",
            GameSection::Tools => "settings.game.section.tools",
            GameSection::Log => "settings.game.section.log",
        })
    }

    fn icon(self) -> IconName {
        match self {
            GameSection::Wine => IconName::Wine,
            GameSection::Performance => IconName::Gauge,
            GameSection::Launch => IconName::Rocket,
            GameSection::CloudSaves => IconName::Cloud,
            GameSection::Tools => IconName::Hammer,
            GameSection::Log => IconName::ScrollText,
        }
    }

    fn field_section(self) -> Option<FieldSection> {
        match self {
            GameSection::Wine => Some(FieldSection::Wine),
            GameSection::Performance => Some(FieldSection::Performance),
            GameSection::Launch => Some(FieldSection::Launch),
            GameSection::CloudSaves => Some(FieldSection::CloudSaves),
            _ => None,
        }
    }
}

/// Prefix tools offered for Windows games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tool {
    Winecfg,
    Winetricks,
    RunExe,
    KillPrefix,
    /// Install (`true`) or remove a DLL tool in the prefix.
    Dx(DxTool, bool),
}

/// Settings of one game: the shared form bound to the game's overrides,
/// prefix tools and the game's log.
pub struct GameSettingsView {
    id: GameId,
    section: GameSection,
    search: Entity<InputState>,
    query: String,
    form: Entity<SettingsForm>,
    log: Entity<LogViewer>,
    winetricks: Entity<WinetricksPanel>,
    running: HashSet<Tool>,
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

impl GameSettingsView {
    /// Settings of `id`, opened on `section` (a [`GameSection`] route id).
    pub fn new(
        id: GameId,
        section: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("settings.common.search_settings"))
        });
        let sub = cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = state.read(cx).value().to_string();
                this.apply_filter(cx);
            }
        });
        let form = cx.new(|cx| SettingsForm::new(FormTarget::Game(id.clone()), window, cx));
        form.update(cx, |f, cx| f.load_wine_versions(window, cx));
        let b = backend(cx);
        let log_path = b.game_log(&id);
        // The upload name identifies the log at the paste service and in the
        // upload history, so it stays the same in every language.
        let log_name = format!(
            "Log of {}",
            b.game(&id)
                .map(|g| g.title.clone())
                .unwrap_or_else(|| id.app_name.to_string())
        );
        let log = cx.new(|cx| LogViewer::new(log_path, log_name, window, cx));
        let winetricks = {
            let id = id.clone();
            cx.new(|cx| WinetricksPanel::new(id, window, cx))
        };
        let library = models(cx).library.clone();
        let observe = cx.observe(&library, |_, _, cx| cx.notify());
        let mut this = Self {
            id,
            section: GameSection::Wine,
            search,
            query: String::new(),
            form,
            log,
            winetricks,
            running: HashSet::new(),
            language: LanguageStamp::new(),
            _subscriptions: vec![sub, observe],
        };
        let initial = GameSection::from_id(&section).unwrap_or_else(|| this.default_section(cx));
        this.open(initial, window, cx);
        this
    }

    /// The game whose settings are shown.
    pub fn game_id(&self) -> &GameId {
        &self.id
    }

    /// The shared form bound to this game.
    pub fn form(&self) -> &Entity<SettingsForm> {
        &self.form
    }

    /// Show a section (route id) without recreating the view.
    pub fn set_section(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let section = GameSection::from_id(id).unwrap_or_else(|| self.default_section(cx));
        self.open(section, window, cx);
    }

    fn platform(&self, cx: &Context<Self>) -> Option<InstallPlatform> {
        backend(cx)
            .game(&self.id)
            .and_then(|g| g.installed_platform())
    }

    fn uses_wine(&self, cx: &Context<Self>) -> bool {
        self.platform(cx).is_none_or(InstallPlatform::needs_wine)
    }

    fn default_section(&self, cx: &Context<Self>) -> GameSection {
        if self.uses_wine(cx) {
            GameSection::Wine
        } else {
            GameSection::Performance
        }
    }

    fn open(&mut self, section: GameSection, window: &mut Window, cx: &mut Context<Self>) {
        if self.section == GameSection::Log && section != GameSection::Log {
            self.log.update(cx, |l, _| l.stop());
        }
        self.section = section;
        if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if section == GameSection::Log {
            self.log.update(cx, |l, cx| l.start(window, cx));
        }
        if section == GameSection::Tools && self.uses_wine(cx) {
            self.winetricks.update(cx, |w, cx| w.activate(window, cx));
        }
        self.apply_filter(cx);
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let q = self.query.trim().to_string();
        let section = self.section.field_section();
        self.form.update(cx, |f, cx| {
            if !q.is_empty() {
                f.set_query(&q, cx);
            } else if let Some(s) = section {
                f.set_sections(vec![s], cx);
            }
        });
        cx.notify();
    }

    fn confirm_reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title(cx);
        let form = self.form.downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let form = form.clone();
            alert
                .title(t_args(
                    "settings.game.reset_all.title",
                    &[("title", &title)],
                ))
                .description(t("settings.game.reset_all.description"))
                .show_cancel(true)
                .cancel_text(t("settings.common.cancel"))
                .ok_text(t("settings.game.reset_all.confirm"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    form.update(cx, |f, cx| f.reset_all(window, cx)).ok();
                    true
                })
        });
    }

    fn title(&self, cx: &Context<Self>) -> SharedString {
        backend(cx)
            .game(&self.id)
            .map(|g| SharedString::from(g.title.clone()))
            .unwrap_or_else(|| SharedString::from(self.id.app_name.to_string()))
    }

    fn run_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if tool == Tool::RunExe {
            let rx = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some(t("settings.tools.run_exe.prompt")),
            });
            cx.spawn_in(window, async move |this, cx| {
                if let Ok(Ok(Some(paths))) = rx.await
                    && let Some(exe) = paths.into_iter().next()
                {
                    this.update_in(cx, |this, window, cx| {
                        this.start_tool(
                            Tool::RunExe,
                            vec![exe.to_string_lossy().into_owned()],
                            window,
                            cx,
                        )
                    })
                    .ok();
                }
            })
            .detach();
            return;
        }
        let argv = match tool {
            Tool::Winecfg => vec!["winecfg".to_string()],
            _ => Vec::new(),
        };
        self.start_tool(tool, argv, window, cx);
    }

    fn start_tool(
        &mut self,
        tool: Tool,
        argv: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.running.insert(tool) {
            return;
        }
        let done: Option<SharedString> = match tool {
            Tool::Dx(dx, true) => Some(t_args(
                "settings.tools.dx.installed",
                &[("tool", dx.label())],
            )),
            Tool::Dx(dx, false) => {
                Some(t_args("settings.tools.dx.removed", &[("tool", dx.label())]))
            }
            _ => None,
        };
        let b = backend(cx);
        let id = self.id.clone();
        let missing = t("settings.error.game_missing");
        let task = ember_ui::run(cx, async move {
            match tool {
                Tool::Winetricks => b.winetricks(&id, Vec::new()).await.map(|_| ()),
                Tool::KillPrefix => ember_runtime::kill_prefix(&b.settings.game(&id)).await,
                Tool::Dx(dx, enable) => {
                    let game = b.game(&id).ok_or_else(|| anyhow::anyhow!("{missing}"))?;
                    let target = Target {
                        id: &id,
                        game: &game,
                        platform: game
                            .installed_platform()
                            .unwrap_or(InstallPlatform::Windows),
                    };
                    ember_runtime::set_dx_tool(
                        &b.settings.app(),
                        &b.settings.game(&id),
                        target,
                        dx,
                        enable,
                        OpContext::detached(),
                    )
                    .await
                }
                _ => b.run_in_prefix(&id, argv).await.map(|_| ()),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.running.remove(&tool);
                match result {
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}")).title(t("settings.tools.failed")),
                        cx,
                    ),
                    Ok(()) => {
                        if let Some(done) = done {
                            window.push_notification(Notification::success(done), cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn render_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.uses_wine(cx);
        let reason = (!enabled).then(|| t("settings.tools.windows_only"));
        let tools = [
            (
                Tool::Winecfg,
                "settings.tools.winecfg.label",
                "settings.tools.winecfg.help",
                "settings.tools.winecfg.action",
            ),
            (
                Tool::Winetricks,
                "settings.tools.winetricks.label",
                "settings.tools.winetricks.help",
                "settings.tools.winetricks.action",
            ),
            (
                Tool::RunExe,
                "settings.tools.run_exe.label",
                "settings.tools.run_exe.help",
                "settings.tools.run_exe.action",
            ),
            (
                Tool::KillPrefix,
                "settings.tools.kill_prefix.label",
                "settings.tools.kill_prefix.help",
                "settings.tools.kill_prefix.action",
            ),
        ];
        let rows = tools.into_iter().map(|(tool, label, help, action)| {
            let row = RowState {
                label: t(label),
                help: t(help),
                disabled_reason: reason.clone(),
                ..Default::default()
            };
            let button = Button::new(ElementId::Name(format!("tool-{tool:?}").into()))
                .label(t(action))
                .loading(self.running.contains(&tool))
                .disabled(!enabled || self.running.contains(&tool))
                .on_click(cx.listener(move |this, _, window, cx| this.run_tool(tool, window, cx)))
                .into_any_element();
            field_row(row, button, cx)
        });
        let dx_rows = DxTool::ALL.into_iter().map(|dx| {
            let row = RowState {
                label: dx.label().into(),
                help: t("settings.tools.dx.help"),
                disabled_reason: reason.clone(),
                ..Default::default()
            };
            let busy = self.running.contains(&Tool::Dx(dx, true))
                || self.running.contains(&Tool::Dx(dx, false));
            let buttons = h_flex()
                .gap_2()
                .child(
                    Button::new(ElementId::Name(format!("dx-install-{}", dx.id()).into()))
                        .label(t("settings.common.install"))
                        .loading(self.running.contains(&Tool::Dx(dx, true)))
                        .disabled(!enabled || busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.run_tool(Tool::Dx(dx, true), window, cx)
                        })),
                )
                .child(
                    Button::new(ElementId::Name(format!("dx-remove-{}", dx.id()).into()))
                        .ghost()
                        .label(t("settings.common.remove"))
                        .loading(self.running.contains(&Tool::Dx(dx, false)))
                        .disabled(!enabled || busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.run_tool(Tool::Dx(dx, false), window, cx)
                        })),
                )
                .into_any_element();
            field_row(row, buttons, cx)
        });
        v_flex()
            .w_full()
            .child(crate::widgets::group_heading(
                t("settings.tools.group.prefix"),
                cx,
            ))
            .children(rows)
            .child(crate::widgets::group_heading(
                t("settings.tools.group.translation_layers"),
                cx,
            ))
            .children(dx_rows)
            .child(crate::widgets::group_heading(
                t("settings.tools.group.winetricks"),
                cx,
            ))
            .child(if enabled {
                div().pt_2().child(self.winetricks.clone())
            } else {
                div()
                    .py_3()
                    .child(muted(t("settings.tools.windows_only"), cx))
            })
            .into_any_element()
    }
}

impl Render for GameSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| {
                let placeholder = t("settings.common.search_settings");
                this.search
                    .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            });
        }
        let searching = !self.query.trim().is_empty();
        let title = self.title(cx);
        let overridden = self.form.read(cx).overridden_count();
        let known = backend(cx).game(&self.id).is_some();
        let id = self.id.clone();
        let nav = v_flex()
            .w_56()
            .flex_shrink_0()
            .gap_1()
            .p_3()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.search)
                    .id("game-settings-search")
                    .aria_label(t("settings.common.search_settings"))
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .mb_2(),
            )
            .children(GameSection::ALL.map(|s| {
                nav_button(
                    ElementId::Name(format!("game-settings-nav-{}", s.id()).into()),
                    s.icon(),
                    s.label(),
                    !searching && s == self.section,
                    cx.listener(move |this, _, window, cx| this.open(s, window, cx)),
                )
            }));

        let description = match overridden {
            0 => t("settings.game.overridden_none"),
            1 => t("settings.game.overridden_one"),
            n => t_args(
                "settings.game.overridden_other",
                &[("count", &n.to_string())],
            ),
        };
        let header = PageHeader::new(title)
            .description(description)
            .action(
                Button::new("game-settings-defaults")
                    .ghost()
                    .label(t("settings.game.open_defaults"))
                    .on_click(|_, _, cx| {
                        ember_ui::navigate(Route::Settings("game-defaults".into()), cx)
                    }),
            )
            .action(
                Button::new("game-settings-reset-all")
                    .outline()
                    .icon(IconName::RotateCcw)
                    .label(t("settings.game.reset_all.action"))
                    .disabled(overridden == 0)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.confirm_reset_all(window, cx)),
                    ),
            )
            .action(
                Button::new("game-settings-back")
                    .outline()
                    .label(t("settings.game.back"))
                    .on_click(move |_, _, cx| ember_ui::navigate(Route::Game(id.clone()), cx)),
            );

        if !known {
            return h_flex()
                .size_full()
                .child(
                    EmptyState::new(IconName::Gamepad2, t("settings.game.unknown.title"))
                        .description(t("settings.game.unknown.description")),
                )
                .into_any_element();
        }

        let content: AnyElement = if !searching && self.section == GameSection::Log {
            v_flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .gap_4()
                .p_6()
                .child(header)
                .child(div().flex_1().min_h_0().child(self.log.clone()))
                .into_any_element()
        } else {
            let wine_note =
                (!searching && self.section == GameSection::Wine && !self.uses_wine(cx))
                    .then(|| info_alert("native-note", t("settings.game.native_note")));
            let body: AnyElement = if !searching && self.section == GameSection::Tools {
                self.render_tools(cx)
            } else if searching && self.form.read(cx).visible_count() == 0 {
                EmptyState::new(
                    IconName::Search,
                    t_args("settings.common.no_match", &[("query", self.query.trim())]),
                )
                .into_any_element()
            } else {
                self.form.clone().into_any_element()
            };
            div()
                .id("game-settings-content")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(rems(48.))
                        .p_6()
                        .pb_12()
                        .gap_2()
                        .child(header)
                        .when(searching, |v| {
                            v.child(muted(t("settings.common.search_results"), cx))
                        })
                        .children(wine_note)
                        .child(body),
                )
                .into_any_element()
        };
        h_flex()
            .id("game-settings")
            .size_full()
            .items_stretch()
            .when(ember_ui::glass::solid_pages(cx), |this| {
                this.bg(cx.theme().background)
            })
            .child(nav)
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_ids_round_trip() {
        for s in GameSection::ALL {
            assert_eq!(GameSection::from_id(s.id()), Some(s));
        }
        assert_eq!(GameSection::from_id("sync"), Some(GameSection::CloudSaves));
        assert_eq!(
            GameSection::from_id("other"),
            Some(GameSection::Performance)
        );
        assert_eq!(GameSection::from_id("?"), None);
    }
}
