package ConveyorSignals {
    part def Conveyor :> CellStructure::Conveyor {
        part doorSwitch :>> doorSwitch {
            out item doorClosed : TransferCellSignalTypes::BooleanSignal;
        }

        part drive :>> drive {
            in item motor : TransferCellSignalTypes::BooleanSignal;
        }

        part entrySensor :>> entrySensor {
            out item atEntry : TransferCellSignalTypes::BooleanSignal;
        }

        part exitSensor :>> exitSensor {
            out item atExit : TransferCellSignalTypes::BooleanSignal;
        }
    }
}
