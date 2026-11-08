use super::expressions::{write_condition, write_equation, write_value};
use super::*;
use crate::ir::Expr;
use std::collections::VecDeque;

pub(super) fn render(
    model: &SystemModel,
    files: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for sequence in &model.sequences {
        let file = format!("behavior/{}.sysml", names::filename(&sequence.package));

        if files.contains_key(&file) {
            anyhow::bail!("Control sequence names produce the same output path: {file}");
        }

        let mut w = Writer::new();
        w.open(&format!("package {}", name(&sequence.package)));

        let step = step_reference(model, sequence);

        if step == "ControlStep" {
            w.line(&format!(
                "private import {}Control::ControlStep;",
                name(&model.package)
            ));
        }

        let prefix = model_imports(&mut w, model, sequence);
        let aliases = command_aliases(&mut w, model, sequence);
        w.blank();
        w.open("part def Control");
        write_inputs(&mut w, sequence, &prefix);
        write_values(&mut w, sequence, &prefix);
        w.blank();

        if !sequence.outputs.is_empty() || sequence.inputs.iter().any(|i| i.update.is_some()) {
            w.line(&format!(
                "ref part {} : Control = self;",
                name(&controller_reference(sequence))
            ));
            w.blank();
        }

        w.open("exhibit state sequence");

        if sequence.graph.reset.is_some() {
            w.line("entry; then Control;");
            w.open("state Control");
        }

        w.line(&format!("entry; then {};", name(&sequence.graph.initial)));

        if let Some(background) = &sequence.background {
            w.blank();
            write_state(&mut w, sequence, &sequence.graph.initial);

            for transition in sequence
                .graph
                .transitions
                .iter()
                .filter(|t| t.from == sequence.graph.initial)
            {
                write_normal(
                    &mut w,
                    transition,
                    "ConcurrentControl",
                    sequence.graph.reset.as_ref(),
                    sequence,
                    &step,
                );
            }

            w.open("state ConcurrentControl parallel");

            for state in &background.states {
                w.line(&format!("state {};", name(state)));
            }

            w.open("state Position");
            w.line(&format!(
                "entry; then {};",
                name(&background.initial_position)
            ));
        }

        let initial = sequence
            .background
            .as_ref()
            .map(|b| &b.initial_position)
            .unwrap_or(&sequence.graph.initial);
        let mut pending = VecDeque::from([initial.as_str()]);
        let mut seen = BTreeSet::new();

        loop {
            let state = pending.pop_front().or_else(|| {
                sequence
                    .graph
                    .states
                    .iter()
                    .map(|s| s.name.as_str())
                    .find(|s| {
                        !seen.contains(s)
                            && (sequence.background.is_none() || *s != sequence.graph.initial)
                    })
            });

            let Some(state) = state else {
                break;
            };

            if !seen.insert(state) {
                continue;
            }

            w.blank();
            write_state(&mut w, sequence, state);

            for transition in sequence
                .graph
                .transitions
                .iter()
                .filter(|t| t.from == state)
            {
                write_normal(
                    &mut w,
                    transition,
                    &transition.to,
                    sequence.graph.reset.as_ref(),
                    sequence,
                    &step,
                );
                pending.push_back(&transition.to);
            }

            if sequence
                .calculations
                .iter()
                .any(|c| c.state.as_deref() == Some(state) && c.output.is_some())
                || sequence.inputs.iter().any(|i| i.update.is_some())
            {
                let outgoing = sequence
                    .graph
                    .transitions
                    .iter()
                    .filter(|t| t.from == state)
                    .map(|t| t.condition.clone())
                    .reduce(|a, b| a.disjoin(b))
                    .unwrap_or_else(|| Expr::boolean(false));
                let condition = outgoing.negated();

                if condition.as_boolean() != Some(false) {
                    let transition = crate::ir::StateTransition {
                        from: state.into(),
                        to: state.into(),
                        condition,
                        source: crate::ir::Source::default(),
                    };

                    write_normal(
                        &mut w,
                        &transition,
                        state,
                        sequence.graph.reset.as_ref(),
                        sequence,
                        &step,
                    );
                }
            }
        }

        if sequence.background.is_some() {
            w.close();
            w.close();
        }

        if let Some(reset) = &sequence.graph.reset {
            w.close();
            write_reset(
                &mut w,
                reset,
                &["Control".into(), sequence.graph.initial.clone()],
                &sequence.inputs,
                &step,
            );
        }

        w.close();
        write_command_dependencies(&mut w, sequence, &aliases);
        write_calculation_dependencies(&mut w, model, sequence);
        w.close();
        w.close();
        files.insert(file, w.finish());
    }

    Ok(())
}

