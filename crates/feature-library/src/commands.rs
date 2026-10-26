//! The game commands the library offers and the policy deciding which apply
//! to a game in its current state. Tiles, list rows, the "…" menu, the context
//! menu and keyboard shortcuts all read this one policy, so they cannot
//! disagree about what a game can do.

use ember_core::StatusKind;
use ember_ui::i18n::t;
use gpui_kit::assets::IconName;

/// A command on one game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameCommand {
    Play,
    Stop,
    /// Open the install sheet (owned by the game feature).
    Install,
    Update,
    RemoveFromQueue,
    OpenPage,
    Settings,
    Logs,
    ToggleFavourite,
    ToggleHidden,
    ToggleCategory(String),
    NewCategory,
    Edit,
    RemoveFromRecent,
    AddShortcut,
    RemoveShortcut,
    /// Add a Steam shortcut, installed or not (Steam starts Ember, which
    /// offers to install a game that isn't installed yet).
    AddToSteam,
    OpenFolder,
    Verify,
    /// Open the uninstall confirmation (owned by the game feature).
    Uninstall,
}

/// Facts about a game that decide which commands apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameState {
    pub installed: bool,
    pub installable: bool,
    pub sideload: bool,
    /// A web app: no settings, logs or files.
    pub browser: bool,
    /// Another launcher (Steam) installs and runs it: Play, page, library
    /// and folder commands only.
    pub external: bool,
    pub status: Option<StatusKind>,
    pub has_update: bool,
    pub favourite: bool,
    pub hidden: bool,
    pub has_install_path: bool,
    /// The game is listed in "Continue playing".
    pub in_recent: bool,
    /// A launch request from this view is in flight.
    pub launching: bool,
    /// Whether a desktop or app-menu shortcut exists; `None` while unknown.
    pub shortcut: Option<bool>,
    /// Installed, but its files were not found.
    pub unavailable: bool,
}

impl GameState {
    pub fn is_playing(&self) -> bool {
        self.status == Some(StatusKind::Playing)
    }

    pub fn is_queued(&self) -> bool {
        self.status == Some(StatusKind::Queued)
    }

    /// An operation (install, update, launch, verify…) other than playing or
    /// waiting in the queue is running.
    pub fn is_busy(&self) -> bool {
        self.launching
            || self.status.is_some_and(|s| {
                s.is_busy() && !matches!(s, StatusKind::Playing | StatusKind::Queued)
            })
    }
}

/// The one-click action of a tile, if any. Busy games show their status
/// instead of an action.
pub fn primary(state: &GameState) -> Option<GameCommand> {
    if state.is_playing() {
        return Some(GameCommand::Stop);
    }
    if state.is_busy() || state.is_queued() {
        return None;
    }
    if state.installed {
        Some(GameCommand::Play)
    } else if state.installable && !state.sideload {
        Some(GameCommand::Install)
    } else {
        None
    }
}

/// Every command for a game, grouped for menus (groups are separated). The
/// categories submenu is rendered after [`GameCommand::NewCategory`]'s group
/// by the menu builder.
pub fn menu(state: &GameState) -> Vec<Vec<GameCommand>> {
    use GameCommand::*;
    let busy = state.is_busy();
    let playing = state.is_playing();
    let mut main = Vec::new();
    if let Some(p) = primary(state) {
        main.push(p);
    }
    if state.installed && state.has_update && !busy && !state.is_queued() && !state.sideload {
        main.push(Update);
    }
    if state.is_queued() {
        main.push(RemoveFromQueue);
    }
    main.push(OpenPage);
    if state.installed && !state.browser && !state.external {
        main.push(Settings);
        main.push(Logs);
    }

    let mut library = vec![ToggleFavourite, ToggleHidden, Edit];
    if state.in_recent {
        library.push(RemoveFromRecent);
    }

    let mut files = Vec::new();
    if state.installed {
        match state.shortcut {
            Some(true) => files.push(RemoveShortcut),
            Some(false) => files.push(AddShortcut),
            None => files.extend([AddShortcut, RemoveShortcut]),
        }
    }
    if !state.browser && !state.external && (state.installed || state.installable) {
        files.push(AddToSteam);
    }
    if state.installed && state.has_install_path && !state.browser {
        files.push(OpenFolder);
    }
    if state.installed && !state.sideload && !state.external && !busy && !playing {
        files.push(Verify);
    }

    let mut danger = Vec::new();
    if state.installed && !state.external && !busy && !playing {
        danger.push(Uninstall);
    }

    [main, library, files, danger]
        .into_iter()
        .filter(|g| !g.is_empty())
        .collect()
}

