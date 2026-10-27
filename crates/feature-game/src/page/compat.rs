//! Compatibility panel: ProtonDB, Steam Deck, anticheat, HowLongToBeat,
//! review scores and known fixes, from the installed compatibility provider.

use gpui_kit::assets::IconName;
use gpui_kit::component::link::Link;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    Styled, div, rems,
};
use irori_ui::i18n::{t, t_args};

use super::GamePage;
use crate::hooks::{AnticheatStatus, CompatibilityInfo, SteamDeckStatus};
use crate::load::Load;

fn row(label: SharedString, value: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .gap_3()
        .justify_between()
        .text_sm()
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().min_w_0().child(value))
        .into_any_element()
}

fn link(id: &'static str, url: &str, label: impl Into<SharedString>) -> AnyElement {
    Link::new(id)
        .href(url.to_string())
        .child(label.into())
        .into_any_element()
}

fn hours(h: Option<f32>) -> String {
    match h {
        Some(h) if h > 0.0 => {
            t_args("game.compat.hours", &[("hours", &format!("{h:.0}"))]).to_string()
        }
        _ => "–".into(),
    }
}

/// Short label of a ProtonDB tier ("gold" → "Gold").
pub(crate) fn tier_label(tier: &str) -> String {
    let mut c = tier.chars();
    match c.next() {
        Some(f) => f
            .to_uppercase()
            .chain(c.flat_map(|c| c.to_lowercase()))
            .collect(),
        None => String::new(),
    }
}

impl GamePage {
    /// Whether there is a Compatibility tab: data is loading, loaded or failed.
    pub(super) fn has_compat(&self) -> bool {
        !matches!(self.compat, Load::Idle)
    }

    /// Content of the Compatibility tab.
    pub(super) fn render_compat(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let note = |text: SharedString| div().text_sm().text_color(muted).child(text);
        let body = match &self.compat {
            Load::Idle => return None,
            Load::Loading => v_flex()
                .gap_2()
                .child(Skeleton::new().h_4().w_full())
                .child(Skeleton::new().h_4().w(rems(12.))),
            Load::Failed(e) => {
                v_flex().child(note(t_args("game.compat.failed", &[("error", e.as_ref())])))
            }
            Load::Ready(info) if info.is_empty() => v_flex().child(note(t("game.compat.empty"))),
            Load::Ready(info) => v_flex().gap_2().children(self.compat_rows(info, cx)),
        };
        Some(body.max_w(rems(40.)).into_any_element())
    }

    fn compat_rows(&self, info: &CompatibilityInfo, cx: &App) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        if let Some(url) = &info.wiki_link {
            rows.push(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        Icon::new(IconName::Info)
                            .small()
                            .text_color(cx.theme().info),
                    )
                    .child(link("compat-wiki", url, t("game.compat.wiki_notes")))
                    .into_any_element(),
            );
        }
        if let Some(tier) = &info.protondb_tier {
            let value: AnyElement = match &info.protondb_url {
                Some(url) => link("compat-protondb", url, tier_label(tier)),
                None => div().child(tier_label(tier)).into_any_element(),
            };
            rows.push(row("ProtonDB".into(), value, cx));
        } else if let Some(url) = &info.protondb_url {
            rows.push(row(
                "ProtonDB".into(),
                link("compat-protondb", url, t("game.compat.reports")),
                cx,
            ));
        }
        if let Some(deck) = info.steam_deck {
            let icon = match deck {
                SteamDeckStatus::Verified => IconName::CircleCheck,
                SteamDeckStatus::Playable => IconName::Info,
                SteamDeckStatus::Unsupported => IconName::Ban,
                SteamDeckStatus::Unknown => IconName::CircleQuestionMark,
            };
            rows.push(row(
                "Steam Deck".into(),
                h_flex()
                    .gap_1()
                    .child(Icon::new(icon).small())
                    .child(deck.label()),
                cx,
            ));
        }
        if let Some(ac) = &info.anticheat {
            let color = match ac.status {
                AnticheatStatus::Broken | AnticheatStatus::Denied => cx.theme().danger,
                AnticheatStatus::Supported | AnticheatStatus::Running => cx.theme().foreground,
                _ => cx.theme().warning,
            };
            let names = if ac.anticheats.is_empty() {
                t("game.compat.anticheat_removed").to_string()
            } else {
                ac.anticheats.join(", ")
            };
            rows.push(row(
                t("game.compat.anticheat"),
                div()
                    .text_color(color)
                    .child(format!("{} · {names}", ac.status.label())),
                cx,
            ));
            for (ix, note) in ac.notes.iter().enumerate().take(3) {
                rows.push(
                    div()
                        .id(("anticheat-note", ix))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note.clone())
                        .into_any_element(),
                );
            }
            if let Some(url) = &ac.reference {
                rows.push(
                    div()
                        .text_xs()
                        .child(link(
                            "compat-anticheat-ref",
                            url,
                            t("game.compat.anticheat_reference"),
                        ))
                        .into_any_element(),
                );
            }
        }
        if let Some(h) = &info.how_long_to_beat {
            let text = format!(
                "{} · {} · {}",
                hours(h.main),
                hours(h.main_extra),
                hours(h.completionist)
            );
            let value: AnyElement = match &h.url {
                Some(url) => link("compat-hltb", url, text),
                None => div().child(text).into_any_element(),
            };
            rows.push(row(t("game.compat.hltb"), value, cx));
            rows.push(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("game.compat.hltb_legend"))
                    .into_any_element(),
            );
        }
        if !info.scores.is_empty() {
            let mut line = h_flex().gap_3().flex_wrap();
            for (ix, s) in info.scores.iter().enumerate() {
                let text = format!("{} {}", s.source, s.score);
                line = line.child(match &s.url {
                    Some(url) => Link::new(("compat-score", ix))
                        .href(url.clone())
                        .child(text)
                        .into_any_element(),
                    None => div().child(text).into_any_element(),
                });
            }
            rows.push(row(t("game.compat.scores"), line, cx));
        }
        if let Some(url) = &info.pcgamingwiki_url {
            rows.push(row(
                "PCGamingWiki".into(),
                link("compat-pcgw", url, t("game.compat.open_page")),
                cx,
            ));
        }
        if !info.known_fixes.is_empty() {
            rows.push(
                v_flex()
                    .gap_1()
                    .pt_2()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(t("game.compat.known_fixes")),
                    )
                    .children(
                        info.known_fixes
                            .iter()
                            .enumerate()
                            .map(|(ix, f)| div().id(("known-fix", ix)).text_xs().child(f.clone())),
                    )
                    .into_any_element(),
            );
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_and_hours() {
        assert_eq!(tier_label("platinum"), "Platinum");
        assert_eq!(tier_label("GOLD"), "Gold");
        assert_eq!(tier_label(""), "");
        crate::i18n::register();
        assert_eq!(hours(Some(12.4)), "12 h");
        assert_eq!(hours(None), "–");
    }
}
