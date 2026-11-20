package conveyor_driveControl {
    private import TransferCellControl::ControlStep;
    private alias motor for TransferCell::cell::conveyor::drive::motor;

    part def Control {
        in item atEntry : TransferCellSignalTypes::BooleanSignal;
        in item atExit : TransferCellSignalTypes::BooleanSignal;
        in item doorClosed : TransferCellSignalTypes::BooleanSignal;
        in item downstreamBusy : TransferCellSignalTypes::BooleanSignal;

        derived attribute ready : ScalarValues::Boolean =
            atEntry.value and doorClosed.value and not downstreamBusy.value and not atExit.value;

        exhibit state sequence {
            entry; then Waiting;

            state Waiting;
            transition first Waiting
                accept ControlStep
                if ready
                then Transfer;

            state Transfer;
            transition first Transfer
                accept ControlStep
                if atExit.value
                then ClearExit;

            state ClearExit;
            transition first ClearExit
                accept ControlStep
                if not atExit.value
                then Waiting;
        }

        dependency 'Possible influences for motor'
            from motor
            to sequence::Transfer;
    }
}
