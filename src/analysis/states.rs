use crate::ir::*;
use crate::{analysis::unit_accesses, resolve::Symbols};

#[derive(Debug, Clone)]
pub(crate) struct StateMachine {
    pub unit: String,
    pub body: String,
    pub graph: StateGraph,
}

pub(super) fn infer(project: &Program) -> Vec<StateMachine> {
    let symbols = Symbols::new(project);
    let accesses = unit_accesses(project);
    let mut states = vec![];

    for unit in &project.units {
        for behavior in &unit.bodies {
            if let Some(graph) = behavior.graph() {
                states.push(StateMachine {
                    unit: unit.id.clone(),
                    body: behavior.id.clone(),
                    graph: graph.clone(),
                });
            } else if behavior.is_ordered() {
                for selector in selectors(&behavior.statements) {
                    let Some(storage) = symbols.endpoint(unit, &selector) else {
                        continue;
                    };

                    let Some(owner) = project.units.iter().find(|u| u.id == storage.unit) else {
                        continue;
                    };

                    let Some(variable) = storage
                        .path
                        .first()
                        .filter(|_| storage.path.len() == 1)
                        .and_then(|name| owner.variable(name))
                    else {
                        continue;
                    };

                    if !matches!(
                        variable.kind,
                        VariableKind::Local | VariableKind::Static | VariableKind::Instance
                    ) || variable.address.is_some()
                        || variable.ty.indirection.is_some()
                        || !variable.ty.dimensions.is_empty()
                        || matches!(
                            variable.retention,
                            Retention::Retained | Retention::Persistent
                        )
                    {
                        continue;
                    }

                    if !assigns(&symbols, unit, &behavior.statements, &storage) {
                        continue;
                    }

                    let external_write = accesses
                        .iter()
                        .any(|(id, a)| id != &unit.id && a.writes(&storage));

                    if external_write {
                        continue;
                    }

                    if let Some(graph) =
                        recover_selector(&symbols, unit, behavior, &selector, variable)
                    {
                        states.push(StateMachine {
                            unit: unit.id.clone(),
                            body: behavior.id.clone(),
                            graph,
                        });
                    }
                }
            }
        }
    }

    states
}

use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_STATES: usize = 64;
const MAX_PATHS: usize = 64;

pub(super) fn assigns(
    symbols: &Symbols<'_>,
    unit: &Unit,
    statements: &[Statement],
    selector: &StorageRef,
) -> bool {
    let mut found = false;
    for statement in statements {
        statement.visit(&mut |statement| {
            if let StatementKind::Assign { target, .. } = &statement.kind {
                found |= symbols
                    .endpoint(unit, target)
                    .is_some_and(|s| s.key() == selector.key());
            }
        });
    }
    found
}

pub(super) fn selectors(statements: &[Statement]) -> Vec<Expr> {
    let mut selectors = BTreeMap::new();
    for statement in statements {
        statement.visit(&mut |statement| {
            if let StatementKind::Case { selector, .. } = &statement.kind {
                selectors
                    .entry(selector.text().to_ascii_lowercase())
                    .or_insert_with(|| selector.clone());
            }
        });
    }
    selectors.into_values().collect()
}

#[derive(Clone)]
struct Path {
    guard: Expr,
    values: BTreeMap<String, Expr>,
    returned: bool,
}

struct SelectorAnalysis<'a, 'b> {
    symbols: &'a Symbols<'b>,
    unit: &'a Unit,
    selector: String,
    assigned: bool,
    labels: BTreeMap<i128, String>,
}

