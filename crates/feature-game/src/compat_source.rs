//! Built-in compatibility source: PCGamingWiki/HLTB/ProtonDB/Steam Deck via
//! `irori_extras::wiki`, AreWeAntiCheatYet via `irori_extras::anticheat`,
//! and Heroic's known fixes via `irori_extras::known_fixes`.

use std::sync::Arc;

use anyhow::Result;
use irori_core::{GameInfo, Runner};
use irori_extras::anticheat::{self, AntiCheatInfo, AntiCheatQuery, AntiCheatStatus};
use irori_extras::known_fixes::{self, KnownFix};
use irori_extras::wiki::{self, GameScore, ProtonDbTier, SteamDeckCompat, StoreIds, WikiInfo};

use irori_ui::i18n::t_args;

use crate::hooks::{
    AnticheatReport, AnticheatStatus, CompatibilityInfo, HowLongToBeat, ReviewScore,
    SteamDeckStatus,
};

/// Gather every report for a game. Sources fail independently; the result
/// is an error only when nothing could be loaded at all.
pub(crate) async fn load(game: Arc<GameInfo>) -> Result<CompatibilityInfo> {
    let ids = StoreIds::of(&game);
    let wiki = wiki::game_info(&game.title, &ids).await;
    let steam_id = wiki
        .as_ref()
        .ok()
        .and_then(|w| w.steam_app_id().map(str::to_owned));
    let query = AntiCheatQuery {
        title: &game.title,
        epic_namespace: game.namespace.as_deref().filter(|n| !n.is_empty()),
        steam_app_id: steam_id.as_deref(),
    };
    let ac = if game.runner() == Runner::Sideload {
        None
    } else {
        anticheat::lookup(&query).await
    };
    let fix = known_fixes::lookup(game.runner(), &game.app_name).await;
    if let Err(e) = &wiki
        && ac.is_none()
        && fix.is_none()
    {
        anyhow::bail!("{e:#}");
    }
    Ok(assemble(
        &game.title,
        wiki.ok().as_deref(),
        ac.as_ref(),
        fix.as_ref(),
    ))
}

fn score(source: &str, s: &GameScore, url: impl Fn(&str) -> String) -> Option<ReviewScore> {
    let value: f32 = s.score.trim().parse().ok()?;
    let value = value.round().clamp(0.0, 100.0) as u8;
    let id = s.url_id.trim();
    Some(ReviewScore::new(
        source,
        value,
        (!id.is_empty()).then(|| url(id)),
    ))
}

fn deck(d: SteamDeckCompat) -> SteamDeckStatus {
    match d {
        SteamDeckCompat::Verified => SteamDeckStatus::Verified,
        SteamDeckCompat::Playable => SteamDeckStatus::Playable,
        SteamDeckCompat::Unsupported => SteamDeckStatus::Unsupported,
        SteamDeckCompat::Unknown => SteamDeckStatus::Unknown,
    }
}

fn status(s: AntiCheatStatus) -> AnticheatStatus {
    match s {
        AntiCheatStatus::Supported => AnticheatStatus::Supported,
        AntiCheatStatus::Running | AntiCheatStatus::Working => AnticheatStatus::Running,
        AntiCheatStatus::Planned => AnticheatStatus::Planned,
        AntiCheatStatus::Broken => AnticheatStatus::Broken,
        AntiCheatStatus::Denied => AnticheatStatus::Denied,
        AntiCheatStatus::Unknown => AnticheatStatus::Unknown,
    }
}

fn hours(h: u32) -> Option<f32> {
    (h > 0).then_some(h as f32)
}

