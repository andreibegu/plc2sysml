package ConveyorSignalsDependencies {
    private import TransferCell::Cell;

    dependency 'Possible inputs for drive'
        from Cell::conveyor::drive::motor
        to Cell::conveyor::doorSwitch::doorClosed,
             Cell::conveyor::downstreamSensor::downstreamBusy,
             Cell::conveyor::entrySensor::atEntry,
             Cell::conveyor::exitSensor::atExit;
}
