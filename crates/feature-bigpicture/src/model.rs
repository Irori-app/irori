//! Big Picture's derived data: shelves of games, the actions a game offers in
//! its overlay, the quick menu, and install defaults. Pure functions over
//! snapshots so they are tested without a window.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ember_core::{
    BIG_PICTURE_SCALES, GameId, GameInfo, InstallInfo, InstallPlatform, InstallRequest, Runner,
    StatusKind,
};
use ember_services::{LibrarySnapshot, UserData};
use ember_ui::i18n::{t, t_args};
use gpui_kit::SharedString;
use gpui_kit::assets::IconName;

use crate::text::store_name;

/// A shelf of games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKind {
    ContinuePlaying,
    Installed,
    Favourites,
    All,
    Store(Runner),
}

impl RowKind {
    /// Translated shelf heading.
    pub fn title(self) -> SharedString {
        match self {
            RowKind::ContinuePlaying => t("bigpicture.shelf.continue_playing"),
            RowKind::Installed => t("bigpicture.shelf.installed"),
            RowKind::Favourites => t("bigpicture.shelf.favourites"),
            RowKind::All => t("bigpicture.shelf.all"),
            RowKind::Store(runner) => store_name(runner),
        }
    }

    /// Stable element id prefix of the row.
    pub fn id(self) -> &'static str {
        match self {
            RowKind::ContinuePlaying => "continue",
            RowKind::Installed => "installed",
            RowKind::Favourites => "favourites",
            RowKind::All => "all",
            RowKind::Store(runner) => runner.id(),
        }
    }
}

/// One shelf and its games, in display order.
#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub games: Vec<Arc<GameInfo>>,
}

fn parse_key(key: &str) -> Option<GameId> {
    let (runner, app) = key.split_once(':')?;
    Some(GameId::new(Runner::from_id(runner)?, app))
}

/// Build the shelves. Hidden games are left out everywhere; empty shelves are
/// dropped; per-store shelves appear only when games come from more than one
/// store (otherwise they would repeat "All games").
pub fn build_rows(library: &LibrarySnapshot, user: &UserData, max_recent: usize) -> Vec<Row> {
    let visible: Vec<&Arc<GameInfo>> = library
        .games
        .iter()
        .filter(|g| !user.hidden.contains(&g.id().key()))
        .collect();
    let by_key: HashMap<String, &Arc<GameInfo>> =
        visible.iter().map(|g| (g.id().key(), *g)).collect();

    let recent: Vec<Arc<GameInfo>> = user
        .recent
        .iter()
        .filter(|k| parse_key(k).is_some())
        .filter_map(|k| by_key.get(k.as_str()).map(|g| (*g).clone()))
        .take(max_recent.max(1))
        .collect();
    let installed = visible
        .iter()
        .filter(|g| g.is_installed)
        .map(|g| (*g).clone())
        .collect();
    let favourites = visible
        .iter()
        .filter(|g| user.favourites.contains(&g.id().key()))
        .map(|g| (*g).clone())
        .collect();
    let all: Vec<Arc<GameInfo>> = visible.iter().map(|g| (*g).clone()).collect();

    let mut rows = vec![
        Row {
            kind: RowKind::ContinuePlaying,
            games: recent,
        },
        Row {
            kind: RowKind::Installed,
            games: installed,
        },
        Row {
            kind: RowKind::Favourites,
            games: favourites,
        },
    ];
    let stores: Vec<Runner> = Runner::ALL
        .into_iter()
        .filter(|r| all.iter().any(|g| g.runner() == *r))
        .collect();
    if stores.len() > 1 {
        for runner in &stores {
            rows.push(Row {
                kind: RowKind::Store(*runner),
                games: all
                    .iter()
                    .filter(|g| g.runner() == *runner)
                    .cloned()
                    .collect(),
            });
        }
    }
    rows.insert(
        3,
        Row {
            kind: RowKind::All,
            games: all,
        },
    );
    rows.retain(|r| !r.games.is_empty());
    rows
}

