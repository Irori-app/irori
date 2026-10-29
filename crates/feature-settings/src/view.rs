//! The application settings page (`Route::Settings`).

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, ElementId, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div, rems,
};
use irori_core::Paths;
use irori_runtime::sysinfo::{self, SystemInfo};
use irori_ui::components::{EmptyState, PageHeader};
use irori_ui::i18n::{t, t_args};

use crate::app_update::AppUpdatePanel;
use crate::backup::BackupPanel;
use crate::form::{FormTarget, SettingsForm};
use crate::i18n::{LanguageStamp, english};
use crate::logs::LogViewer;
use crate::registry::FieldSection;
use crate::widgets::{group_heading, muted, nav_button, panel};

/// Sections of the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsSection {
    General,
    GameDefaults,
    Appearance,
    Advanced,
    Backup,
    Logs,
    SystemInfo,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 8] = [
        SettingsSection::General,
        SettingsSection::GameDefaults,
        SettingsSection::Appearance,
        SettingsSection::Advanced,
        SettingsSection::Backup,
        SettingsSection::Logs,
        SettingsSection::SystemInfo,
        SettingsSection::About,
    ];

    /// Route id of the section.
    pub fn id(self) -> &'static str {
        match self {
            SettingsSection::General => "general",
            SettingsSection::GameDefaults => "game-defaults",
            SettingsSection::Appearance => "appearance",
            SettingsSection::Advanced => "advanced",
            SettingsSection::Backup => "backup",
            SettingsSection::Logs => "logs",
            SettingsSection::SystemInfo => "system",
            SettingsSection::About => "about",
        }
    }

    /// Parse a route id; accepts Heroic's route names. Unknown ids open General.
    pub fn from_id(id: &str) -> Self {
        match id {
            "game-defaults" | "games" | "games_settings" | "wine" | "defaults" => {
                SettingsSection::GameDefaults
            }
            "appearance" | "accessibility" | "theme" => SettingsSection::Appearance,
            "advanced" => SettingsSection::Advanced,
            "backup" | "restore" | "export" => SettingsSection::Backup,
            "logs" | "log" => SettingsSection::Logs,
            "system" | "systeminfo" | "system-info" => SettingsSection::SystemInfo,
            "about" => SettingsSection::About,
            _ => SettingsSection::General,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            SettingsSection::General => "settings.page.general",
            SettingsSection::GameDefaults => "settings.page.game_defaults",
            SettingsSection::Appearance => "settings.page.appearance",
            SettingsSection::Advanced => "settings.page.advanced",
            SettingsSection::Backup => "settings.page.backup",
            SettingsSection::Logs => "settings.page.logs",
            SettingsSection::SystemInfo => "settings.page.system_info",
            SettingsSection::About => "settings.page.about",
        }
    }

    /// Name of the section in the interface language.
    pub fn label(self) -> SharedString {
        t(self.label_key())
    }

    fn description(self) -> SharedString {
        let key = match self {
            SettingsSection::General => "settings.page.general_description",
            SettingsSection::GameDefaults => "settings.page.game_defaults_description",
            SettingsSection::Appearance => "settings.page.appearance_description",
            SettingsSection::Advanced => "settings.page.advanced_description",
            SettingsSection::Backup => "settings.page.backup_description",
            SettingsSection::Logs => "settings.page.logs_description",
            SettingsSection::SystemInfo => "settings.page.system_info_description",
            SettingsSection::About => return SharedString::default(),
        };
        t(key)
    }

    fn icon(self) -> IconName {
        match self {
            SettingsSection::General => IconName::Settings,
            SettingsSection::GameDefaults => IconName::Gamepad2,
            SettingsSection::Appearance => IconName::Palette,
            SettingsSection::Advanced => IconName::Wrench,
            SettingsSection::Backup => IconName::ArchiveRestore,
            SettingsSection::Logs => IconName::ScrollText,
            SettingsSection::SystemInfo => IconName::Cpu,
            SettingsSection::About => IconName::Info,
        }
    }

    /// English search terms for pages without fields.
    fn keywords(self) -> &'static str {
        match self {
            SettingsSection::Logs => "logs log launcher debug report problem",
            SettingsSection::SystemInfo => {
                "system information hardware cpu gpu memory driver os report"
            }
            SettingsSection::About => "about version license credits",
            _ => "",
        }
    }

    fn field_section(self) -> Option<FieldSection> {
        match self {
            SettingsSection::General => Some(FieldSection::General),
            SettingsSection::Appearance => Some(FieldSection::Appearance),
            SettingsSection::Advanced => Some(FieldSection::Advanced),
            _ => None,
        }
    }
}

#[derive(Default)]
struct SysInfoState {
    info: Option<Arc<SystemInfo>>,
    loading: bool,
    copied: bool,
}

