use super::types as values;
use super::*;
pub(super) fn add(
    project: &Program,
    analysis: &Analysis,
    context: &LoadedContext,
    endpoints: &[Vec<Vec<String>>],
    model: &mut SystemModel,
) {
    let enums = values::enums(project, &model.signal_type_package);
    let mapped: BTreeMap<_, _> = context
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| (c.storage.key(), i))
        .collect();

    let channel_types: Vec<_> = context
        .channels
        .iter()
        .map(|channel| {
            channel.item_type.clone().unwrap_or_else(|| {
                format!(
                    "{}::{}",
                    model.signal_type_package,
                    channel
                        .value_type
                        .as_ref()
                        .map(|v| format!("{v}Signal"))
                        .unwrap_or("Signal".into())
                )
            })
        })
        .collect();
    let mut packages = BTreeSet::new();

    for machine in &analysis.instances {
        let commands: Vec<_> = context
            .channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.role == IoRole::Command)
            .filter(|(_, c)| {
                reaches(
                    analysis,
                    &c.storage.key(),
                    &machine.id,
                    &machine.selector,
                    &mut BTreeSet::new(),
                    0,
                )
            })
            .map(|(i, _)| i)
            .collect();

        if commands.is_empty() {
            continue;
        }

        let receivers: BTreeSet<_> = commands
            .iter()
            .flat_map(|i| {
                context.channels[*i]
                    .targets
                    .iter()
                    .map(|t| t.physical.clone())
            })
            .collect();

        let label = if machine.instance.is_some() && receivers.len() == 1 {
            receivers.first().unwrap().replace('.', "_")
        } else {
            machine.name.replace('.', "_")
        };

        let mut package = format!("{}Control", identifier(&label));

        if !packages.insert(package.clone()) {
            let base = package.clone();
            let mut suffix = packages.len() + 1;

            loop {
                package = format!("{base}_{suffix}");

                if packages.insert(package.clone()) {
                    break;
                }

                suffix += 1;
            }
        }

        let mut usage = identifier(&label);
        let mut suffix = 2;

        while model.sequences.iter().any(|s| s.usage == usage) {
            usage = format!("{}_{suffix}", identifier(&label));
            suffix += 1;
        }

        let mut graph = machine.graph.clone();
        let background = background(&graph);
        let mut names = BTreeMap::new();
        let mut used = BTreeSet::new();

        for state in &graph.states {
            let label = if let Some(background) = &background
                && state.name != graph.initial
            {
                state
                    .active_steps
                    .iter()
                    .filter(|s| !background.states.contains(s))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("_and_")
            } else {
                state.name.replace(" + ", "_and_")
            };

            let base = identifier(&label);
            let mut label = base.clone();
            let mut suffix = 2;

            while !used.insert(label.clone()) {
                label = format!("{base}_{suffix}");
                suffix += 1;
            }

            names.insert(state.name.clone(), label);
        }

        let mut projection = expressions::Projection {
            source: project,
            analysis,
            context,
            endpoints,
            mapped: &mapped,
            machine,
            names: &names,
            enums: &enums,
            channels: &channel_types,
            inputs: BTreeMap::new(),
            steps: 0,
            derived: vec![],
        };

        let transitions = graph
            .transitions
            .iter_mut()
            .zip(&machine.conditions)
            .try_for_each(|(transition, condition)| {
                transition.condition = projection.guard(condition, Some(&transition.from))?;
                Some(())
            });

        if transitions.is_none() {
            continue;
        }

        graph.reset = if let Some(reset) = &machine.reset {
            let Some(condition) = projection.guard(reset, None) else {
                continue;
            };
            (condition.as_boolean() != Some(false)).then_some(condition)
        } else {
            None
        };

        graph
            .transitions
            .retain(|t| t.condition.as_boolean() != Some(false));

        for state in &mut graph.states {
            state.name = names[&state.name].clone();
        }

        graph.initial = names[&graph.initial].clone();

        for t in &mut graph.transitions {
            t.from = names[&t.from].clone();
            t.to = names[&t.to].clone();
        }

        let mut calculations = vec![];
        let mut outputs = vec![];

        for &command in &commands {
            let channel = &context.channels[command];

            let Some(expression) = analysis
                .cells
                .get(&channel.storage.key())
                .and_then(|c| c.expression.as_ref())
            else {
                continue;
            };

            let Some(ty) = &channel.value_type else {
                continue;
            };

            let base = identifier(&format!("{}_value", channel.name.replace(' ', "_")));
            let start = calculations.len();
            let mut state_dependent = machine.selector.is_some();

            if !state_dependent {
                let stops = mapped.keys().cloned().collect();
                analysis
                    .slice(&channel.storage.key(), None, &stops)
                    .visit(&mut |e| {
                        if let ControlExpr::Active {
                            machine: source, ..
                        } = e
                        {
                            state_dependent |= source == &machine.id;
                        }
                    });
            }

            let states: Vec<_> = if state_dependent {
                machine
                    .graph
                    .states
                    .iter()
                    .map(|s| Some(s.name.as_str()))
                    .collect()
            } else {
                vec![None]
            };

            for state in states {
                let Some(calc) = projection.calculation(
                    &base,
                    expression,
                    ty,
                    endpoints[command].clone(),
                    state,
                ) else {
                    continue;
                };

                let forwards_input = matches!(
                    &calc.expression,
                    ControlExpr::Read { key, .. }
                        if projection.inputs.values().any(|i| i.name == *key)
                );

                if state_dependent || !forwards_input {
                    calculations.push(calc);
                }
            }

            if calculations.len() == start {
                continue;
            }

            if machine.selector.is_some()
                && calculations.len() - start == machine.graph.states.len()
            {
                let mut name = identifier(&channel.name);
                let original = name.clone();
                let mut suffix = 2;

                while projection.inputs.values().any(|i| i.name == name)
                    || projection.derived.iter().any(|d| d.name == name)
                    || outputs.iter().any(|o: &ControlOutput| o.name == name)
                {
                    name = format!("{original}_{suffix}");
                    suffix += 1;
                }

                for calc in &mut calculations[start..] {
                    calc.output = Some(name.clone());
                }

                outputs.push(ControlOutput {
                    name,
                    item_type: channel_types[command].clone(),
                    endpoints: endpoints[command].clone(),
                });
            } else if calculations.len() - start == machine.graph.states.len()
                && calculations[start..]
                    .iter()
                    .all(|c| c.expression == calculations[start].expression)
            {
                let mut calculation = calculations[start].clone();
                calculation.state = None;
                calculations.truncate(start);
                calculations.push(calculation);
            }
        }

        let mut state_commands = BTreeMap::<String, BTreeSet<Vec<String>>>::new();

        for (key, cell) in &analysis.cells {
            let Some(expression) = &cell.expression else {
                continue;
            };

            let mut activity = false;
            expression.visit(&mut |e| {
                if let ControlExpr::Active { machine: id, .. } = e {
                    activity |= id == &machine.id;
                }
            });

            if !activity || !matches!(cell.storage.ty.base.as_str(), "BOOL" | "BIT") {
                continue;
            }

            for state in &machine.graph.states {
                let simplified =
                    analysis.slice(key, Some((machine, &state.name)), &BTreeSet::new());

                if simplified.truth() == Some(false) {
                    continue;
                }

                for &command in &commands {
                    if analysis.depends(
                        &context.channels[command].storage.key(),
                        key,
                        Some((machine, &state.name)),
                    ) {
                        state_commands
                            .entry(names[&state.name].clone())
                            .or_default()
                            .extend(endpoints[command].clone());
                    }
                }
            }
        }

        let contributions = state_commands
            .into_iter()
            .map(|(state, commands)| StateContribution {
                state,
                commands: commands.into_iter().collect(),
            })
            .collect();

        projection.memory();

        let inputs = std::mem::take(&mut projection.inputs)
            .into_values()
            .collect();

        model.sequences.push(ControlSequence {
            source_id: machine.id.clone(),
            usage,
            package,
            graph,
            inputs,
            background: background.map(|mut b| {
                b.initial_position = names[&b.initial_position].clone();
                b.states = b.states.iter().map(|s| identifier(s)).collect();
                b
            }),
            calculations,
            contributions,
            derived: projection.derived,
            outputs,
        });
    }

    let mut owners = BTreeMap::<Vec<String>, BTreeSet<String>>::new();

    for sequence in &model.sequences {
        for output in &sequence.outputs {
            for endpoint in &output.endpoints {
                owners
                    .entry(endpoint.clone())
                    .or_default()
                    .insert(sequence.source_id.clone());
            }
        }
    }

    for sequence in &mut model.sequences {
        let ambiguous: BTreeSet<_> = sequence
            .outputs
            .iter()
            .filter(|o| o.endpoints.iter().any(|p| owners[p].len() > 1))
            .map(|o| o.name.clone())
            .collect();

        if !ambiguous.is_empty() {
            sequence.outputs.retain(|o| !ambiguous.contains(&o.name));

            for calculation in &mut sequence.calculations {
                if calculation
                    .output
                    .as_ref()
                    .is_some_and(|o| ambiguous.contains(o))
                {
                    calculation.output = None;
                }
            }
        }
    }

    for source in enums.values() {
        if model
            .sequences
            .iter()
            .flat_map(|s| &s.inputs)
            .any(|i| i.value_type == source.name)
        {
            model.enum_types.insert(
                source.name.rsplit("::").next().unwrap().into(),
                source.values.values().cloned().collect(),
            );
        }
    }

    let mut states = BTreeMap::new();

    for sequence in &model.sequences {
        let machine = analysis
            .instances
            .iter()
            .find(|m| m.id == sequence.source_id)
            .expect("control instance");

        for (original, state) in machine.graph.states.iter().zip(&sequence.graph.states) {
            let mut path = vec![
                model.package.clone(),
                model.root_instance_name(),
                "control".into(),
                sequence.usage.clone(),
            ];
            path.extend(sequence.state_path(&state.name));
            states.insert((machine.id.clone(), original.name.clone()), path);
        }
    }

    for sequence in &mut model.sequences {
        for input in &mut sequence.inputs {
            if let Some((machine, names)) = &input.state_source {
                input.state_links = names
                    .iter()
                    .filter_map(|s| states.get(&(machine.clone(), s.clone())).cloned())
                    .collect();

                if input.state_links.len() != names.len() {
                    input.state_links.clear();
                }
            }
        }
    }

    for sequence in &mut model.sequences {
        sequence.prune();
    }
}