/// What the overlay needs to know about a game's current state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameState {
    pub installed: bool,
    pub installable: bool,
    pub playing: bool,
    /// Current operation, if any (installing, launching, verifying…).
    pub busy: Option<StatusKind>,
    /// Queued or downloading in the download queue.
    pub in_queue: bool,
    pub has_update: bool,
    pub favourite: bool,
}

/// A command in the game overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameAction {
    Play,
    Stop,
    Install,
    CancelDownload,
    Update,
    AddFavourite,
    RemoveFavourite,
    Uninstall,
}

impl GameAction {
    /// Translated button label.
    pub fn label(self) -> SharedString {
        t(match self {
            GameAction::Play => "bigpicture.action.play",
            GameAction::Stop => "bigpicture.action.stop",
            GameAction::Install => "bigpicture.action.install",
            GameAction::CancelDownload => "bigpicture.action.cancel_download",
            GameAction::Update => "bigpicture.action.update",
            GameAction::AddFavourite => "bigpicture.action.add_favourite",
            GameAction::RemoveFavourite => "bigpicture.action.remove_favourite",
            GameAction::Uninstall => "bigpicture.action.uninstall",
        })
    }

    pub fn icon(self) -> IconName {
        match self {
            GameAction::Play => IconName::Play,
            GameAction::Stop => IconName::CircleStop,
            GameAction::Install => IconName::Download,
            GameAction::CancelDownload => IconName::X,
            GameAction::Update => IconName::CircleArrowUp,
            GameAction::AddFavourite => IconName::Star,
            GameAction::RemoveFavourite => IconName::StarOff,
            GameAction::Uninstall => IconName::Trash,
        }
    }

    /// Stable element id of the action's button.
    pub fn id(self) -> &'static str {
        match self {
            GameAction::Play => "bp-action-play",
            GameAction::Stop => "bp-action-stop",
            GameAction::Install => "bp-action-install",
            GameAction::CancelDownload => "bp-action-cancel-download",
            GameAction::Update => "bp-action-update",
            GameAction::AddFavourite | GameAction::RemoveFavourite => "bp-action-favourite",
            GameAction::Uninstall => "bp-action-uninstall",
        }
    }
}

/// The overlay's commands for a game, main action first.
pub fn game_actions(state: GameState) -> Vec<GameAction> {
    let mut out = Vec::with_capacity(4);
    if state.playing {
        out.push(GameAction::Stop);
    } else if state.installed {
        out.push(GameAction::Play);
        if state.has_update && !state.in_queue {
            out.push(GameAction::Update);
        }
    } else if state.in_queue {
        out.push(GameAction::CancelDownload);
    } else {
        out.push(GameAction::Install);
    }
    if state.installed && state.in_queue {
        out.push(GameAction::CancelDownload);
    }
    out.push(if state.favourite {
        GameAction::RemoveFavourite
    } else {
        GameAction::AddFavourite
    });
    if state.installed && !state.playing {
        out.push(GameAction::Uninstall);
    }
    out
}

/// Whether `action` can run now, and if not, why (translated).
pub fn disabled_reason(action: GameAction, state: GameState) -> Option<SharedString> {
    let busy = state
        .busy
        .filter(|k| *k != StatusKind::Queued && *k != StatusKind::Playing);
    match action {
        GameAction::Play | GameAction::Update | GameAction::Uninstall if busy.is_some() => {
            Some(t("bigpicture.action.busy"))
        }
        GameAction::Install if !state.installable => Some(t("bigpicture.action.not_installable")),
        _ => None,
    }
}

/// Entries of the quick menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    PauseDownloads,
    ResumeDownloads,
    /// Cycles Big Picture's interface size; carries the current percent.
    InterfaceSize(u32),
    ToggleFullscreen,
    ExitBigPicture,
    QuitEmber,
}

