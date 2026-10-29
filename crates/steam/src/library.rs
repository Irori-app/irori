//! The account's games: owned games (`IPlayerService/GetOwnedGames`), the
//! Steam Family library shared with it (`IFamilyGroupsService`), and store
//! details of one app (`store.steampowered.com/api/appdetails`).
//!
//! Steam writes 64-bit ids as strings and is loose about some numbers, so
//! those fields accept either form.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::api::Client;

/// A game the account owns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OwnedGame {
    pub appid: u32,
    #[serde(default)]
    pub name: String,
    /// Minutes played in total.
    #[serde(default)]
    pub playtime_forever: u64,
    /// Minutes played in the last two weeks.
    #[serde(default)]
    pub playtime_2weeks: u64,
    #[serde(default)]
    pub playtime_windows_forever: u64,
    #[serde(default)]
    pub playtime_mac_forever: u64,
    #[serde(default)]
    pub playtime_linux_forever: u64,
    #[serde(default)]
    pub playtime_deck_forever: u64,
    /// Unix time of the last session, 0 if never played.
    #[serde(default)]
    pub rtime_last_played: u64,
    /// Hash naming the icon on Steam's CDN.
    #[serde(default)]
    pub img_icon_url: String,
    /// Capsule art file name on Steam's CDN.
    #[serde(default)]
    pub capsule_filename: String,
    #[serde(default)]
    pub has_dlc: bool,
    #[serde(default)]
    pub has_workshop: bool,
}

/// A game another member of the account's Steam Family shares with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SharedApp {
    pub appid: u32,
    pub name: String,
    /// Steam ids of the members who own it.
    #[serde(deserialize_with = "strings")]
    pub owner_steamids: Vec<String>,
    pub capsule_filename: String,
    pub img_icon_hash: String,
    /// Why Steam won't share it (0 when it does).
    pub exclude_reason: i32,
    pub rt_time_acquired: u64,
    pub rt_last_played: u64,
    /// Minutes played.
    pub rt_playtime: u64,
    /// `EProtoAppType` (1 is a game).
    pub app_type: i32,
}

/// What the store page says about an app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AppDetails {
    pub name: String,
    pub short_description: String,
    /// HTML.
    pub about_the_game: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub platforms: Platforms,
    pub release_date: ReleaseDate,
    pub genres: Vec<Genre>,
    pub header_image: String,
    #[serde(deserialize_with = "lenient_u64")]
    pub required_age: u64,
    /// `full` or `partial`, when the game supports controllers.
    pub controller_support: Option<String>,
    pub metacritic: Option<Metacritic>,
    pub screenshots: Vec<Screenshot>,
    /// Background art without the store's tint.
    pub background_raw: String,
    /// HTML, with the languages that have full audio marked.
    pub supported_languages: String,
    #[serde(deserialize_with = "requirements")]
    pub pc_requirements: Requirements,
    #[serde(deserialize_with = "requirements")]
    pub mac_requirements: Requirements,
    #[serde(deserialize_with = "requirements")]
    pub linux_requirements: Requirements,
}

