; Added to Tauri's installer script (bundle.windows.nsis.installerHooks).
;
; "Create desktop shortcut" on the last page of the installer starts
; unticked; tick it to get one. The Start menu entry is always made.
; Updates never make a desktop shortcut either way: the updater runs the
; installer with /UPDATE, which skips it.
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