fn local_name(sequence: &ControlSequence, candidate: &str) -> bool {
    ["Control", "sequence"].contains(&candidate)
        || sequence.inputs.iter().any(|i| i.name == candidate)
        || sequence.derived.iter().any(|d| d.name == candidate)
        || sequence.graph.states.iter().any(|s| s.name == candidate)
        || sequence.calculations.iter().any(|c| c.name == candidate)
        || sequence.outputs.iter().any(|o| o.name == candidate)
}

fn controller_reference(sequence: &ControlSequence) -> String {
    let mut candidate = "controller".to_string();
    let mut index = 2;

    while local_name(sequence, &candidate) {
        candidate = format!("controller_{index}");
        index += 1;
    }

    candidate
}

fn command_aliases(
    w: &mut Writer,
    model: &SystemModel,
    sequence: &ControlSequence,
) -> BTreeMap<Vec<String>, String> {
    let commands: BTreeSet<_> = sequence
        .calculations
        .iter()
        .filter(|c| c.output.is_none())
        .flat_map(|c| c.commands.iter().cloned())
        .chain(
            sequence
                .contributions
                .iter()
                .flat_map(|c| c.commands.iter().cloned()),
        )
        .collect();
    let mut result = BTreeMap::new();
    let mut used = BTreeSet::new();

    for command in commands {
        let base = command.last().unwrap().clone();
        let mut label = base.clone();
        let mut index = 2;

        while local_name(sequence, &label) || !used.insert(label.clone()) {
            label = format!("{base} {index}");
            index += 1;
        }

        let path = [model.package.clone(), model.root_instance_name()]
            .into_iter()
            .chain(command.iter().cloned())
            .collect::<Vec<_>>();

        w.document(
            Doc::text(format!("private alias {} for", name(&label)))
                .append(
                    Doc::line()
                        .append(Doc::text(qualified(&path.join("::"))))
                        .nest(4),
                )
                .append(";")
                .group(),
        );
        result.insert(command, label);
    }

    result
}

fn step_reference(model: &SystemModel, sequence: &ControlSequence) -> String {
    if local_name(sequence, "ControlStep") {
        format!("{}Control::ControlStep", name(&model.package))
    } else {
        "ControlStep".into()
    }
}

fn physical_prefix(w: &mut Writer, model: &SystemModel, sequence: &ControlSequence) -> Vec<String> {
    if local_name(sequence, &model.root_name) {
        vec![model.package.clone(), model.root_name.clone()]
    } else {
        w.line(&format!(
            "private import {}::{};",
            name(&model.package),
            name(&model.root_name)
        ));

        vec![model.root_name.clone()]
    }
}

fn model_imports(w: &mut Writer, model: &SystemModel, sequence: &ControlSequence) -> Vec<String> {
    let prefix = if sequence
        .inputs
        .iter()
        .any(|i| !i.endpoints.is_empty() && i.item_type.is_none())
    {
        physical_prefix(w, model, sequence)
    } else {
        vec![]
    };

    if sequence.inputs.iter().any(|i| !i.state_links.is_empty()) {
        if !local_name(sequence, "isDuring") {
            w.line("private import OccurrenceFunctions::isDuring;");
        }

        let instance = model.root_instance_name();

        if sequence
            .inputs
            .iter()
            .flat_map(|i| &i.state_links)
            .any(|p| !own_state(p, sequence))
            && !local_name(sequence, &instance)
        {
            w.line(&format!(
                "private import {}::{};",
                name(&model.package),
                name(&instance)
            ));
        }
    }

    prefix
}

