mod application;
mod transfer;
mod values;

use self::values::*;
use super::cfg::{ControlFlowGraph, FlowNode, Operation};
use super::{Influence, SignalBoundary, Symbols};
use crate::ir::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Default)]
struct Summary {
    writes: BTreeMap<String, Value>,
}

type SummaryKey = (String, Vec<(String, BTreeSet<Constant>)>);

pub(super) fn analyze(model: &Program, boundaries: &[SignalBoundary]) -> Vec<Influence> {
    let mut engine = Engine {
        model,
        symbols: Symbols::new(model),
        summaries: BTreeMap::new(),
        active: BTreeSet::new(),
        cells: BTreeMap::new(),
        boundaries,
    };

    engine.application()
}

struct Engine<'a> {
    model: &'a Program,
    symbols: Symbols<'a>,
    summaries: BTreeMap<SummaryKey, Summary>,
    active: BTreeSet<String>,
    cells: BTreeMap<String, StorageRef>,
    boundaries: &'a [SignalBoundary],
}

impl Engine<'_> {
    fn cell(&mut self, storage: StorageRef) -> String {
        let key = storage.key();
        self.cells.entry(key.clone()).or_insert(storage);
        key
    }

    fn summary(&mut self, unit: &Unit) -> Summary {
        self.specialized_summary(unit, &BTreeMap::new())
    }

    fn specialized_summary(&mut self, unit: &Unit, actuals: &BTreeMap<String, Value>) -> Summary {
        let mut constants: Vec<_> = actuals
            .iter()
            .filter_map(|(name, value)| value.constants.clone().map(|c| (name.clone(), c)))
            .collect();

        if !constants.is_empty()
            && self
                .summaries
                .keys()
                .filter(|(id, _)| id == &unit.id)
                .count()
                >= 32
            && !self
                .summaries
                .contains_key(&(unit.id.clone(), constants.clone()))
        {
            constants.clear();
        }

        let cache_key = (unit.id.clone(), constants.clone());

        if let Some(summary) = self.summaries.get(&cache_key) {
            return summary.clone();
        }

        if !self.active.insert(unit.id.clone()) {
            let value = Value::default();
            return self.unknown_summary(unit, value);
        }

        let mut state = State::default();

        for variable in &unit.variables {
            let key = self.cell(StorageRef {
                unit: unit.id.clone(),
                path: vec![variable.name.clone()],
                ty: variable.ty.clone(),
                source: variable.source.clone(),
            });

            let temporary = variable.kind == VariableKind::Temporary
                || matches!(
                    unit.kind,
                    UnitKind::Function | UnitKind::Method | UnitKind::Action
                ) && !matches!(
                    variable.kind,
                    VariableKind::Input
                        | VariableKind::InOut
                        | VariableKind::External
                        | VariableKind::Instance
                        | VariableKind::Static
                );

            state.values.insert(
                key.clone(),
                if temporary {
                    initial(variable)
                } else {
                    Value::entry(&key)
                },
            );
        }

        for (name, constants) in &constants {
            if let Some(storage) = self
                .symbols
                .endpoint(unit, &Expr::Reference { name: name.clone() })
            {
                let key = self.cell(storage);
                let mut value = state.read(&key);
                value.constants = Some(constants.clone());
                state.values.insert(key, value);
            }
        }

        if let Some(ty) = &unit.return_type {
            let key = self.cell(StorageRef {
                unit: unit.id.clone(),
                path: vec!["$return".into()],
                ty: ty.clone(),
                source: unit.source.clone(),
            });
            state.values.insert(key.clone(), initial_type(ty));
            state.writes.insert(key);
        }

        if unit.intrinsic.is_some() {
            state = self.standard(unit, state);
        } else if unit.bodies.is_empty() {
            let value = Value::default();
            self.active.remove(&unit.id);
            let summary = self.unknown_summary(unit, value);
            self.summaries.insert(cache_key, summary.clone());
            return summary;
        } else {
            for behavior in &unit.bodies {
                state.conditions.clear();

                if behavior.is_ordered() {
                    state = self.run_graph(unit, &ControlFlowGraph::new(behavior), state);
                } else {
                    let nodes: Vec<_> = behavior.statements.iter().map(FlowNode::effect).collect();
                    state = self.run_network(unit, &nodes, state);
                }
            }
        }

        let summary = Summary {
            writes: state
                .writes
                .iter()
                .map(|key| (key.clone(), state.read(key)))
                .collect(),
        };

        self.active.remove(&unit.id);
        self.summaries.insert(cache_key, summary.clone());

        summary
    }

    fn unknown_summary(&mut self, unit: &Unit, value: Value) -> Summary {
        let mut summary = Summary {
            ..Summary::default()
        };

        for variable in unit
            .variables
            .iter()
            .filter(|v| matches!(v.kind, VariableKind::Output | VariableKind::InOut))
        {
            let key = self.cell(StorageRef {
                unit: unit.id.clone(),
                path: vec![variable.name.clone()],
                ty: variable.ty.clone(),
                source: variable.source.clone(),
            });
            summary.writes.insert(key, value.clone());
        }

        if let Some(ty) = &unit.return_type {
            let key = self.cell(StorageRef {
                unit: unit.id.clone(),
                path: vec!["$return".into()],
                ty: ty.clone(),
                source: unit.source.clone(),
            });
            summary.writes.insert(key, value);
        }

        summary
    }

    fn standard(&mut self, unit: &Unit, mut state: State) -> State {
        let pairs = unit
            .intrinsic
            .map(Intrinsic::dependencies)
            .unwrap_or_default();

        for (input, output) in pairs {
            let input = format!("{}::{input}", unit.id).to_ascii_lowercase();
            let output = format!("{}::{output}", unit.id).to_ascii_lowercase();
            let value = state.read(&input);
            state
                .values
                .entry(output.clone())
                .and_modify(|v| v.join(&value))
                .or_insert(value);
            state.writes.insert(output);
        }

        state
    }

    fn run_graph(&mut self, unit: &Unit, graph: &ControlFlowGraph, start: State) -> State {
        let mut incoming = vec![Partitions::default(); graph.nodes.len()];
        incoming[graph.entry].insert(start);

        let mut pending = VecDeque::from([graph.entry]);
        let mut budget = 20_000;

        while let Some(index) = pending.pop_front() {
            if budget == 0 {
                let state = incoming[index].joined().unwrap();

                incoming[0].insert(state);
                break;
            }

            budget -= 1;

            let node = &graph.nodes[index];

            for mut state in incoming[index].states.clone() {
                let mut gate = Value::default();

                for parent in &graph.controls[index] {
                    if let Some(condition) = state.conditions.get(parent) {
                        gate.add_control(condition);
                    }
                }

                if matches!(node.operation, Operation::Exit) {
                    continue;
                }
                let condition = self.transfer(unit, node, &mut state, &gate, true);
                let choice = condition.as_ref().and_then(Value::truth);
                if let Some(condition) = condition {
                    state.conditions.insert(index, condition);
                }

                for (branch, successor) in node.successors.iter().enumerate() {
                    if node.successors.len() == 2
                        && choice.is_some_and(|truth| usize::from(!truth) != branch)
                    {
                        continue;
                    }

                    let mut next = state.clone();

                    if let Operation::Branch(expression) = &node.operation {
                        self.refine(unit, expression, branch == 0, &mut next);
                    }

                    let changed = incoming[*successor].insert(next);

                    if changed {
                        pending.push_back(*successor);
                    }
                }
            }
        }

        if let Some(exit) = incoming[0].joined() {
            return exit;
        }

        let mut state = State::default();

        for reachable in incoming.iter().filter_map(Partitions::joined) {
            state.join(&reachable);
        }

        state
    }

    fn run_network(&mut self, unit: &Unit, nodes: &[FlowNode], mut state: State) -> State {
        for _ in 0..128 {
            let previous = state.clone();
            state.conditions.clear();
            for node in nodes {
                let entry = state.clone();
                self.transfer(unit, node, &mut state, &Value::default(), false);
                state.join(&entry);
            }
            if state == previous {
                break;
            }
        }
        state
    }

    fn transfer(
        &mut self,
        unit: &Unit,
        node: &FlowNode,
        state: &mut State,
        gate: &Value,
        strong: bool,
    ) -> Option<Value> {
        match &node.operation {
            Operation::Exit => {}

            Operation::Assign(..) => self.assign(unit, node, state, gate, strong),

            Operation::Call(expression) => {
                self.call(unit, expression, state, &node.source, gate, strong);
            }

            Operation::Branch(expression) => {
                let condition = self.expression(unit, expression, state, &node.source);
                return Some(condition);
            }

            Operation::MayAffect(condition, target) => {
                let mut gate = self.expression(unit, condition, state, &node.source);
                gate.constants = None;

                if self
                    .symbols
                    .callable_name(unit, target)
                    .is_some_and(|u| matches!(u.kind, UnitKind::Action | UnitKind::Method))
                {
                    self.call(
                        unit,
                        &Expr::Call {
                            callee: Box::new(target.clone()),
                            arguments: vec![],
                        },
                        state,
                        &node.source,
                        &gate,
                        false,
                    );
                } else if let Some(storage) = self.symbols.endpoint(unit, target) {
                    let key = self.cell(storage);
                    let mut value = state.read(&key);
                    value.constants = None;
                    value.add_control(&gate);
                    self.write(state, key, value, false);
                }
            }

            Operation::Unknown => {}

            Operation::Advance(variable) => {
                if let Some(storage) = self.symbols.endpoint(unit, variable) {
                    let key = self.cell(storage);
                    let mut value = state.read(&key);
                    value.constants = None;
                    self.write(state, key, value, true);
                }
            }
        }
        None
    }

    fn write(&self, state: &mut State, key: String, mut value: Value, strong: bool) {
        if !strong {
            value.join(&state.read(&key));
        }

        state.values.insert(key.clone(), value);
        state.writes.insert(key);
    }

    fn expression(
        &mut self,
        unit: &Unit,
        expression: &Expr,
        state: &mut State,
        source: &Source,
    ) -> Value {
        if let Some(storage) = self.symbols.endpoint(unit, expression) {
            if let Expr::Member { base, name } = expression
                && let Ok(bit) = name.parse::<u8>()
                && storage.ty.base.eq_ignore_ascii_case("BOOL")
            {
                let mut value = self.expression(unit, base, state, source);
                value.constants = value.constants.and_then(|values| {
                    values
                        .into_iter()
                        .map(|c| match c {
                            Constant::Integer(n) => {
                                Some(Constant::Boolean(n & (1i128 << bit) != 0))
                            }

                            _ => None,
                        })
                        .collect()
                });
                return value;
            }

            let key = self.cell(storage);
            return state.read(&key);
        }

        match expression {
            Expr::Literal { value } => self
                .symbols
                .enum_value(unit, value)
                .map(|v| Value::literal(&v.to_string()))
                .unwrap_or_else(|| Value::literal(value)),

            Expr::Unary { operator, operand } => {
                Value::unary(*operator, self.expression(unit, operand, state, source))
            }

            Expr::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(unit, left, state, source);
                let right = self.expression(unit, right, state, source);
                Value::binary(*operator, left, right)
            }

            Expr::Call { .. } => {
                self.call(unit, expression, state, source, &Value::default(), true)
            }

            Expr::Index { base, indices } => {
                let mut value = self.expression(unit, base, state, source);

                for index in indices {
                    value.add_control(&self.expression(unit, index, state, source));
                }

                value.constants = None;

                value
            }

            Expr::Unknown { operands } => {
                let mut value = Value::default();
                for operand in operands {
                    value.join(&self.expression(unit, operand, state, source));
                }
                value.constants = None;
                value
            }

            _ => self
                .symbols
                .enum_value(unit, &expression.text())
                .map(|v| Value::literal(&v.to_string()))
                .unwrap_or_default(),
        }
    }

    fn refine(&mut self, unit: &Unit, expression: &Expr, truth: bool, state: &mut State) {
        match expression {
            Expr::Unary { operator, operand } if *operator == UnaryOp::Not => {
                self.refine(unit, operand, !truth, state)
            }

            Expr::Binary {
                operator,
                left,
                right,
            } if *operator == BinaryOp::And && truth || *operator == BinaryOp::Or && !truth => {
                self.refine(unit, left, truth, state);
                self.refine(unit, right, truth, state);
            }

            Expr::Binary {
                operator,
                left,
                right,
            } if matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual) => {
                let equal = truth == (*operator == BinaryOp::Equal);
                let right_value = self
                    .symbols
                    .enum_value(unit, &right.text())
                    .map(|v| Value::literal(&v.to_string()))
                    .unwrap_or_else(|| Value::literal(&right.text()));

                if let Some(storage) = self.symbols.endpoint(unit, left)
                    && let Some(constants) = right_value.constants
                {
                    let key = self.cell(storage);
                    let mut value = state.read(&key);

                    if equal {
                        value.constants = Some(constants);
                    } else if let Some(values) = &mut value.constants {
                        values.retain(|v| !constants.contains(v));
                    }

                    state.values.insert(key, value);
                }
            }

            _ => {
                if let Some(storage) = self.symbols.endpoint(unit, expression)
                    && matches!(
                        storage.ty.base.to_ascii_uppercase().as_str(),
                        "BOOL" | "BIT"
                    )
                {
                    let key = self.cell(storage);
                    let mut value = state.read(&key);
                    value.constants = Some(BTreeSet::from([Constant::Boolean(truth)]));
                    state.values.insert(key, value);
                }
            }
        }
    }
}

fn initial(variable: &Variable) -> Value {
    if let Some(expression) = &variable.initial {
        return initial_expression(expression);
    }

    initial_type(&variable.ty)
}

fn initial_expression(expression: &Expr) -> Value {
    match expression {
        Expr::Literal { value } => Value::literal(value),
        Expr::Unary { operator, operand } => Value::unary(*operator, initial_expression(operand)),
        _ => Value::default(),
    }
}

fn initial_type(ty: &TypeRef) -> Value {
    match ty.base.to_ascii_uppercase().as_str() {
        "BOOL" | "BIT" => Value::literal("FALSE"),

        "BYTE" | "WORD" | "DWORD" | "LWORD" | "SINT" | "USINT" | "INT" | "UINT" | "DINT"
        | "UDINT" | "LINT" | "ULINT" => Value::literal("0"),

        _ => Value::default(),
    }
}
