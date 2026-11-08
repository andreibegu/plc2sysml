use super::types::{SourceEnum, constant, literal, value_type};
use super::*;

const MAX_NODES: usize = 512;
const MAX_DEPTH: usize = 32;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum InputKey {
    Storage {
        key: String,
        previous: bool,
    },

    State {
        machine: String,
        states: Vec<String>,
    },

    Undeclared(String),
}

pub(super) struct Projection<'a> {
    pub source: &'a Program,
    pub analysis: &'a Analysis,
    pub context: &'a LoadedContext,
    pub endpoints: &'a [Vec<Vec<String>>],
    pub mapped: &'a BTreeMap<String, usize>,
    pub machine: &'a ControlInstance,
    pub names: &'a BTreeMap<String, String>,
    pub enums: &'a BTreeMap<String, SourceEnum>,
    pub channels: &'a [String],
    pub inputs: BTreeMap<InputKey, StateInput>,
    pub steps: usize,
    pub derived: Vec<ControlDerived>,
}

impl Projection<'_> {
    pub fn memory(&mut self) {
        if self
            .analysis
            .instances
            .iter()
            .filter(|m| m.unit == self.machine.unit)
            .count()
            != 1
        {
            return;
        }

        let mut seen = BTreeSet::new();

        loop {
            let pending: Vec<_> = self
                .inputs
                .keys()
                .filter(|k| {
                    matches!(k, InputKey::Storage { previous: true, .. }) && !seen.contains(*k)
                })
                .cloned()
                .collect();

            if pending.is_empty() {
                break;
            }

            for key in pending {
                seen.insert(key.clone());

                let InputKey::Storage { key: source, .. } = &key else {
                    continue;
                };

                let Some(memory) =
                    self.analysis.memories.get(source).filter(|m| {
                        m.owner == self.machine.unit && self.machine.selector.is_some()
                    })
                else {
                    continue;
                };

                self.steps = 0;

                let inputs = self.inputs.clone();
                let derived = self.derived.clone();
                let ty = self.inputs[&key].value_type.clone();

                if let Some(update) = self.expand(
                    &memory.expression,
                    None,
                    &mut BTreeSet::new(),
                    ty == "Boolean",
                ) && self.check_type(&update, &ty).is_some()
                {
                    let input = self.inputs.get_mut(&key).unwrap();
                    input.update = Some(update);
                    input.default = memory.initial.as_ref().and_then(constant);
                } else {
                    self.inputs = inputs;
                    self.derived = derived;
                }
            }
        }
    }

    pub fn guard(&mut self, expression: &ControlExpr, state: Option<&str>) -> Option<Expr> {
        self.steps = 0;
        let expression = self.expand(expression, state, &mut BTreeSet::new(), true)?;
        self.check_type(&expression, "Boolean")?;
        to_expr(&expression.reduced_boolean())
    }

    pub fn calculation(
        &mut self,
        name: &str,
        expression: &ControlExpr,
        ty: &str,
        commands: Vec<Vec<String>>,
        state: Option<&str>,
    ) -> Option<ControlCalculation> {
        let saved_inputs = self.inputs.clone();
        let saved_derived = self.derived.clone();
        let result = (|| {
            self.steps = 0;

            let expression =
                self.expand(expression, state, &mut BTreeSet::new(), ty == "Boolean")?;
            self.check_type(&expression, ty)?;

            let expression = if ty == "Boolean" {
                expression.reduced_boolean()
            } else {
                expression
            };

            Some(ControlCalculation {
                state: state.map(|s| self.names[s].clone()),
                name: name.into(),
                value_type: ty.into(),
                expression,
                commands,
                output: None,
            })
        })();

        if result.is_none() {
            self.inputs = saved_inputs;
            self.derived = saved_derived;
        }

        result
    }

    fn expand(
        &mut self,
        e: &ControlExpr,
        state: Option<&str>,
        active: &mut BTreeSet<String>,
        boolean: bool,
    ) -> Option<ControlExpr> {
        let known = state.and_then(|s| self.machine.value_in_state(e, s));
        let e = known.as_ref().unwrap_or(e);
        self.steps += 1;

        if self.steps > MAX_NODES || active.len() > MAX_DEPTH {
            return None;
        }

        Some(match e {
            ControlExpr::Duration { nanoseconds } => ControlExpr::Duration {
                nanoseconds: *nanoseconds,
            },

            ControlExpr::Literal { value } => ControlExpr::Literal {
                value: literal(value)?,
            },

            ControlExpr::Read { key, previous } => {
                if !previous
                    && self.machine.selector.as_ref() != Some(key)
                    && !self.mapped.contains_key(key)
                    && self.analysis.cells.get(key).is_none_or(|c| !c.sampled)
                    && active.insert(key.clone())
                {
                    if let Some(value) = self
                        .analysis
                        .cells
                        .get(key)
                        .and_then(|c| c.expression.as_ref())
                    {
                        let inputs = self.inputs.clone();
                        let derived = self.derived.clone();
                        let result = self.expand(value, state, active, boolean);
                        active.remove(key);

                        let result = result.and_then(|e| {
                            if let Some(ty) = value_type(&self.analysis.cells[key].storage.ty) {
                                self.check_type(&e, ty)?;
                            }

                            Some(e)
                        });

                        if result.is_none() {
                            self.inputs = inputs;
                            self.derived = derived;
                        }

                        if let Some(expression) = result {
                            let mut size = 0;
                            expression.visit(&mut |_| size += 1);

                            if size >= 8 || matches!(expression, ControlExpr::Choose { .. }) {
                                let cell = &self.analysis.cells[key];

                                if let Some(ty) = value_type(&cell.storage.ty) {
                                    self.check_type(&expression, ty)?;

                                    let expression = if ty == "Boolean" {
                                        expression.reduced_boolean()
                                    } else {
                                        expression
                                    };

                                    let label = self.storage_label(&cell.storage);

                                    if let Some(existing) = self
                                        .derived
                                        .iter()
                                        .find(|d| d.expression == expression && d.value_type == ty)
                                    {
                                        return Some(ControlExpr::Read {
                                            key: existing.name.clone(),
                                            previous: false,
                                        });
                                    }

                                    let mut name = label.clone();
                                    let mut suffix = 2;

                                    while self.inputs.values().any(|i| i.name == name)
                                        || self.derived.iter().any(|d| d.name == name)
                                    {
                                        name = format!("{label}_{suffix}");
                                        suffix += 1;
                                    }

                                    self.derived.push(ControlDerived {
                                        name: name.clone(),
                                        value_type: ty.into(),
                                        expression,
                                    });
                                    return Some(ControlExpr::Read {
                                        key: name,
                                        previous: false,
                                    });
                                }
                            }

                            return Some(expression);
                        }
                    }

                    active.remove(key);
                }

                let cell = self.analysis.cells.get(key)?;

                let ty = if self.machine.selector.as_ref() == Some(key) {
                    Some("Integer")
                } else {
                    value_type(&cell.storage.ty).or_else(|| {
                        cell.storage
                            .ty
                            .target
                            .as_ref()
                            .and_then(|id| self.enums.get(id))
                            .map(|e| e.name.as_str())
                    })
                }?;
                let mapped = self.mapped.get(key).copied();

                let label = if let Some(i) = mapped {
                    self.context.channels[i].name.clone()
                } else {
                    self.storage_label(&cell.storage)
                };

                let label = if *previous {
                    if cell.invocation_memory {
                        format!("last_call_{label}")
                    } else {
                        format!("previous_{label}")
                    }
                } else if cell.sampled {
                    format!("stored_{label}")
                } else {
                    label
                };

                let label = identifier(&label.replace(' ', "_"));
                let input_key = InputKey::Storage {
                    key: key.clone(),
                    previous: *previous,
                };

                let mut endpoints = mapped
                    .map(|i| self.endpoints[i].clone())
                    .unwrap_or_default();

                if mapped.is_none() || cell.sampled || *previous {
                    let mut seen = BTreeSet::new();
                    self.origins(
                        key,
                        if *previous || cell.sampled {
                            None
                        } else {
                            state
                        },
                        &mut seen,
                        &mut endpoints,
                    );
                    endpoints.sort();
                    endpoints.dedup();
                }

                let label = self.input(input_key.clone(), &label, ty, endpoints);
                let input = self
                    .inputs
                    .get_mut(&input_key)
                    .expect("registered value input");

                input.bounds = cell
                    .storage
                    .ty
                    .integer_bounds()
                    .map(|(min, max)| {
                        (
                            ControlExpr::Literal {
                                value: min.to_string(),
                            },
                            ControlExpr::Literal {
                                value: max.to_string(),
                            },
                        )
                    })
                    .or_else(|| {
                        cell.storage.ty.duration_bounds().map(|(min, max)| {
                            (
                                ControlExpr::Duration { nanoseconds: min },
                                ControlExpr::Duration { nanoseconds: max },
                            )
                        })
                    });

                if !previous && !cell.sampled && mapped.is_none() {
                    input.default = cell.initial.as_ref().and_then(constant);
                }

                ControlExpr::Read {
                    key: label,
                    previous: false,
                }
            }

            ControlExpr::Active { machine, states } => self.state_input(machine, states)?,

            ControlExpr::Unary { operator, operand } => {
                let operand = self.expand(operand, state, active, *operator == UnaryOp::Not)?;

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
            } => {
                if matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual) {
                    for (selector, value) in [
                        (left.as_ref(), right.as_ref()),
                        (right.as_ref(), left.as_ref()),
                    ] {
                        if let (
                            ControlExpr::Read {
                                key,
                                previous: false,
                            },
                            ControlExpr::Literal { value },
                        ) = (selector, value)
                            && let Ok(number) = value.parse::<i128>()
                            && let Some((source, ty)) = self.enum_source(key)
                        {
                            let definition = &self.enums[&ty];

                            let Some(label) = definition.values.get(&number) else {
                                return Some(ControlExpr::boolean(*operator == BinaryOp::NotEqual));
                            };

                            let literal = ControlExpr::Literal {
                                value: format!("{}::{label}", definition.name),
                            };
                            let source = self.expand(
                                &ControlExpr::Read {
                                    key: source,
                                    previous: false,
                                },
                                state,
                                active,
                                false,
                            )?;

                            return Some(ControlExpr::binary(*operator, source, literal));
                        }

                        if let (
                            ControlExpr::Read {
                                key,
                                previous: false,
                            },
                            ControlExpr::Literal { value },
                        ) = (selector, value)
                            && let Some(label) =
                                literal(value).and_then(|v| serde_json::from_str::<String>(&v).ok())
                        {
                            let mut matches =
                                self.analysis.instances.iter().filter_map(|instance| {
                                    let observation =
                                        instance.observation.as_ref().filter(|o| &o.key == key)?;
                                    Some((instance, observation))
                                });

                            if let Some((instance, observation)) = matches.next()
                                && matches.next().is_none()
                                && let Some(states) = observation.labels.get(&label)
                            {
                                let expression = ControlExpr::Active {
                                    machine: instance.id.clone(),
                                    states: states.clone(),
                                };

                                let result = self.expand(&expression, state, active, true)?;
                                return Some(if *operator == BinaryOp::NotEqual {
                                    result.negated()
                                } else {
                                    result
                                });
                            }
                        }

                        if let (ControlExpr::Read { key, .. }, ControlExpr::Literal { value }) =
                            (selector, value)
                            && self.machine.selector.as_ref() == Some(key)
                            && let Some(position) = self
                                .machine
                                .graph
                                .states
                                .iter()
                                .find(|s| s.selector_value.as_ref() == Some(value))
                        {
                            let expression = ControlExpr::Active {
                                machine: self.machine.id.clone(),
                                states: vec![position.name.clone()],
                            };

                            let result = self.expand(&expression, state, active, true)?;
                            return Some(if *operator == BinaryOp::NotEqual {
                                result.negated()
                            } else {
                                result
                            });
                        }
                    }
                }

                let logical = matches!(operator, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor);
                let left = self.expand(left, state, active, logical)?;

                if (*operator == BinaryOp::And && left.truth() == Some(false))
                    || (*operator == BinaryOp::Or && left.truth() == Some(true))
                {
                    return Some(left);
                }

                let mut right = self.expand(right, state, active, logical)?;
                let mut left = left;

                if let Some(ty) = self.expression_type(&left)
                    && let Some(source) = self.enums.values().find(|e| e.name == ty)
                    && let ControlExpr::Literal { value: number } = &right
                    && let Ok(number) = number.parse::<i128>()
                {
                    let label = source.values.get(&number)?;
                    right = ControlExpr::Literal {
                        value: format!("{}::{label}", source.name),
                    };
                }

                if let Some(ty) = self.expression_type(&right)
                    && let Some(source) = self.enums.values().find(|e| e.name == ty)
                    && let ControlExpr::Literal { value: number } = &left
                    && let Ok(number) = number.parse::<i128>()
                {
                    let label = source.values.get(&number)?;
                    left = ControlExpr::Literal {
                        value: format!("{}::{label}", source.name),
                    };
                }

                ControlExpr::binary(*operator, left, right)
            }

            ControlExpr::Choose { condition, yes, no } => {
                let condition = self.expand(condition, state, active, true)?;

                match condition.truth() {
                    Some(true) => return self.expand(yes, state, active, boolean),
                    Some(false) => return self.expand(no, state, active, boolean),

                    None => ControlExpr::choose(
                        condition,
                        self.expand(yes, state, active, boolean)?,
                        self.expand(no, state, active, boolean)?,
                    ),
                }
            }

            ControlExpr::Unsupported { text, operands }
                if boolean
                    && operands.is_empty()
                    && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                let key = self.input(
                    InputKey::Undeclared(text.clone()),
                    &identifier(text),
                    "Boolean",
                    vec![],
                );

                ControlExpr::Read {
                    key,
                    previous: false,
                }
            }

            ControlExpr::Unsupported { .. } => {
                return None;
            }
        })
    }

    fn state_input(&mut self, machine: &str, states: &[String]) -> Option<ControlExpr> {
        if states.is_empty() {
            return Some(ControlExpr::boolean(false));
        }

        let other = self.analysis.instances.iter().find(|m| m.id == machine)?;

        let prefix = if machine == self.machine.id {
            String::new()
        } else {
            let own: Vec<_> = self.machine.name.split('.').collect();
            let path: Vec<_> = other.name.split('.').collect();
            let shared = own.iter().zip(&path).take_while(|(a, b)| a == b).count();

            let start = if shared == path.len() {
                shared.saturating_sub(1)
            } else {
                shared
            };

            format!("{}_", path[start..].join("_"))
        };

        let label = identifier(&format!(
            "{prefix}{}Active",
            states
                .iter()
                .map(|s| s.replace(" + ", "_"))
                .collect::<Vec<_>>()
                .join("_or_")
        ));

        let key = InputKey::State {
            machine: machine.into(),
            states: states.to_vec(),
        };

        let label = self.input(key.clone(), &label, "Boolean", vec![]);
        self.inputs.get_mut(&key).unwrap().state_source = Some((machine.into(), states.to_vec()));

        Some(ControlExpr::Read {
            key: label,
            previous: false,
        })
    }

    fn input(
        &mut self,
        key: InputKey,
        label: &str,
        ty: &str,
        endpoints: Vec<Vec<String>>,
    ) -> String {
        if let Some(input) = self.inputs.get(&key) {
            return input.name.clone();
        }

        let mut name = label.to_string();
        let mut suffix = 2;

        while self.inputs.values().any(|i| i.name == name)
            || self.derived.iter().any(|d| d.name == name)
        {
            name = format!("{label}_{suffix}");
            suffix += 1;
        }

        let mapped = match &key {
            InputKey::Storage {
                key,
                previous: false,
            } => self.mapped.get(key).copied(),

            _ => None,
        }
        .filter(|i| {
            self.context.channels[*i].role == IoRole::Feedback
                && self.context.channels[*i].item_type.is_none()
                && self.context.channels[*i].value_type.as_deref() == Some(ty)
        });

        self.inputs.insert(
            key,
            StateInput {
                name: name.clone(),
                value_type: ty.into(),
                endpoints,
                item_type: mapped.map(|i| self.channels[i].clone()),
                state_source: None,
                state_links: vec![],
                bounds: None,
                default: None,
                update: None,
            },
        );

        name
    }

    fn storage_label(&self, storage: &StorageRef) -> String {
        let mut owner = self
            .machine
            .instance
            .as_ref()
            .map(|s| s.unit.as_str())
            .unwrap_or(&self.machine.unit);

        while let Some(parent) = self
            .source
            .units
            .iter()
            .find(|u| u.id == owner)
            .and_then(|u| u.owner.as_deref())
        {
            owner = parent;
        }

        let prefix = self
            .machine
            .instance
            .as_ref()
            .map(|s| s.path.as_slice())
            .unwrap_or(&[]);

        if storage.unit == owner {
            let path = storage
                .path
                .strip_prefix(prefix)
                .filter(|p| !p.is_empty())
                .unwrap_or(&storage.path);

            identifier(&path.join("_"))
        } else {
            let owner = self
                .source
                .units
                .iter()
                .find(|u| u.id == storage.unit)
                .map(|u| u.name.as_str())
                .unwrap_or("external");

            identifier(
                &std::iter::once(owner)
                    .chain(storage.path.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join("_"),
            )
        }
    }

    fn enum_source(&self, key: &str) -> Option<(String, String)> {
        let mut key = key.to_string();
        let mut seen = BTreeSet::new();

        while seen.len() < MAX_DEPTH && seen.insert(key.clone()) {
            let cell = self.analysis.cells.get(&key)?;

            if let Some(ty) = &cell.storage.ty.target
                && self.enums.contains_key(ty)
            {
                return Some((key, ty.clone()));
            }

            if cell.sampled {
                return None;
            }

            if let Some(ControlExpr::Read {
                key: next,
                previous: false,
            }) = &cell.expression
            {
                key = next.clone();
            } else {
                return None;
            }
        }

        None
    }

    fn origins(
        &self,
        key: &str,
        state: Option<&str>,
        seen: &mut BTreeSet<String>,
        endpoints: &mut Vec<Vec<String>>,
    ) {
        if seen.len() >= 256 || !seen.insert(key.into()) {
            return;
        }

        if let Some(i) = self.mapped.get(key) {
            endpoints.extend(self.endpoints[*i].clone());
        } else if let Some(cell) = self.analysis.cells.get(key) {
            if cell.expression.is_some() && !cell.sampled {
                let stops = self.mapped.keys().cloned().collect();
                let expression = self
                    .analysis
                    .slice(key, state.map(|s| (self.machine, s)), &stops);

                expression.visit(&mut |e| {
                    if let ControlExpr::Read {
                        key: next,
                        previous,
                    } = e
                    {
                        if next != key {
                            self.origins(
                                next,
                                if *previous { None } else { state },
                                seen,
                                endpoints,
                            );
                        } else {
                            for next in &cell.dependencies {
                                self.origins(next, None, seen, endpoints);
                            }
                        }
                    }
                });
            } else {
                for key in &cell.dependencies {
                    self.origins(key, None, seen, endpoints);
                }
            }
        }
    }
}

fn to_expr(expression: &ControlExpr) -> Option<Expr> {
    Some(match expression {
        ControlExpr::Duration { nanoseconds } => Expr::Duration {
            nanoseconds: *nanoseconds,
        },

        ControlExpr::Literal { value } => Expr::Literal {
            value: value.clone(),
        },

        ControlExpr::Read { key, .. } => Expr::Reference { name: key.clone() },

        ControlExpr::Unary { operator, operand } => Expr::Unary {
            operator: *operator,
            operand: Box::new(to_expr(operand)?),
        },

        ControlExpr::Binary {
            operator,
            left,
            right,
        } => Expr::Binary {
            operator: *operator,
            left: Box::new(to_expr(left)?),
            right: Box::new(to_expr(right)?),
        },

        ControlExpr::Choose { condition, yes, no } => Expr::Call {
            callee: Box::new(Expr::Reference {
                name: "$choose".into(),
            }),
            arguments: vec![condition, yes, no]
                .into_iter()
                .map(|e| {
                    Some(Argument {
                        name: None,
                        direction: ArgumentDirection::Input,
                        value: to_expr(e)?,
                    })
                })
                .collect::<Option<_>>()?,
        },

        _ => return None,
    })
}
