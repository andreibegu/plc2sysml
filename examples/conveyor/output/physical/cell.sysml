package CellStructure {
    part def Sensor;
    part def DoorSwitch;
    part def Drive;
    part def Conveyor {
        part entrySensor : Sensor;
        part exitSensor : Sensor;
        part downstreamSensor : Sensor;
        part doorSwitch : DoorSwitch;
        part drive : Drive;
    }
    part def Cell {
        part conveyor : Conveyor;
    }
}
