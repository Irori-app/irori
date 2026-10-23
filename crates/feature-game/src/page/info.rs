//! Tabs below the header: about, details, achievements, screenshots,
//! compatibility, requirements and changelog.

use std::sync::Arc;

use ember_core::util::{format_bytes, unix_now};
use ember_core::{GameId, GameInfo};
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    Styled, Window, div, rems,
};

use super::{GamePage, InfoTab};
use crate::i18n::{platform_label, playtime_label, status_label};
use crate::load::{Load, format_day, looks_like_html};
use crate::saves::CloudSupport;
use crate::state::{ActionInputs, PrimaryAction, completion_percent};
use crate::{dlc, ops};

/// Rich store text (HTML from GOG, Markdown elsewhere).
pub(super) fn rich_text(id: impl Into<gpui_kit::ElementId>, text: &str) -> AnyElement {
    if looks_like_html(text) {
        TextView::html(id, text.to_string()).into_any_element()
    } else {
        TextView::markdown(id, text.to_string()).into_any_element()
    }
}

fn section_title(title: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(title.into())
        .into_any_element()
}

/// One fact of the Details tab: a small card with a label above its value.
fn detail_row(label: SharedString, value: impl IntoElement, cx: &App) -> AnyElement {
    v_flex()
        .flex_1()
        .min_w(rems(14.))
        .gap_1()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(cx.theme().secondary)
        .text_sm()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().min_w_0().truncate().child(value))
        .into_any_element()
}

/// Characters of a changelog shown before "Show full changelog".
const CHANGELOG_EXCERPT_CHARS: usize = 12_000;

/// The start of `text` up to the first heading after `budget` characters, and
/// whether anything was cut. Cutting at a heading keeps entries whole.
pub(super) fn changelog_excerpt(text: &str, budget: usize) -> (&str, bool) {
    if text.len() <= budget {
        return (text, false);
    }
    let html = looks_like_html(text);
    let heading_at = |i: usize| {
        let rest = &text[i..];
        if html {
            let b = rest.as_bytes();
            b.len() > 3
                && b[0] == b'<'
                && (b[1] == b'h' || b[1] == b'H')
                && (b'1'..=b'6').contains(&b[2])
        } else {
            rest.starts_with('#') && (i == 0 || text.as_bytes()[i - 1] == b'\n')
        }
    };
    let cut = text
        .char_indices()
        .map(|(i, _)| i)
        .skip_while(|i| *i < budget)
        .find(|i| heading_at(*i));
    match cut {
        Some(i) => (&text[..i], true),
        None => (text, false),
    }
}

