use super::*;

impl Engine<'_> {
    pub(super) fn application(&mut self) -> Vec<Influence> {
        let programs: Vec<_> = self
            .model
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Program && u.owner.is_none())
            .collect();
        let mut tasks = vec![];

        if self.model.tasks.is_empty() {
            for &program in &programs {
                tasks.push(vec![program]);
            }
        } else {
            for task in &self.model.tasks {
                let mut calls = vec![];

                for entry in &task.programs {
                    if let Some(program) = programs
                        .iter()
                        .find(|u| Some(&u.id) == entry.target.as_ref())
                    {
                        calls.push(*program);
                    }
                }

                if !calls.is_empty() {
                    tasks.push(calls);
                }
            }
        }

        let mut initial_state = State::default();
        let mut inputs = BTreeSet::new();
        let mut outputs = BTreeSet::new();

        for unit in self
            .model
            .units
            .iter()
            .filter(|u| matches!(u.kind, UnitKind::Program | UnitKind::GlobalVariables))
        {
            for variable in &unit.variables {
                if !variable.ty.dimensions.is_empty() || variable.ty.indirection.is_some() {
                    continue;
                }

                let key = self.cell(StorageRef {
                    unit: unit.id.clone(),
                    path: vec![variable.name.clone()],
                    ty: variable.ty.clone(),
                    source: variable.source.clone(),
                });

                let address = variable
                    .address
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_uppercase();

                if IoDirection::from_address(&address) == Some(IoDirection::Input) {
                    inputs.insert(key.clone());
                    initial_state.values.insert(key.clone(), Value::entry(&key));
                } else {
                    if IoDirection::from_address(&address) == Some(IoDirection::Output) {
                        outputs.insert(key.clone());
                    }

                    initial_state.values.insert(key, initial(variable));
                }
            }
        }

        for boundary in self.boundaries {
            let key = self.cell(boundary.storage.clone());

            match boundary.direction {
                IoDirection::Input => {
                    inputs.insert(key.clone());
                    initial_state.values.insert(key.clone(), Value::entry(&key));
                }

                IoDirection::Output => {
                    outputs.insert(key);
                }
            }
        }

        let mut boundary = initial_state.clone();

        for _ in 0..128 {
            let mut entry = boundary.clone();

            for (key, value) in &mut entry.values {
                if inputs.contains(key) {
                    *value = Value::entry(key);
                }
            }

            let mut ending = initial_state.clone();

            for task in &tasks {
                let mut state = entry.clone();

                for program in task {
                    let summary = self.summary(program);
                    self.apply(
                        program,
                        &summary,
                        &super::transfer::Invocation {
                            strong: true,
                            ..Default::default()
                        },
                        &mut state,
                    );
                }

                ending.join(&state);
            }

            if ending == boundary {
                break;
            }

            boundary = ending;
        }

        let mut influences = vec![];

        for output in outputs {
            let value = boundary.read(&output);
            for input in &value.origins {
                if inputs.contains(input) {
                    influences.push(Influence {
                        supplier: self.cells[input].clone(),
                        dependent: self.cells[&output].clone(),
                    });
                }
            }
        }

        influences
    }
}
