; The Windows installer, built by scripts/package-windows.ps1 with Inno Setup 6:
;
;   iscc /DAppVersion=1.2.3 /DArch=x64 /DSourceExe=<mudling-desktop.exe> /DOutputDir=<dir> mudling.iss
;
; It installs for the current user alone, so it never asks for admin rights,
; and adds a Start menu entry and an uninstaller. Neither touches MUDling's
; data folder.

[Setup]
; The id Windows knows the installation by: never change it, or upgrades install beside it.
AppId={{5B0F5C2E-8D47-4C1B-9E0A-6A3D2F7B1C94}
AppName=MUDling
AppVersion={#AppVersion}
AppPublisher=Vyttle LLC
AppPublisherURL=https://github.com/Vyttle-LLC/mudling
DefaultDirName={autopf}\MUDling
DefaultGroupName=MUDling
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif
SetupIconFile=..\assets\brand\mudling.ico
UninstallDisplayIcon={app}\mudling.ico
UninstallDisplayName=MUDling
OutputDir={#OutputDir}
OutputBaseFilename=MUDling-{#AppVersion}-windows-{#Arch}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "MUDling.exe"; Flags: ignoreversion
Source: "..\assets\brand\mudling.ico"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Icons]
Name: "{autoprograms}\MUDling"; Filename: "{app}\MUDling.exe"; IconFilename: "{app}\mudling.ico"
Name: "{autodesktop}\MUDling"; Filename: "{app}\MUDling.exe"; IconFilename: "{app}\mudling.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\MUDling.exe"; Description: "{cm:LaunchProgram,MUDling}"; Flags: nowait postinstall skipifsilent
