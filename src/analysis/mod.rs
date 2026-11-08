mod access;
pub(crate) mod cfg;
mod dependencies;
mod states;
mod symbolic;

pub use symbolic::ControlExpr;

use crate::ir::validate_graph;
use crate::{ir::*, resolve::Symbols};
pub(crate) use access::{Accesses, CodeUse, compute as io_accesses};
pub(crate) use access::{overlaps, per_unit as unit_accesses};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct SignalBoundary {
    pub storage: StorageRef,
    pub direction: IoDirection,
}

pub fn analyze(program: &Program, boundaries: &[SignalBoundary]) -> Analysis {
    let influences = dependencies::analyze(program, boundaries);
    let mut result = symbolic::analyze(program, boundaries);
    result.influences = influences;
    result
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub influences: Vec<Influence>,
    pub cells: BTreeMap<String, ControlCell>,
    pub instances: Vec<ControlInstance>,
    pub memories: BTreeMap<String, ControlMemory>,
}

#[derive(Debug, Clone)]
pub struct ControlMemory {
    pub owner: String,
    pub expression: ControlExpr,
    pub initial: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct ControlCell {
    pub storage: StorageRef,
    pub expression: Option<ControlExpr>,
    pub sampled: bool,
    pub invocation_memory: bool,
    pub dependencies: Vec<String>,
    pub initial: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct ControlInstance {
    pub id: String,
    pub unit: String,
    pub instance: Option<StorageRef>,
    pub name: String,
    pub graph: StateGraph,
    pub conditions: Vec<ControlExpr>,
    pub reset: Option<ControlExpr>,
    pub selector: Option<String>,
    pub observation: Option<ControlObservation>,
}

#[derive(Debug, Clone)]
pub struct ControlObservation {
    pub key: String,
    pub labels: BTreeMap<String, Vec<String>>,
}

pub fn validate(model: &Program, control: &Analysis) -> Vec<String> {
    let mut errors = vec![];
    let units: BTreeSet<_> = model.units.iter().map(|u| u.id.as_str()).collect();
    let endpoint_valid = |e: &StorageRef| {
        units.contains(e.unit.as_str())
            && !e.path.is_empty()
            && e.path.iter().all(|p| !p.is_empty())
    };

    let mut instances = BTreeMap::new();

    for instance in &control.instances {
        if instances.insert(instance.id.as_str(), instance).is_some() {
            errors.push(format!("Duplicate control instance {}", instance.id));
        }

        if !units.contains(instance.unit.as_str()) {
            errors.push(format!(
                "Control instance {} has an unknown unit",
                instance.name
            ));
        }

        validate_graph(&instance.graph, &mut errors);

        if instance.conditions.len() != instance.graph.transitions.len() {
            errors.push(format!(
                "Control instance {} has unmatched transition conditions",
                instance.name
            ));
        }

        if instance
            .selector
            .as_ref()
            .is_some_and(|key| !control.cells.contains_key(key))
        {
            errors.push(format!(
                "Control instance {} has unknown selector storage",
                instance.name
            ));
        }

        if let Some(observation) = &instance.observation
            && (!control.cells.contains_key(&observation.key)
                || observation
                    .labels
                    .values()
                    .flatten()
                    .any(|state| !instance.graph.states.iter().any(|s| &s.name == state)))
        {
            errors.push(format!(
                "Control instance {} has an invalid state observation",
                instance.name
            ));
        }
    }

    let validate_expression = |expression: &ControlExpr, errors: &mut Vec<String>| {
        expression.visit(&mut |e| match e {
            ControlExpr::Read { key, .. } if !control.cells.contains_key(key) => {
                errors.push(format!("Control expression reads unknown storage {key}"));
            }

            ControlExpr::Active { machine, states }
                if instances.get(machine.as_str()).is_none_or(|instance| {
                    states
                        .iter()
                        .any(|state| !instance.graph.states.iter().any(|s| s.name == *state))
                }) =>
            {
                errors.push(format!(
                    "Control expression references unknown states of {machine}"
                ));
            }

            _ => {}
        });
    };

    for (key, cell) in &control.cells {
        if !endpoint_valid(&cell.storage) || *key != cell.storage.key() {
            errors.push(format!("Invalid control storage {key}"));
        }

        if cell
            .dependencies
            .iter()
            .any(|key| !control.cells.contains_key(key))
        {
            errors.push(format!("Control storage {key} has unknown dependencies"));
        }

        if let Some(expression) = &cell.expression {
            validate_expression(expression, &mut errors);
        }
    }

    for instance in &control.instances {
        for condition in &instance.conditions {
            validate_expression(condition, &mut errors);
        }

        if let Some(reset) = &instance.reset {
            validate_expression(reset, &mut errors);
        }
    }

    for (key, memory) in &control.memories {
        if !control.cells.contains_key(key) || !units.contains(memory.owner.as_str()) {
            errors.push(format!("Invalid retained control storage {key}"));
        }

        validate_expression(&memory.expression, &mut errors);
    }

    errors
}

#[derive(Debug, Clone)]
pub struct Influence {
    pub supplier: StorageRef,
    pub dependent: StorageRef,
}
