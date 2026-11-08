mod behavior;
mod expressions;
mod render;
mod types;

pub(crate) use render::identifier;
pub use render::render;

use crate::{
    analysis::*,
    input::physical::{IoRole, LoadedContext},
    ir::*,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct SystemModel {
    pub package: String,
    pub root_definition: String,
    pub root_name: String,
    pub structure: BTreeMap<String, String>,
    pub signal_type_package: String,
    pub signal_types: BTreeMap<String, Option<String>>,
    pub enum_types: BTreeMap<String, Vec<String>>,
    pub equipment: Vec<EquipmentSignals>,
    pub sequences: Vec<ControlSequence>,
}

impl SystemModel {
    pub(crate) fn root_instance_name(&self) -> String {
        let first = self.root_name.chars().next().expect("named root");

        format!(
            "{}{}",
            first.to_lowercase(),
            &self.root_name[first.len_utf8()..]
        )
    }
}

#[derive(Debug)]
pub struct EquipmentSignals {
    pub scope: String,
    pub package: String,
    pub definition: String,
    pub base: String,
    pub channels: Vec<SignalChannel>,
    pub dependencies: Vec<SignalDependency>,
    pub cross_dependencies: Vec<SignalDependency>,
}

#[derive(Debug)]
pub struct ControlSequence {
    pub source_id: String,
    pub package: String,
    pub usage: String,
    pub graph: StateGraph,
    pub inputs: Vec<StateInput>,
    pub background: Option<ConcurrentRegion>,
    pub calculations: Vec<ControlCalculation>,
    pub contributions: Vec<StateContribution>,
    pub derived: Vec<ControlDerived>,
    pub outputs: Vec<ControlOutput>,
}

#[derive(Debug)]
pub struct ControlOutput {
    pub name: String,
    pub item_type: String,
    pub endpoints: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ControlDerived {
    pub name: String,
    pub value_type: String,
    pub expression: ControlExpr,
}

#[derive(Debug, Clone)]
pub struct ControlCalculation {
    pub state: Option<String>,
    pub name: String,
    pub value_type: String,
    pub expression: ControlExpr,
    pub commands: Vec<Vec<String>>,
    pub output: Option<String>,
}

#[derive(Debug)]
pub struct StateContribution {
    pub state: String,
    pub commands: Vec<Vec<String>>,
}

#[derive(Debug)]
pub struct ConcurrentRegion {
    pub states: Vec<String>,
    pub initial_position: String,
}

#[derive(Debug, Clone)]
pub struct StateInput {
    pub name: String,
    pub value_type: String,
    pub endpoints: Vec<Vec<String>>,
    pub item_type: Option<String>,
    pub state_source: Option<(String, Vec<String>)>,
    pub state_links: Vec<Vec<String>>,
    pub bounds: Option<(ControlExpr, ControlExpr)>,
    pub default: Option<ControlExpr>,
    pub update: Option<ControlExpr>,
}

#[derive(Debug)]
pub struct SignalChannel {
    pub part: Vec<String>,
    pub name: String,
    pub item_type: String,
    pub input: bool,
    pub bounds: Option<(i128, i128)>,
}

#[derive(Debug)]
pub struct SignalDependency {
    pub label: String,
    pub outputs: Vec<Vec<String>>,
    pub inputs: Vec<Vec<String>>,
}

impl ControlSequence {
    pub(crate) fn physical_inputs(&self, expression: &ControlExpr) -> Vec<Vec<String>> {
        let mut pending = vec![expression];
        let mut seen = BTreeSet::new();
        let mut result = BTreeSet::new();

        while let Some(expression) = pending.pop() {
            expression.visit(&mut |e| {
                if let ControlExpr::Read { key, .. } = e
                    && seen.insert(key.clone())
                {
                    if let Some(input) = self.inputs.iter().find(|i| i.name == *key) {
                        result.extend(input.endpoints.iter().cloned());
                    } else if let Some(derived) = self.derived.iter().find(|d| d.name == *key) {
                        pending.push(&derived.expression);
                    }
                }
            });
        }

        result.into_iter().collect()
    }

    pub(crate) fn state_path(&self, state: &str) -> Vec<String> {
        let mut path = vec!["sequence".into()];

        if self.graph.reset.is_some() {
            path.push("Control".into());
        }

        if self.background.is_some() && state != self.graph.initial {
            path.extend(["ConcurrentControl".into(), "Position".into()]);
        }

        path.push(state.into());

        path
    }

    pub fn prune(&mut self) {
        fn expression(e: &crate::ir::Expr, used: &mut BTreeSet<String>) {
            e.visit(&mut |e| {
                if let Expr::Reference { name } = e {
                    used.insert(name.clone());
                }
            });
        }

        fn reads(e: &ControlExpr, used: &mut BTreeSet<String>) {
            e.visit(&mut |e| {
                if let ControlExpr::Read { key, .. } = e {
                    used.insert(key.clone());
                }
            });
        }

        let mut used = BTreeSet::new();

        for t in &self.graph.transitions {
            expression(&t.condition, &mut used);
        }

        if let Some(e) = &self.graph.reset {
            expression(e, &mut used);
        }

        for c in &self.calculations {
            reads(&c.expression, &mut used);
        }

        loop {
            let before = used.len();

            for d in &self.derived {
                if used.contains(&d.name) {
                    reads(&d.expression, &mut used);
                }
            }

            for input in &self.inputs {
                if used.contains(&input.name)
                    && let Some(update) = &input.update
                {
                    reads(update, &mut used);
                }
            }

            if before == used.len() {
                break;
            }
        }

        self.inputs.retain(|i| used.contains(&i.name));
        self.derived.retain(|d| used.contains(&d.name));
    }
}

pub fn build(
    project: &Program,
    behavior: &crate::analysis::Analysis,
    context: &LoadedContext,
) -> SystemModel {
    let mut model = SystemModel {
        package: context.system_name.clone(),
        root_definition: context.root_definition.clone(),
        root_name: context.root_definition.rsplit("::").next().unwrap().into(),
        structure: context.index.files.clone(),
        signal_type_package: format!("{}SignalTypes", context.system_name),
        signal_types: BTreeMap::new(),
        enum_types: BTreeMap::new(),
        equipment: vec![],
        sequences: vec![],
    };

    let keys: Vec<_> = context.channels.iter().map(|c| c.storage.key()).collect();
    let sources: BTreeMap<_, _> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let mut endpoints = vec![vec![]; keys.len()];
    let mut channels = BTreeMap::<String, Vec<SignalChannel>>::new();
    let mut used_names = BTreeMap::<String, BTreeSet<String>>::new();
    let mut endpoint_counts = BTreeMap::<Vec<String>, usize>::new();

    for channel in &context.channels {
        for target in &channel.targets {
            *endpoint_counts.entry(target.path()).or_default() += 1;
        }
    }

    let mut relationships = BTreeSet::<(usize, usize)>::new();

    for influence in &behavior.influences {
        if let (Some(&input), Some(&output)) = (
            sources.get(influence.supplier.key().as_str()),
            sources.get(influence.dependent.key().as_str()),
        ) && context.channels[input].role == IoRole::Feedback
            && context.channels[output].role == IoRole::Command
        {
            relationships.insert((input, output));
        }
    }

    for (i, channel) in context.channels.iter().enumerate() {
        for target in &channel.targets {
            let scope = target.physical.split('.').next().unwrap().to_string();
            channels.entry(scope.clone()).or_default();

            if !target.features.is_empty() && endpoint_counts[&target.path()] == 1 {
                endpoints[i].push(target.path());
                continue;
            }

            let names = used_names.entry(target.physical.clone()).or_default();
            let mut label = channel.name.clone();

            if target.features.first().is_some_and(|f| f == &label) || !names.insert(label.clone())
            {
                label = format!("{} ({})", channel.name, channel.source_name);
                names.insert(label.clone());
            }

            let full: Vec<_> = target
                .physical
                .split('.')
                .map(String::from)
                .chain([label.clone()])
                .collect();

            endpoints[i].push(full.clone());

            let scope = full[0].clone();
            let relative = full[1..full.len() - 1].to_vec();
            channels.entry(scope).or_default().push(SignalChannel {
                part: relative,
                name: label,
                item_type: channel.item_type.clone().unwrap_or_else(|| {
                    let ty = channel
                        .value_type
                        .as_ref()
                        .map(|t| format!("{t}Signal"))
                        .unwrap_or_else(|| "Signal".into());

                    model
                        .signal_types
                        .insert(ty.clone(), channel.value_type.clone());

                    format!("{}::{ty}", model.signal_type_package)
                }),
                input: channel.role == IoRole::Command,
                bounds: channel
                    .item_type
                    .is_none()
                    .then(|| channel.storage.ty.integer_bounds())
                    .flatten(),
            });
        }
    }

    type OutputKey = (Vec<String>, String, String);

    let mut per_output = BTreeMap::<OutputKey, BTreeSet<Vec<String>>>::new();

    for (input, output) in relationships {
        for (output_target, to) in endpoints[output].iter().enumerate() {
            for (input_target, from) in endpoints[input].iter().enumerate() {
                let actuator = context.channels[output].targets[output_target]
                    .physical
                    .clone();
                let sensor = context.channels[input].targets[input_target]
                    .physical
                    .clone();
                let feedback = context
                    .physical_transfers
                    .contains(&(actuator.clone(), sensor.clone()));
                let local = from[0] == to[0];

                let receiver = if local {
                    actuator
                        .split_once('.')
                        .map_or(actuator.as_str(), |(_, part)| part)
                } else {
                    actuator.as_str()
                };

                let mut label = format!("Possible inputs for {receiver}");

                if feedback {
                    label.push_str(" (physical feedback)");
                } else if !local {
                    label.push_str(&format!(" from {}", from[0]));
                }

                per_output
                    .entry((to.clone(), from[0].clone(), label))
                    .or_default()
                    .insert(from.clone());
            }
        }
    }

    type GroupKey = (String, bool, String, Vec<Vec<String>>);

    let mut groups = BTreeMap::<GroupKey, Vec<Vec<String>>>::new();

    for ((output, supplier_scope, label), inputs) in per_output {
        let scope = output[0].clone();
        let local = scope == supplier_scope;
        let offset = usize::from(local);
        let inputs = inputs.into_iter().map(|p| p[offset..].to_vec()).collect();
        groups
            .entry((scope, local, label, inputs))
            .or_default()
            .push(output[offset..].to_vec());
    }

    let mut local_dependencies = BTreeMap::<String, Vec<SignalDependency>>::new();
    let mut cross_dependencies = BTreeMap::<String, Vec<SignalDependency>>::new();
    let mut label_counts = BTreeMap::new();

    for (scope, local, label, _) in groups.keys() {
        *label_counts
            .entry((scope.clone(), *local, label.clone()))
            .or_insert(0) += 1;
    }

    for ((scope, local, mut label, inputs), outputs) in groups {
        if label_counts[&(scope.clone(), local, label.clone())] > 1 {
            label.push_str(&format!(
                " [{}]",
                outputs
                    .iter()
                    .map(|p| p.last().unwrap().as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let dependency = SignalDependency {
            outputs,
            inputs,
            label,
        };

        if local {
            local_dependencies
                .entry(scope)
                .or_default()
                .push(dependency);
        } else {
            cross_dependencies
                .entry(scope)
                .or_default()
                .push(dependency);
        }
    }

    let bases: BTreeMap<_, _> = channels
        .keys()
        .map(|scope| {
            (
                scope.clone(),
                context
                    .index
                    .type_at_path(&context.root_definition, scope)
                    .expect("validated scope"),
            )
        })
        .collect();
    let mut names = BTreeSet::from([model.package.clone()]);

    for (scope, channels) in channels {
        let base = &bases[&scope];
        let definition = base.rsplit("::").next().unwrap().to_string();
        let mut package = format!("{definition}Signals");

        if !names.insert(package.clone()) {
            let base = format!("{scope}{definition}Signals");
            package = base.clone();
            let mut suffix = 2;

            while !names.insert(package.clone()) {
                package = format!("{base}_{suffix}");
                suffix += 1;
            }
        }

        model.equipment.push(EquipmentSignals {
            scope: scope.clone(),
            package,
            definition,
            base: base.clone(),
            channels,
            dependencies: local_dependencies.remove(&scope).unwrap_or_default(),
            cross_dependencies: cross_dependencies.remove(&scope).unwrap_or_default(),
        });
    }

    behavior::add(project, behavior, context, &endpoints, &mut model);

    model
}