fn recover_selector(
    symbols: &Symbols<'_>,
    unit: &Unit,
    behavior: &Body,
    selector: &Expr,
    variable: &Variable,
) -> Option<StateGraph> {
    let initial = variable
        .initial
        .as_ref()
        .and_then(|value| number(symbols, unit, value))?;

    let mut analysis = SelectorAnalysis {
        symbols,
        unit,
        selector: symbols
            .endpoint(unit, selector)
            .expect("resolved selector")
            .key(),
        assigned: false,
        labels: BTreeMap::new(),
    };

    let entry = analysis.entry_values(&behavior.statements)?;
    let mut pending = VecDeque::from([initial]);
    let mut seen = BTreeSet::from([initial]);
    let mut edges = BTreeMap::<(i128, i128), Expr>::new();

    while let Some(current) = pending.pop_front() {
        let paths = analysis.block(
            &behavior.statements,
            vec![Path {
                guard: Expr::boolean(true),
                values: {
                    let mut values = entry.clone();
                    values.insert(
                        analysis.selector.clone(),
                        Expr::Literal {
                            value: current.to_string(),
                        },
                    );
                    values
                },
                returned: false,
            }],
        )?;

        for path in paths {
            if path.guard.as_boolean() == Some(false) {
                continue;
            }

            let next = path
                .values
                .get(&analysis.selector)
                .and_then(|e| number(symbols, unit, e))?;

            if seen.insert(next) {
                if seen.len() > MAX_STATES {
                    return None;
                }

                pending.push_back(next);
            }

            if current != next {
                edges
                    .entry((current, next))
                    .and_modify(|e| *e = e.clone().disjoin(path.guard.clone()))
                    .or_insert(path.guard);
            }
        }
    }

    if !analysis.assigned || seen.len() < 2 {
        return None;
    }

    let label = |state: &i128| {
        analysis
            .labels
            .get(state)
            .cloned()
            .unwrap_or_else(|| format!("State {state}"))
    };

    Some(StateGraph {
        activities: vec![],
        reset: None,
        name: selector.text(),
        selector: Some(selector.clone()),
        observation: None,
        initial: label(&initial),
        states: seen
            .iter()
            .map(|s| ControlState {
                name: label(s),
                active_steps: vec![label(s)],
                selector_value: Some(s.to_string()),
            })
            .collect(),
        transitions: edges
            .into_iter()
            .map(|((from, to), condition)| StateTransition {
                from: label(&from),
                to: label(&to),
                condition,
                source: behavior.source.clone(),
            })
            .collect(),
    })
}

impl SelectorAnalysis<'_, '_> {
    fn entry_values(&self, statements: &[Statement]) -> Option<BTreeMap<String, Expr>> {
        let mut values = BTreeMap::new();
        let mut calls = BTreeSet::new();
        let mut pending: Vec<_> = statements.iter().collect();

        while let Some(statement) = pending.pop() {
            let mut retain = |expression: Expr| {
                values
                    .entry(self.key(&expression))
                    .or_insert(Expr::Snapshot {
                        value: Box::new(expression),
                    });
            };

            match &statement.kind {
                StatementKind::Assign { target, .. } => retain(target.clone()),

                StatementKind::Call {
                    expression: Expr::Call { callee, arguments },
                } => {
                    if let Some(signature) = self
                        .symbols
                        .callable_name(self.unit, callee)
                        .filter(|u| u.intrinsic.is_some())
                    {
                        if !calls.insert(self.key(callee)) {
                            return None;
                        }

                        for variable in signature
                            .variables
                            .iter()
                            .filter(|v| v.kind == VariableKind::Output)
                        {
                            retain(Expr::Member {
                                base: callee.clone(),
                                name: variable.name.clone(),
                            });
                        }

                        for (argument, parameter) in arguments
                            .iter()
                            .zip(crate::resolve::parameters(signature, arguments))
                        {
                            if argument.direction == ArgumentDirection::Output
                                && parameter.is_some_and(|p| p.kind == VariableKind::Output)
                            {
                                retain(argument.value.clone());
                            }
                        }
                    }
                }

                StatementKind::If {
                    branches,
                    otherwise,
                } => {
                    pending.extend(otherwise);

                    for branch in branches {
                        pending.extend(&branch.body);
                    }
                }

                StatementKind::Case {
                    arms, otherwise, ..
                } => {
                    pending.extend(otherwise);

                    for arm in arms {
                        pending.extend(&arm.body);
                    }
                }

                _ => {}
            }
        }

        Some(values)
    }

