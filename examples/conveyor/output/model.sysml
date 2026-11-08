package TransferCell {
    part def Cell :> CellStructure::Cell {
        part conveyor : ConveyorSignals::Conveyor :>> conveyor;
        part control : TransferCellControl::ControlLogic;

        flow from conveyor.entrySensor.atEntry to control.conveyor_drive.atEntry;
        flow from conveyor.exitSensor.atExit to control.conveyor_drive.atExit;
    }

    part cell : Cell;
}
