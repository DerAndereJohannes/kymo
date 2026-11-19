; Kymo's installer for Windows (Inno Setup 6), built by .github/workflows/binaries.yml:
;
;   iscc /DVersion=0.1.0 /DNumericVersion=0.1.0 /DBinary=path\to\kymo.exe
;        /DOutput=Kymo-0.1.0-windows-x86_64-setup packaging\windows\kymo.iss
;
; It installs for the current user by default (to %LOCALAPPDATA%\Programs\Kymo, without
; administrator rights), or for all users if chosen; adds a Start menu entry, a desktop
; shortcut and Kymo to PATH if chosen; and offers Kymo under Open With for the recordings it
; reads, without taking over their default app. The uninstaller removes all of it and, if
; asked (No by default), Kymo's settings (%APPDATA%\kymo) and its cache of recordings'
; overviews (%LOCALAPPDATA%\kymo).

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef Binary
  #define Binary "..\..\target\release\kymo.exe"
#endif
#ifndef Output
  #define Output "Kymo-" + Version + "-windows-x86_64-setup"
#endif

[Setup]
AppId={{79414964-BF2D-4E96-BC0F-8B3D002ED07E}
AppName=Kymo
AppVersion={#Version}
AppVerName=Kymo {#Version}
AppPublisher=Kymo developers
AppPublisherURL=https://github.com/cbrnr/kymo
AppSupportURL=https://github.com/cbrnr/kymo/issues
AppCopyright=© Kymo developers
VersionInfoVersion={#NumericVersion}
DefaultDirName={autopf}\Kymo
DisableProgramGroupPage=yes
DisableReadyPage=no
; Per user by default; the first page offers all users (which asks for administrator rights).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\..\crates\kymo\assets\kymo.ico
UninstallDisplayIcon={app}\kymo.exe
UninstallDisplayName=Kymo
OutputDir=.
OutputBaseFilename={#Output}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ChangesAssociations=yes
ChangesEnvironment=yes
CloseApplications=yes

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: addtopath; Description: "Add Kymo to PATH, to start it with ""kymo"" in a terminal"; GroupDescription: "Terminal:"; Flags: unchecked

[Files]
Source: "{#Binary}"; DestDir: "{app}"; DestName: "kymo.exe"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Kymo"; Filename: "{app}\kymo.exe"
Name: "{autodesktop}\Kymo"; Filename: "{app}\kymo.exe"; Tasks: desktopicon

[Registry]
; A file type for the recordings, which Open With lists Kymo for; HKA is the current user's
; classes or, for all users, the machine's.
Root: HKA; Subkey: "Software\Classes\Kymo.Recording"; ValueType: string; ValueData: "Recording"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Kymo.Recording\DefaultIcon"; ValueType: string; ValueData: "{app}\kymo.exe,0"
Root: HKA; Subkey: "Software\Classes\Kymo.Recording\shell\open\command"; ValueType: string; ValueData: """{app}\kymo.exe"" ""%1"""
; Kymo itself, with the types it supports.
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Kymo"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\DefaultIcon"; ValueType: string; ValueData: "{app}\kymo.exe,0"
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\shell\open\command"; ValueType: string; ValueData: """{app}\kymo.exe"" ""%1"""
; Each recording's extension: Kymo under Open With, and among the types Kymo supports.
Root: HKA; Subkey: "Software\Classes\.edf\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".edf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.bdf\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".bdf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.vhdr\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".vhdr"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.bvrh\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".bvrh"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.set\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".set"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.fif\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".fif"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.gdf\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".gdf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.xdf\OpenWithProgids"; ValueType: string; ValueName: "Kymo.Recording"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\kymo.exe\SupportedTypes"; ValueType: string; ValueName: ".xdf"; ValueData: ""

[Code]
const
  SystemEnvironment = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';

{ PATH: the user's for a per-user install, the machine's for all users. }
function EnvironmentRoot: Integer;
begin
  if IsAdminInstallMode then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

function EnvironmentKey: String;
begin
  if IsAdminInstallMode then
    Result := SystemEnvironment
  else
    Result := 'Environment';
end;

function ReadPath: String;
begin
  if not RegQueryStringValue(EnvironmentRoot, EnvironmentKey, 'Path', Result) then
    Result := '';
end;

procedure AddToPath(Dir: String);
var
  Paths: String;
begin
  Paths := ReadPath;
  if Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0 then
    Exit;
  if (Paths <> '') and (Paths[Length(Paths)] <> ';') then
    Paths := Paths + ';';
  RegWriteExpandStringValue(EnvironmentRoot, EnvironmentKey, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(Dir: String);
var
  Paths, Padded: String;
  P: Integer;
begin
  Paths := ReadPath;
  Padded := ';' + Paths + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Padded));
  if P = 0 then
    Exit;
  Delete(Padded, P, Length(Dir) + 1);
  { Without the semicolons added above. }
  Paths := Copy(Padded, 2, Length(Padded) - 2);
  RegWriteExpandStringValue(EnvironmentRoot, EnvironmentKey, 'Path', Paths);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  RemoveFromPath(ExpandConstant('{app}'));
  { Settings and cache stay unless the user asks; a silent uninstall keeps them. }
  if UninstallSilent then
    Exit;
  if MsgBox('Also remove Kymo''s settings and its cache of recording overviews?' + #13#10#13#10 +
            'They are kept otherwise, for a later install.',
            mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
  begin
    DelTree(ExpandConstant('{userappdata}\kymo'), True, True, True);
    DelTree(ExpandConstant('{localappdata}\kymo'), True, True, True);
  end;
end;