impl GameCommand {
    /// Menu and button label. Labels of commands that open another surface
    /// for more input end with an ellipsis.
    pub fn label(&self, state: &GameState) -> String {
        use GameCommand::*;
        let key = match self {
            ToggleCategory(name) => return name.clone(),
            Play => "library.command.play",
            Stop => "library.command.stop",
            Install => "library.command.install",
            Update => "library.command.update",
            RemoveFromQueue => "library.command.remove_from_queue",
            OpenPage => "library.command.open_page",
            Settings => "library.command.settings",
            Logs => "library.command.logs",
            ToggleFavourite if state.favourite => "library.command.remove_favourite",
            ToggleFavourite => "library.command.add_favourite",
            ToggleHidden if state.hidden => "library.command.unhide",
            ToggleHidden => "library.command.hide",
            NewCategory => "library.command.new_category",
            Edit if state.sideload => "library.command.edit_app",
            Edit => "library.command.edit_details",
            RemoveFromRecent => "library.command.remove_from_recent",
            AddShortcut => "library.command.add_shortcut",
            RemoveShortcut => "library.command.remove_shortcut",
            AddToSteam => "library.command.add_to_steam",
            OpenFolder => "library.command.open_folder",
            Verify => "library.command.verify",
            Uninstall if state.sideload => "library.command.remove_app",
            Uninstall => "library.command.uninstall",
        };
        t(key).to_string()
    }

