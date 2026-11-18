#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef AssetVersion
  #error AssetVersion is required
#endif
[Setup]
AppId={{DA0ABCF1-A516-48B0-9FC3-0C87128E90BE}
AppName=LoL Records
AppVersion={#AppVersion}
AppPublisher=FFACK
AppPublisherURL=https://github.com/FFACK/lol-records
DefaultDirName={localappdata}\Programs\LoL Records
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
DisableProgramGroupPage=yes
WizardStyle=modern
SetupIconFile=..\assets\app.ico
UninstallDisplayIcon={app}\lol-records.exe
OutputDir=..\dist
OutputBaseFilename=lol-records-setup-v{#AppVersion}-x64
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; Flags: unchecked
[Files]
Source: "..\dist\lol-records.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\assets\portraits\{#AssetVersion}\*.png"; DestDir: "{localappdata}\LolRecords\portraits\{#AssetVersion}"; Flags: ignoreversion uninsneveruninstall
Source: "..\dist\assets\items\{#AssetVersion}\*.png"; DestDir: "{localappdata}\LolRecords\items\{#AssetVersion}"; Flags: ignoreversion uninsneveruninstall
[Icons]
Name: "{autoprograms}\LoL Records"; Filename: "{app}\lol-records.exe"
Name: "{autodesktop}\LoL Records"; Filename: "{app}\lol-records.exe"; Tasks: desktopicon
[Run]
Filename: "{app}\lol-records.exe"; Description: "Launch LoL Records"; Flags: nowait postinstall skipifsilent