impl MenuItem {
    /// Translated entry label.
    pub fn label(self) -> SharedString {
        if let MenuItem::InterfaceSize(percent) = self {
            return t_args(
                "bigpicture.menu.interface_size",
                &[("percent", percent.to_string().as_str())],
            );
        }
        t(match self {
            MenuItem::InterfaceSize(_) => unreachable!("handled above"),
            MenuItem::PauseDownloads => "bigpicture.menu.pause_downloads",
            MenuItem::ResumeDownloads => "bigpicture.menu.resume_downloads",
            MenuItem::ToggleFullscreen => "bigpicture.menu.toggle_fullscreen",
            MenuItem::ExitBigPicture => "bigpicture.exit",
            MenuItem::QuitEmber => "bigpicture.menu.quit",
        })
    }

    pub fn icon(self) -> IconName {
        match self {
            MenuItem::PauseDownloads => IconName::CircleStop,
            MenuItem::ResumeDownloads => IconName::Play,
            MenuItem::InterfaceSize(_) => IconName::ZoomIn,
            MenuItem::ToggleFullscreen => IconName::Maximize,
            MenuItem::ExitBigPicture => IconName::LogOut,
            MenuItem::QuitEmber => IconName::Power,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            MenuItem::PauseDownloads => "bp-menu-pause",
            MenuItem::ResumeDownloads => "bp-menu-resume",
            MenuItem::InterfaceSize(_) => "bp-menu-size",
            MenuItem::ToggleFullscreen => "bp-menu-fullscreen",
            MenuItem::ExitBigPicture => "bp-menu-exit",
            MenuItem::QuitEmber => "bp-menu-quit",
        }
    }
}

/// Quick menu entries given the download queue state and the interface
/// size in percent.
pub fn menu_items(has_downloads: bool, paused: bool, scale: u32) -> Vec<MenuItem> {
    let mut out = Vec::with_capacity(6);
    if has_downloads {
        out.push(if paused {
            MenuItem::ResumeDownloads
        } else {
            MenuItem::PauseDownloads
        });
    }
    out.extend([
        MenuItem::InterfaceSize(scale),
        MenuItem::ToggleFullscreen,
        MenuItem::ExitBigPicture,
        MenuItem::QuitEmber,
    ]);
    out
}

/// The interface size after `current` in [`BIG_PICTURE_SCALES`], wrapping
/// back to the smallest.
pub fn next_scale(current: u32) -> u32 {
    BIG_PICTURE_SCALES
        .iter()
        .copied()
        .find(|s| *s > current)
        .unwrap_or(BIG_PICTURE_SCALES[0])
}

/// Platform to install when the user picks no options (see
/// [`GameInfo::default_platform`]).
pub fn default_platform(game: &GameInfo) -> InstallPlatform {
    game.default_platform()
}

