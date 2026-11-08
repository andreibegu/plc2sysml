mod expressions;
mod intrinsics;
mod slice;
mod statements;

pub use expressions::ControlExpr;

use super::{
    Analysis, ControlCell, ControlInstance, ControlMemory, ControlObservation, SignalBoundary,
    states,
};

use crate::resolve::Symbols;
use crate::{
    ir::*,
    resolve::{instance_storage, parameters},
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_INSTANCES: usize = 1024;

#[derive(Clone)]
struct Scope<'a> {
    unit: &'a Unit,
    instance: Option<StorageRef>,
    name: String,
}

struct SymbolicAnalysis<'a> {
    model: &'a Program,
    symbols: Symbols<'a>,
    result: Analysis,
    scopes: BTreeMap<String, Scope<'a>>,
    pending: Vec<String>,
    ambiguous: BTreeSet<String>,
    captured: BTreeSet<String>,
}

pub(super) fn analyze(model: &Program, boundaries: &[SignalBoundary]) -> Analysis {
    let states = states::infer(model);
    let mut r = SymbolicAnalysis {
        model,
        symbols: Symbols::new(model),
        result: Analysis::default(),
        scopes: BTreeMap::new(),
        pending: vec![],
        ambiguous: BTreeSet::new(),
        captured: BTreeSet::new(),
    };

    for boundary in boundaries {
        r.cell(boundary.storage.clone());
    }

    let programs: Vec<_> = model
        .units
        .iter()
        .filter(|u| {
            u.kind == UnitKind::Program
                && (model.tasks.is_empty()
                    || model
                        .tasks
                        .iter()
                        .any(|t| t.programs.iter().any(|p| p.target.as_ref() == Some(&u.id))))
        })
        .collect();

    for unit in programs {
        r.scope(unit, None, unit.name.clone());
    }

    while let Some(key) = r.pending.pop() {
        let scope = r.scopes[&key].clone();

        for behavior in &scope.unit.bodies {
            r.discover(&scope, behavior);
        }
    }

    let scopes: Vec<_> = r.scopes.values().cloned().collect();

    for scope in &scopes {
        for machine in states.iter().filter(|m| m.unit == scope.unit.id) {
            let id = id(&[
                "control_instance",
                &scope.unit.id,
                &scope.name,
                &machine.body,
            ]);

            let selector = machine
                .graph
                .selector
                .as_ref()
                .and_then(|e| r.storage(scope, e))
                .map(|s| r.cell(s));

            let observation = machine.graph.observation.as_ref().and_then(|o| {
                let storage = r.storage(scope, &o.variable)?;
                Some(ControlObservation {
                    key: r.cell(storage),
                    labels: o.labels.clone(),
                })
            });

            let conditions = machine
                .graph
                .transitions
                .iter()
                .map(|t| r.expression(scope, &t.condition, &BTreeMap::new()))
                .collect();

            let reset = machine
                .graph
                .reset
                .as_ref()
                .map(|e| r.expression(scope, e, &BTreeMap::new()));

            r.result.instances.push(ControlInstance {
                id: id.clone(),
                unit: scope.unit.id.clone(),
                instance: scope.instance.clone(),
                name: scope.name.clone(),
                graph: machine.graph.clone(),
                conditions,
                reset,
                selector,
                observation,
            });
            r.activities(scope, &id, &machine.graph);
        }
    }

    for scope in &scopes {
        r.standard(scope);

        for behavior in &scope.unit.bodies {
            r.footprint(scope, behavior);

            if behavior.is_ordered() {
                r.structured(scope, behavior);
            } else {
                r.wiring(scope, &behavior.statements);
            }
        }
    }

    r.result
}

impl<'a> SymbolicAnalysis<'a> {
    fn cell(&mut self, storage: StorageRef) -> String {
        let key = storage.key();
        let variable = self.symbols.storage_variable(&storage);
        let initial = variable.and_then(|v| v.initial.clone());
        let expression = variable
            .filter(|v| v.constant)
            .and(initial.as_ref())
            .and_then(ControlExpr::constant);

        self.result
            .cells
            .entry(key.clone())
            .or_insert_with(|| ControlCell {
                storage,
                expression,
                sampled: false,
                invocation_memory: false,
                dependencies: vec![],
                initial,
            });

        key
    }

    fn storage(&self, scope: &Scope, expr: &Expr) -> Option<StorageRef> {
        self.symbols
            .endpoint(scope.unit, expr)
            .map(|s| instance_storage(scope.unit, scope.instance.as_ref(), &s))
    }

    fn define(&mut self, key: String, value: ControlExpr) {
        self.record_dependencies(&key, &value);

        if self.ambiguous.contains(&key) {
            return;
        }

        let cell = self
            .result
            .cells
            .get_mut(&key)
            .expect("registered control storage");

        if cell
            .expression
            .as_ref()
            .is_some_and(|previous| previous != &value)
        {
            cell.expression = None;
            self.ambiguous.insert(key);
        } else {
            cell.expression = Some(value);
        }
    }

