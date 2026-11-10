; NapCat telepítő (Inno Setup 6).
; Felhasználói szintű telepítés, rendszergazdai jog és UAC ablak nélkül, így a frissítés is
; csendben lefuthat. A beállítások az %APPDATA%\NapCat mappában vannak, azokat nem érinti.
;
; Fordítás: iscc /DAppVersion=0.1.0 installer\napcat.iss
; Kimenet:  target\installer\NapCat-Setup-<verzió>.exe

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{6C1F3A52-8D4E-4B7A-9F21-3E5A7C0D9B14}
AppName=NapCat
AppVersion={#AppVersion}
AppVerName=NapCat {#AppVersion}
AppPublisher=Cat Ninth
AppPublisherURL=https://github.com/catninth/napcat
AppSupportURL=https://github.com/catninth/napcat/issues
DefaultDirName={localappdata}\Programs\NapCat
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\target\installer
OutputBaseFilename=NapCat-Setup-{#AppVersion}
SetupIconFile=..\res\napcat.ico
UninstallDisplayIcon={app}\napcat.exe
UninstallDisplayName=NapCat
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; A futó példányt a telepítő bezárja; az újraindítást a [Run] szakasz végzi.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "hungarian"; MessagesFile: "compiler:Languages\Hungarian.isl"

[CustomMessages]
english.AutoStart=Start NapCat with Windows
hungarian.AutoStart=Indítás a Windows-zal

[Tasks]
Name: "autostart"; Description: "{cm:AutoStart}"

[Files]
Source: "..\target\release\napcat.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\NapCat"; Filename: "{app}\napcat.exe"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "NapCat"; \
  ValueData: """{app}\napcat.exe"""; Flags: uninsdeletevalue; Tasks: autostart
; Az értesítések neve és ikonja (az app indításkor írja); eltávolításkor törlődik.
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\catninth.NapCat"; Flags: uninsdeletekey dontcreatekey

[UninstallDelete]
Type: files; Name: "{localappdata}\NapCat\toast.png"
Type: dirifempty; Name: "{localappdata}\NapCat"

[Run]
; Kézi telepítésnél a befejező oldalon, csendes frissítésnél (/VERYSILENT) automatikusan indul.
Filename: "{app}\napcat.exe"; Description: "{cm:LaunchProgram,NapCat}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\napcat.exe"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM napcat.exe /F"; Flags: runhidden; RunOnceId: "StopNapCat"