/// Store text (HTML or Markdown) as one plain paragraph, for summaries.
pub(super) fn plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if in_tag => {}
            '#' | '*' | '_' | '`' => {}
            c => out.push(c),
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl GamePage {
    fn tabs(&self) -> Vec<(InfoTab, SharedString)> {
        let mut tabs = vec![
            (InfoTab::About, t("game.info.tab_about")),
            (InfoTab::Details, t("game.info.details")),
        ];
        if let Some(list) = self.achievements.ready().filter(|a| !a.is_empty()) {
            let unlocked = list.iter().filter(|a| a.unlocked_at.is_some()).count();
            tabs.push((
                InfoTab::Achievements,
                t_args(
                    "game.info.tab_achievements",
                    &[(
                        "percent",
                        &completion_percent(unlocked, list.len()).to_string(),
                    )],
                ),
            ));
        }
        let extra = self.extra.ready();
        if extra.is_some_and(|e| !e.screenshots.is_empty()) {
            tabs.push((InfoTab::Screenshots, t("game.info.tab_screenshots")));
        }
        if self.has_compat() {
            tabs.push((InfoTab::Compatibility, t("game.compat.title")));
        }
        if extra.is_some_and(|e| !e.requirements.is_empty()) {
            tabs.push((InfoTab::Requirements, t("game.info.tab_requirements")));
        }
        if extra.is_some_and(|e| e.changelog.as_ref().is_some_and(|c| !c.trim().is_empty())) {
            tabs.push((InfoTab::Changelog, t("game.info.tab_changelog")));
        }
        // GOG offers offline installers and goodies for every game it sells.
        if self.id.runner == ember_core::Runner::Gog {
            tabs.push((InfoTab::Extras, t("game.info.tab_extras")));
        }
        tabs
    }

    pub(super) fn render_body(
        &mut self,
        game: &Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tabs = self.tabs();
        if !tabs.iter().any(|(t, _)| *t == self.tab) {
            self.tab = InfoTab::About;
        }
        let selected = tabs.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        let kinds: Vec<InfoTab> = tabs.iter().map(|(t, _)| *t).collect();
        let bar = TabBar::new("game-tabs")
            .underline()
            .selected_index(selected)
            .children(tabs.into_iter().map(|(_, label)| Tab::new().label(label)))
            .on_click(cx.listener(move |this, ix: &usize, window, cx| {
                if let Some(t) = kinds.get(*ix) {
                    this.tab = *t;
                    if *t == InfoTab::Extras {
                        this.load_extras(window, cx);
                    }
                    cx.notify();
                }
            }));
        let content = match self.tab {
            InfoTab::About => self.render_about(game, window, cx),
            InfoTab::Details => self.render_details(game, cx),
            InfoTab::Achievements => self.render_achievements(window, cx),
            InfoTab::Screenshots => self.render_screenshots(window, cx),
            InfoTab::Compatibility => self
                .render_compat(cx)
                .unwrap_or_else(|| div().into_any_element()),
            InfoTab::Requirements => self.render_requirements(cx),
            InfoTab::Changelog => self.render_changelog(cx),
            InfoTab::Extras => self.render_extras(game, window, cx),
        };
        v_flex()
            .id("game-body")
            .w_full()
            .flex_shrink_0()
            .gap_4()
            .px_8()
            .pb_8()
            .child(bar)
            .child(content)
            .into_any_element()
    }

    fn render_about(
        &mut self,
        game: &Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let description = match &self.extra {
            Load::Ready(e) => e
                .description
                .clone()
                .or_else(|| e.short_description.clone())
                .or_else(|| game.description.clone()),
            _ => game.description.clone(),
        }
        .filter(|d| !d.trim().is_empty());
        let body: AnyElement = match (&self.extra, description) {
            (_, Some(text)) => rich_text("game-description", &text),
            (Load::Loading, None) => v_flex()
                .gap_2()
                .child(Skeleton::new().h_4().w_full())
                .child(Skeleton::new().h_4().w_full())
                .child(Skeleton::new().h_4().w(rems(16.)))
                .into_any_element(),
            (Load::Failed(e), None) => v_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t_args(
                            "game.info.store_details_failed",
                            &[("error", e.as_ref())],
                        )),
                )
                .child(
                    Button::new("retry-extra")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(t("game.common.retry"))
                        .on_click(cx.listener(|this, _, window, cx| this.load_extra(window, cx))),
                )
                .into_any_element(),
            _ => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t("game.info.no_description"))
                .into_any_element(),
        };
        let dlcs = self.render_dlcs(game, window, cx);
        v_flex()
            .max_w(rems(48.))
            .gap_6()
            .child(body)
            .children(dlcs)
            .children(self.render_redmod(game, cx))
            .into_any_element()
    }

    /// Entry to REDmod management for Cyberpunk 2077 on GOG.
    fn render_redmod(&self, game: &GameInfo, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !crate::redmod::applies(game, cx) {
            return None;
        }
        let id = self.id.clone();
        Some(
            v_flex()
                .gap_2()
                .child(section_title("REDmod", cx))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("game.info.redmod_description")),
                )
                .child(
                    h_flex().child(
                        Button::new("open-redmod")
                            .small()
                            .outline()
                            .icon(IconName::Puzzle)
                            .label(t("game.info.manage_mods"))
                            .on_click(move |_, window, cx| {
                                crate::redmod::open_redmod_sheet(id.clone(), window, cx)
                            }),
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_dlcs(
        &self,
        game: &Arc<GameInfo>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if game.dlcs.is_empty() {
            return None;
        }
        let m = models(cx);
        let queue = &m.downloads.read(cx).queue;
        let statuses = m.statuses.read(cx);
        let base_inputs = ActionInputs::from_game(game, statuses.kind(&self.id), queue, false);
        let base_idle = PrimaryAction::from_inputs(&base_inputs).is_idle();
        let editable = game.is_installed && dlc::dlcs_editable(game);
        let mut col = v_flex().gap_2().child(section_title(
            t_args(
                "game.info.dlc_title",
                &[("count", &game.dlcs.len().to_string())],
            ),
            cx,
        ));
        if !game.is_installed {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("game.info.dlc_choose_on_install")),
            );
        } else if !editable {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t_args(
                        "game.dlc.not_editable",
                        &[("store", game.runner().store_name())],
                    )),
            );
        }
        for d in &game.dlcs {
            let dlc_id = GameId::new(game.runner(), d.app_name.as_str());
            let busy = statuses.kind(&dlc_id).filter(|k| k.is_busy());
            let queued = queue.active.iter().any(|i| i.id == dlc_id);
            let state: Option<SharedString> = busy
                .map(status_label)
                .or_else(|| queued.then(|| status_label(ember_core::StatusKind::Queued)));
            let base = game.clone();
            let info = d.clone();
            col = col.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .py_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(d.title.clone()),
                    )
                    .when_some(state, |row, s| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(s),
                        )
                    })
                    .when(editable, |row| {
                        row.child(
                            Switch::new(SharedString::from(format!("dlc-toggle-{}", d.app_name)))
                                .checked(d.is_installed)
                                .accessibility_label(t_args(
                                    "game.info.dlc_install_toggle",
                                    &[("title", &d.title)],
                                ))
                                .disabled(
                                    !base_idle
                                        || busy.is_some()
                                        || queued
                                        || !d.owned && !d.is_installed,
                                )
                                .on_change(move |on: &bool, window, cx| {
                                    dlc::toggle_dlc(&base, &info, *on, window, cx)
                                }),
                        )
                    })
                    .when(!editable, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if d.is_installed {
                                    t("game.info.dlc_installed")
                                } else if d.owned {
                                    t("game.info.dlc_owned")
                                } else {
                                    SharedString::default()
                                }),
                        )
                    }),
            );
        }
        Some(col.into_any_element())
    }

    fn render_requirements(&self, cx: &App) -> AnyElement {
        let Some(extra) = self.extra.ready() else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let header = h_flex()
            .w_full()
            .gap_4()
            .pb_2()
            .border_b_1()
            .border_color(border)
            .text_xs()
            .font_semibold()
            .text_color(muted)
            .child(div().w(rems(8.)).flex_shrink_0())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(t("game.info.requirements_minimum")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(t("game.info.requirements_recommended")),
            );
        v_flex()
            .gap_2()
            .child(header)
            .children(extra.requirements.iter().map(|r| {
                h_flex()
                    .w_full()
                    .gap_4()
                    .items_start()
                    .text_sm()
                    .child(
                        div()
                            .w(rems(8.))
                            .flex_shrink_0()
                            .text_color(muted)
                            .child(r.title.clone()),
                    )
                    .child(div().flex_1().min_w_0().child(r.minimum.clone()))
                    .child(div().flex_1().min_w_0().child(r.recommended.clone()))
            }))
            .into_any_element()
    }

    fn render_changelog(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self
            .extra
            .ready()
            .and_then(|e| e.changelog.clone())
            .unwrap_or_default();
        // Long changelogs (GOG keeps a game's whole history) are slow to lay out,
        // so the latest entries come first and the rest on request.
        let (shown, cut) = if self.changelog_expanded {
            (text.as_str(), false)
        } else {
            changelog_excerpt(&text, CHANGELOG_EXCERPT_CHARS)
        };
        v_flex()
            .max_w(rems(48.))
            .gap_4()
            .items_start()
            .child(rich_text(
                if cut {
                    "game-changelog-latest"
                } else {
                    "game-changelog"
                },
                shown,
            ))
            .when(cut, |col| {
                col.child(
                    Button::new("changelog-show-all")
                        .outline()
                        .label(t("game.info.changelog_show_all"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.changelog_expanded = true;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_details(&self, game: &Arc<GameInfo>, cx: &mut Context<Self>) -> AnyElement {
        let m = models(cx);
        let library = m.library.read(cx);
        let record = library.user.playtime.get(&self.id.key()).cloned();
        let now = unix_now() as i64;
        let minutes = record.as_ref().map(|r| r.total_minutes).unwrap_or(0);
        let b = backend(cx);
        let mut col = h_flex().w_full().flex_wrap().gap_3().child(detail_row(
            t("game.info.playtime"),
            playtime_label(
                minutes,
                record.as_ref().is_some_and(|r| r.last_played.is_some()),
            ),
            cx,
        ));
        if let Some(r) = &record {
            if let Some(last) = &r.last_played {
                col = col.child(detail_row(
                    t("game.info.last_played"),
                    format_day(last, now),
                    cx,
                ));
            }
            if let Some(first) = &r.first_played {
                col = col.child(detail_row(
                    t("game.info.first_played"),
                    format_day(first, now),
                    cx,
                ));
            }
        }
        match &game.install {
            Some(install) if game.is_installed => {
                col = col.child(detail_row(
                    t("game.common.platform"),
                    platform_label(install.platform),
                    cx,
                ));
                let mut version = install.version.clone();
                if let Some(branch) = install.branch.as_ref().filter(|b| !b.is_empty()) {
                    version = format!("{version} ({branch})");
                }
                if install.pinned_version {
                    version =
                        t_args("game.info.version_pinned", &[("version", &version)]).to_string();
                }
                if !version.trim().is_empty() {
                    col = col.child(detail_row(t("game.common.version"), version, cx));
                }
                let size = if install.install_size_bytes > 0 {
                    format_bytes(install.install_size_bytes)
                } else {
                    install.install_size.clone()
                };
                if !size.is_empty() {
                    col = col.child(detail_row(t("game.info.size"), size, cx));
                }
                if let Some(language) = &install.language {
                    col = col.child(detail_row(
                        t("game.common.language"),
                        crate::load::language_name(language),
                        cx,
                    ));
                }
                if let Some(path) = game.install_path() {
                    let id = self.id.clone();
                    col = col.child(detail_row(
                        t("game.common.location"),
                        Button::new("open-install-path")
                            .xsmall()
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .label(path.display().to_string())
                            .tooltip(t("game.common.open_folder"))
                            .on_click(move |_, window, cx| {
                                ops::open_install_folder(&id, window, cx)
                            }),
                        cx,
                    ));
                }
                if install.platform.needs_wine() {
                    let settings = b.settings.game(&self.id);
                    let wine = if settings.wine_version.is_set() {
                        settings.wine_version.name.clone()
                    } else {
                        t("game.info.not_set").to_string()
                    };
                    col = col.child(detail_row("Wine".into(), wine, cx));
                    let prefix = ember_runtime::prefix_path(&b.settings.app(), &settings, game);
                    col = col.child(detail_row(
                        t("game.common.prefix"),
                        prefix.display().to_string(),
                        cx,
                    ));
                }
            }
            _ => {
                let platforms = crate::install::installable_platforms(game)
                    .iter()
                    .map(|p| platform_label(*p).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                col = col.child(detail_row(t("game.info.platforms"), platforms, cx));
            }
        }
        col = col.child(detail_row(
            t("game.info.offline_play"),
            if game.can_run_offline {
                t("game.info.offline_supported")
            } else {
                t("game.info.offline_needs_connection")
            },
            cx,
        ));
        let store_supports = b
            .store(self.id.runner)
            .is_some_and(|s| s.supports_cloud_saves());
        let saves = match CloudSupport::of(game, store_supports) {
            CloudSupport::Supported => {
                if b.settings.game(&self.id).auto_sync_saves {
                    t("game.info.saves_auto")
                } else {
                    t("game.info.saves_manual")
                }
            }
            CloudSupport::NotInstalled => t("game.info.saves_available"),
            CloudSupport::NativeBuild => t("game.info.saves_windows_only"),
            CloudSupport::StoreUnsupported | CloudSupport::GameUnsupported => {
                t("game.info.saves_not_supported")
            }
        };
        col = col.child(detail_row(t("game.info.cloud_saves"), saves, cx));
        if let Some(app) = &game.third_party_managed_app {
            col = col.child(detail_row(t("game.info.launcher"), app.clone(), cx));
        }
        v_flex()
            .gap_6()
            .child(col)
            .children(self.overlay.clone())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::changelog_excerpt;

    #[test]
    fn changelog_excerpt_cuts_at_a_heading() {
        let html = "<h4>2.0</h4><p>aaaa</p><h4>1.9</h4><p>bbbb</p><h4>1.8</h4>";
        assert_eq!(changelog_excerpt(html, 1000), (html, false));
        assert_eq!(
            changelog_excerpt(html, 10),
            ("<h4>2.0</h4><p>aaaa</p>", true)
        );
        let md = "# 2.0\nfixes\n# 1.9\nmore";
        assert_eq!(changelog_excerpt(md, 3), ("# 2.0\nfixes\n", true));
        assert!(!changelog_excerpt("no headings at all here", 3).1);
    }
}
