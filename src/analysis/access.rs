use crate::ir::*;
use crate::resolve::Symbols;
use std::collections::{BTreeMap, BTreeSet};

const READ: u8 = 1;
const WRITE: u8 = 2;
const UNKNOWN: u8 = 4;
type Footprint = BTreeMap<String, (StorageRef, u8)>;

#[derive(Debug, Clone)]
pub(crate) struct Accesses {
    footprint: Footprint,
}

pub(crate) enum CodeUse {
    ReadOnly,
    WriteOnly,
}

impl Accesses {
    pub fn writes(&self, storage: &StorageRef) -> bool {
        self.overlaps(storage)
            .any(|(_, flags)| flags & (WRITE | UNKNOWN) != 0)
    }

    fn overlaps<'a>(
        &'a self,
        storage: &'a StorageRef,
    ) -> impl Iterator<Item = &'a (StorageRef, u8)> {
        let key = storage.key();
        self.footprint
            .iter()
            .filter(move |(other, _)| overlaps(&key, other))
            .map(|(_, access)| access)
    }

    pub fn one_sided(&self, storage: &StorageRef) -> Option<CodeUse> {
        let flags = self
            .overlaps(storage)
            .fold(0, |flags, (_, access)| flags | access);

        match flags {
            READ => Some(CodeUse::ReadOnly),
            WRITE => Some(CodeUse::WriteOnly),
            _ => None,
        }
    }
}

pub(crate) fn per_unit(model: &Program) -> BTreeMap<String, Accesses> {
    let mut survey = Survey {
        symbols: Symbols::new(model),
        summaries: BTreeMap::new(),
        active: BTreeSet::new(),
    };

    let footprints: BTreeMap<_, _> = model
        .units
        .iter()
        .filter(|u| u.intrinsic.is_none())
        .map(|u| (u.id.clone(), survey.summary(u)))
        .collect();

    footprints
        .into_iter()
        .map(|(id, footprint)| (id, Accesses { footprint }))
        .collect()
}

pub(crate) fn compute(model: &Program) -> Accesses {
    let mut survey = Survey {
        symbols: Symbols::new(model),
        summaries: BTreeMap::new(),
        active: BTreeSet::new(),
    };

    let mut accesses = Footprint::new();

    for unit in model.units.iter().filter(|u| u.kind == UnitKind::Program) {
        merge(&mut accesses, survey.summary(unit));
    }

    Accesses {
        footprint: accesses,
    }
}

struct Survey<'a> {
    symbols: Symbols<'a>,
    summaries: BTreeMap<String, Footprint>,
    active: BTreeSet<String>,
}

