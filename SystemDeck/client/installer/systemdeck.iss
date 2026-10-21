; SystemDeck desktop client installer (Inno Setup 6).
; Built by build.ps1, which passes /DAppVersion and /DSourceDir:
;   ISCC.exe /DAppVersion=0.1.0 /DSourceDir=..\..\release\SystemDeck-Client systemdeck.iss
; Installs per user by default (no admin prompt); the setup dialog offers
; "Install for all users" too. User data in %APPDATA%\SystemDeck is kept on uninstall.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\release\SystemDeck-Client"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\release"
#endif

[Setup]
AppId={{6F2C8E41-3B7A-4D59-9E1F-5A0C7B2D8E93}
AppName=SystemDeck
AppVersion={#AppVersion}
AppVerName=SystemDeck {#AppVersion}
AppPublisher=SystemDeck
DefaultDirName={autopf}\SystemDeck
DefaultGroupName=SystemDeck
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=SystemDeck-Setup-{#AppVersion}
UninstallDisplayIcon={app}\systemdeck.exe
UninstallDisplayName=SystemDeck
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#SourceDir}\systemdeck.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\SystemDeck"; Filename: "{app}\systemdeck.exe"
Name: "{autodesktop}\SystemDeck"; Filename: "{app}\systemdeck.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\systemdeck.exe"; Description: "{cm:LaunchProgram,SystemDeck}"; Flags: nowait postinstall skipifsilent

[Code]
// Web entries open in tabs through the Microsoft Edge WebView2 Runtime. It ships
// with Windows 11 and most Windows 10 installs; warn (don't block) if it's missing.
function WebView2Installed(): Boolean;
var
  Version: String;
  Key: String;
begin
  Key := 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  Result :=
    (RegQueryStringValue(HKLM64, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0')) or
    (RegQueryStringValue(HKLM, Key, 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0')) or
    (RegQueryStringValue(HKCU, Key, 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0'));
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and (not WizardSilent()) and (not WebView2Installed()) then
    MsgBox('The Microsoft Edge WebView2 Runtime was not found.' + #13#10 + #13#10 +
      'SystemDeck works without it, but website entries cannot open in a tab until it is installed:' + #13#10 +
      'https://developer.microsoft.com/microsoft-edge/webview2/', mbInformation, MB_OK);
end;