    fn block(&mut self, statements: &[Statement], mut paths: Vec<Path>) -> Option<Vec<Path>> {
        for statement in statements {
            let mut out = vec![];

            for mut path in paths {
                if path.returned || path.guard.as_boolean() == Some(false) {
                    out.push(path);
                    continue;
                }

                match &statement.kind {
                    StatementKind::Assign { target, value } => {
                        let key = self.key(target);

                        if matches!(target, Expr::Unknown { .. } | Expr::Index { .. }) {
                            return None;
                        }

                        let value = self.substitute(value, &path.values);

                        if key == self.selector {
                            self.assigned = true;

                            number(self.symbols, self.unit, &value)?;
                        }

                        path.values.insert(key, value);
                        out.push(path);
                    }

                    StatementKind::If {
                        branches,
                        otherwise,
                    } => {
                        let mut remaining = path;

                        for branch in branches {
                            let condition = self.substitute(&branch.test, &remaining.values);
                            let mut selected = remaining.clone();
                            selected.guard = selected.guard.conjoin(condition.clone());
                            out.extend(self.block(&branch.body, vec![selected])?);
                            remaining.guard = remaining.guard.conjoin(condition.negated());
                        }

                        out.extend(self.block(otherwise, vec![remaining])?);
                    }

                    StatementKind::Case {
                        selector,
                        arms,
                        otherwise,
                    } => {
                        let value = self.substitute(selector, &path.values);
                        let mut previous = Expr::boolean(false);

                        for arm in arms {
                            let mut guard = Expr::boolean(false);

                            for label in &arm.test {
                                let values = case_values(self.symbols, self.unit, label)?;

                                for number in values {
                                    if self.key(selector) == self.selector
                                        && let CaseLabel::Value(
                                            name @ (Expr::Reference { .. } | Expr::Member { .. }),
                                        ) = label
                                    {
                                        let text = name.text();
                                        self.labels.insert(
                                            number,
                                            text.rsplit('.').next().unwrap_or(&text).into(),
                                        );
                                    }

                                    guard = guard.disjoin(fold(Expr::Binary {
                                        operator: BinaryOp::Equal,
                                        left: Box::new(value.clone()),
                                        right: Box::new(Expr::Literal {
                                            value: number.to_string(),
                                        }),
                                    }));
                                }
                            }

                            let mut selected = path.clone();
                            selected.guard = selected
                                .guard
                                .conjoin(previous.clone().negated())
                                .conjoin(guard.clone());
                            out.extend(self.block(&arm.body, vec![selected])?);
                            previous = previous.disjoin(guard);
                        }

                        path.guard = path.guard.conjoin(previous.negated());
                        out.extend(self.block(otherwise, vec![path])?);
                    }

                    StatementKind::Return => {
                        path.returned = true;
                        out.push(path);
                    }

                    StatementKind::Call {
                        expression: Expr::Call { callee, arguments },
                    } => {
                        let signature = self
                            .symbols
                            .callable_name(self.unit, callee)
                            .filter(|u| u.intrinsic.is_some())?;

                        for variable in signature
                            .variables
                            .iter()
                            .filter(|v| v.kind == VariableKind::Output)
                        {
                            let output = Expr::Member {
                                base: callee.clone(),
                                name: variable.name.clone(),
                            };

                            path.values.insert(self.key(&output), output);
                        }

                        for (arg, parameter) in arguments
                            .iter()
                            .zip(crate::resolve::parameters(signature, arguments))
                        {
                            if arg.direction == ArgumentDirection::Output
                                || parameter.is_some_and(|p| p.kind == VariableKind::InOut)
                            {
                                let key = self.key(&arg.value);

                                if key == self.selector {
                                    return None;
                                }

                                let parameter = parameter?;

                                path.values.insert(
                                    key,
                                    Expr::Member {
                                        base: callee.clone(),
                                        name: parameter.name.clone(),
                                    },
                                );
                            }
                        }

                        out.push(path);
                    }

                    _ => return None,
                }
            }

            paths = out
                .into_iter()
                .filter(|p| p.guard.as_boolean() != Some(false))
                .collect();

            if paths.len() > MAX_PATHS {
                return None;
            }
        }

        Some(paths)
    }

