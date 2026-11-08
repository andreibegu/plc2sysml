FUNCTION_BLOCK TransferControl
VAR_INPUT
    atEntry, atExit, doorClosed : BOOL;
END_VAR
VAR_OUTPUT motor : BOOL; END_VAR
VAR permit, ready, active : BOOL; END_VAR

ReadPermit();
CheckReady();
Sequence();
motor := active AND NOT atExit;
END_FUNCTION_BLOCK
