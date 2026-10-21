//! Epic achievements from the launcher's GraphQL service: what a game offers
//! (public, by its namespace) and what the signed-in player unlocked.

use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use ember_core::Achievement;
use serde_json::Value;

use crate::EpicStore;
use crate::extra::{escape_gql, graphql, graphql_signed, store_locale};

/// The achievements a game offers, with the product id the player query needs.
fn definitions_query(namespace: &str, locale: &str) -> String {
    format!(
        "{{ Achievement {{ productAchievementsRecordBySandbox(sandboxId: \"{}\", locale: \"{}\") {{ \
         productId achievements {{ achievement {{ name hidden unlockedDisplayName lockedDisplayName \
         unlockedDescription lockedDescription unlockedIconLink lockedIconLink rarity {{ percent }} \
         }} }} }} }} }}",
        escape_gql(namespace),
        escape_gql(locale)
    )
}

fn player_query(account_id: &str, product_id: &str) -> String {
    format!(
        "{{ PlayerProfile {{ playerProfile(epicAccountId: \"{}\") {{ productAchievements(productId: \"{}\") {{ \
         __typename ... on PlayerProductAchievementsResponseSuccess {{ data {{ playerAchievements {{ \
         playerAchievement {{ achievementName unlocked unlockDate }} }} }} }} }} }} }} }}",
        escape_gql(account_id),
        escape_gql(product_id)
    )
}

/// The product id and achievements of a definitions answer; `None` for a
/// game without achievements.
pub fn parse_definitions(v: &Value) -> Option<(String, Vec<Achievement>)> {
    let record = v.pointer("/data/Achievement/productAchievementsRecordBySandbox")?;
    let product = record.get("productId")?.as_str()?.to_string();
    let s = |a: &Value, k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let list: Vec<Achievement> = record
        .get("achievements")?
        .as_array()?
        .iter()
        .filter_map(|entry| entry.get("achievement"))
        .map(|a| {
            let name = s(a, "unlockedDisplayName");
            let description = s(a, "unlockedDescription");
            Achievement {
                id: s(a, "name"),
                key: s(a, "name"),
                visible: !a.get("hidden").and_then(Value::as_bool).unwrap_or(false),
                name: if name.is_empty() {
                    s(a, "lockedDisplayName")
                } else {
                    name
                },
                description: if description.is_empty() {
                    s(a, "lockedDescription")
                } else {
                    description
                },
                image_unlocked: s(a, "unlockedIconLink"),
                image_locked: s(a, "lockedIconLink"),
                rarity: a
                    .pointer("/rarity/percent")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0) as f32,
                rarity_label: String::new(),
                unlocked_at: None,
            }
        })
        .collect();
    (!list.is_empty()).then_some((product, list))
}

/// Unlock dates by achievement name from a player answer.
pub fn parse_player(v: &Value) -> Result<HashMap<String, String>> {
    let progress = v
        .pointer("/data/PlayerProfile/playerProfile/productAchievements")
        .context("Epic sent no achievement progress")?;
    if progress.get("__typename").and_then(Value::as_str)
        != Some("PlayerProductAchievementsResponseSuccess")
    {
        bail!("Epic did not share your achievement progress");
    }
    // A player who never played the game has no record yet.
    let Some(list) = progress
        .pointer("/data/playerAchievements")
        .and_then(Value::as_array)
    else {
        return Ok(HashMap::new());
    };
    Ok(list
        .iter()
        .filter_map(|e| e.get("playerAchievement"))
        .filter(|a| a.get("unlocked").and_then(Value::as_bool) == Some(true))
        .filter_map(|a| {
            let name = a.get("achievementName")?.as_str()?.to_string();
            let date = a
                .get("unlockDate")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some((name, date))
        })
        .collect())
}

impl EpicStore {
    /// A game's achievements with the signed-in player's unlocks. Empty for
    /// games without achievements.
    pub(crate) async fn game_achievements(
        &self,
        app_name: &str,
        language: &str,
    ) -> Result<Vec<Achievement>> {
        let Some(namespace) = self
            .state
            .read()
            .games
            .get(app_name)
            .and_then(|g| g.namespace.clone())
        else {
            return Ok(Vec::new());
        };
        let defs = graphql(definitions_query(&namespace, &store_locale(language))).await?;
        let Some((product, mut list)) = parse_definitions(&defs) else {
            return Ok(Vec::new());
        };
        let session = self.session().await?;
        let player = graphql_signed(player_query(&session.account_id, &product), &session).await?;
        let unlocked = parse_player(&player)?;
        for a in &mut list {
            if let Some(date) = unlocked.get(&a.id) {
                // An unlock without a date still counts as unlocked.
                a.unlocked_at = Some(date.clone());
            }
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_definitions_and_unlocks() {
        let defs = json!({"data": {"Achievement": {"productAchievementsRecordBySandbox": {
        "productId": "0de2", "achievements": [
            {"achievement": {"name": "Act1", "hidden": true, "unlockedDisplayName": "The Wait",
             "lockedDisplayName": "???", "unlockedDescription": "Complete Act I",
             "lockedDescription": "", "unlockedIconLink": "https://u", "lockedIconLink": "https://l",
             "rarity": {"percent": 7}}},
            {"achievement": {"name": "Act2", "hidden": false, "unlockedDisplayName": "",
             "lockedDisplayName": "The Ascent", "unlockedDescription": "",
             "lockedDescription": "Complete Act II", "rarity": {"percent": 4.5}}}
        ]}}}});
        let (product, list) = parse_definitions(&defs).unwrap();
        assert_eq!(product, "0de2");
        assert_eq!(list[0].name, "The Wait");
        assert!(!list[0].visible);
        assert_eq!(list[0].rarity, 7.0);
        assert_eq!(list[1].name, "The Ascent");
        assert_eq!(list[1].description, "Complete Act II");

        let player = json!({"data": {"PlayerProfile": {"playerProfile": {"productAchievements": {
        "__typename": "PlayerProductAchievementsResponseSuccess",
        "data": {"playerAchievements": [
            {"playerAchievement": {"achievementName": "Act1", "unlocked": true,
              "unlockDate": "2022-05-22T07:39:26.283Z"}},
            {"playerAchievement": {"achievementName": "Act2", "unlocked": false}}
        ]}}}}}});
        let unlocked = parse_player(&player).unwrap();
        assert_eq!(unlocked.len(), 1);
        assert_eq!(unlocked["Act1"], "2022-05-22T07:39:26.283Z");
    }

    #[test]
    fn games_without_achievements_and_refusals() {
        let none = json!({"data": {"Achievement": {"productAchievementsRecordBySandbox":
            {"productId": null, "achievements": null}}}});
        assert!(parse_definitions(&none).is_none());
        let refused = json!({"data": {"PlayerProfile": {"playerProfile": {"productAchievements":
            {"__typename": "ServiceError"}}}}});
        assert!(parse_player(&refused).is_err());
        let never_played = json!({"data": {"PlayerProfile": {"playerProfile": {"productAchievements":
            {"__typename": "PlayerProductAchievementsResponseSuccess", "data": null}}}}});
        assert!(parse_player(&never_played).unwrap().is_empty());
    }
}
