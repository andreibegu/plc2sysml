FUNCTION_BLOCK TransferControl
VAR_INPUT
    atEntry, atExit, doorClosed, downstreamFree, exitClear : BOOL;
END_VAR
VAR_OUTPUT motor : BOOL; END_VAR
VAR permit, ready, active : BOOL; END_VAR

ReadPermit();
CheckReady();
Sequence();
IF active THEN
    motor := NOT atExit;
ELSE
    motor := FALSE;
END_IF;
END_FUNCTION_BLOCK
