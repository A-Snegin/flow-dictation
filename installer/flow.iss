; Flow installer.
;
; Per-user by default: no administrator prompt, which matters because most
; people who are handed a dictation tool are not going to have, or want to use,
; local admin. It installs into %LOCALAPPDATA%, registers a Start Menu entry and
; an uninstaller, and offers to start with Windows.
;
; The speech model is the only large thing. Bundling it makes a 138 MB
; installer that works with no internet; leaving it out makes a 22 MB one that
; fetches the model on first run. Both are built by scripts\build-installer.ps1,
; which sets IncludeModel.

#define AppName "Flow"
#define AppVersion "1.0.0"
#define AppPublisher "Lift-Off Consulting"
#define AppExe "flow-core.exe"

; Set by ISCC /DIncludeModel=1
#ifndef IncludeModel
  #define IncludeModel "0"
#endif
#ifndef StageDir
  #define StageDir "..\..\..\..\AppData\Local\Flow\target\release"
#endif
#ifndef ModelDir
  #define ModelDir ""
#endif

[Setup]
AppId={{8F3A6C21-4E7D-4C2B-9A55-1D0F2B7E9C10}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppComments=Local dictation. Hold a key, talk, let go.
DefaultDirName={localappdata}\Flow\app
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=yes
; Per-user: no UAC prompt, and nothing outside this account is touched.
PrivilegesRequired=lowest
OutputDir=..\dist
#if IncludeModel == "1"
OutputBaseFilename=Flow-Setup-with-model
#else
OutputBaseFilename=Flow-Setup
#endif
SetupIconFile=..\assets\flow.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=
InfoBeforeFile=
AppSupportURL=https://github.com/A-Snegin/flow-dictation

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startup"; Description: "Start Flow when I sign in"; GroupDescription: "After installing:"
Name: "launch"; Description: "Start Flow now"; GroupDescription: "After installing:"

[Files]
Source: "{#StageDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\onnxruntime.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\bench-e2e.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StageDir}\wer.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\installer\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
#if IncludeModel == "1"
; Straight into where the app looks for it, so first run has nothing to do.
Source: "{#ModelDir}\*"; DestDir: "{localappdata}\Flow\models\small-streaming-en"; Flags: ignoreversion
#endif

[Icons]
Name: "{group}\Flow"; Filename: "{app}\{#AppExe}"
Name: "{group}\Flow settings"; Filename: "{app}\{#AppExe}"; Parameters: "--settings"
Name: "{group}\Uninstall Flow"; Filename: "{uninstallexe}"
Name: "{userstartup}\Flow"; Filename: "{app}\{#AppExe}"; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start Flow"; Flags: nowait postinstall skipifsilent; Tasks: launch

[UninstallRun]
; Stop the running copy first, or the files cannot be removed.
Filename: "{cmd}"; Parameters: "/C taskkill /IM {#AppExe} /F"; Flags: runhidden; RunOnceId: "StopFlow"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
// The model is 136 MB and lives outside the app folder, so it survives an
// upgrade. On uninstall the user is asked, because someone reinstalling should
// not have to download it again, and someone leaving should not be left with
// it.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ModelPath: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    ModelPath := ExpandConstant('{localappdata}\Flow\models');
    if DirExists(ModelPath) then
    begin
      if MsgBox('Also remove the speech model (about 136 MB)?' + #13#10 +
                'Keep it if you plan to reinstall Flow.',
                mbConfirmation, MB_YESNO) = IDYES then
        DelTree(ModelPath, True, True, True);
    end;
    if MsgBox('Also remove your settings and dictionary?',
              mbConfirmation, MB_YESNO) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\Flow'), True, True, True);
      DeleteFile(ExpandConstant('{localappdata}\Flow\traces.jsonl'));
    end;
  end;
end;