/// The settings page: section navigation, a search field filtering every
/// field, and the selected section.
pub struct SettingsView {
    section: SettingsSection,
    search: Entity<InputState>,
    query: String,
    app_form: Entity<SettingsForm>,
    defaults_form: Entity<SettingsForm>,
    log: Entity<LogViewer>,
    backup: Entity<BackupPanel>,
    app_update: Entity<AppUpdatePanel>,
    sysinfo: SysInfoState,
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// The page opened on `section` (a [`SettingsSection`] route id).
    pub fn new(section: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("settings.common.search_settings"))
        });
        let sub = cx.subscribe_in(
            &search,
            window,
            |this, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let q = state.read(cx).value().to_string();
                    this.set_query(q, window, cx);
                }
            },
        );
        let app_form = cx.new(|cx| SettingsForm::new(FormTarget::App, window, cx));
        let defaults_form = cx.new(|cx| SettingsForm::new(FormTarget::Defaults, window, cx));
        // The upload name identifies the log at the paste service and in the
        // upload history, so it stays the same in every language.
        let log =
            cx.new(|cx| LogViewer::new(Paths::get().launcher_log_file(), "Irori log", window, cx));
        let mut this = Self {
            section: SettingsSection::General,
            search,
            query: String::new(),
            app_form,
            defaults_form,
            log,
            backup: cx.new(|_| BackupPanel::new()),
            app_update: cx.new(AppUpdatePanel::new),
            sysinfo: SysInfoState::default(),
            language: LanguageStamp::new(),
            _subscriptions: vec![sub],
        };
        this.set_section(&section, window, cx);
        this
    }

    /// The current section.
    pub fn section(&self) -> SettingsSection {
        self.section
    }

    /// The current search query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Show a section (route id); the shell calls this when the route
    /// changes while the page is open.
    pub fn set_section(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open(SettingsSection::from_id(id), window, cx);
    }

    /// The shared application-fields form (General, Appearance, Advanced).
    pub fn app_form(&self) -> &Entity<SettingsForm> {
        &self.app_form
    }

    #[cfg(test)]
    pub(crate) fn search_input(&self) -> Entity<InputState> {
        self.search.clone()
    }

    /// The game defaults form.
    pub fn defaults_form(&self) -> &Entity<SettingsForm> {
        &self.defaults_form
    }

    fn open(&mut self, section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) {
        if self.section == SettingsSection::Logs && section != SettingsSection::Logs {
            self.log.update(cx, |l, _| l.stop());
        }
        self.section = section;
        if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if let Some(fs) = section.field_section() {
            self.app_form
                .update(cx, |f, cx| f.set_sections(vec![fs], cx));
        }
        match section {
            SettingsSection::GameDefaults => {
                self.defaults_form.update(cx, |f, cx| {
                    f.set_sections(FieldSection::GAME.to_vec(), cx);
                    f.load_wine_versions(window, cx);
                });
            }
            SettingsSection::Logs => self.log.update(cx, |l, cx| l.start(window, cx)),
            SettingsSection::SystemInfo if self.sysinfo.info.is_none() => {
                self.load_sysinfo(window, cx)
            }
            _ => {}
        }
        cx.notify();
    }

    fn set_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        if query.trim() == self.query.trim() {
            self.query = query;
            return;
        }
        self.query = query;
        if self.query.trim().is_empty() {
            let section = self.section;
            self.query.clear();
            self.open(section, window, cx);
            return;
        }
        let q = self.query.clone();
        self.app_form.update(cx, |f, cx| f.set_query(&q, cx));
        self.defaults_form.update(cx, |f, cx| {
            f.set_query(&q, cx);
            f.load_wine_versions(window, cx);
        });
        cx.notify();
    }

    fn load_sysinfo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sysinfo.loading {
            return;
        }
        self.sysinfo.loading = true;
        let task = irori_ui::run(cx, async move { Ok(sysinfo::get(true).await) });
        cx.spawn_in(window, async move |this, cx| {
            let info = task.await.ok();
            this.update(cx, |this, cx| {
                this.sysinfo.loading = false;
                this.sysinfo.info = info;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn copy_sysinfo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.sysinfo.info else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(sysinfo::format(info)));
        self.sysinfo.copied = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |this, cx| {
                this.sysinfo.copied = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let searching = !self.query.trim().is_empty();
        let buttons = SettingsSection::ALL.map(|s| {
            nav_button(
                ElementId::Name(format!("settings-nav-{}", s.id()).into()),
                s.icon(),
                s.label(),
                !searching && s == self.section,
                cx.listener(move |this, _, window, cx| this.open(s, window, cx)),
            )
        });
        v_flex()
            .w_56()
            .flex_shrink_0()
            .gap_1()
            .p_3()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.search)
                    .id("settings-search")
                    .aria_label(t("settings.common.search_settings"))
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .mb_2(),
            )
            .children(buttons)
    }

    fn render_search(&self, cx: &mut Context<Self>) -> AnyElement {
        let q = self.query.trim().to_lowercase();
        let pages: Vec<SettingsSection> = SettingsSection::ALL
            .into_iter()
            .filter(|s| {
                let hay = format!("{} {} {}", s.label(), english(s.label_key()), s.keywords())
                    .to_lowercase();
                !s.keywords().is_empty() && q.split_whitespace().all(|t| hay.contains(t))
            })
            .collect();
        let count =
            self.app_form.read(cx).visible_count() + self.defaults_form.read(cx).visible_count();
        if count == 0 && pages.is_empty() {
            return EmptyState::new(
                IconName::Search,
                t_args("settings.common.no_match", &[("query", self.query.trim())]),
            )
            .description(t("settings.search.empty_hint"))
            .into_any_element();
        }
        v_flex()
            .w_full()
            .when(!pages.is_empty(), |v| {
                v.child(group_heading(t("settings.search.pages"), cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .children(pages.into_iter().map(|s| {
                                Button::new(ElementId::Name(
                                    format!("search-page-{}", s.id()).into(),
                                ))
                                .outline()
                                .icon(s.icon())
                                .label(s.label())
                                .on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.open(s, window, cx)
                                    }),
                                )
                            })),
                    )
            })
            .child(self.app_form.clone())
            .child(self.defaults_form.clone())
            .into_any_element()
    }

    fn render_sysinfo(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(info) = &self.sysinfo.info else {
            return muted(
                if self.sysinfo.loading {
                    t("settings.sysinfo.loading")
                } else {
                    t("settings.sysinfo.unavailable")
                },
                cx,
            )
            .into_any_element();
        };
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        let opt = |v: &Option<String>| -> SharedString {
            v.clone()
                .map(SharedString::from)
                .unwrap_or_else(|| t("settings.sysinfo.not_installed"))
        };
        let mut rows: Vec<(SharedString, SharedString)> = vec![
            (
                t("settings.sysinfo.cpu"),
                format!("{} × {}", info.cpu.cores, info.cpu.model).into(),
            ),
            (
                t("settings.sysinfo.memory"),
                t_args(
                    "settings.sysinfo.memory_value",
                    &[
                        ("used", &irori_core::util::format_bytes(info.memory.used)),
                        ("total", &irori_core::util::format_bytes(info.memory.total)),
                    ],
                ),
            ),
        ];
        for (i, gpu) in info.gpus.iter().enumerate() {
            let label = if info.gpus.len() > 1 {
                t_args(
                    "settings.sysinfo.gpu_numbered",
                    &[("number", &(i + 1).to_string())],
                )
            } else {
                t("settings.sysinfo.gpu")
            };
            let name = match (&gpu.vendor_name, &gpu.device_name) {
                (Some(v), Some(d)) => format!("{v} {d}"),
                (None, Some(d)) => d.clone(),
                _ => format!("PCI {}:{}", gpu.vendor_id, gpu.device_id),
            };
            let driver: SharedString = match (&gpu.driver, &gpu.driver_version) {
                (Some(d), Some(v)) => format!("{d} {v}").into(),
                (Some(d), None) => d.clone().into(),
                _ => t("settings.sysinfo.no_driver"),
            };
            rows.push((label, format!("{name} · {driver}").into()));
        }
        let os = format!(
            "{} {}",
            info.os.name,
            info.os.version.as_deref().unwrap_or("")
        );
        rows.push((
            t("settings.sysinfo.os"),
            t_args(
                "settings.sysinfo.os_value",
                &[("os", os.trim()), ("kernel", &info.os.kernel)],
            ),
        ));
        if cfg!(target_os = "linux") {
            let desktop = info
                .desktop
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| t("settings.sysinfo.unknown_desktop"));
            let session = info
                .session_type
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| t("settings.sysinfo.unknown_session"));
            rows.push((
                t("settings.sysinfo.session"),
                format!(
                    "{desktop} · {session}{}",
                    if info.is_flatpak { " · Flatpak" } else { "" }
                )
                .into(),
            ));
            rows.push((
                "Vulkan".into(),
                if info.vulkan.available() {
                    info.vulkan
                        .instance_version
                        .clone()
                        .map(SharedString::from)
                        .unwrap_or_else(|| t("settings.sysinfo.available"))
                } else {
                    t("settings.sysinfo.not_available")
                },
            ));
        }
        let software = &info.software;
        rows.push(("Irori".into(), software.irori.clone().into()));
        for (name, v) in [
            ("Wine", &software.wine),
            ("gamescope", &software.gamescope),
            ("GameMode", &software.gamemode),
            ("MangoHud", &software.mangohud),
            ("umu-launcher", &software.umu),
        ] {
            if cfg!(target_os = "linux") || (name == "Wine" && !cfg!(windows)) {
                rows.push((name.into(), opt(v)));
            }
        }
        let list = rows
            .into_iter()
            .enumerate()
            .fold(panel(cx), |p, (i, (k, v))| {
                p.child(
                    h_flex()
                        .gap_4()
                        .px_4()
                        .py_2()
                        .when(i > 0, |r| r.border_t_1().border_color(border))
                        .child(
                            div()
                                .w_40()
                                .flex_shrink_0()
                                .min_w_0()
                                .text_sm()
                                .text_color(muted_fg)
                                .child(k),
                        )
                        .child(div().flex_1().min_w_0().text_sm().child(v)),
                )
            });
        v_flex().gap_4().child(list).into_any_element()
    }

    fn render_about(&self, cx: &mut Context<Self>) -> AnyElement {
        let credits: [(SharedString, SharedString); 6] = [
            (t("settings.about.credit_gpui"), "Apache-2.0".into()),
            (t("settings.about.credit_lucide"), "ISC".into()),
            (t("settings.about.credit_themes"), "Apache-2.0".into()),
            ("legendary, gogdl, nile".into(), "GPL-3.0".into()),
            ("comet".into(), "Apache-2.0".into()),
            ("umu-launcher".into(), "GPL-3.0".into()),
        ];
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .child(format!("Irori {}", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(muted(t("settings.about.tagline"), cx)),
            )
            .child(self.app_update.clone())
            .child(group_heading(t("settings.about.third_party"), cx))
            .child(credits.into_iter().fold(panel(cx), |p, (name, license)| {
                p.child(
                    h_flex()
                        .justify_between()
                        .gap_4()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(name))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(license),
                        ),
                )
            }))
            .into_any_element()
    }

    fn header_actions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.section {
            SettingsSection::SystemInfo => Some(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("sysinfo-refresh")
                            .outline()
                            .icon(IconName::RefreshCw)
                            .label(t("settings.common.refresh"))
                            .loading(self.sysinfo.loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.load_sysinfo(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("sysinfo-copy")
                            .icon(if self.sysinfo.copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .label(if self.sysinfo.copied {
                                t("settings.common.copied")
                            } else {
                                t("settings.common.copy")
                            })
                            .disabled(self.sysinfo.info.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.copy_sysinfo(window, cx)),
                            ),
                    )
                    .into_any_element(),
            ),
            _ => None,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| {
                let placeholder = t("settings.common.search_settings");
                this.search
                    .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            });
        }
        let searching = !self.query.trim().is_empty();
        let section = self.section;
        let content: AnyElement = if !searching && section == SettingsSection::Logs {
            v_flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .gap_4()
                .p_6()
                .child(PageHeader::new(section.label()).description(section.description()))
                .child(div().flex_1().min_h_0().child(self.log.clone()))
                .into_any_element()
        } else {
            let (title, description): (SharedString, SharedString) = if searching {
                (t("settings.common.search_results"), SharedString::default())
            } else {
                (section.label(), section.description())
            };
            let mut header = PageHeader::new(title);
            if !description.is_empty() {
                header = header.description(description);
            }
            if !searching && let Some(actions) = self.header_actions(cx) {
                header = header.action(actions);
            }
            let body: AnyElement = if searching {
                self.render_search(cx)
            } else {
                match section {
                    SettingsSection::General
                    | SettingsSection::Appearance
                    | SettingsSection::Advanced => self.app_form.clone().into_any_element(),
                    SettingsSection::GameDefaults => self.defaults_form.clone().into_any_element(),
                    SettingsSection::Backup => self.backup.clone().into_any_element(),
                    SettingsSection::SystemInfo => self.render_sysinfo(cx),
                    SettingsSection::About => self.render_about(cx),
                    SettingsSection::Logs => div().into_any_element(),
                }
            };
            div()
                .id("settings-content")
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
                        .child(body),
                )
                .into_any_element()
        };
        h_flex()
            .id("settings")
            .size_full()
            .items_stretch()
            .child(self.render_nav(cx))
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_ids_round_trip_and_accept_heroic_names() {
        for s in SettingsSection::ALL {
            assert_eq!(SettingsSection::from_id(s.id()), s);
        }
        assert_eq!(
            SettingsSection::from_id("games_settings"),
            SettingsSection::GameDefaults
        );
        assert_eq!(
            SettingsSection::from_id("systeminfo"),
            SettingsSection::SystemInfo
        );
        assert_eq!(SettingsSection::from_id("log"), SettingsSection::Logs);
        assert_eq!(
            SettingsSection::from_id("nonsense"),
            SettingsSection::General
        );
    }
}
