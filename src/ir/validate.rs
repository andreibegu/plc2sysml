use crate::ir::*;
use petgraph::{algo::toposort, graphmap::DiGraphMap};
use std::collections::BTreeSet;

pub fn validate(model: &Program) -> Vec<String> {
    let mut errors = vec![];
    let mut ids = BTreeSet::new();
    let units: BTreeSet<_> = model.units.iter().map(|u| u.id.as_str()).collect();
    let types: BTreeSet<_> = units
        .iter()
        .copied()
        .chain(model.types.iter().map(|t| t.id.as_str()))
        .collect();

    for unit in &model.units {
        insert(&mut ids, &unit.id, &mut errors);

        for v in &unit.variables {
            insert(&mut ids, &v.id, &mut errors);
        }

        for behavior in &unit.bodies {
            insert(&mut ids, &behavior.id, &mut errors);
            statement_ids(&behavior.statements, &mut ids, &mut errors);
            if let Some(graph) = behavior.graph() {
                validate_graph(graph, &mut errors);
            }
            if !behavior.is_ordered()
                && behavior.statements.iter().any(|s| {
                    !matches!(
                        s.kind,
                        StatementKind::Assign { .. }
                            | StatementKind::Call { .. }
                            | StatementKind::MayAffect { .. }
                            | StatementKind::Unknown { .. }
                    )
                })
            {
                errors.push(format!(
                    "Unordered effects contain structured control in {}",
                    behavior.name
                ));
            }
        }
    }

    for ty in &model.types {
        insert(&mut ids, &ty.id, &mut errors);

        for f in ty.fields() {
            insert(&mut ids, &f.id, &mut errors);
        }

        for literal in ty.literals() {
            insert(
                &mut ids,
                &id(&["enum_literal", &ty.id, &literal.name]),
                &mut errors,
            );
        }
    }

    for unit in &model.units {
        if unit.owner.as_deref().is_some_and(|o| !units.contains(o)) {
            errors.push(format!("Dangling owner for {}", unit.id));
        }
        if unit
            .base_type
            .as_deref()
            .is_some_and(|base| !units.contains(base))
        {
            errors.push(format!("Dangling base type for {}", unit.id));
        }
    }

    for task in &model.tasks {
        for entry in &task.programs {
            if entry.target.as_ref().is_some_and(|target| {
                !model
                    .units
                    .iter()
                    .any(|u| &u.id == target && u.kind == UnitKind::Program)
            }) {
                errors.push(format!(
                    "Invalid task entry {} in {}",
                    entry.name, task.name
                ));
            }
        }
    }

    let ownership = DiGraphMap::<&str, ()>::from_edges(
        model
            .units
            .iter()
            .filter_map(|unit| Some((unit.id.as_str(), unit.owner.as_deref()?))),
    );

    if let Err(cycle) = toposort(&ownership, None) {
        errors.push(format!("Ownership cycle involving {}", cycle.node_id()));
    }

    let inheritance = DiGraphMap::<&str, ()>::from_edges(
        model
            .types
            .iter()
            .filter_map(|t| Some((t.id.as_str(), t.base()?.target.as_deref()?)))
            .chain(
                model
                    .units
                    .iter()
                    .filter_map(|u| Some((u.id.as_str(), u.base_type.as_deref()?))),
            ),
    );
    if let Err(cycle) = toposort(&inheritance, None) {
        errors.push(format!(
            "Type or inheritance cycle involving {}",
            cycle.node_id()
        ));
    }

    let references = model
        .units
        .iter()
        .flat_map(|u| {
            u.variables
                .iter()
                .map(|v| &v.ty)
                .chain(u.return_type.iter())
        })
        .chain(
            model
                .types
                .iter()
                .flat_map(|t| t.fields().iter().map(|f| &f.ty).chain(t.base())),
        );
    if references
        .filter_map(|ty| ty.target.as_ref())
        .any(|target| !types.contains(target.as_str()))
    {
        errors.push("Type refers to an unknown declaration".into());
    }

    for ((owner, _), binding) in &model.bindings.references {
        if !units.contains(owner.as_str())
            || binding
                .callable
                .as_ref()
                .is_some_and(|target| !units.contains(target.as_str()))
            || binding
                .storage
                .as_ref()
                .is_some_and(|storage| !units.contains(storage.unit.as_str()))
        {
            errors.push("Binding refers to an unknown declaration".into());
        }
    }

    errors
}

fn insert(ids: &mut BTreeSet<String>, id: &str, errors: &mut Vec<String>) {
    if !ids.insert(id.into()) {
        errors.push(format!("Duplicate element ID {id}"));
    }
}

fn statement_ids(statements: &[Statement], ids: &mut BTreeSet<String>, errors: &mut Vec<String>) {
    for statement in statements {
        statement.visit(&mut |statement| {
            insert(ids, &statement.id, errors);
            if let StatementKind::Case { arms, .. } = &statement.kind
                && arms.iter().any(|arm| arm.test.is_empty())
            {
                errors.push(format!(
                    "CASE arm has no labels at {}:{}",
                    statement.source.path, statement.source.line
                ));
            }
        });
    }
}

pub(crate) fn validate_graph(graph: &StateGraph, errors: &mut Vec<String>) {
    let positions: BTreeSet<_> = graph.states.iter().map(|s| s.name.as_str()).collect();

    if positions.len() != graph.states.len()
        || !positions.contains(graph.initial.as_str())
        || graph
            .transitions
            .iter()
            .any(|t| !positions.contains(t.from.as_str()) || !positions.contains(t.to.as_str()))
    {
        errors.push(format!("Invalid state graph for {}", graph.name));
    }
}
