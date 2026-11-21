/*
 * Oxide Flux - per-user Windows installer (NSIS 3.x, Unicode)
 * ===========================================================================
 *
 * Why a per-user NSIS .exe and not MSI/MSIX?
 *
 *   Oxide Flux is a single-user desktop app. It writes only HKCU
 *   (file associations, Add/Remove Programs), keeps its state under
 *   %APPDATA%\OxideFlux, and never needs a service, a driver or a machine-wide
 *   registry key. A per-user install therefore needs no elevation at all:
 *   RequestExecutionLevel user, files in
 *   %LOCALAPPDATA%\Programs\OxideFlux, shortcuts in the current user's profile.
 *   That is the same model VS Code, Discord and other unsigned/short-lived
 *   builds use, and it avoids a UAC prompt that would otherwise confuse the
 *   per-user .torrent / magnet: associations.
 *
 *   An MSI (WiX) is the right answer later for enterprise / managed
 *   deployment: GPO/Intune push, per-machine install, repair and patching,
 *   and an administrative install point. It buys nothing for a single user.
 *
 *   MSIX is NOT recommended yet: package identity changes AppData and
 *   registry virtualisation, packaged Win32 apps have restricted file
 *   association takeover, mpv is hosted in a child Win32 window created from
 *   the app (packaged-app interop for that is fragile), and signing requires
 *   an MSIX-compatible certificate that is harder to obtain than a
 *   SignPath/Azure Trusted Signing setup. Revisit when auto-update exists.
 *
 * Build (NSIS installed; run from anywhere):
 *
 *   makensis /DVERSION=1.2.3 /DFILEVERSION=1.2.3.0 installer\oxide-flux.nsi
 *
 * Unqualified paths are resolved against this script's directory, so the
 * defaults ("..\stage", "..\LICENSE", "..\dist\...") point at the repository
 * root. CI passes absolute paths for every define, which avoids the question
 * entirely and is worth copying for local one-off builds.
 *
 * Defines:
 *   VERSION      display version, digits and dots only (default 0.0.0)
 *   FILEVERSION  4-part numeric version for the PE version resource
 *                (default 0.0.0.0)
 *   STAGEDIR     directory holding oxide-flux.exe, fluxd.exe, README.md,
 *                LICENSE, THIRD_PARTY.md (default "..\stage")
 *   LICENSEFILE  license text shown on the license page (default "..\LICENSE")
 *   OUTFILE      installer path (default ..\dist\oxide-flux-<VERSION>-...setup.exe)
 *
 * The installer never starts or kills oxide-flux.exe or fluxd.exe. If either
 * is running from the target directory, it shows a Retry/Cancel message and
 * lets the user close it. Image files are probed with FileOpen: a running
 * executable holds its image without write sharing, so the open fails and that
 * is our detection - no FindWindow, no taskkill, no process enumeration.
 *
 * File associations are owned by the app, not by this installer. Setup does
 * not write HKCU\Software\Classes at all; the app registers .torrent and
 * magnet: on first launch (or with --register). The uninstaller asks the app
 * to undo exactly those registrations with --unregister, which refuses to
 * touch entries that do not point at this installation. Other applications'
 * associations are never read, rewritten or deleted, and Windows UserChoice is
 * left alone.
 */

Unicode true

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "Sections.nsh"

; --------------------------------------------------------------------------
; Build inputs
; --------------------------------------------------------------------------

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

!ifndef FILEVERSION
  !define FILEVERSION "0.0.0.0"
!endif

!ifndef STAGEDIR
  !define STAGEDIR "..\stage"
!endif

!ifndef LICENSEFILE
  !define LICENSEFILE "..\LICENSE"
!endif

!ifndef OUTFILE
  !define OUTFILE "..\dist\oxide-flux-${VERSION}-windows-x86_64-setup.exe"
!endif

!define APP_NAME        "Oxide Flux"
!define APP_EXE         "oxide-flux.exe"
!define DAEMON_EXE      "fluxd.exe"
!define APP_PUBLISHER   "Oxide Flux contributors"
!define APP_URL         "https://github.com/edhases/oxide-flux"
!define UNINST_KEY      "Software\Microsoft\Windows\CurrentVersion\Uninstall\OxideFlux"
!define APP_KEY         "Software\OxideFlux"

Name "${APP_NAME} ${VERSION}"
Caption "${APP_NAME} ${VERSION} Setup"
OutFile "${OUTFILE}"
BrandingText "${APP_NAME} ${VERSION}"
InstallDir "$LOCALAPPDATA\Programs\OxideFlux"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 32
CRCCheck on
XPStyle on
ShowInstDetails show
ShowUninstDetails show