impl AppDetails {
    /// The long description (HTML) when there is one, else the short one.
    pub fn description(&self) -> &str {
        if self.about_the_game.trim().is_empty() {
            &self.short_description
        } else {
            &self.about_the_game
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Screenshot {
    pub path_thumbnail: String,
    pub path_full: String,
}

/// System requirements for one operating system, as HTML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Requirements {
    pub minimum: Option<String>,
    pub recommended: Option<String>,
}

/// Operating systems an app runs on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Platforms {
    pub windows: bool,
    pub mac: bool,
    pub linux: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ReleaseDate {
    pub coming_soon: bool,
    /// As the store shows it, in the requested language ("21 Aug, 2012").
    pub date: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Genre {
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Metacritic {
    pub score: u32,
}

/// The `{"response": …}` wrapper of Web API answers.
#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    response: Option<T>,
}

impl Client {
    /// The account's games, with playtime. An access token that may not see
    /// the games gets an empty list (Steam answers `{"response": {}}`).
    pub async fn owned_games(&self, access_token: &str, steamid: u64) -> Result<Vec<OwnedGame>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Owned {
            games: Vec<OwnedGame>,
        }
        let url = format!("{}/IPlayerService/GetOwnedGames/v1/", self.endpoints().api);
        let query = [
            ("access_token", access_token.to_string()),
            ("steamid", steamid.to_string()),
            ("include_appinfo", "1".into()),
            ("include_played_free_games", "1".into()),
            ("include_extended_appinfo", "1".into()),
            ("skip_unvetted_apps", "0".into()),
            ("format", "json".into()),
        ];
        let answer: Envelope<Owned> = self
            .get_json(url, &query, "the list of owned games")
            .await?;
        Ok(answer.response.unwrap_or_default().games)
    }

    /// The Steam Family the account belongs to, if any.
    pub async fn family_group(&self, access_token: &str, steamid: u64) -> Result<Option<u64>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Group {
            #[serde(deserialize_with = "lenient_u64")]
            family_groupid: u64,
            is_not_member_of_any_group: bool,
        }
        let url = format!(
            "{}/IFamilyGroupsService/GetFamilyGroupForUser/v1/",
            self.endpoints().api
        );
        let query = [
            ("access_token", access_token.to_string()),
            ("steamid", steamid.to_string()),
        ];
        let answer: Envelope<Group> = self.get_json(url, &query, "the Steam Family").await?;
        let group = answer.response.unwrap_or_default();
        Ok(
            (!group.is_not_member_of_any_group && group.family_groupid != 0)
                .then_some(group.family_groupid),
        )
    }

    /// Games other members of the Steam Family share with the account (its
    /// own games, free games and non-games left out).
    pub async fn family_shared_apps(
        &self,
        access_token: &str,
        family_groupid: u64,
        steamid: u64,
    ) -> Result<Vec<SharedApp>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Shared {
            apps: Vec<SharedApp>,
        }
        let url = format!(
            "{}/IFamilyGroupsService/GetSharedLibraryApps/v1/",
            self.endpoints().api
        );
        let query = [
            ("access_token", access_token.to_string()),
            ("family_groupid", family_groupid.to_string()),
            ("steamid", steamid.to_string()),
            ("include_own", "false".into()),
            ("include_excluded", "false".into()),
            ("include_free", "false".into()),
            ("include_non_games", "false".into()),
        ];
        let answer: Envelope<Shared> = self
            .get_json(url, &query, "the Steam Family library")
            .await?;
        Ok(answer.response.unwrap_or_default().apps)
    }

    /// Store details of an app in `language` (Steam's language names:
    /// `english`, `german`, `schinese`…). `None` when the store has no page
    /// for it in this region.
    pub async fn app_details(&self, appid: u32, language: &str) -> Result<Option<AppDetails>> {
        let url = format!("{}/api/appdetails", self.endpoints().store);
        let query = [("appids", appid.to_string()), ("l", language.to_string())];
        let answer: Value = self
            .get_json(url, &query, "the game's store details")
            .await?;
        let Some(entry) = answer.get(appid.to_string()) else {
            return Ok(None);
        };
        if entry.get("success").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        match entry.get("data") {
            Some(data) if data.is_object() => serde_json::from_value(data.clone())
                .map(Some)
                .context("Steam sent unexpected store details for the game"),
            _ => Ok(None),
        }
    }
}

/// A number written as a number or a string; anything else is 0.
pub(crate) fn lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().unwrap_or_default(),
        Value::String(s) => s.trim().parse().unwrap_or_default(),
        _ => 0,
    })
}

/// Requirements: an object, or `[]` when the store lists none.
fn requirements<'de, D: Deserializer<'de>>(d: D) -> Result<Requirements, D::Error> {
    Ok(match Value::deserialize(d)? {
        v @ Value::Object(_) => serde_json::from_value(v).unwrap_or_default(),
        _ => Requirements::default(),
    })
}

