export const REPO = 'https://github.com/Irori-app/irori'

export interface Screen {
  /** File stem under /screenshots; a `light-` twin exists when `light` is set. */
  id: string
  title: string
  caption: string
  light?: boolean
}

export const SCREENS: Screen[] = [
  { id: 'library', title: 'Library', caption: 'Every store, your emulated games and the games you added, in one grid.', light: true },
  { id: 'game', title: 'Game page', caption: 'Playtime, last session and one button to play.', light: true },
  { id: 'game-kiln', title: 'Game page', caption: 'Each game keeps its own settings, art and logs.' },
  { id: 'downloads', title: 'Downloads', caption: 'A queue you can pause, reorder and resume after a restart.', light: true },
  { id: 'emulators', title: 'Emulators', caption: 'RetroArch, DuckStation, PCSX2, Dolphin and more, installed from their own projects.', light: true },
  { id: 'stats', title: 'Stats', caption: 'Hours per week, streaks and your most played games.', light: true },
  { id: 'library-list', title: 'List view', caption: 'Store, size, playtime and status for every game, sortable.' },
  { id: 'game-emulated', title: 'Emulated game', caption: 'ROMs get their system, region and revision from the file name.' },
  { id: 'wine', title: 'Wine manager', caption: 'Game Porting Toolkit, Wine and Proton builds, side by side.', light: true },
  { id: 'bigpicture', title: 'Big Picture', caption: 'The whole launcher with a controller, scaled for a TV.' },
  { id: 'accounts', title: 'Accounts', caption: 'Several accounts per store, in one library.' },
  { id: 'settings-appearance', title: 'Settings', caption: 'Searchable settings, with per-game overrides that show what you changed.' },
]

export const STORES = [
  'Epic Games', 'GOG', 'Amazon Games', 'ZOOM Platform', 'itch.io', 'Steam',
  'Battle.net', 'EA app', 'Ubisoft Connect', 'Rockstar Games Launcher',
]

/** Systems as Irori badges them in the library. */
export const SYSTEMS = ['NES', 'SNES', 'N64', 'GB', 'GBC', 'GBA', 'NDS', '3DS', 'GC', 'Wii', 'PS1', 'PS2', 'PSP', 'Genesis', 'PCE', 'DOS', 'Arcade']

export interface InstallOption {
  label: string
  commands: string[]
  note?: string
}

export const INSTALL: Record<'linux' | 'macos' | 'windows', InstallOption[]> = {
  linux: [
    {
      label: 'Debian, Ubuntu',
      commands: [
        'curl -fsSL https://packages.irori.app/irori.gpg | sudo tee /usr/share/keyrings/irori.gpg > /dev/null',
        'curl -fsSL https://packages.irori.app/irori.list | sudo tee /etc/apt/sources.list.d/irori.list',
        'sudo apt update && sudo apt install irori',
      ],
    },
    {
      label: 'Fedora, openSUSE',
      commands: [
        'sudo curl -fsSLo /etc/yum.repos.d/irori.repo https://packages.irori.app/irori.repo',
        'sudo dnf install irori',
      ],
    },
    { label: 'Flatpak', commands: ['flatpak install flathub app.irori.Irori'], note: 'Also the way to install on Steam Deck.' },
    {
      label: 'Arch Linux',
      commands: [
        'curl -fsSLo /tmp/irori.asc https://packages.irori.app/irori.asc',
        'sudo pacman-key --add /tmp/irori.asc && sudo pacman-key --lsign-key 6DD7B72703FE3BD5C719F4CE6FE681AB798DF656',
        "printf '\\n[irori]\\nServer = https://packages.irori.app/arch/$arch\\n' | sudo tee -a /etc/pacman.conf",
        'sudo pacman -Syu irori',
      ],
      note: 'Also for Manjaro, EndeavourOS and CachyOS.',
    },
  ],
  macos: [
    { label: 'Homebrew', commands: ['brew install --cask irori-app/tap/irori'] },
    { label: 'Disk image', commands: [], note: 'Download the .dmg for Apple silicon or Intel from the releases page and drag Irori to Applications.' },
  ],
  windows: [
    { label: 'winget', commands: ['winget install Irori.Irori'] },
    { label: 'Scoop', commands: ['scoop bucket add irori https://github.com/irori-app/scoop-bucket', 'scoop install irori'] },
    { label: 'Installer', commands: [], note: 'Download Irori-setup.exe from the releases page. It installs for your user and never asks for administrator rights.' },
  ],
}

export const FAQ = [
  {
    q: 'Is Irori a fork of Heroic?',
    a: 'No. Irori is written from scratch in Rust and draws its interface with GPUI on your graphics card. It matches what Heroic Games Launcher can do, and its store support is a Rust port of legendary, gogdl and nile, the tools Heroic runs.',
  },
  {
    q: 'Do I have to sign in to anything?',
    a: 'Only to the stores whose games you want to see. Steam games are read from Steam’s own files without a sign-in, and emulated and added games need no account at all.',
  },
  {
    q: 'How do Windows games run on Linux and macOS?',
    a: 'On Linux through Wine, Proton or umu; on macOS through Wine, CrossOver or Apple’s Game Porting Toolkit. Irori downloads and manages these builds for you, and each game can use its own.',
  },
  {
    q: 'Does Irori download BIOS files or games for emulators?',
    a: 'Never. Irori installs emulators from their projects’ own servers on request. Point it at your own BIOS and game folders, and it checks a game has what it needs before it starts.',
  },
  {
    q: 'Can I bring my Heroic or Lutris library?',
    a: 'Yes. Irori imports Heroic’s accounts, installs, settings, favourites and categories in one click, and reads Lutris’s games, categories and playtime without changing anything in Lutris.',
  },
  {
    q: 'What does it cost?',
    a: 'Nothing. Irori is free software under the GPL-3.0 licence, and its source is on GitHub.',
  },
]