    fn substitute(&self, expression: &Expr, values: &BTreeMap<String, Expr>) -> Expr {
        let key = self.key(expression);

        if let Some(value) = values.get(&key) {
            return value.clone();
        }

        if let Some(value) = number(self.symbols, self.unit, expression) {
            return Expr::Literal {
                value: value.to_string(),
            };
        }

        fold(match expression {
            Expr::Unary { operator, operand } => Expr::Unary {
                operator: *operator,
                operand: Box::new(self.substitute(operand, values)),
            },

            Expr::Call { callee, arguments } => Expr::Call {
                callee: callee.clone(),
                arguments: arguments
                    .iter()
                    .map(|argument| Argument {
                        name: argument.name.clone(),
                        direction: argument.direction,
                        value: self.substitute(&argument.value, values),
                    })
                    .collect(),
            },

            Expr::Binary {
                operator,
                left,
                right,
            } => Expr::Binary {
                operator: *operator,
                left: Box::new(self.substitute(left, values)),
                right: Box::new(self.substitute(right, values)),
            },

            _ => expression.clone(),
        })
    }

    fn key(&self, expression: &Expr) -> String {
        self.symbols
            .endpoint(self.unit, expression)
            .map(|s| s.key())
            .unwrap_or_else(|| expression.text().to_ascii_lowercase())
    }
}

fn number(symbols: &Symbols<'_>, unit: &Unit, expression: &Expr) -> Option<i128> {
    match expression {
        Expr::Unary {
            operator: UnaryOp::Negate,
            operand,
        } => number(symbols, unit, operand)?.checked_neg(),

        Expr::Unary {
            operator: UnaryOp::Positive,
            operand,
        } => number(symbols, unit, operand),

        _ => expression
            .text()
            .parse()
            .ok()
            .or_else(|| symbols.enum_value(unit, &expression.text())),
    }
}

fn case_values(symbols: &Symbols<'_>, unit: &Unit, label: &CaseLabel) -> Option<Vec<i128>> {
    match label {
        CaseLabel::Value(value) => number(symbols, unit, value).map(|v| vec![v]),
        CaseLabel::Range { lower, upper } => {
            let lower = number(symbols, unit, lower)?;
            let upper = number(symbols, unit, upper)?;
            if upper < lower || upper.saturating_sub(lower) >= MAX_STATES as i128 {
                return None;
            }
            Some((lower..=upper).collect())
        }
    }
}

fn fold(expression: Expr) -> Expr {
    match expression {
        Expr::Unary {
            operator: UnaryOp::Not,
            operand,
        } => operand.negated(),

        Expr::Binary {
            operator,
            left,
            right,
        } => {
            if operator == BinaryOp::And {
                return left.conjoin(*right);
            }

            if operator == BinaryOp::Or {
                return left.disjoin(*right);
            }

            let numbers = left
                .text()
                .parse::<i128>()
                .ok()
                .zip(right.text().parse::<i128>().ok());

            if let Some((a, b)) = numbers {
                let predicate = match operator {
                    BinaryOp::Equal => Some(a == b),
                    BinaryOp::NotEqual => Some(a != b),
                    BinaryOp::Less => Some(a < b),
                    BinaryOp::Greater => Some(a > b),
                    BinaryOp::LessEqual => Some(a <= b),
                    BinaryOp::GreaterEqual => Some(a >= b),
                    _ => None,
                };

                if let Some(value) = predicate {
                    return Expr::boolean(value);
                }
            }

            Expr::Binary {
                operator,
                left,
                right,
            }
        }

        other => other,
    }
}
