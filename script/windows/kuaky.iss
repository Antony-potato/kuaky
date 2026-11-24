; Instalador de kuaky para Windows (Inno Setup 6).
; Uso: iscc /DVersion=0.2.0 /DExe=..\..\target\release\kuaky.exe script\windows\kuaky.iss
; Instala por usuario (sin permisos de administrador), con acceso en el menú Inicio
; y desinstalador. Lo usa el workflow .github/workflows/release.yml.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Exe
  #define Exe "..\..\target\release\kuaky.exe"
#endif

[Setup]
AppId={{8E0B5C55-3F0D-4C7A-9B8E-6B1D2C4A7F10}
AppName=kuaky
AppVersion={#Version}
AppPublisher=kuaky
DefaultDirName={localappdata}\Programs\kuaky
DefaultGroupName=kuaky
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\..\target\release\installer
OutputBaseFilename=kuaky-{#Version}-windows-x64-setup
SetupIconFile=..\..\assets\icon\kuaky.ico
UninstallDisplayIcon={app}\kuaky.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Las actualizaciones corren este instalador con /SILENT: cierra kuaky y la vuelve a abrir.
CloseApplications=yes
RestartApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Exe}"; DestDir: "{app}"; DestName: "kuaky.exe"; Flags: ignoreversion
Source: "..\..\assets\icon\kuaky.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\kuaky"; Filename: "{app}\kuaky.exe"; IconFilename: "{app}\kuaky.ico"
Name: "{userdesktop}\kuaky"; Filename: "{app}\kuaky.exe"; IconFilename: "{app}\kuaky.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\kuaky.exe"; Description: "{cm:LaunchProgram,kuaky}"; Flags: nowait postinstall skipifsilent