fn write_values(w: &mut Writer, sequence: &ControlSequence, prefix: &[String]) {
    for input in &sequence.inputs {
        if input.state_links.is_empty() {
            continue;
        }

        let calls = input.state_links.iter().map(|p| {
            Doc::text(format!(
                "{}({})",
                if local_name(sequence, "isDuring") {
                    "OccurrenceFunctions::isDuring"
                } else {
                    "isDuring"
                },
                occurrence_path(p, sequence)
            ))
        });

        let value = Doc::intersperse(calls, Doc::line().append("or "))
            .nest(4)
            .group();

        w.blank();
        write_value(
            w,
            &format!(
                "derived attribute {} : ScalarValues::Boolean",
                name(&input.name)
            ),
            value,
        );
        write_input_details(w, input, sequence, prefix);
    }

    for derived in &sequence.derived {
        w.blank();
        write_equation(
            w,
            &format!(
                "derived attribute {} : {}",
                name(&derived.name),
                value_type_name(&derived.value_type)
            ),
            &derived.expression,
            &sequence.inputs,
        );
    }

    for calculation in sequence.calculations.iter().filter(|c| c.state.is_none()) {
        w.blank();
        write_equation(
            w,
            &format!(
                "derived attribute {} : {}",
                name(&calculation.name),
                value_type_name(&calculation.value_type)
            ),
            &calculation.expression,
            &sequence.inputs,
        );
    }
}

fn write_command_dependencies(
    w: &mut Writer,
    sequence: &ControlSequence,
    aliases: &BTreeMap<Vec<String>, String>,
) {
    let mut inputs = BTreeMap::<Vec<String>, BTreeSet<Vec<String>>>::new();

    for calculation in sequence.calculations.iter().filter(|c| c.output.is_none()) {
        let mut path = calculation
            .state
            .as_ref()
            .map(|s| sequence.state_path(s))
            .unwrap_or_default();

        path.push(calculation.name.clone());

        for command in &calculation.commands {
            inputs
                .entry(command.clone())
                .or_default()
                .insert(path.clone());
        }
    }

    for contribution in &sequence.contributions {
        for command in &contribution.commands {
            inputs
                .entry(command.clone())
                .or_default()
                .insert(sequence.state_path(&contribution.state));
        }
    }

    let states: BTreeSet<_> = sequence
        .graph
        .states
        .iter()
        .map(|s| sequence.state_path(&s.name))
        .collect();
    let mut groups = BTreeMap::<BTreeSet<Vec<String>>, Vec<Vec<String>>>::new();

    for (command, mut targets) in inputs {
        if states.is_subset(&targets) {
            targets.retain(|p| !states.contains(p));
            targets.insert(vec!["sequence".into()]);
        }

        groups.entry(targets).or_default().push(command);
    }

    let mut labels = BTreeSet::new();

    for (targets, commands) in groups {
        let endpoints = commands
            .iter()
            .filter_map(|p| p.last())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let base = format!("Possible influences for {endpoints}");
        let mut label = base.clone();
        let mut index = 2;

        while !labels.insert(label.clone()) {
            label = format!("{base} {index}");
            index += 1;
        }

        w.blank();
        w.line(&format!("dependency {}", name(&label)));

        let commands = commands
            .iter()
            .map(|p| vec![aliases[p].clone()])
            .collect::<Vec<_>>();

        write_references(w, "from", &commands, &[], "");
        write_references(w, "to", &targets.into_iter().collect::<Vec<_>>(), &[], ";");
    }
}

