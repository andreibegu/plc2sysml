use super::*;

#[derive(Clone)]
struct Path {
    guard: ControlExpr,
    values: BTreeMap<String, ControlExpr>,
    returned: bool,
    samples: BTreeMap<String, ControlExpr>,
}

impl SymbolicAnalysis<'_> {
    pub(super) fn footprint(&mut self, scope: &Scope, body: &Body) {
        use crate::analysis::cfg::{ControlFlowGraph, Operation};
        if body.is_ordered() {
            let graph = ControlFlowGraph::new(body);
            for (index, node) in graph.nodes.iter().enumerate() {
                if let Operation::Assign(target, value) = &node.operation {
                    let guards: Vec<_> = graph.controls[index]
                        .iter()
                        .filter_map(|parent| match &graph.nodes[*parent].operation {
                            Operation::Branch(condition) => Some(condition.clone()),
                            _ => None,
                        })
                        .collect();
                    self.assignment_dependencies(scope, target, value, &guards);
                }
            }
        } else {
            for statement in &body.statements {
                if let StatementKind::Assign { target, value } = &statement.kind {
                    self.assignment_dependencies(scope, target, value, &[]);
                }
            }
        }
    }

    fn assignment_dependencies(
        &mut self,
        scope: &Scope,
        target: &Expr,
        expression: &Expr,
        guards: &[Expr],
    ) {
        let Some(storage) = self.storage(scope, target) else {
            return;
        };
        let key = self.cell(storage);
        let value = self.expression(scope, expression, &BTreeMap::new());
        self.record_dependencies(&key, &value);
        for condition in guards {
            let guard = self.expression(scope, condition, &BTreeMap::new());
            self.record_dependencies(&key, &guard);
        }
    }

    pub(super) fn wiring(&mut self, scope: &Scope, statements: &[Statement]) {
        for statement in statements {
            if let StatementKind::Assign { target, value } = &statement.kind {
                let Some(storage) = self.storage(scope, target) else {
                    continue;
                };

                let key = self.cell(storage);
                let mut expression = self.expression(scope, value, &BTreeMap::new());

                if let Expr::Call { arguments, .. } = value
                    && let Some(enable) = arguments.iter().find(|a| {
                        a.name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case("EN"))
                    })
                {
                    let condition = self.expression(scope, &enable.value, &BTreeMap::new());
                    expression = ControlExpr::choose(
                        condition,
                        expression,
                        ControlExpr::Read {
                            key: key.clone(),
                            previous: true,
                        },
                    );
                }

                if matches!(expression, ControlExpr::Read { .. })
                    && let Some(original) = self.symbols.endpoint(scope.unit, target)
                    && let Some(unit) = self.model.units.iter().find(|u| u.id == original.unit)
                    && unit.variables.iter().any(|v| {
                        original.path.first() == Some(&v.name)
                            && matches!(v.kind, VariableKind::Local | VariableKind::Static)
                    })
                {
                    self.result
                        .cells
                        .get_mut(&key)
                        .expect("registered control storage")
                        .sampled = true;
                }

                self.define(key, expression);
            }
        }
    }

    pub(super) fn structured(&mut self, scope: &Scope, behavior: &Body) {
        let mut initial = BTreeMap::new();
        self.written(scope, &behavior.statements, &mut initial);

        let paths = self.block(
            scope,
            &behavior.statements,
            vec![Path {
                guard: ControlExpr::boolean(true),
                values: initial,
                returned: false,
                samples: BTreeMap::new(),
            }],
            0,
        );

        let Some(paths) = paths else {
            return;
        };

        let keys: BTreeSet<_> = paths
            .iter()
            .flat_map(|p| p.values.keys().cloned())
            .collect();

        for key in keys {
            let mut groups: Vec<(ControlExpr, ControlExpr)> = vec![];

            for path in &paths {
                let next = path.values.get(&key).cloned().unwrap_or(ControlExpr::Read {
                    key: key.clone(),
                    previous: true,
                });

                if let Some((_, guard)) = groups.iter_mut().find(|(value, _)| value == &next) {
                    *guard = ControlExpr::binary(BinaryOp::Or, guard.clone(), path.guard.clone());
                } else {
                    groups.push((next, path.guard.clone()));
                }
            }

            if groups.iter().all(|(value, _)| value.truth().is_some()) {
                let value = groups
                    .iter()
                    .filter(|(value, _)| value.truth() == Some(true))
                    .map(|(_, guard)| guard.clone())
                    .reduce(|a, b| ControlExpr::binary(BinaryOp::Or, a, b))
                    .unwrap_or(ControlExpr::boolean(false));
                self.complete(scope, key, value);
                continue;
            }

            let mut value = groups.pop().expect("reachable path").0;

            for (next, guard) in groups.into_iter().rev() {
                value = ControlExpr::choose(guard, next, value);
            }

            self.complete(scope, key, value);
        }
    }

    fn complete(&mut self, scope: &Scope, key: String, value: ControlExpr) {
        if scope.unit.kind == UnitKind::Program
            && self.result.cells[&key].storage.unit == scope.unit.id
        {
            self.memory(&key, &value, scope);
            self.capture(key, value);
        } else {
            self.define(key, value);
        }
    }

    fn memory(&mut self, key: &str, value: &ControlExpr, scope: &Scope) {
        if self
            .result
            .cells
            .get(key)
            .is_none_or(|c| c.storage.unit != scope.unit.id)
        {
            return;
        }

        let initial = self
            .result
            .cells
            .get(key)
            .and_then(|c| c.initial.clone())
            .or_else(|| {
                let ty = &self.result.cells.get(key)?.storage.ty;

                match ty.base.to_ascii_uppercase().as_str() {
                    "BOOL" | "BIT" => Some(Expr::boolean(false)),
                    _ if ty.integer_bounds().is_some() => Some(Expr::Literal { value: "0".into() }),
                    _ => None,
                }
            });

        self.result.memories.insert(
            key.into(),
            ControlMemory {
                owner: scope.unit.id.clone(),
                expression: value.clone(),
                initial,
            },
        );
    }

    fn written(
        &mut self,
        scope: &Scope,
        statements: &[Statement],
        values: &mut BTreeMap<String, ControlExpr>,
    ) {
        for statement in statements {
            match &statement.kind {
                StatementKind::Assign { target, .. } => {
                    if let Some(storage) = self.storage(scope, target) {
                        let key = self.cell(storage);
                        values.insert(
                            key.clone(),
                            ControlExpr::Read {
                                key,
                                previous: true,
                            },
                        );
                    }
                }

                StatementKind::Call {
                    expression: Expr::Call { callee, arguments },
                } => {
                    if let Some(signature) = self.symbols.callable_name(scope.unit, callee) {
                        let instance = self
                            .symbols
                            .call_instance(scope.unit, signature, callee, &statement.source)
                            .map(|storage| {
                                instance_storage(scope.unit, scope.instance.as_ref(), &storage)
                            });

                        let called = Scope {
                            unit: signature,
                            instance,
                            name: scope.name.clone(),
                        };

                        if signature.intrinsic.is_some() {
                            self.standard_values(&called);

                            for output in signature
                                .variables
                                .iter()
                                .filter(|v| v.kind == VariableKind::Output)
                            {
                                if let Some(storage) = self.storage(
                                    &called,
                                    &Expr::Reference {
                                        name: output.name.clone(),
                                    },
                                ) {
                                    let key = self.cell(storage);
                                    values.entry(key.clone()).or_insert(ControlExpr::Read {
                                        key,
                                        previous: true,
                                    });
                                }
                            }
                        }

                        for (argument, parameter) in
                            arguments.iter().zip(parameters(signature, arguments))
                        {
                            if parameter.is_some_and(|p| p.kind == VariableKind::Output)
                                && argument.direction == ArgumentDirection::Output
                                && let Some(storage) = self.storage(scope, &argument.value)
                            {
                                let key = self.cell(storage);
                                values.entry(key.clone()).or_insert(ControlExpr::Read {
                                    key,
                                    previous: true,
                                });
                            }
                        }
                    }
                }

                StatementKind::If {
                    branches,
                    otherwise,
                } => {
                    for b in branches {
                        self.written(scope, &b.body, values);
                    }

                    self.written(scope, otherwise, values);
                }

                StatementKind::Case {
                    arms, otherwise, ..
                } => {
                    for a in arms {
                        self.written(scope, &a.body, values);
                    }

                    self.written(scope, otherwise, values);
                }

                _ => {}
            }
        }
    }

    fn block(
        &mut self,
        scope: &Scope,
        statements: &[Statement],
        mut paths: Vec<Path>,
        depth: usize,
    ) -> Option<Vec<Path>> {
        if depth > 32 {
            return None;
        }

        for statement in statements {
            let mut out = vec![];

            for mut path in paths {
                if path.returned || path.guard.truth() == Some(false) {
                    out.push(path);
                    continue;
                }

                match &statement.kind {
                    StatementKind::Assign { target, value } => {
                        let storage = self.storage(scope, target)?;
                        let key = self.cell(storage);
                        let value = self.expression(scope, value, &path.values);
                        path.values.insert(key, value);
                        out.push(path);
                    }

                    StatementKind::If {
                        branches,
                        otherwise,
                    } => {
                        let mut remaining = path;

                        for branch in branches {
                            let condition = self.expression(scope, &branch.test, &remaining.values);
                            let mut selected = remaining.clone();
                            selected.guard = ControlExpr::binary(
                                BinaryOp::And,
                                selected.guard,
                                condition.clone(),
                            );
                            out.extend(self.block(
                                scope,
                                &branch.body,
                                vec![selected],
                                depth + 1,
                            )?);
                            remaining.guard = ControlExpr::binary(
                                BinaryOp::And,
                                remaining.guard,
                                condition.negated(),
                            );
                        }

                        out.extend(self.block(scope, otherwise, vec![remaining], depth + 1)?);
                    }

                    StatementKind::Case {
                        selector,
                        arms,
                        otherwise,
                    } => {
                        let mut remaining = path;

                        for arm in arms {
                            let mut condition = ControlExpr::boolean(false);

                            for label in &arm.test {
                                let guard = self.expression(
                                    scope,
                                    &label.condition(selector),
                                    &remaining.values,
                                );

                                condition = ControlExpr::binary(BinaryOp::Or, condition, guard);
                            }

                            let mut selected = remaining.clone();
                            selected.guard = ControlExpr::binary(
                                BinaryOp::And,
                                selected.guard,
                                condition.clone(),
                            );
                            out.extend(self.block(scope, &arm.body, vec![selected], depth + 1)?);
                            remaining.guard = ControlExpr::binary(
                                BinaryOp::And,
                                remaining.guard,
                                condition.negated(),
                            );
                        }

                        out.extend(self.block(scope, otherwise, vec![remaining], depth + 1)?);
                    }

                    StatementKind::Return => {
                        path.returned = true;
                        out.push(path);
                    }

                    StatementKind::Call { .. } => {
                        if !self.safe_call(scope, statement, &mut path) {
                            return None;
                        }

                        out.push(path);
                    }

                    _ => return None,
                }
            }

            paths = out
                .into_iter()
                .filter(|p| p.guard.truth() != Some(false))
                .collect();

            if paths.len() > 64 {
                return None;
            }
        }

        Some(paths)
    }

    fn safe_call(&mut self, scope: &Scope, statement: &Statement, path: &mut Path) -> bool {
        let StatementKind::Call {
            expression: Expr::Call { callee, arguments },
        } = &statement.kind
        else {
            return false;
        };

        let Some(signature) = self.symbols.callable_name(scope.unit, callee) else {
            return false;
        };

        let instance = self
            .symbols
            .call_instance(scope.unit, signature, callee, &statement.source)
            .map(|s| instance_storage(scope.unit, scope.instance.as_ref(), &s));
        let called = Scope {
            unit: signature,
            instance: instance.clone(),
            name: scope.name.clone(),
        };

        let standard_values = self.standard_values(&called);
        let actuals: Vec<_> = parameters(signature, arguments);
        let mut destinations = BTreeMap::new();

        for (argument, parameter) in arguments.iter().zip(actuals) {
            let Some(parameter) = parameter else {
                return false;
            };

            if parameter.kind == VariableKind::Output
                && argument.direction == ArgumentDirection::Output
            {
                let Some(formal) = self.storage(
                    &called,
                    &Expr::Reference {
                        name: parameter.name.clone(),
                    },
                ) else {
                    return false;
                };

                let Some(target) = self.storage(scope, &argument.value) else {
                    return false;
                };

                let target = self.cell(target);

                if destinations
                    .values()
                    .any(|existing: &String| crate::analysis::overlaps(existing, &target))
                {
                    return false;
                }

                destinations.insert(self.cell(formal), target);
                continue;
            }

            if parameter.kind != VariableKind::Input
                || argument.direction == ArgumentDirection::Output
            {
                return false;
            }

            let Some(formal) = self.storage(
                &called,
                &Expr::Reference {
                    name: parameter.name.clone(),
                },
            ) else {
                return false;
            };

            let key = self.cell(formal);
            let value = self.expression(scope, &argument.value, &path.values);
            self.capture(key.clone(), value.clone());
            path.values.insert(key, value);
        }

        if signature.kind == UnitKind::Method || signature.kind == UnitKind::Action {
            let fields: Vec<_> = signature
                .bodies
                .iter()
                .flat_map(|b| b.statements.iter())
                .filter_map(|s| {
                    if let StatementKind::Assign { target, .. } = &s.kind {
                        self.symbols.endpoint(signature, target)
                    } else {
                        None
                    }
                })
                .collect();

            if signature
                .bodies
                .iter()
                .flat_map(|b| b.statements.iter())
                .any(|s| !matches!(s.kind, StatementKind::Assign { .. }))
            {
                return false;
            }

            for field in fields {
                let key = self.cell(instance_storage(signature, instance.as_ref(), &field));
                path.values.entry(key.clone()).or_insert(ControlExpr::Read {
                    key,
                    previous: true,
                });
            }

            for behavior in &signature.bodies {
                for statement in &behavior.statements {
                    if let StatementKind::Assign { target, value } = &statement.kind {
                        let Some(storage) = self.storage(&called, target) else {
                            return false;
                        };

                        let key = self.cell(storage);
                        let value = self.expression(&called, value, &path.values);
                        path.values.insert(key, value);
                    }
                }
            }

            true
        } else if signature.kind == UnitKind::FunctionBlock {
            let standard = signature.intrinsic.is_some();
            let mut output_keys = vec![];

            for variable in &signature.variables {
                if variable.kind == VariableKind::Output {
                    let field = StorageRef {
                        unit: signature.id.clone(),
                        path: vec![variable.name.clone()],
                        ty: variable.ty.clone(),
                        source: variable.source.clone(),
                    };

                    let key = self.cell(instance_storage(signature, instance.as_ref(), &field));

                    if standard && !standard_values.contains_key(&key) {
                        path.values.insert(
                            key.clone(),
                            ControlExpr::Read {
                                key: key.clone(),
                                previous: false,
                            },
                        );
                    }

                    output_keys.push(key);
                }
            }

            let outputs: Vec<_> = output_keys
                .into_iter()
                .map(|key| {
                    let value = standard_values
                        .get(&key)
                        .map(|e| substitute(e, &path.values, &path.samples))
                        .unwrap_or(ControlExpr::Read {
                            key: key.clone(),
                            previous: false,
                        });

                    (key, value)
                })
                .collect();

            for (key, value) in outputs {
                if let Some(target) = destinations.get(&key) {
                    path.values.insert(target.clone(), value.clone());
                }

                if standard {
                    path.samples.insert(key.clone(), value.clone());
                }

                path.values.insert(key, value);
            }

            if standard {
                for variable in signature
                    .variables
                    .iter()
                    .filter(|v| v.kind == VariableKind::Input)
                {
                    if let Some(storage) = self.storage(
                        &called,
                        &Expr::Reference {
                            name: variable.name.clone(),
                        },
                    ) {
                        let key = self.cell(storage);

                        if self.result.cells[&key].invocation_memory
                            && let Some(value) = path.values.get(&key)
                        {
                            path.samples.insert(key, value.clone());
                        }
                    }
                }
            }

            true
        } else {
            false
        }
    }
}

fn substitute(
    e: &ControlExpr,
    values: &BTreeMap<String, ControlExpr>,
    samples: &BTreeMap<String, ControlExpr>,
) -> ControlExpr {
    match e {
        ControlExpr::Read { key, previous } => if *previous {
            samples.get(key)
        } else {
            values.get(key)
        }
        .cloned()
        .unwrap_or_else(|| e.clone()),

        ControlExpr::Unary { operator, operand } => {
            let operand = substitute(operand, values, samples);

            if *operator == UnaryOp::Not {
                operand.negated()
            } else {
                ControlExpr::Unary {
                    operator: *operator,
                    operand: Box::new(operand),
                }
            }
        }

        ControlExpr::Binary {
            operator,
            left,
            right,
        } => ControlExpr::binary(
            *operator,
            substitute(left, values, samples),
            substitute(right, values, samples),
        ),

        ControlExpr::Choose { condition, yes, no } => ControlExpr::choose(
            substitute(condition, values, samples),
            substitute(yes, values, samples),
            substitute(no, values, samples),
        ),

        _ => e.clone(),
    }
}
