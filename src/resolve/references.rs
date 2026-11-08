use super::Lookup;
use crate::ir::*;
use std::collections::BTreeSet;

pub(super) fn bind(model: &Program) -> Bindings {
    let lookup = Lookup::new(model);
    let mut bindings = Bindings::default();
    for unit in &model.units {
        let mut names = BTreeSet::from([unit.name.clone()]);

        for declaration in &model.units {
            if declaration.id == unit.id || declaration.owner.as_deref() == Some(&unit.id) {
                names.insert(declaration.name.clone());
            }
        }

        for variable in &unit.variables {
            names.insert(variable.name.clone());
            for bound in &variable.ty.dimensions {
                expression(&bound.lower, &mut names);
                expression(&bound.upper, &mut names);
            }
            if let Some(initial) = &variable.initial {
                expression(initial, &mut names);
            }
        }

        for ty in &model.types {
            for literal in ty.literals() {
                names.insert(literal.name.clone());
                names.insert(format!("{}.{}", ty.name, literal.name));
            }
        }

        for body in &unit.bodies {
            body.visit_expressions(&mut |expression| {
                names.insert(expression.text());
            });
        }

        for name in names {
            let reference = Expr::Reference { name: name.clone() };
            bindings.references.insert(
                (unit.id.clone(), name.to_ascii_lowercase()),
                ReferenceBinding {
                    storage: lookup.endpoint(unit, &reference),
                    callable: lookup.callable_name(unit, &reference).map(|u| u.id.clone()),
                    constant: lookup.enum_value(unit, &name),
                },
            );
        }
    }
    bindings
}

fn expression(value: &Expr, names: &mut BTreeSet<String>) {
    value.visit(&mut |expression| {
        names.insert(expression.text());
    });
}

pub(crate) struct Symbols<'a> {
    pub(super) model: &'a Program,
}

impl<'a> Symbols<'a> {
    pub(crate) fn new(model: &'a Program) -> Self {
        Self { model }
    }

    pub(crate) fn endpoint(&self, unit: &Unit, expression: &Expr) -> Option<StorageRef> {
        if matches!(expression, Expr::Index { .. }) {
            return None;
        }
        self.model
            .bindings
            .get(&unit.id, &expression.text())?
            .storage
            .clone()
    }

    pub(crate) fn callable_name(&self, unit: &Unit, callee: &Expr) -> Option<&'a Unit> {
        let id = self
            .model
            .bindings
            .get(&unit.id, &callee.text())?
            .callable
            .as_ref()?;
        self.model.units.iter().find(|u| &u.id == id)
    }

    pub(crate) fn enum_value(&self, unit: &Unit, name: &str) -> Option<i128> {
        self.model.bindings.get(&unit.id, name)?.constant
    }

    pub(crate) fn storage_variable(&self, storage: &StorageRef) -> Option<&Variable> {
        self.model.variable(storage)
    }

    pub(crate) fn effective_type<'t>(&'t self, ty: &'t TypeRef) -> Option<&'t TypeRef> {
        self.model.effective_type(ty)
    }
}

impl Symbols<'_> {
    pub(crate) fn call_instance(
        &self,
        caller: &Unit,
        signature: &Unit,
        callee: &Expr,
        source: &Source,
    ) -> Option<StorageRef> {
        if signature.kind == UnitKind::FunctionBlock {
            self.endpoint(caller, callee)
        } else if signature.owner.is_some() {
            match callee {
                Expr::Member { base, .. } => self.endpoint(caller, base),

                _ => Some(StorageRef {
                    unit: caller.owner.clone().unwrap_or_else(|| caller.id.clone()),
                    path: vec![],
                    ty: TypeRef::default(),
                    source: source.clone(),
                }),
            }
        } else {
            None
        }
    }
}

pub(crate) fn parameters<'a>(
    signature: &'a Unit,
    arguments: &[Argument],
) -> Vec<Option<&'a Variable>> {
    let mut positions = [0, 0];

    arguments
        .iter()
        .map(|argument| {
            if let Some(name) = &argument.name {
                signature.variables.iter().find(|v| {
                    v.name.eq_ignore_ascii_case(name)
                        && matches!(
                            v.kind,
                            VariableKind::Input | VariableKind::Output | VariableKind::InOut
                        )
                })
            } else {
                let output = argument.direction == ArgumentDirection::Output;
                let slot = usize::from(output);
                let parameter = signature
                    .variables
                    .iter()
                    .filter(|v| {
                        if output {
                            v.kind == VariableKind::Output
                        } else {
                            matches!(v.kind, VariableKind::Input | VariableKind::InOut)
                        }
                    })
                    .nth(positions[slot]);

                positions[slot] += 1;

                parameter
            }
        })
        .collect()
}

pub(crate) fn instance_storage(
    signature: &Unit,
    occurrence: Option<&StorageRef>,
    storage: &StorageRef,
) -> StorageRef {
    let owner = signature.owner.as_deref().unwrap_or(&signature.id);

    if let Some(occurrence) = occurrence
        && storage.unit == signature.id
        && storage.path.first().is_some_and(|name| {
            signature
                .variables
                .iter()
                .any(|v| v.name.eq_ignore_ascii_case(name) && v.kind == VariableKind::Instance)
        })
    {
        let mut mapped = storage.clone();
        mapped.unit = occurrence.unit.clone();
        mapped.path = occurrence
            .path
            .iter()
            .cloned()
            .chain([format!("${}", signature.name)])
            .chain(storage.path.iter().cloned())
            .collect();
        return mapped;
    }

    if let Some(occurrence) = occurrence
        && storage.unit == owner
    {
        let mut mapped = storage.clone();
        mapped.unit = occurrence.unit.clone();
        mapped.path = occurrence
            .path
            .iter()
            .chain(&storage.path)
            .cloned()
            .collect();

        mapped
    } else {
        storage.clone()
    }
}