fn write_inputs(w: &mut Writer, sequence: &ControlSequence, prefix: &[String]) {
    let mut inputs: Vec<_> = sequence
        .inputs
        .iter()
        .filter(|i| i.state_links.is_empty())
        .collect();

    inputs.sort_by_key(|i| (i.item_type.is_none(), &i.name));

    let mut previous = None;

    for input in inputs {
        let item = input.item_type.is_some();
        let details = input.bounds.is_some() || !input.endpoints.is_empty() && !item;

        if previous
            .is_some_and(|(was_item, had_details)| was_item != item || had_details || details)
        {
            w.blank();
        }

        if let Some(ty) = &input.item_type {
            w.typed(
                &format!("in item {}", name(&input.name)),
                &qualified(ty),
                "",
            );
        } else {
            let ty = value_type_name(&input.value_type);

            let default = input
                .default
                .as_ref()
                .map(|v| {
                    let v = super::expressions::direct_formula(v, &sequence.inputs);

                    if input.update.is_some() {
                        format!(" := {v}")
                    } else {
                        format!(" default = {v}")
                    }
                })
                .unwrap_or_default();

            let direction = if input.update.is_some() { "" } else { "in " };

            w.typed(
                &format!("{direction}attribute {}", name(&input.name)),
                &ty,
                &default,
            );
        }

        write_input_details(w, input, sequence, prefix);
        previous = Some((item, details));
    }

    if !sequence.outputs.is_empty() {
        w.blank();
    }

    for output in &sequence.outputs {
        w.typed(
            &format!("out item {}", name(&output.name)),
            &qualified(&output.item_type),
            "",
        );
    }
}

fn write_input_details(
    w: &mut Writer,
    input: &StateInput,
    sequence: &ControlSequence,
    prefix: &[String],
) {
    if let Some((min, max)) = &input.bounds {
        let value = if input.item_type.is_some() {
            format!("{}.value", name(&input.name))
        } else {
            name(&input.name)
        };

        let min = super::expressions::direct_formula(min, &sequence.inputs);
        let max = super::expressions::direct_formula(max, &sequence.inputs);
        w.range(&value, &min, &max);
    }

    if !input.endpoints.is_empty() && input.item_type.is_none() {
        w.line(&format!("dependency from {}", name(&input.name)));
        write_references(w, "to", &input.endpoints, prefix, ";");
    }
}

fn own_state(path: &[String], sequence: &ControlSequence) -> bool {
    path.get(3) == Some(&sequence.usage)
}

fn occurrence_path(path: &[String], sequence: &ControlSequence) -> String {
    if own_state(path, sequence) {
        return feature_path(&path[4..]);
    }

    format!(
        "{}{}",
        if local_name(sequence, &path[1]) {
            format!("{}::{}", name(&path[0]), name(&path[1]))
        } else {
            name(&path[1])
        },
        path[2..]
            .iter()
            .map(|p| format!(".{}", name(p)))
            .collect::<String>()
    )
}

fn write_state(w: &mut Writer, sequence: &ControlSequence, state: &str) {
    let equations: Vec<_> = sequence
        .calculations
        .iter()
        .filter(|c| c.state.as_deref() == Some(state))
        .collect();
    let updates = equations.iter().any(|c| c.output.is_some())
        || sequence.inputs.iter().any(|i| i.update.is_some());

    if equations.is_empty() && !updates {
        w.line(&format!("state {};", name(state)));
        return;
    }

    w.open(&format!("state {}", name(state)));

    for equation in equations {
        write_equation(
            w,
            &format!(
                "derived attribute {} : {}",
                name(&equation.name),
                value_type_name(&equation.value_type)
            ),
            &equation.expression,
            &sequence.inputs,
        );
    }

    if updates {
        w.blank();
        write_update(w, sequence, state);
    }

    w.close();
}