/// Install request with default options (see
/// [`InstallRequest::with_defaults`]).
pub fn default_install_request(
    info: &InstallInfo,
    install_dir: &Path,
    platform: InstallPlatform,
) -> InstallRequest {
    InstallRequest::with_defaults(info, install_dir, platform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::SdlOption;

    fn game(runner: Runner, app: &str, installed: bool) -> Arc<GameInfo> {
        Arc::new(
            GameInfo {
                runner: Some(runner),
                app_name: app.into(),
                title: app.to_uppercase(),
                is_installed: installed,
                installable: true,
                ..Default::default()
            }
            .finalize(),
        )
    }

    fn kinds(rows: &[Row]) -> Vec<RowKind> {
        rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn rows_skip_hidden_and_empty_shelves() {
        let mut lib = LibrarySnapshot::default();
        lib.games = vec![
            game(Runner::Gog, "a", true),
            game(Runner::Gog, "b", false),
            game(Runner::Gog, "c", true),
        ];
        let mut user = UserData::default();
        user.hidden.insert("gog:c".into());
        user.recent = vec!["gog:c".into(), "gog:b".into(), "bogus".into()];
        let rows = build_rows(&lib, &user, 8);
        assert_eq!(
            kinds(&rows),
            vec![RowKind::ContinuePlaying, RowKind::Installed, RowKind::All]
        );
        assert_eq!(rows[0].games[0].app_name, "b");
        assert_eq!(rows[1].games.len(), 1);
        assert_eq!(rows[2].games.len(), 2);
    }

    #[test]
    fn store_rows_only_with_several_stores() {
        let mut lib = LibrarySnapshot::default();
        lib.games = vec![
            game(Runner::Gog, "a", false),
            game(Runner::Legendary, "b", false),
        ];
        let mut user = UserData::default();
        user.favourites.insert("legendary:b".into());
        let rows = build_rows(&lib, &user, 8);
        assert_eq!(
            kinds(&rows),
            vec![
                RowKind::Favourites,
                RowKind::All,
                RowKind::Store(Runner::Legendary),
                RowKind::Store(Runner::Gog)
            ]
        );
    }

    #[test]
    fn overlay_actions_follow_state() {
        let installed = GameState {
            installed: true,
            installable: true,
            has_update: true,
            ..Default::default()
        };
        assert_eq!(
            game_actions(installed),
            vec![
                GameAction::Play,
                GameAction::Update,
                GameAction::AddFavourite,
                GameAction::Uninstall
            ]
        );
        let playing = GameState {
            playing: true,
            favourite: true,
            ..installed
        };
        assert_eq!(
            game_actions(playing),
            vec![GameAction::Stop, GameAction::RemoveFavourite]
        );
        let queued = GameState {
            in_queue: true,
            installable: true,
            busy: Some(StatusKind::Queued),
            ..Default::default()
        };
        assert_eq!(
            game_actions(queued),
            vec![GameAction::CancelDownload, GameAction::AddFavourite]
        );
        let verifying = GameState {
            busy: Some(StatusKind::Repairing),
            ..installed
        };
        assert!(disabled_reason(GameAction::Play, verifying).is_some());
        assert!(disabled_reason(GameAction::AddFavourite, verifying).is_none());
        let browser_only = GameState::default();
        assert!(disabled_reason(GameAction::Install, browser_only).is_some());
    }

    #[test]
    fn menu_offers_download_control_only_with_downloads() {
        assert_eq!(
            menu_items(false, false, 100)[0],
            MenuItem::InterfaceSize(100)
        );
        assert_eq!(menu_items(true, false, 100)[0], MenuItem::PauseDownloads);
        assert_eq!(menu_items(true, true, 150)[0], MenuItem::ResumeDownloads);
        assert_eq!(menu_items(true, true, 150)[1], MenuItem::InterfaceSize(150));
    }

    #[test]
    fn interface_size_cycles_through_the_offered_scales() {
        assert_eq!(next_scale(100), 125);
        assert_eq!(next_scale(175), 200);
        assert_eq!(next_scale(200), 100);
        assert_eq!(next_scale(130), 150);
        assert_eq!(next_scale(0), 100);
    }

    #[test]
    fn install_defaults() {
        let mut g = GameInfo {
            platforms: vec![InstallPlatform::Windows, InstallPlatform::Linux],
            ..Default::default()
        };
        assert_eq!(
            default_platform(&g),
            if cfg!(target_os = "linux") {
                InstallPlatform::Linux
            } else {
                InstallPlatform::Windows
            }
        );
        g.platforms = vec![InstallPlatform::Windows];
        assert_eq!(default_platform(&g), InstallPlatform::Windows);
        g.platforms.clear();
        g.is_linux_native = true;
        assert_eq!(
            default_platform(&g),
            if cfg!(target_os = "linux") {
                InstallPlatform::Linux
            } else {
                InstallPlatform::Windows
            }
        );

        let info = InstallInfo {
            sdl: vec![
                SdlOption {
                    tags: vec!["core".into()],
                    mandatory: true,
                    ..Default::default()
                },
                SdlOption {
                    tags: vec!["hd".into()],
                    default_selected: true,
                    ..Default::default()
                },
                SdlOption {
                    tags: vec!["extra".into()],
                    ..Default::default()
                },
            ],
            dependencies: vec!["vcredist".into()],
            ..Default::default()
        };
        let req = default_install_request(&info, Path::new("/games"), InstallPlatform::Linux);
        assert_eq!(req.sdl, vec!["core", "hd"]);
        assert_eq!(req.dependencies, vec!["vcredist"]);
        assert_eq!(req.path, Path::new("/games"));
        assert!(req.dlcs.is_empty());
    }
}
