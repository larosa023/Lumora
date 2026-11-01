; Lumora's NSIS installer hooks.
;
; Lumora's 32-bit USBXpress companion runs from the install folder while
; Lumora reaches a Corsair Hydro Pro cooler. Lumora stops it before an update,
; but a copy left behind (say, after Lumora was force-closed) would block the
; installer from replacing the file, so the installer stops it first.

!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM lumora-usbxpress.exe'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM lumora-usbxpress.exe'
  Pop $0
!macroend