    fn record_dependencies(&mut self, key: &str, value: &ControlExpr) {
        let mut reads = BTreeSet::new();
        value.visit(&mut |e| {
            if let ControlExpr::Read { key: read, .. } = e {
                reads.insert(read.clone());
            }
        });

        let cell = self.result.cells.get_mut(key).expect("registered storage");
        cell.dependencies.extend(reads);
        cell.dependencies.sort();
        cell.dependencies.dedup();
    }

    fn capture(&mut self, key: String, value: ControlExpr) {
        if !self.ambiguous.contains(&key) && self.captured.insert(key.clone()) {
            self.result
                .cells
                .get_mut(&key)
                .expect("registered call input")
                .expression = None;
        }

        self.define(key, value);
    }

    fn scope(&mut self, unit: &'a Unit, instance: Option<StorageRef>, name: String) {
        let key = format!(
            "{}:{}",
            unit.id,
            instance.as_ref().map(StorageRef::key).unwrap_or_default()
        );

        if self.scopes.contains_key(&key) || self.scopes.len() >= MAX_INSTANCES {
            return;
        }

        self.scopes.insert(
            key.clone(),
            Scope {
                unit,
                instance,
                name,
            },
        );
        self.pending.push(key);
    }

    fn discover(&mut self, scope: &Scope, body: &Body) {
        body.visit_expressions(&mut |expression| match expression {
            Expr::Call { callee, arguments } => self.call(scope, callee, arguments),
            Expr::Reference { .. } | Expr::Member { .. }
                if self
                    .symbols
                    .callable_name(scope.unit, expression)
                    .is_some_and(|u| u.kind == UnitKind::Action) =>
            {
                self.call(scope, expression, &[]);
            }
            _ => {}
        });
    }

    fn call(&mut self, caller: &Scope, callee: &Expr, arguments: &[Argument]) {
        let Some(signature) = self.symbols.callable_name(caller.unit, callee) else {
            return;
        };

        let instance = self
            .symbols
            .call_instance(caller.unit, signature, callee, &caller.unit.source)
            .map(|s| instance_storage(caller.unit, caller.instance.as_ref(), &s));
        let name = instance
            .as_ref()
            .map(|s| {
                let root = self
                    .model
                    .units
                    .iter()
                    .find(|u| u.id == s.unit)
                    .map(|u| u.name.as_str())
                    .unwrap_or("Control");

                std::iter::once(root.to_string())
                    .chain(s.path.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(".")
            })
            .unwrap_or_else(|| caller.name.clone());

        let scope = Scope {
            unit: signature,
            instance: instance.clone(),
            name: name.clone(),
        };

        let params: Vec<_> = parameters(signature, arguments);

        for (arg, param) in arguments.iter().zip(params) {
            let Some(param) = param else {
                continue;
            };

            let Some(formal) = self.storage(
                &scope,
                &Expr::Reference {
                    name: param.name.clone(),
                },
            ) else {
                continue;
            };

            let key = self.cell(formal);

            if param.kind == VariableKind::Input {
                let value = self.expression(caller, &arg.value, &BTreeMap::new());
                self.define(key, value);
            } else if param.kind == VariableKind::Output
                && let Some(target) = self.storage(caller, &arg.value)
            {
                let target = self.cell(target);
                self.define(
                    target,
                    ControlExpr::Read {
                        key,
                        previous: false,
                    },
                );
            }
        }

        self.scope(signature, instance, name);
    }

    fn activities(&mut self, scope: &Scope, machine: &str, graph: &StateGraph) {
        let mut groups = BTreeMap::<String, Vec<&StateActivity>>::new();

        for activity in &graph.activities {
            if let Some(storage) = self.storage(scope, &activity.target)
                && storage.ty.base.eq_ignore_ascii_case("BOOL")
            {
                groups.entry(self.cell(storage)).or_default().push(activity);
            }
        }

        for (key, activities) in groups {
            let active = |kind: ActivityKind| {
                let states = graph
                    .states
                    .iter()
                    .filter(|s| {
                        activities
                            .iter()
                            .any(|a| a.kind == kind && s.active_steps.contains(&a.step))
                    })
                    .map(|s| s.name.clone())
                    .collect();

                ControlExpr::Active {
                    machine: machine.into(),
                    states,
                }
            };
            let kinds: BTreeSet<_> = activities.iter().map(|a| a.kind).collect();

            let expression = if kinds == BTreeSet::from([ActivityKind::WhileActive]) {
                active(ActivityKind::WhileActive)
            } else if kinds
                .iter()
                .all(|q| matches!(q, ActivityKind::Set | ActivityKind::Reset))
            {
                ControlExpr::choose(
                    active(ActivityKind::Reset),
                    ControlExpr::boolean(false),
                    ControlExpr::choose(
                        active(ActivityKind::Set),
                        ControlExpr::boolean(true),
                        ControlExpr::Read {
                            key: key.clone(),
                            previous: true,
                        },
                    ),
                )
            } else {
                continue;
            };

            self.define(key, expression);
        }
    }
}
