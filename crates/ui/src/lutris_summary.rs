//! What a Lutris import did: counts, the store games to log in for, games
//! that use the default Wine build, and every game that was not imported
//! with its reason. Shown after the import from onboarding and Settings.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, WindowExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, rems,
};

use crate::hooks::{LutrisSkip, LutrisSummary};
use crate::i18n::{t, t_args};

/// The translated reason a game was not imported.
pub fn skip_reason(skip: &LutrisSkip) -> SharedString {
    let detail = skip.detail.clone().unwrap_or_default();
    match skip.reason.as_str() {
        "notInstalled" => t("ui.lutris.reason.not_installed"),
        "noRunner" => t("ui.lutris.reason.no_runner"),
        "unknownRunner" => t_args("ui.lutris.reason.unknown_runner", &[("runner", &detail)]),
        "missingEntryPoint" => t("ui.lutris.reason.missing_entry_point"),
        "fileMissing" => t_args("ui.lutris.reason.file_missing", &[("path", &detail)]),
        "missingCore" => t("ui.lutris.reason.missing_core"),
        "unknownCore" => t_args("ui.lutris.reason.unknown_core", &[("core", &detail)]),
        "systemNotSupported" => t("ui.lutris.reason.system_not_supported"),
        "steamNotFound" => t("ui.lutris.reason.steam_not_found"),
        "storeNotSupported" => t_args(
            "ui.lutris.reason.store_not_supported",
            &[("store", &detail)],
        ),
        _ => t_args("ui.lutris.reason.failed", &[("error", &detail)]),
    }
}

/// The headline lines of a summary.
pub fn summary_lines(s: &LutrisSummary) -> Vec<SharedString> {
    let n = |v: usize| v.to_string();
    let mut lines = vec![t_args(
        "ui.lutris.summary.games",
        &[
            ("found", &n(s.found)),
            ("imported", &n(s.imported)),
            ("matched", &n(s.matched)),
        ],
    )];
    if s.favourites + s.categories + s.playtime_games > 0 {
        lines.push(t_args(
            "ui.lutris.summary.data",
            &[
                ("favourites", &n(s.favourites)),
                ("categories", &n(s.categories)),
                ("playtime", &n(s.playtime_games)),
            ],
        ));
    }
    lines
}

/// Show the summary of a finished import.
pub fn open_lutris_summary(summary: LutrisSummary, window: &mut Window, cx: &mut App) {
    let width = window.rem_size() * 36.;
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let section = |title: SharedString, rows: Vec<AnyElement>| {
            v_flex()
                .gap_1()
                .child(div().text_sm().font_semibold().child(title))
                .children(rows)
                .into_any_element()
        };
        let row = |title: &str, detail: SharedString| {
            v_flex()
                .child(div().text_sm().child(title.to_string()))
                .child(div().text_xs().text_color(muted).child(detail))
                .into_any_element()
        };
        let mut sections = Vec::new();
        if !summary.store_games.is_empty() {
            sections.push(section(
                t("ui.lutris.summary.store_games"),
                summary
                    .store_games
                    .iter()
                    .map(|(title, store)| {
                        row(
                            title,
                            t_args("ui.lutris.summary.log_in", &[("store", store)]),
                        )
                    })
                    .collect(),
            ));
        }
        if !summary.wine_missing.is_empty() {
            sections.push(section(
                t("ui.lutris.summary.wine_missing"),
                summary
                    .wine_missing
                    .iter()
                    .map(|(title, build)| {
                        row(
                            title,
                            t_args("ui.lutris.summary.wine_build", &[("build", build)]),
                        )
                    })
                    .collect(),
            ));
        }
        if !summary.skipped.is_empty() {
            sections.push(section(
                t("ui.lutris.summary.skipped"),
                summary
                    .skipped
                    .iter()
                    .map(|s| row(&s.title, skip_reason(s)))
                    .collect(),
            ));
        }
        if !summary.warnings.is_empty() {
            sections.push(section(
                t("ui.lutris.summary.warnings"),
                summary
                    .warnings
                    .iter()
                    .map(|w| {
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(w.clone())
                            .into_any_element()
                    })
                    .collect(),
            ));
        }
        dialog
            .title(t("ui.lutris.summary.title"))
            .w(width)
            .child(
                v_flex()
                    .id("lutris-summary")
                    .gap_4()
                    .max_h(rems(28.))
                    .overflow_y_scrollbar()
                    .children(
                        summary_lines(&summary)
                            .into_iter()
                            .map(|l| div().text_sm().child(l).into_any_element()),
                    )
                    .children(sections),
            )
            .footer(
                DialogFooter::new().child(
                    Button::new("lutris-summary-close")
                        .primary()
                        .label(t("ui.lutris.summary.close"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_text() {
        crate::i18n::register(crate::i18n::CATALOGS);
        for reason in [
            "notInstalled",
            "noRunner",
            "unknownRunner",
            "missingEntryPoint",
            "fileMissing",
            "missingCore",
            "unknownCore",
            "systemNotSupported",
            "steamNotFound",
            "storeNotSupported",
            "failed",
        ] {
            let text = skip_reason(&LutrisSkip {
                title: "Game".into(),
                reason: reason.into(),
                detail: Some("x".into()),
            });
            assert!(!text.starts_with("ui."), "{reason} has no text");
        }
    }
}
