; NSIS Modern User-Mode Installer Script for Adult Hub Desktop
; Builds per-user standalone installer: adult-hub-desktop-setup.exe

!include "MUI2.nsh"
!include "LogicLib.nsh"

Name "Adult Hub Desktop"
OutFile "${OUTDIR}/adult-hub-desktop-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\AdultHubDesktop"
InstallDirRegKey HKCU "Software\AdultHubDesktop" "InstallDir"
RequestExecutionLevel user

; UI Settings
!define MUI_ABORTWARNING
!define MUI_ICON "${SRCDIR}/desktop/assets/adult-hub-desktop.ico"
!define MUI_UNICON "${SRCDIR}/desktop/assets/adult-hub-desktop.ico"

; Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\adult-hub-desktop.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Adult Hub Desktop"
!insertmacro MUI_PAGE_FINISH

; Uninstaller Pages
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; Languages
!insertmacro MUI_LANGUAGE "English"

Section "Adult Hub Desktop Core" SecCore
    SetOutPath "$INSTDIR"

    ; Copy binaries and bundled dependencies
    File "${BINDIR}/adult-hub-desktop.exe"
    File "${BINDIR}/libmpv-2.dll"
    File "${SRCDIR}/desktop/assets/adult-hub-desktop.ico"
    File "${SRCDIR}/NOTICES.txt"
    File "${SRCDIR}/LICENSE"

    File /nonfatal "${BINDIR}/sample.mp4"
    File /nonfatal "${BINDIR}/README.txt"

    ; Registry install location
    WriteRegStr HKCU "Software\AdultHubDesktop" "InstallDir" "$INSTDIR"

    ; Uninstaller
    WriteUninstaller "$INSTDIR\Uninstall.exe"

    ; Shortcuts - Start Menu
    CreateDirectory "$SMPROGRAMS\Adult Hub Desktop"
    CreateShortcut "$SMPROGRAMS\Adult Hub Desktop\Adult Hub Desktop.lnk" "$INSTDIR\adult-hub-desktop.exe" "" "$INSTDIR\adult-hub-desktop.ico" 0
    CreateShortcut "$SMPROGRAMS\Adult Hub Desktop\Uninstall.lnk" "$INSTDIR\Uninstall.exe" "" "$INSTDIR\Uninstall.exe" 0

    ; Desktop shortcut
    CreateShortcut "$DESKTOP\Adult Hub Desktop.lnk" "$INSTDIR\adult-hub-desktop.exe" "" "$INSTDIR\adult-hub-desktop.ico" 0

    ; Windows Add/Remove Programs integration
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "DisplayName" "Adult Hub Desktop"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "DisplayVersion" "0.1.0"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "Publisher" "Adult Hub"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "DisplayIcon" "$INSTDIR\adult-hub-desktop.ico"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "InstallLocation" "$INSTDIR"
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "NoRepair" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop" "EstimatedSize" 75000
SectionEnd

Section "Uninstall"
    ; Remove shortcuts
    Delete "$DESKTOP\Adult Hub Desktop.lnk"
    Delete "$SMPROGRAMS\Adult Hub Desktop\Adult Hub Desktop.lnk"
    Delete "$SMPROGRAMS\Adult Hub Desktop\Uninstall.lnk"
    RMDir "$SMPROGRAMS\Adult Hub Desktop"

    ; Remove files
    Delete "$INSTDIR\adult-hub-desktop.exe"
    Delete "$INSTDIR\libmpv-2.dll"
    Delete "$INSTDIR\sample.mp4"
    Delete "$INSTDIR\README.txt"
    Delete "$INSTDIR\NOTICES.txt"
    Delete "$INSTDIR\LICENSE"
    Delete "$INSTDIR\adult-hub-desktop.ico"
    Delete "$INSTDIR\Uninstall.exe"

    ; Remove registry keys
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AdultHubDesktop"
    DeleteRegKey HKCU "Software\AdultHubDesktop"

    ; Remove directory
    RMDir "$INSTDIR"
SectionEnd