fn write_update(w: &mut Writer, sequence: &ControlSequence, state: &str) {
    w.open("exit action");

    let mut captures = vec![];
    let mut names = BTreeSet::new();
    let mut snapshot = |label: &str| {
        let base = format!("next_{label}");
        let mut name = base.clone();
        let mut index = 2;

        while local_name(sequence, &name) || !names.insert(name.clone()) {
            name = format!("{base}_{index}");
            index += 1;
        }

        name
    };

    for calculation in sequence
        .calculations
        .iter()
        .filter(|c| c.state.as_deref() == Some(state) && c.output.is_some())
    {
        let output = calculation.output.as_ref().expect("calculated output");
        let next = snapshot(output);
        w.line(&format!(
            "in attribute {} : {} = {};",
            name(&next),
            value_type_name(&calculation.value_type),
            name(&calculation.name)
        ));
        captures.push(format!(
            "{}.{}.value := {}",
            name(&controller_reference(sequence)),
            name(output),
            name(&next)
        ));
    }

    for input in sequence.inputs.iter().filter(|i| i.update.is_some()) {
        let next = snapshot(&input.name);
        write_equation(
            w,
            &format!(
                "in attribute {} : {}",
                name(&next),
                value_type_name(&input.value_type)
            ),
            input.update.as_ref().expect("retained update"),
            &sequence.inputs,
        );
        captures.push(format!(
            "{}.{} := {}",
            name(&controller_reference(sequence)),
            name(&input.name),
            name(&next)
        ));
    }

    for assignment in captures {
        w.line(&format!("assign {assignment};"));
    }

    w.close();
}

fn write_transition(
    w: &mut Writer,
    transition: &crate::ir::StateTransition,
    target: &str,
    sequence: &ControlSequence,
    step: &str,
) {
    w.line(&format!("transition first {}", name(&transition.from)));
    w.line(&format!("    accept {step}"));

    if transition.condition.as_boolean() != Some(true) {
        w.depth += 1;
        write_condition(w, &transition.condition, &sequence.inputs);
        w.depth -= 1;
    }

    w.line(&format!("    then {};", name(target)));
}

fn write_normal(
    w: &mut Writer,
    transition: &crate::ir::StateTransition,
    target: &str,
    reset: Option<&Expr>,
    sequence: &ControlSequence,
    step: &str,
) {
    let mut transition = transition.clone();

    if let Some(reset) = reset {
        transition.condition = reset.clone().negated().conjoin(transition.condition);
    }

    write_transition(w, &transition, target, sequence, step);
}

fn write_reset(
    w: &mut Writer,
    condition: &Expr,
    target: &[String],
    inputs: &[StateInput],
    step: &str,
) {
    if condition.as_boolean() == Some(false) {
        return;
    }

    w.blank();
    w.line("transition first Control");
    w.line(&format!("    accept {step}"));
    w.depth += 1;
    write_condition(w, condition, inputs);
    w.depth -= 1;
    w.line(&format!(
        "    then {};",
        target.iter().map(|s| name(s)).collect::<Vec<_>>().join(".")
    ));
}

fn value_type_name(value_type: &str) -> String {
    if value_type.contains("::") {
        qualified(value_type)
    } else {
        format!("ScalarValues::{value_type}")
    }
}

fn write_calculation_dependencies(w: &mut Writer, model: &SystemModel, sequence: &ControlSequence) {
    let mut groups = BTreeMap::<Vec<Vec<String>>, Vec<Vec<String>>>::new();

    for calculation in sequence.calculations.iter().filter(|c| c.state.is_some()) {
        let mut path = sequence.state_path(calculation.state.as_ref().expect("state calculation"));
        path.push(calculation.name.clone());
        groups
            .entry(sequence.physical_inputs(&calculation.expression))
            .or_default()
            .push(path);
    }

    if groups.len() <= 1 {
        return;
    }

    let prefix = vec![model.package.clone(), model.root_name.clone()];

    for (inputs, calculations) in groups.into_iter().filter(|(inputs, _)| !inputs.is_empty()) {
        w.blank();
        w.line("dependency");
        write_references(w, "from", &calculations, &[], "");
        write_references(w, "to", &inputs, &prefix, ";");
    }
}