; PE version resource, shown by Explorer and by the Add/Remove Programs entry.
VIProductVersion "${FILEVERSION}"
VIAddVersionKey /LANG=1033 "ProductName"      "${APP_NAME}"
VIAddVersionKey /LANG=1033 "FileDescription"  "${APP_NAME} installer"
VIAddVersionKey /LANG=1033 "FileVersion"      "${VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion"   "${VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName"      "${APP_PUBLISHER}"
VIAddVersionKey /LANG=1033 "LegalCopyright"   "GPL-3.0-or-later"
VIAddVersionKey /LANG=1033 "Comments"         "${APP_URL}"

; --------------------------------------------------------------------------
; Interface
; --------------------------------------------------------------------------

!define MUI_ABORTWARNING
; No icon is provided on purpose: the repository ships no .ico (the window icon
; is built at runtime). Add MUI_ICON/MUI_UNICON here once an .ico exists.
!define MUI_WELCOMEPAGE_TITLE "Install ${APP_NAME} ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "This installs ${APP_NAME} for the current user only.$\r$\n$\r$\nNo administrator rights are needed: the program goes to your local AppData folder, shortcuts go to your own Start Menu, and nothing outside your user profile is modified.$\r$\n$\r$\nClose ${APP_NAME} before continuing if it is running. Setup will ask you to close it rather than terminating it for you."
!define MUI_DIRECTORYPAGE_TEXT_TOP "Setup installs ${APP_NAME} into the folder below. It is under your profile, so no elevation is required."
!define MUI_COMPONENTSPAGE_TEXT_TOP "Choose whether to add a desktop shortcut. The application itself is always installed."
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP_NAME}"
!define MUI_FINISHPAGE_NOREBOOTSUPPORT

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSEFILE}"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; --------------------------------------------------------------------------
; Helpers
; --------------------------------------------------------------------------

; Best-effort "is the app running?" probe, shared in spirit by the installer
; and the uninstaller (uninstaller code lives in its own namespace, so the two
; functions are separate but identical). Both the GUI and the daemon are
; checked: either one locks its own image file.
;
; A loaded .exe image is opened with FILE_SHARE_READ | FILE_SHARE_DELETE and
; no write sharing, so requesting write access fails while it runs. We retry
; until the user closes the app; silent installs cancel instead of guessing.
Function EnsureAppNotRunning
  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
    install_gui_retry:
      ClearErrors
      FileOpen $0 "$INSTDIR\${APP_EXE}" a
      ${If} ${Errors}
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION \
          "${APP_NAME} is still running.$\r$\n$\r$\nClose it (window, tray or Task Manager) and choose Retry. Setup does not terminate running programs." \
          /SD IDCANCEL IDRETRY install_gui_retry
        Abort "Installation cancelled: close ${APP_NAME} and run setup again."
      ${EndIf}
      FileClose $0
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\${DAEMON_EXE}"
    install_daemon_retry:
      ClearErrors
      FileOpen $0 "$INSTDIR\${DAEMON_EXE}" a
      ${If} ${Errors}
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION \
          "fluxd is still running from $\"$INSTDIR$\".$\r$\n$\r$\nStop it (Ctrl-C in its console or Task Manager) and choose Retry. Setup does not terminate running programs." \
          /SD IDCANCEL IDRETRY install_daemon_retry
        Abort "Installation cancelled: stop fluxd and run setup again."
      ${EndIf}
      FileClose $0
  ${EndIf}
FunctionEnd

Function un.EnsureAppNotRunning
  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
    uninstall_gui_retry:
      ClearErrors
      FileOpen $0 "$INSTDIR\${APP_EXE}" a
      ${If} ${Errors}
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION \
          "${APP_NAME} is still running.$\r$\n$\r$\nClose it (window, tray or Task Manager) and choose Retry. The uninstaller does not terminate running programs." \
          /SD IDCANCEL IDRETRY uninstall_gui_retry
        Abort "Uninstall cancelled: close ${APP_NAME} and run the uninstaller again."
      ${EndIf}
      FileClose $0
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\${DAEMON_EXE}"
    uninstall_daemon_retry:
      ClearErrors
      FileOpen $0 "$INSTDIR\${DAEMON_EXE}" a
      ${If} ${Errors}
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION \
          "fluxd is still running from $\"$INSTDIR$\".$\r$\n$\r$\nStop it (Ctrl-C in its console or Task Manager) and choose Retry. The uninstaller does not terminate running programs." \
          /SD IDCANCEL IDRETRY uninstall_daemon_retry
        Abort "Uninstall cancelled: stop fluxd and run the uninstaller again."
      ${EndIf}
      FileClose $0
  ${EndIf}