impl Survey<'_> {
    fn summary(&mut self, unit: &Unit) -> Footprint {
        if let Some(summary) = self.summaries.get(&unit.id) {
            return summary.clone();
        }

        if !self.active.insert(unit.id.clone()) {
            return Footprint::new();
        }

        let mut out = Footprint::new();

        if let Some(intrinsic) = unit.intrinsic {
            for &(name, ty, kind) in intrinsic.ports() {
                let flags = if kind == VariableKind::Input {
                    READ
                } else {
                    READ | WRITE
                };
                add(
                    &mut out,
                    StorageRef {
                        unit: unit.id.clone(),
                        path: vec![name.into()],
                        ty: TypeRef::scalar(ty),
                        source: unit.source.clone(),
                    },
                    flags,
                );
            }
        }

        for behavior in &unit.bodies {
            self.statements(unit, &behavior.statements, &mut out);
        }

        self.active.remove(&unit.id);
        self.summaries.insert(unit.id.clone(), out.clone());

        out
    }

    fn statements(&mut self, unit: &Unit, statements: &[Statement], out: &mut Footprint) {
        for statement in statements {
            statement.visit(&mut |statement| match &statement.kind {
                StatementKind::Assign { target, value } => {
                    self.expression(unit, target, WRITE, out);
                    self.expression(unit, value, READ, out);
                }

                StatementKind::Call { expression } => self.expression(unit, expression, READ, out),

                StatementKind::MayAffect { condition, target } => {
                    self.expression(unit, condition, READ, out);

                    if self.symbols.callable_name(unit, target).is_some() {
                        self.call(unit, target, &[], out);
                    } else {
                        self.expression(unit, target, READ | WRITE, out);
                    }
                }

                StatementKind::If { branches, .. } => {
                    for branch in branches {
                        self.expression(unit, &branch.test, READ, out);
                    }
                }

                StatementKind::Case { selector, .. } => {
                    self.expression(unit, selector, READ, out);
                }

                StatementKind::Loop { control, .. } => match control {
                    LoopControl::Condition { condition, .. } => {
                        self.expression(unit, condition, READ, out);
                    }

                    LoopControl::Range {
                        variable,
                        start,
                        end,
                        step,
                    } => {
                        self.expression(unit, variable, READ | WRITE, out);

                        for expression in [Some(start), Some(end), step.as_ref()]
                            .into_iter()
                            .flatten()
                        {
                            self.expression(unit, expression, READ, out);
                        }
                    }
                },

                StatementKind::Unknown { operands } => {
                    for operand in operands {
                        self.expression(unit, operand, UNKNOWN, out);
                    }
                }

                _ => {}
            });
        }
    }

    fn expression(&mut self, unit: &Unit, expression: &Expr, flags: u8, out: &mut Footprint) {
        if let Some(storage) = self.symbols.endpoint(unit, expression) {
            let flags = if storage.ty.indirection.is_some() {
                flags | UNKNOWN
            } else {
                flags
            };

            add(out, storage, flags);
            return;
        }

        match expression {
            Expr::Call { callee, arguments } => self.call(unit, callee, arguments, out),
            Expr::Unary { operand, .. } | Expr::Snapshot { value: operand } => {
                self.expression(unit, operand, flags, out)
            }

            Expr::Binary { left, right, .. } => {
                self.expression(unit, left, flags, out);
                self.expression(unit, right, flags, out);
            }

            Expr::Index { base, indices } => {
                self.expression(unit, base, flags, out);

                for index in indices {
                    self.expression(unit, index, READ, out);
                }
            }

            Expr::Member { base, .. } => self.expression(unit, base, flags | UNKNOWN, out),
            Expr::Unknown { operands } => {
                for operand in operands {
                    self.expression(unit, operand, flags | UNKNOWN, out);
                }
            }
            _ => {}
        }
    }

    fn call(&mut self, caller: &Unit, callee: &Expr, arguments: &[Argument], out: &mut Footprint) {
        let signature = self.symbols.callable_name(caller, callee);
        let mut aliases = BTreeMap::new();
        let parameters = signature
            .as_ref()
            .map(|s| crate::resolve::parameters(s, arguments))
            .unwrap_or_else(|| vec![None; arguments.len()]);

        for (argument, parameter) in arguments.iter().zip(parameters) {
            let flags = match parameter.map(|p| p.kind) {
                Some(VariableKind::Output) => WRITE,
                Some(VariableKind::InOut) => READ | WRITE | UNKNOWN,
                Some(VariableKind::Input) => READ,
                _ if argument.direction == ArgumentDirection::Output => WRITE,

                _ if crate::resolve::builtin_call(&callee.text()) => {
                    if matches!(callee.text().to_ascii_uppercase().as_str(), "ADR" | "REF") {
                        READ | UNKNOWN
                    } else {
                        READ
                    }
                }

                _ => READ | WRITE | UNKNOWN,
            };

            self.expression(caller, &argument.value, flags, out);

            if let Some(parameter) = parameter
                && let Some(storage) = self.symbols.endpoint(caller, &argument.value)
            {
                aliases.insert(parameter.name.to_ascii_lowercase(), (storage, flags));
            }
        }

        let Some(signature) = signature else {
            return;
        };

        let occurrence = self
            .symbols
            .call_instance(caller, signature, callee, &caller.source);

        if self.active.contains(&signature.id) {
            if let Some(occurrence) = occurrence {
                add(out, occurrence, UNKNOWN);
            }

            return;
        }

        for (_, (mut storage, mut flags)) in self.summary(signature) {
            if storage.unit == signature.id
                && let Some((alias, allowed)) = storage
                    .path
                    .first()
                    .and_then(|p| aliases.get(&p.to_ascii_lowercase()))
            {
                flags &= allowed;
                storage.unit = alias.unit.clone();
                storage.path = alias
                    .path
                    .iter()
                    .chain(&storage.path[1..])
                    .cloned()
                    .collect();
            } else {
                storage =
                    crate::resolve::instance_storage(signature, occurrence.as_ref(), &storage);
            }

            add(out, storage, flags);
        }
    }
}

fn add(out: &mut Footprint, storage: StorageRef, flags: u8) {
    if flags == 0 {
        return;
    }

    out.entry(storage.key())
        .and_modify(|(_, f)| *f |= flags)
        .or_insert((storage, flags));
}

fn merge(out: &mut Footprint, other: Footprint) {
    for (_, (storage, flags)) in other {
        add(out, storage, flags);
    }
}

pub(crate) fn overlaps(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|tail| tail.starts_with('.'))
        || right
            .strip_prefix(left)
            .is_some_and(|tail| tail.starts_with('.'))
}
