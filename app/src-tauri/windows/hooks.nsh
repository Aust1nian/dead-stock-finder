; Dead Stock Finder installer hooks (Tauri NSIS, current-user mode).
;
; Install or upgrade: put the background check back exactly as it was
; (re-registered only if the owner had it on).
; Uninstall: remove the background check. Tauri's uninstall page asks
; whether to delete app data (its "Delete the application data" box, which
; removes %LOCALAPPDATA%\com.tightline.deadstockfinder, the cache); when
; ticked, the Square connection is deleted from Credential Manager too.
; Upgrades run the old uninstaller with /UPDATE and keep everything.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\dead-stock-finder.exe" --schedule restore'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    ${If} $DeleteAppDataCheckboxState = 1
      nsExec::Exec '"$INSTDIR\dead-stock-finder.exe" --uninstall --remove-data'
    ${Else}
      nsExec::Exec '"$INSTDIR\dead-stock-finder.exe" --uninstall'
    ${EndIf}
    Pop $0
  ${EndIf}
!macroend