    pub fn icon(&self, state: &GameState) -> IconName {
        use GameCommand::*;
        match self {
            Play => IconName::Play,
            Stop => IconName::CircleStop,
            Install => IconName::Download,
            Update => IconName::CircleArrowDown,
            RemoveFromQueue => IconName::X,
            OpenPage => IconName::Gamepad2,
            Settings => IconName::Settings,
            Logs => IconName::ScrollText,
            ToggleFavourite if state.favourite => IconName::StarOff,
            ToggleFavourite => IconName::Star,
            ToggleHidden if state.hidden => IconName::Eye,
            ToggleHidden => IconName::EyeOff,
            ToggleCategory(_) => IconName::Tag,
            NewCategory => IconName::Plus,
            Edit => IconName::Pencil,
            RemoveFromRecent => IconName::Minus,
            AddShortcut => IconName::Link2,
            RemoveShortcut => IconName::Link2Off,
            AddToSteam => IconName::Gamepad2,
            OpenFolder => IconName::FolderOpen,
            Verify => IconName::ShieldCheck,
            Uninstall => IconName::Trash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GameCommand::*;

    fn flat(state: &GameState) -> Vec<GameCommand> {
        menu(state).into_iter().flatten().collect()
    }

    #[test]
    fn primary_follows_state() {
        let mut s = GameState {
            installable: true,
            ..Default::default()
        };
        assert_eq!(primary(&s), Some(Install));
        s.sideload = true;
        assert_eq!(primary(&s), None);
        s.sideload = false;
        s.installed = true;
        assert_eq!(primary(&s), Some(Play));
        s.status = Some(StatusKind::Playing);
        assert_eq!(primary(&s), Some(Stop));
        s.status = Some(StatusKind::Updating);
        assert_eq!(primary(&s), None);
        s.status = Some(StatusKind::Queued);
        assert_eq!(primary(&s), None);
        s.status = None;
        s.launching = true;
        assert_eq!(primary(&s), None);
        s.launching = false;
        s.installable = false;
        s.installed = false;
        assert_eq!(primary(&s), None);
    }

    #[test]
    fn menu_for_installed_game() {
        let s = GameState {
            installed: true,
            installable: true,
            has_update: true,
            has_install_path: true,
            ..Default::default()
        };
        let m = menu(&s);
        assert_eq!(m.len(), 4);
        assert_eq!(m[0], vec![Play, Update, OpenPage, Settings, Logs]);
        assert_eq!(m[3], vec![Uninstall]);
        assert!(flat(&s).contains(&Verify));
        assert!(flat(&s).contains(&OpenFolder));
        assert!(!flat(&s).contains(&RemoveFromRecent));
    }

    #[test]
    fn busy_and_playing_games_cannot_be_uninstalled_or_verified() {
        let mut s = GameState {
            installed: true,
            status: Some(StatusKind::Playing),
            ..Default::default()
        };
        assert!(!flat(&s).contains(&Uninstall));
        assert!(!flat(&s).contains(&Verify));
        assert_eq!(flat(&s)[0], Stop);
        s.status = None;
        s.external = true;
        s.has_install_path = true;
        let steam = flat(&s);
        assert_eq!(steam[0], Play);
        for gone in [Settings, Logs, AddToSteam, Verify, Uninstall] {
            assert!(!steam.contains(&gone), "{gone:?}");
        }
        assert!(steam.contains(&OpenFolder));
        s.status = Some(StatusKind::Playing);
        s.status = Some(StatusKind::Repairing);
        assert!(!flat(&s).contains(&Uninstall));
        s.status = Some(StatusKind::Queued);
        assert!(flat(&s).contains(&RemoveFromQueue));
        assert!(!flat(&s).contains(&Update));
    }

    #[test]
    fn browser_and_sideload_apps() {
        crate::i18n::register();
        let s = GameState {
            installed: true,
            sideload: true,
            browser: true,
            has_update: true,
            in_recent: true,
            ..Default::default()
        };
        let all = flat(&s);
        assert!(!all.contains(&Settings));
        assert!(!all.contains(&Update));
        assert!(!all.contains(&Verify));
        assert!(!all.contains(&OpenFolder));
        assert!(all.contains(&RemoveFromRecent));
        assert_eq!(Uninstall.label(&s), "Remove from library…");
        assert_eq!(Edit.label(&s), "Edit app…");
    }

    #[test]
    fn shortcut_items_follow_known_state() {
        let mut s = GameState {
            installed: true,
            ..Default::default()
        };
        let shortcut_items = |s: &GameState| -> Vec<GameCommand> {
            flat(s)
                .into_iter()
                .filter(|c| matches!(c, AddShortcut | RemoveShortcut))
                .collect()
        };
        assert_eq!(shortcut_items(&s), vec![AddShortcut, RemoveShortcut]);
        s.shortcut = Some(true);
        assert_eq!(shortcut_items(&s), vec![RemoveShortcut]);
        s.shortcut = Some(false);
        assert_eq!(shortcut_items(&s), vec![AddShortcut]);
        s.installed = false;
        assert!(shortcut_items(&s).is_empty());
    }

    #[test]
    fn steam_shortcuts_need_no_install() {
        let mut s = GameState {
            installable: true,
            ..Default::default()
        };
        assert!(flat(&s).contains(&AddToSteam));
        s.installed = true;
        assert!(flat(&s).contains(&AddToSteam));
        s.browser = true;
        assert!(!flat(&s).contains(&AddToSteam));
        let not_installable = GameState::default();
        assert!(!flat(&not_installable).contains(&AddToSteam));
    }

    #[test]
    fn labels_reflect_toggles() {
        crate::i18n::register();
        let mut s = GameState::default();
        assert_eq!(ToggleFavourite.label(&s), "Add to favourites");
        assert_eq!(ToggleHidden.label(&s), "Hide");
        s.favourite = true;
        s.hidden = true;
        assert_eq!(ToggleFavourite.label(&s), "Remove from favourites");
        assert_eq!(ToggleHidden.label(&s), "Unhide");
        assert_eq!(Install.label(&s), "Install");
    }
}
