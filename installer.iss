#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{7D3A9C41-2E6B-4F85-B1A0-8C5E4D9F2A63}
AppName=Notify
AppVersion={#AppVersion}
AppPublisher=Notify
DefaultDirName={autopf}\Notify
DefaultGroupName=Notify
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\notify.exe
SetupIconFile=assets\notify.ico
OutputDir=dist
OutputBaseFilename=notify-Windows-Setup
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "target\release\notify.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Notify"; Filename: "{app}\notify.exe"
Name: "{autodesktop}\Notify"; Filename: "{app}\notify.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Notify"; Flags: dontcreatekey uninsdeletevalue

[Run]
Filename: "{app}\notify.exe"; Description: "Launch Notify"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im notify.exe"; Flags: runhidden; RunOnceId: "KillNotify"
