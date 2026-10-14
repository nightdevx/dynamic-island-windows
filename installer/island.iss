; Per-user installer for Dynamic Island: no administrator rights, nothing outside the
; user's profile. Built by scripts\package.ps1, which passes the path of the release exe:
;   ISCC.exe /DExe=<path to island.exe> installer\island.iss

#ifndef Exe
  #error Build with scripts\package.ps1 (it passes /DExe=...)
#endif
#define AppVersion GetStringFileInfo(Exe, "ProductVersion")

[Setup]
AppId={{AE02BD3E-2DB0-4AE3-A310-DC52E75860FB}
AppName=Dynamic Island
AppVersion={#AppVersion}
AppVerName=Dynamic Island {#AppVersion}
AppPublisher=nightdijital
AppPublisherURL=https://nightdijital.com
AppSupportURL=https://nightdijital.com
AppUpdatesURL=https://github.com/nightdevx/dynamic-island-windows/releases
AppContact=iletisim@nightdijital.com
AppCopyright=© 2026 nightdijital
VersionInfoVersion={#AppVersion}
; Claude Code's hooks call the exe at this path (see README), so it does not move.
DefaultDirName={localappdata}\DynamicIsland
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
SetupIconFile=..\assets\island.ico
UninstallDisplayIcon={app}\island.exe
UninstallDisplayName=Dynamic Island
OutputDir=..\dist
OutputBaseFilename=DynamicIsland-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "tr"; MessagesFile: "compiler:Languages\Turkish.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
tr.Autostart=Windows açılınca başlat
en.Autostart=Start with Windows
tr.Launch=Dynamic Island'ı başlat
en.Launch=Start Dynamic Island
tr.Claude=Claude Code'a bağla (oturumlar ve limitler Claude sekmesinde görünür)
en.Claude=Connect to Claude Code (sessions and limits in the Claude tab)

[Tasks]
Name: "autostart"; Description: "{cm:Autostart}"
; Offered only when Claude Code is installed. Adds the island's hooks to ~/.claude/settings.json.
Name: "claude"; Description: "{cm:Claude}"; Check: ClaudeInstalled

[Files]
Source: "{#Exe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Dynamic Island"; Filename: "{app}\island.exe"
; Explorer's "Send to" menu: puts the selected files on the shelf.
Name: "{usersendto}\Dynamic Island"; Filename: "{app}\island.exe"; Parameters: "--shelf"

[Registry]
; The same value the app's own "Başlangıçta çalıştır" menu item writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "DynamicIsland"; ValueData: """{app}\island.exe"""; Tasks: autostart

[Run]
; Edits settings.json in place, keeping everything else (and a one-time backup next to it).
Filename: "{app}\island.exe"; Parameters: "--setup-claude"; Tasks: claude; Flags: runhidden waituntilterminated
Filename: "{app}\island.exe"; Description: "{cm:Launch}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Takes the island's hooks out again, so Claude Code does not call a program that is gone.
Filename: "{app}\island.exe"; Parameters: "--remove-claude"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveClaude"

[UninstallDelete]
; Logs and the Claude Code session state. Settings (%APPDATA%\DynamicIsland) stay.
Type: filesandordirs; Name: "{app}"

[Code]
function ClaudeInstalled(): Boolean;
begin
  Result := DirExists(ExpandConstant('{%USERPROFILE}\.claude'));
end;

procedure StopIsland();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im island.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(300);
end;

// A running island holds its exe open: stop it before replacing or removing it.
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopIsland();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopIsland();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // Whoever turned autostart on (this installer or the app's menu), it goes with the app.
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'DynamicIsland');
end;