FunctionEnd

; --------------------------------------------------------------------------
; Install
; --------------------------------------------------------------------------

Function .onInit
  ; Everything below is per-user; keep the shortcut constants in user context
  ; even when setup is launched from an elevated shell.
  SetShellVarContext current
FunctionEnd

; Hidden: always installed, never presented as an option on the components page
; (a read-only section would need an InstType; hiding is simpler and honest).
Section "-${APP_NAME}" SecCore

  Call EnsureAppNotRunning

  SetOutPath "$INSTDIR"
  SetOverwrite on

  File "${STAGEDIR}\${APP_EXE}"
  File "${STAGEDIR}\${DAEMON_EXE}"
  File "${STAGEDIR}\README.md"
  File "${STAGEDIR}\LICENSE"
  File "${STAGEDIR}\THIRD_PARTY.md"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Start Menu (current user). No app-registered shortcut is created here: the
  ; app owns .torrent / magnet: and does that on first launch.
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortCut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortCut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\uninstall.exe"

  ; Add/Remove Programs entry (HKCU: per-user installs are listed per user).
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion"  "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\${APP_EXE},0"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher"       "${APP_PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout"    "${APP_URL}"
  WriteRegStr HKCU "${UNINST_KEY}" "HelpLink"        "${APP_URL}/issues"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"

  ; Our own key, read back by InstallDirRegKey on the next upgrade.
  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${APP_KEY}" "Version"    "${VERSION}"

  ; File associations are not written here on purpose. The app registers
  ; .torrent and magnet: itself on first launch (or with --register), so a
  ; silent install can never race the GUI or rewrite another client's entries.
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortCut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
SectionEnd

; An upgrade with the shortcut unchecked should not keep the old one around.
Function .onInstSuccess
  SectionGetFlags ${SecDesktop} $0
  IntOp $0 $0 & ${SF_SELECTED}
  ${If} $0 == 0
    Delete "$DESKTOP\${APP_NAME}.lnk"
  ${EndIf}
FunctionEnd

; --------------------------------------------------------------------------
; Uninstall
; --------------------------------------------------------------------------

Function un.onInit
  SetShellVarContext current
FunctionEnd

Section "Uninstall"
  ; 1. Make sure the app is closed - ask, never kill.
  Call un.EnsureAppNotRunning

  ; 2. Let the app remove its own per-user registrations (.torrent, magnet:,
  ;    and only the values that point at this exact $INSTDIR\oxide-flux.exe).
  ;    Setup / the uninstaller never touch HKCU\Software\Classes directly, so
  ;    another client's associations and Windows UserChoice stay intact.
  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
    DetailPrint "Removing file associations registered by ${APP_NAME}..."
    ExecWait '"$INSTDIR\${APP_EXE}" --unregister' $0
    ${If} $0 != 0
      MessageBox MB_OK|MB_ICONEXCLAMATION \
        "${APP_NAME} returned $0 while removing its file associations.$\r$\n$\r$\nYou can retry later with:$\r$\n$\"$INSTDIR\${APP_EXE}$\" --unregister" \
        /SD IDOK
    ${EndIf}
  ${EndIf}

  ; 3. Shortcuts.
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  ; 4. Files. Only the files we ship are deleted, then the directory itself
  ;    (RMDir without /r fails while anything else is still in there, so user
  ;    files are never removed).
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\${DAEMON_EXE}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY.md"
  Delete "$INSTDIR\uninstall.exe"
  ; Leave $INSTDIR before removing it: a process cannot remove its own current
  ; directory (the uninstaller would otherwise fail with a silent RMDir).
  SetOutPath "$TEMP"
  RMDir "$INSTDIR"

  ; 5. Our registry entries. Nothing else is deleted - in particular
  ;    %APPDATA%\OxideFlux (session state, settings) stays.
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"

  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
    MessageBox MB_OK|MB_ICONEXCLAMATION \
      "Some files in $\"$INSTDIR$\" could not be removed. Close ${APP_NAME} and run the uninstaller again, or delete that folder manually." \
      /SD IDOK
  ${EndIf}
SectionEnd