/// A list of ids written as strings or numbers.
fn strings<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Endpoints;
    use crate::error::SteamError;
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    const STEAMID: u64 = 76561197960265729;

    fn client(server: &TestServer) -> Client {
        Client::with_endpoints(Endpoints {
            api: server.base(),
            store: server.base(),
            assets: server.base(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn reads_owned_games() {
        let server = TestServer::start(|req| {
            if !req.path.starts_with("/IPlayerService/GetOwnedGames/v1/?") {
                return Resp::status(404);
            }
            if req.path.contains("access_token=hidden") {
                return Resp::json(json!({"response": {}}));
            }
            if req.path.contains("access_token=revoked") {
                return Resp::status(401);
            }
            Resp::json(json!({"response": {
                "game_count": 2,
                "games": [
                    {
                        "appid": 220,
                        "name": "Half-Life 2",
                        "playtime_forever": 612,
                        "playtime_2weeks": 30,
                        "img_icon_url": "fcfb366051782b8ebf2aa297f3b746395858cb62",
                        "has_community_visible_stats": true,
                        "playtime_windows_forever": 600,
                        "playtime_mac_forever": 0,
                        "playtime_linux_forever": 12,
                        "playtime_deck_forever": 5,
                        "rtime_last_played": 1_700_000_000u64,
                        "capsule_filename": "library_600x900.jpg",
                        "has_workshop": false,
                        "has_market": false,
                        "has_dlc": true,
                        "content_descriptorids": [2, 5]
                    },
                    {"appid": 10}
                ]
            }}))
        });
        let api = client(&server);
        let games = api.owned_games("token", STEAMID).await.unwrap();
        assert_eq!(games.len(), 2);
        let hl2 = &games[0];
        assert_eq!((hl2.appid, hl2.name.as_str()), (220, "Half-Life 2"));
        assert_eq!((hl2.playtime_forever, hl2.playtime_2weeks), (612, 30));
        assert_eq!(hl2.playtime_linux_forever, 12);
        assert_eq!(hl2.playtime_deck_forever, 5);
        assert_eq!(hl2.rtime_last_played, 1_700_000_000);
        assert_eq!(hl2.capsule_filename, "library_600x900.jpg");
        assert!(hl2.has_dlc && !hl2.has_workshop);
        assert_eq!(
            games[1],
            OwnedGame {
                appid: 10,
                ..Default::default()
            }
        );
        let path = &server.requests()[0].path;
        for part in [
            "access_token=token",
            &format!("steamid={STEAMID}"),
            "include_appinfo=1",
            "include_played_free_games=1",
            "include_extended_appinfo=1",
            "skip_unvetted_apps=0",
            "format=json",
        ] {
            assert!(path.contains(part), "{path} lacks {part}");
        }

        assert!(api.owned_games("hidden", STEAMID).await.unwrap().is_empty());
        let e = api.owned_games("revoked", STEAMID).await.unwrap_err();
        assert_eq!(e.downcast_ref(), Some(&SteamError::SessionRejected));
        assert!(!format!("{e:#}").contains("revoked"), "{e:#}");
    }

    #[tokio::test]
    async fn reads_the_family_library() {
        let server = TestServer::start(|req| {
            let path = req.path.as_str();
            if path.starts_with("/IFamilyGroupsService/GetFamilyGroupForUser/v1/?") {
                if path.contains("steamid=1&") || path.ends_with("steamid=1") {
                    Resp::json(json!({"response": {"is_not_member_of_any_group": true}}))
                } else {
                    Resp::json(json!({"response": {
                        "family_groupid": "4242",
                        "is_not_member_of_any_group": false,
                        "role": 1
                    }}))
                }
            } else if path.starts_with("/IFamilyGroupsService/GetSharedLibraryApps/v1/?") {
                Resp::json(json!({"response": {
                    "apps": [
                        {
                            "appid": 570,
                            "owner_steamids": ["76561197960265731", 76561197960265732u64],
                            "name": "Portal",
                            "capsule_filename": "capsule.jpg",
                            "img_icon_hash": "abc",
                            "exclude_reason": 0,
                            "rt_time_acquired": 1_600_000_000u64,
                            "rt_last_played": 1_650_000_000u64,
                            "rt_playtime": 90,
                            "app_type": 1,
                            "content_descriptors": []
                        },
                        {"appid": 400}
                    ],
                    "owner_steamid": "76561197960265729"
                }}))
            } else {
                Resp::status(404)
            }
        });
        let api = client(&server);
        assert_eq!(api.family_group("t", STEAMID).await.unwrap(), Some(4242));
        assert_eq!(api.family_group("t", 1).await.unwrap(), None);
        let apps = api.family_shared_apps("t", 4242, STEAMID).await.unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Portal");
        assert_eq!(
            apps[0].owner_steamids,
            ["76561197960265731", "76561197960265732"]
        );
        assert_eq!(apps[0].rt_playtime, 90);
        assert_eq!(apps[0].app_type, 1);
        assert_eq!(apps[1].appid, 400);
        let shared = server
            .requests()
            .into_iter()
            .find(|r| r.path.contains("GetSharedLibraryApps"))
            .unwrap();
        for part in [
            "family_groupid=4242",
            "include_own=false",
            "include_excluded=false",
            "include_free=false",
            "include_non_games=false",
        ] {
            assert!(shared.path.contains(part), "{} lacks {part}", shared.path);
        }
    }

    #[tokio::test]
    async fn reads_app_details() {
        let server = TestServer::start(|req| {
            let path = req.path.as_str();
            if path == "/api/appdetails?appids=220&l=english" {
                Resp::json(json!({"220": {"success": true, "data": {
                    "type": "game",
                    "name": "Half-Life 2",
                    "steam_appid": 220,
                    "required_age": "16",
                    "short_description": "The sequel.",
                    "about_the_game": "<p>Long text</p>",
                    "header_image": "https://cdn.example/header.jpg",
                    "developers": ["Valve"],
                    "publishers": ["Valve"],
                    "platforms": {"windows": true, "mac": false, "linux": true},
                    "metacritic": {"score": 96, "url": "https://example"},
                    "controller_support": "full",
                    "genres": [{"id": "1", "description": "Action"}],
                    "release_date": {"coming_soon": false, "date": "16 Nov, 2004"},
                    "screenshots": [
                        {"id": 0, "path_thumbnail": "https://cdn.example/s0.116x65.jpg",
                         "path_full": "https://cdn.example/s0.1920x1080.jpg"}
                    ],
                    "background_raw": "https://cdn.example/page_bg_raw.jpg",
                    "supported_languages": "English<strong>*</strong>, German",
                    "pc_requirements": {"minimum": "<ul>1.2 GHz</ul>", "recommended": "<ul>2 GHz</ul>"},
                    "mac_requirements": [],
                    "linux_requirements": {"minimum": "<ul>Ubuntu</ul>"}
                }}}))
            } else if path == "/api/appdetails?appids=4&l=english" {
                Resp::json(json!({"4": {"success": true, "data": {
                    "name": "Short",
                    "short_description": "Only short.",
                    "about_the_game": ""
                }}}))
            } else if path == "/api/appdetails?appids=1&l=english" {
                Resp::json(json!({"1": {"success": false}}))
            } else if path == "/api/appdetails?appids=2&l=english" {
                Resp::json(json!(null))
            } else if path == "/api/appdetails?appids=3&l=english" {
                Resp::status(429)
            } else {
                Resp::status(404)
            }
        });
        let api = client(&server);
        let d = api.app_details(220, "english").await.unwrap().unwrap();
        assert_eq!(d.name, "Half-Life 2");
        assert_eq!(d.required_age, 16);
        assert_eq!(d.developers, ["Valve"]);
        assert_eq!(
            d.platforms,
            Platforms {
                windows: true,
                mac: false,
                linux: true
            }
        );
        assert_eq!(d.genres[0].description, "Action");
        assert_eq!(d.release_date.date, "16 Nov, 2004");
        assert_eq!(d.controller_support.as_deref(), Some("full"));
        assert_eq!(d.metacritic.as_ref().map(|m| m.score), Some(96));
        assert!(d.description().contains("Long text"));
        assert_eq!(
            d.screenshots[0].path_full,
            "https://cdn.example/s0.1920x1080.jpg"
        );
        assert_eq!(d.background_raw, "https://cdn.example/page_bg_raw.jpg");
        assert!(d.supported_languages.starts_with("English"));
        assert_eq!(
            d.pc_requirements.recommended.as_deref(),
            Some("<ul>2 GHz</ul>")
        );
        assert_eq!(d.mac_requirements, Requirements::default());
        assert_eq!(
            d.linux_requirements.minimum.as_deref(),
            Some("<ul>Ubuntu</ul>")
        );
        assert_eq!(d.linux_requirements.recommended, None);
        let short = api.app_details(4, "english").await.unwrap().unwrap();
        assert_eq!(short.description(), "Only short.");
        assert_eq!(api.app_details(1, "english").await.unwrap(), None);
        assert_eq!(api.app_details(2, "english").await.unwrap(), None);
        let e = api.app_details(3, "english").await.unwrap_err();
        assert_eq!(e.downcast_ref(), Some(&SteamError::RateLimited));
    }
}