fn reaches(
    analysis: &Analysis,
    key: &str,
    machine: &str,
    selector: &Option<String>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> bool {
    if depth > 64 || !seen.insert(key.into()) {
        return false;
    }

    if selector.as_deref() == Some(key) {
        return true;
    }

    let Some(cell) = analysis.cells.get(key) else {
        return false;
    };

    let Some(expression) = &cell.expression else {
        return cell
            .dependencies
            .iter()
            .any(|k| reaches(analysis, k, machine, selector, seen, depth + 1));
    };

    let mut result = false;
    expression.visit(&mut |e| match e {
        ControlExpr::Active { machine: id, .. } => result |= id == machine,

        ControlExpr::Read { key, .. } => {
            result |= reaches(analysis, key, machine, selector, seen, depth + 1)
        }

        _ => {}
    });

    result
}

fn background(graph: &StateGraph) -> Option<ConcurrentRegion> {
    let positions: Vec<_> = graph
        .states
        .iter()
        .filter(|s| s.name != graph.initial)
        .collect();
    let first = positions.first()?;
    let mut common: BTreeSet<_> = first.active_steps.iter().cloned().collect();

    for position in &positions {
        common.retain(|s| position.active_steps.contains(s));
    }

    if common.is_empty()
        || positions
            .iter()
            .any(|s| s.active_steps.len() <= common.len())
        || graph
            .states
            .iter()
            .find(|s| s.name == graph.initial)?
            .active_steps
            .iter()
            .any(|s| common.contains(s))
        || graph.transitions.iter().any(|t| t.to == graph.initial)
    {
        return None;
    }

    let starts: BTreeSet<_> = graph
        .transitions
        .iter()
        .filter(|t| t.from == graph.initial)
        .map(|t| t.to.clone())
        .collect();
    let starts: Vec<_> = starts.into_iter().collect();

    let [initial_position] = starts.as_slice() else {
        return None;
    };

    Some(ConcurrentRegion {
        states: common.into_iter().collect(),
        initial_position: initial_position.clone(),
    })
}
