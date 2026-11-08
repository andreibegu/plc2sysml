package conveyor_driveControl {
    private import TransferCellControl::ControlStep;
    private import TransferCell::Cell;
    private alias motor for TransferCell::cell::conveyor::drive::motor;

    part def Control {
        in item atEntry : TransferCellSignalTypes::BooleanSignal;
        in item atExit : TransferCellSignalTypes::BooleanSignal;

        in attribute stored_permit : ScalarValues::Boolean;
        dependency from stored_permit
            to Cell::conveyor::doorSwitch::doorClosed;

        exhibit state sequence {
            entry; then Waiting;

            state Waiting;
            transition first Waiting
                accept ControlStep
                if atEntry.value and stored_permit
                then Transfer;

            state Transfer;
            transition first Transfer
                accept ControlStep
                if atExit.value
                then Waiting;
        }

        dependency 'Possible influences for motor'
            from motor
            to sequence::Transfer;
    }
}