/// Map the extras data into the panel model.
pub(crate) fn assemble(
    title: &str,
    wiki: Option<&WikiInfo>,
    ac: Option<&AntiCheatInfo>,
    fix: Option<&KnownFix>,
) -> CompatibilityInfo {
    let mut info = CompatibilityInfo::default();
    if let Some(w) = wiki {
        let steam = w.steam.as_ref();
        info.protondb_tier = steam
            .and_then(|s| s.protondb)
            .filter(|t| *t != ProtonDbTier::Unknown)
            .map(|t| t.label().to_string());
        info.protondb_url = Some(match w.steam_app_id() {
            Some(id) => format!("https://www.protondb.com/app/{id}"),
            None => format!(
                "https://www.protondb.com/search?q={}",
                url_query(&wiki::clean_title(title))
            ),
        });
        info.steam_deck = steam.and_then(|s| s.steam_deck).map(deck);
        if let Some(h) = &w.howlongtobeat {
            let hltb = HowLongToBeat {
                main: hours(h.main_story),
                main_extra: hours(h.main_extra),
                completionist: hours(h.completionist),
                url: h.game_web_link.clone().filter(|u| !u.is_empty()),
            };
            if hltb.main.is_some() || hltb.main_extra.is_some() || hltb.completionist.is_some() {
                info.how_long_to_beat = Some(hltb);
            }
        }
        if let Some(p) = &w.pcgamingwiki {
            if p.page_id > 0 {
                info.pcgamingwiki_url = Some(p.page_url());
            }
            info.scores = [
                score("Metacritic", &p.metacritic, |id| {
                    format!("https://www.metacritic.com/game/pc/{id}")
                }),
                score("OpenCritic", &p.opencritic, |id| {
                    format!("https://opencritic.com/game/{id}")
                }),
                // IGDB links use IGDB's own ID (Heroic used Metacritic's).
                score("IGDB", &p.igdb, |id| {
                    format!("https://www.igdb.com/games/{id}")
                }),
            ]
            .into_iter()
            .flatten()
            .collect();
        }
    }
    if let Some(a) = ac {
        let mut report = AnticheatReport::new(status(a.status));
        report.anticheats = a.anticheats.clone();
        report.notes = a
            .notes
            .iter()
            .map(|n| n.text.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        report.reference = a
            .updates
            .last()
            .map(|u| u.reference.clone())
            .filter(|r| !r.is_empty())
            .or_else(|| (!a.reference.is_empty()).then(|| a.reference.clone()));
        info.anticheat = Some(report);
    }
    if let Some(f) = fix {
        info.wiki_link = f.wiki_link.clone().filter(|l| !l.is_empty());
        info.known_fixes.extend(f.notes.values().cloned());
        if !f.winetricks.is_empty() {
            info.known_fixes
                .push(format!("Winetricks: {}", f.winetricks.join(", ")));
        }
        if !f.run_in_prefix.is_empty() {
            info.known_fixes.push(
                t_args(
                    "game.compat.fix_run_in_prefix",
                    &[("programs", &f.run_in_prefix.join(", "))],
                )
                .to_string(),
            );
        }
        if !f.env_variables.is_empty() {
            let env: Vec<String> = f
                .env_variables
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            info.known_fixes.push(
                t_args(
                    "game.compat.fix_environment",
                    &[("variables", &env.join(" "))],
                )
                .to_string(),
            );
        }
    }
    info
}

fn url_query(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || "-_.".contains(c) => c.to_string(),
            c => c.to_string().bytes().map(|b| format!("%{b:02X}")).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_extras::wiki::{PcGamingWikiInfo, SteamCompat};

    #[test]
    fn maps_wiki_anticheat_and_fixes() {
        let wiki = WikiInfo {
            pcgamingwiki: Some(PcGamingWikiInfo {
                page_id: 42,
                steam_id: "570".into(),
                metacritic: GameScore {
                    score: "90".into(),
                    url_id: "dota-2".into(),
                },
                igdb: GameScore {
                    score: "85.6".into(),
                    url_id: "dota-2-igdb".into(),
                },
                ..Default::default()
            }),
            steam: Some(SteamCompat {
                protondb: Some(ProtonDbTier::Gold),
                steam_deck: Some(SteamDeckCompat::Verified),
            }),
            ..Default::default()
        };
        let ac = AntiCheatInfo {
            status: AntiCheatStatus::Denied,
            anticheats: vec!["BattlEye".into()],
            reference: "https://example.org/ref".into(),
            ..Default::default()
        };
        let fix = KnownFix {
            winetricks: vec!["vcrun2019".into()],
            wiki_link: Some("https://example.org/wiki".into()),
            ..Default::default()
        };
        let info = assemble("Dota 2", Some(&wiki), Some(&ac), Some(&fix));
        assert_eq!(info.protondb_tier.as_deref(), Some("Gold"));
        assert_eq!(
            info.protondb_url.as_deref(),
            Some("https://www.protondb.com/app/570")
        );
        assert_eq!(info.steam_deck, Some(SteamDeckStatus::Verified));
        assert_eq!(info.scores.len(), 2);
        assert_eq!(info.scores[1].score, 86);
        assert_eq!(
            info.scores[1].url.as_deref(),
            Some("https://www.igdb.com/games/dota-2-igdb")
        );
        let report = info.anticheat.as_ref().unwrap();
        assert!(report.status.blocks_multiplayer());
        assert_eq!(report.reference.as_deref(), Some("https://example.org/ref"));
        assert_eq!(info.known_fixes, vec!["Winetricks: vcrun2019".to_string()]);
        assert_eq!(info.wiki_link.as_deref(), Some("https://example.org/wiki"));
        assert!(assemble("x", None, None, None).is_empty());
    }

    #[test]
    fn protondb_search_without_steam_id() {
        let info = assemble("Hades: Deluxe", Some(&WikiInfo::default()), None, None);
        assert_eq!(
            info.protondb_url.as_deref(),
            Some("https://www.protondb.com/search?q=Hades+Deluxe")
        );
    }
}
