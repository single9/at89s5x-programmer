; Blink an LED on P1.0 (pin 1) of an AT89S51/52, ~0.5 s per toggle at 12 MHz.
; P1 has internal pull-ups but can only source a little current, so wire the
; LED active-low: VCC -- ~330 ohm -- LED anode, LED cathode -- P1.0.
        ORG 0000H
MAIN:   CPL  P1.0          ; B2 90
        LCALL DELAY        ; 12 00 07
        SJMP MAIN          ; 80 F9

DELAY:  MOV  R7,#2         ; 7F 02
D1:     MOV  R6,#0         ; 7E 00
D2:     MOV  R5,#0         ; 7D 00
D3:     DJNZ R5,D3         ; DD FE
        DJNZ R6,D2         ; DE FA
        DJNZ R7,D1         ; DF F6
        RET                ; 22
        END
