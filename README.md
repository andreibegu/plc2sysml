# plc2sysml

plc2sysml is a prototype for deriving signal dependencies and controller
behaviour from mixed-language PLC projects and adding them to a SysML v2 model.
It handles Structured Text and CODESYS XML, including CFC, Ladder/FBD and SFC.

Given a structural model and associations between the program's inputs and
outputs and its physical components, the tool adds typed signals,
flows, possible influences on commands and selected controller states and
conditions.

## Try the conveyor example

With Rust and Cargo installed, run this from the repository root:

```sh
cargo run -- generate --input examples/conveyor/source \
  --context examples/conveyor/context.json \
  --output examples/conveyor/output
```

The [source inputs](examples/conveyor/source) describe a conveyor with entry and
exit sensors, a door switch and a motor.

The [context file](examples/conveyor/context.json) supplies the signal
associations and points to the [physical structure](examples/conveyor/physical).
Open the `.sysml` files under [output](examples/conveyor/output) together in a
SysML v2 tool, starting with `model.sysml`.

## Use your own project

Export the PLC sources as ST or native CODESYS XML. Given a folder of SysML
structural definitions, generate a starting association file:

```sh
cargo run -- context-template --input path/to/source \
  --physical-model path/to/structure --output context.json
```

Fill in the physical targets for the signals you want to include, then run:

```sh
cargo run -- generate --input path/to/source \
  --context context.json --output model
```

The physical-model path is relative to the context file. The draft includes
variables with hardware I/O addresses; other signals can be added manually.
Generation replaces the contents of the output folder. Keep manual edits in the
structural inputs instead.
