; Irori's Windows installer (Inno Setup 6). Installs per user, like
; install-windows.ps1, so it never asks for administrator rights.
;
;   iscc /DVersion=0.1.0 /DSource=dist\Irori /DOutput=dist packaging\windows\irori.iss
;
; Source is the folder package.py stages for Windows.

#ifndef Version
  #error Pass the version: /DVersion=x.y.z
#endif
#ifndef Source
  #define Source "..\..\dist\Irori"
#endif
#ifndef Output
  #define Output "..\..\dist"
#endif

[Setup]
AppId={{6E2B7C1A-0D4F-4C8E-9A35-2F1B8D7E4C60}
AppName=Irori
AppVersion={#Version}
AppVerName=Irori {#Version}
AppPublisher=Irori developers
AppPublisherURL=https://irori.app
AppSupportURL=https://github.com/irori-app/irori/issues
DefaultDirName={localappdata}\Programs\Irori
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile={#Source}\LICENSE.txt
SetupIconFile=..\irori.ico
UninstallDisplayIcon={app}\irori.exe
UninstallDisplayName=Irori
OutputDir={#Output}
OutputBaseFilename=Irori-{#Version}-windows-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "cs"; MessagesFile: "compiler:Languages\Czech.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "it"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "nl"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "pl"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "pt"; MessagesFile: "compiler:Languages\Portuguese.isl"
Name: "pt_BR"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "tr"; MessagesFile: "compiler:Languages\Turkish.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "uk"; MessagesFile: "compiler:Languages\Ukrainian.isl"
Name: "ja"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Source}\irori.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\irori-sign-in.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\vcruntime140_1.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\irori.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Windows shows toasts only for apps whose Start menu shortcut carries their
; AppUserModelID; it matches APP_ID in crates/ui/src/system_notify.rs.
Name: "{autoprograms}\Irori"; Filename: "{app}\irori.exe"; IconFilename: "{app}\irori.ico"; AppUserModelID: "app.irori.Irori"
Name: "{autodesktop}\Irori"; Filename: "{app}\irori.exe"; IconFilename: "{app}\irori.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Classes\irori"; ValueType: string; ValueData: "URL:Irori"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\irori"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\irori\DefaultIcon"; ValueType: string; ValueData: "{app}\irori.ico"
Root: HKCU; Subkey: "Software\Classes\irori\shell\open\command"; ValueType: string; ValueData: """{app}\irori.exe"" ""%1"""

; Tells Irori who updates it (crates/services/src/app_update.rs). winget
; runs this setup with /CHANNEL=winget; run by hand, it says "setup".
[INI]
Filename: "{app}\install.ini"; Section: "Install"; Key: "Channel"; String: "{param:channel|setup}"

[Run]
Filename: "{app}\irori.exe"; Description: "{cm:LaunchProgram,Irori}"; Flags: nowait postinstall skipifsilent

; The uninstaller doesn't close running programs when it runs silently, as
; winget runs it, and a running Irori keeps its files. Without /T, a game
; started from Irori keeps running.
[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM irori-sign-in.exe"; Flags: runhidden; RunOnceId: "CloseSignIn"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM irori.exe"; Flags: runhidden; RunOnceId: "CloseIrori"

; WebView2's data folders, made next to the program that shows the sign-in
; window: irori-sign-in.exe, or irori.exe when it shows the window itself.
[UninstallDelete]
Type: filesandordirs; Name: "{app}\irori-sign-in.exe.WebView2"
Type: filesandordirs; Name: "{app}\irori.exe.WebView2"
Type: files; Name: "{app}\install.ini"
