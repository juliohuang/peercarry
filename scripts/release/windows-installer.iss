; Build with ISCC /DAppVersion=0.3.1 /DBinaryDir=... /DOutputDir=... this-file
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef BinaryDir
  #error BinaryDir is required
#endif
#ifndef OutputDir
  #error OutputDir is required
#endif

[Setup]
AppId=PeerCarry.Desktop
AppName=PeerCarry
AppVersion={#AppVersion}
AppPublisher=PeerCarry contributors
AppPublisherURL=https://github.com/juliohuang/peercarry
DefaultDirName={code:InstallDirectory}
DefaultGroupName=PeerCarry
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=PeerCarry-{#AppVersion}-windows-x64-Setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
UninstallDisplayIcon={app}\peercarry-tray.exe
CloseApplications=yes
RestartApplications=no
SetupLogging=yes

[Types]
Name: "compact"; Description: "Compact - clipboard, files and automatic updates"
Name: "full"; Description: "Full - also install CLI and AI Hook support"
Name: "custom"; Description: "Custom"; Flags: iscustom

[Components]
Name: "desktop"; Description: "PeerCarry desktop application"; Types: compact full custom; Flags: fixed
Name: "cli"; Description: "Command-line tools and AI Hook support"; Types: full

[Tasks]
Name: "startup"; Description: "Start PeerCarry when I sign in"; Flags: unchecked
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#BinaryDir}\peercarry-tray.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: desktop
Source: "{#BinaryDir}\peercarry.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: cli
Source: "{#BinaryDir}\peercarry.exe"; DestDir: "{app}"; DestName: "sclip.exe"; Flags: ignoreversion; Components: cli
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\PeerCarry"; Filename: "{app}\peercarry-tray.exe"; Check: not WizardNoIcons
Name: "{autodesktop}\PeerCarry"; Filename: "{app}\peercarry-tray.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "peercarry"; ValueData: """{app}\peercarry-tray.exe"""; Tasks: startup; Flags: uninsdeletevalue

[Run]
Filename: "{app}\peercarry-tray.exe"; Description: "Launch PeerCarry"; Flags: nowait postinstall skipifsilent

[Code]
function InstallDirectory(Param: String): String;
begin
  { Retain the old path so existing absolute Hook commands keep working. }
  if DirExists(ExpandConstant('{localappdata}\sync-clip')) then
    Result := ExpandConstant('{localappdata}\sync-clip')
  else
    Result := ExpandConstant('{localappdata}\peercarry');
end;

procedure RegisterExtraCloseApplicationsResources;
begin
  RegisterExtraCloseApplicationsResource(False, ExpandConstant('{app}\sync-clip-tray.exe'));
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Command: String;
begin
  if CurStep = ssPostInstall then begin
    { Only remove entries belonging to this installation, never another copy. }
    if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'sync-clip', Command) then
      if (CompareText(Command, ExpandConstant('"{app}\sync-clip-tray.exe"')) = 0) or
         (CompareText(Command, ExpandConstant('{app}\sync-clip-tray.exe')) = 0) then
        RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'sync-clip');
    if not WizardIsTaskSelected('startup') then
      if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'peercarry', Command) then
        if (CompareText(Command, ExpandConstant('"{app}\peercarry-tray.exe"')) = 0) or
           (CompareText(Command, ExpandConstant('{app}\peercarry-tray.exe')) = 0) then
          RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'peercarry');
  end;
end;
