; Session-Loom NSIS installer hooks.
;
; Registers "<install-dir>\bin" on the current user's PATH so the bundled
; `ssl` CLI can be used from any terminal right after installation, and
; removes it again on uninstall.
;
; The string scanning is implemented with plain NSIS instructions + LogicLib
; (no StrFunc dependency), so the exact same code runs unmodified inside both
; the installer and the uninstaller sections.

!include "LogicLib.nsh"
!include "WinMessages.nsh"

; ---------------------------------------------------------------------------
; SL_PathContainsBin
;   in : $0 = current PATH value
;   out: $1 = "1" when "$INSTDIR\bin" is an exact PATH segment, "" otherwise
; ---------------------------------------------------------------------------
!macro SL_PathContainsBin
  Push $2
  Push $3
  Push $4
  Push $5
  Push $R0
  Push $R1
  StrCpy $1 ""
  StrCpy $2 ";$0;"              ; normalize to ";a;b;" for exact-segment match
  StrCpy $3 ";$INSTDIR\bin;"
  StrLen $4 $2
  StrLen $5 $3
  ${If} $5 <= $4
    IntOp $4 $4 - $5            ; last valid start index
    ${For} $R0 0 $4
      StrCpy $R1 $2 $5 $R0
      ${If} $R1 == $3
        StrCpy $1 "1"
        ${Break}
      ${EndIf}
    ${Next}
  ${EndIf}
  Pop $R1
  Pop $R0
  Pop $5
  Pop $4
  Pop $3
  Pop $2
!macroend

; ---------------------------------------------------------------------------
; SL_PathRemoveBin
;   in/out: $0 = PATH value; removes our bin entry (first match), absorbing a
;   preceding ';' when present, and strips a leftover leading ';'.
;   Uses $3 $4 $5 and $R0-$R6 (saved/restored); never touches $1 or $2.
; ---------------------------------------------------------------------------
!macro SL_PathRemoveBin
  Push $3
  Push $4
  Push $5
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5
  Push $R6
  StrCpy $3 "$INSTDIR\bin"
  StrLen $4 $0
  StrLen $5 $3
  ${If} $5 <= $4
    IntOp $4 $4 - $5
    ${For} $R0 0 $4
      StrCpy $R1 $0 $5 $R0
      ${If} $R1 == $3
        ; $R0 = entry start, $5 = entry length
        StrCpy $R2 $R0          ; span start
        IntOp $R3 $5 + 0        ; span length
        ${If} $R0 > 0
          IntOp $R4 $R0 - 1
          StrCpy $R5 $0 1 $R4
          ${If} $R5 == ";"      ; absorb the separator before the entry
            StrCpy $R2 $R4
            IntOp $R3 $5 + 1
          ${EndIf}
        ${EndIf}
        StrCpy $R4 $0 $R2       ; part before the removed span
        IntOp $R5 $R2 + $R3
        StrCpy $R6 $0 "" $R5    ; part after the removed span
        StrCpy $0 "$R4$R6"
        ${Break}
      ${EndIf}
    ${Next}
  ${EndIf}
  ; strip a leftover leading ';' (entry used to be the first segment)
  StrCpy $R0 $0 1
  ${If} $R0 == ";"
    StrCpy $0 $0 "" 1
  ${EndIf}
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
  Pop $5
  Pop $4
  Pop $3
!macroend

!macro SL_NotifyEnvironmentChange
  ; Let new terminal windows pick up the updated PATH.
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  ReadRegStr $0 HKCU "Environment" "Path"
  !insertmacro SL_PathContainsBin
  ${If} $1 == ""
    ${If} $0 == ""
      StrCpy $0 "$INSTDIR\bin"
    ${Else}
      StrCpy $0 "$0;$INSTDIR\bin"
    ${EndIf}
    WriteRegExpandStr HKCU "Environment" "Path" "$0"
    !insertmacro SL_NotifyEnvironmentChange
    DetailPrint "Added $INSTDIR\bin to the user PATH"
  ${EndIf}
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Push $0
  Push $2
  ReadRegStr $0 HKCU "Environment" "Path"
  ${If} $0 != ""
    StrCpy $2 $0
    !insertmacro SL_PathRemoveBin
    ${If} $0 != $2
      WriteRegExpandStr HKCU "Environment" "Path" "$0"
      !insertmacro SL_NotifyEnvironmentChange
      DetailPrint "Removed $INSTDIR\bin from the user PATH"
    ${EndIf}
  ${EndIf}
  Pop $2
  Pop $0
!macroend
