use super::*;

#[derive(Default)]
pub(super) struct Invocation {
    pub instance: Option<StorageRef>,
    pub actuals: BTreeMap<String, Value>,
    pub destinations: BTreeMap<String, StorageRef>,
    pub gate: Value,
    pub strong: bool,
}

impl Engine<'_> {
    pub(super) fn call(
        &mut self,
        caller: &Unit,
        expression: &Expr,
        state: &mut State,
        source: &Source,
        gate: &Value,
        strong: bool,
    ) -> Value {
        let Expr::Call { callee, arguments } = expression else {
            return Value::default();
        };

        if crate::resolve::builtin_call(&callee.text()) {
            let mut result = Value::default();
            let simple = callee.text().to_ascii_uppercase();

            if matches!(simple.as_str(), "SEL" | "MUX") {
                return self.select(caller, &simple, arguments, state, source, gate);
            }

            for argument in arguments
                .iter()
                .filter(|a| a.direction != ArgumentDirection::Output)
            {
                let value = self.expression(caller, &argument.value, state, source);
                result.join(&value);
            }

            result.add_control(gate);
            return result;
        }

        let Some(signature) = self.symbols.callable_name(caller, callee) else {
            return Value::default();
        };

        let mut actuals = BTreeMap::new();
        let mut destinations = BTreeMap::new();

        for (argument, parameter) in arguments
            .iter()
            .zip(crate::resolve::parameters(signature, arguments))
        {
            let Some(parameter) = parameter else {
                continue;
            };

            if argument.direction != ArgumentDirection::Output {
                actuals.insert(
                    parameter.name.to_ascii_lowercase(),
                    self.expression(caller, &argument.value, state, source),
                );

                if let Some(fields) = self.record_fields(&parameter.ty, 0)
                    && let Some(actual) = self.symbols.endpoint(caller, &argument.value)
                {
                    for (path, ty) in fields {
                        let mut field = actual.clone();
                        field.path.extend(path.clone());
                        field.ty = ty;
                        let key = self.cell(field);
                        actuals.insert(
                            format!("{}.{}", parameter.name, path.join(".")).to_ascii_lowercase(),
                            state.read(&key),
                        );
                    }
                }
            }

            if (argument.direction == ArgumentDirection::Output
                || parameter.kind == VariableKind::InOut)
                && let Some(storage) = self.symbols.endpoint(caller, &argument.value)
            {
                destinations.insert(parameter.name.to_ascii_lowercase(), storage);
            }
        }

        let occurrence = self
            .symbols
            .call_instance(caller, signature, callee, source);
        let mut gate = gate.clone();
        let aliases = destinations
            .values()
            .map(StorageRef::key)
            .collect::<Vec<_>>();
        let overlaps = aliases.iter().enumerate().any(|(index, key)| {
            aliases[index + 1..]
                .iter()
                .any(|other| crate::analysis::overlaps(key, other))
        });
        let mut strong = strong && !overlaps;

        if (signature.kind == UnitKind::FunctionBlock || signature.owner.is_some())
            && occurrence.is_none()
        {
            strong = false;
        }

        if let Some(enable) = arguments.iter().find(|a| {
            a.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case("EN"))
        }) {
            let value = self.expression(caller, &enable.value, state, source);

            if value.truth() == Some(false) {
                return Value::default();
            }

            if value.truth().is_none() {
                strong = false;
            }

            gate.add_control(&value);
        }

        let summary = self.specialized_summary(signature, &actuals);

        self.apply(
            signature,
            &summary,
            &Invocation {
                instance: occurrence,
                actuals,
                destinations,
                gate,
                strong,
            },
            state,
        )
    }

    pub(super) fn apply(
        &mut self,
        signature: &Unit,
        summary: &Summary,
        invocation: &Invocation,
        state: &mut State,
    ) -> Value {
        let occurrence = invocation.instance.as_ref();
        let actuals = &invocation.actuals;
        let destinations = &invocation.destinations;
        let gate = &invocation.gate;
        let strong = invocation.strong;
        let mut before = state.clone();

        for (name, value) in actuals {
            if let Some(formal) = self
                .symbols
                .endpoint(signature, &Expr::Reference { name: name.clone() })
            {
                let mapped = crate::resolve::instance_storage(signature, occurrence, &formal);
                let key = self.cell(mapped);
                before.values.insert(key, value.clone());
            }
        }

        let mut result = Value::default();
        let mut writes = BTreeMap::<String, Value>::new();

        for (output, symbolic) in &summary.writes {
            let storage = self.cells[output].clone();
            let mut value = Value {
                constants: symbolic.constants.clone(),

                ..Value::default()
            };

            for origin in &symbolic.origins {
                let reference = self.cells[origin].clone();
                let actual = crate::resolve::instance_storage(signature, occurrence, &reference);
                let key = self.cell(actual);
                let supplied = before.values.get(&key).cloned().unwrap_or_else(|| {
                    if reference.unit == signature.id
                        && reference.path.len() == 1
                        && let Some(variable) = signature.variable(&reference.path[0])
                    {
                        if matches!(variable.kind, VariableKind::Input | VariableKind::InOut) {
                            return Value::entry(&key);
                        }

                        return initial(variable);
                    }

                    Value::entry(&key)
                });
                let constants = value.constants.take();

                value.join(&supplied);

                value.constants = constants;
            }

            value.add_control(gate);

            if storage.unit == signature.id && storage.path == ["$return"] {
                result = value;
                continue;
            }

            let destination = if storage.unit == signature.id && !storage.path.is_empty() {
                destinations
                    .get(&storage.path[0].to_ascii_lowercase())
                    .cloned()
                    .map(|mut target| {
                        target.path.extend(storage.path[1..].iter().cloned());
                        target.ty = storage.ty.clone();
                        target
                    })
            } else {
                None
            };

            let local = storage
                .path
                .first()
                .and_then(|name| signature.variable(name));
            let temporary = storage.unit == signature.id
                && (local.is_some_and(|v| v.kind == VariableKind::Temporary)
                    || matches!(
                        signature.kind,
                        UnitKind::Function | UnitKind::Method | UnitKind::Action
                    ) && !local.is_some_and(|v| {
                        matches!(v.kind, VariableKind::Instance | VariableKind::Static)
                    }));

            if temporary && destination.is_none() {
                continue;
            }

            let mapped = destination.unwrap_or_else(|| {
                crate::resolve::instance_storage(signature, occurrence, &storage)
            });
            let key = self.cell(mapped);
            writes
                .entry(key)
                .and_modify(|v| v.join(&value))
                .or_insert(value);
        }

        for (key, value) in writes {
            self.write(state, key, value, strong);
        }

        if signature.kind == UnitKind::FunctionBlock {
            for (name, value) in actuals {
                if let Some(reference) = self
                    .symbols
                    .endpoint(signature, &Expr::Reference { name: name.clone() })
                {
                    let mapped =
                        crate::resolve::instance_storage(signature, occurrence, &reference);
                    let key = self.cell(mapped);
                    self.write(state, key, value.clone(), strong);
                }
            }
        }

        result
    }
}

impl Engine<'_> {
    pub(super) fn select(
        &mut self,
        caller: &Unit,
        operator: &str,
        arguments: &[Argument],
        state: &mut State,
        source: &Source,
        gate: &Value,
    ) -> Value {
        let mut operands = BTreeMap::new();
        let mut gate = gate.clone();
        let mut unmatched = false;

        for (position, argument) in arguments.iter().enumerate() {
            if argument
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("EN"))
            {
                gate.add_control(&self.expression(caller, &argument.value, state, source));
                continue;
            }

            if argument.direction == ArgumentDirection::Output {
                continue;
            }

            let index = match argument.name.as_deref() {
                None => Some(position),

                Some(name)
                    if name.eq_ignore_ascii_case(if operator == "SEL" { "G" } else { "K" }) =>
                {
                    Some(0)
                }

                Some(name) => name
                    .to_ascii_uppercase()
                    .strip_prefix("IN")
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| n.checked_add(1)),
            };

            if let Some(index) = index {
                unmatched |= operands.insert(index, &argument.value).is_some();
            } else {
                unmatched = true;
            }
        }

        if unmatched
            || operands.len() < 3
            || (operator == "SEL" && operands.len() != 3)
            || operands.keys().copied().ne(0..operands.len())
        {
            let mut value = Value::default();

            for argument in arguments {
                value.join(&self.expression(caller, &argument.value, state, source));
            }

            value.add_control(&gate);
            return value;
        }

        let selector = self.expression(caller, operands[&0], state, source);
        let last = operands.len() - 2;
        let choices = selector
            .constants
            .as_ref()
            .and_then(|constants| {
                constants
                    .iter()
                    .map(|c| match (operator, c) {
                        ("SEL", Constant::Boolean(b)) => Some(usize::from(*b)),

                        ("MUX", Constant::Integer(n)) if *n >= 0 => {
                            Some((*n).min(last as i128) as usize)
                        }

                        _ => None,
                    })
                    .collect::<Option<BTreeSet<_>>>()
            })
            .filter(|choices| !choices.is_empty())
            .unwrap_or_else(|| (0..=last).collect());

        let graphical = caller.bodies.iter().any(|b| !b.is_ordered());
        let before = state.clone();
        let mut ending = None::<State>;
        let mut result = None::<Value>;

        for index in 0..=last {
            if !graphical && !choices.contains(&index) {
                continue;
            }

            let mut branch = if graphical {
                state.clone()
            } else {
                before.clone()
            };

            if !graphical && operator == "SEL" {
                self.refine(caller, operands[&0], index == 1, &mut branch);
            }

            let mut value = self.expression(caller, operands[&(index + 1)], &mut branch, source);
            value.add_control(&selector);
            value.add_control(&gate);

            if !graphical {
                for (key, value) in &mut branch.values {
                    if before.values.get(key) != Some(value) {
                        value.add_control(&selector);
                        value.add_control(&gate);
                    }
                }
            }

            if choices.contains(&index) {
                if let Some(result) = &mut result {
                    result.join(&value);
                } else {
                    result = Some(value);
                }
            }

            if graphical {
                state.join(&branch);
            } else if let Some(ending) = &mut ending {
                ending.join(&branch);
            } else {
                ending = Some(branch);
            }
        }

        if let Some(ending) = ending {
            *state = ending;
        }

        result.unwrap_or_default()
    }
}

impl Engine<'_> {
    pub(super) fn assign(
        &mut self,
        unit: &Unit,
        node: &FlowNode,
        state: &mut State,
        gate: &Value,
        mut strong: bool,
    ) {
        let Operation::Assign(target, expression) = &node.operation else {
            return;
        };

        let mut destination = self.symbols.endpoint(unit, target);

        if let Some(storage) = &destination
            && let Some(fields) = self.record_fields(&storage.ty, 0)
        {
            let Some(source) = self.symbols.endpoint(unit, expression) else {
                return;
            };

            let source_fields = self.record_fields(&source.ty, 0);

            if source_fields.as_ref().is_none_or(|source| {
                source.len() != fields.len()
                    || source
                        .iter()
                        .zip(&fields)
                        .any(|((a, at), (b, bt))| a != b || !at.same_type(bt))
            }) {
                return;
            }

            let values: Vec<_> = fields
                .into_iter()
                .map(|(path, ty)| {
                    let mut input = source.clone();
                    input.path.extend(path.clone());
                    input.ty = ty.clone();

                    let mut output = storage.clone();
                    output.path.extend(path);
                    output.ty = ty;

                    let input = self.cell(input);
                    let output = self.cell(output);
                    let mut value = state.read(&input);
                    value.add_control(gate);

                    (output, value)
                })
                .collect();

            for (key, value) in values {
                self.write(state, key, value, strong);
            }

            return;
        }

        let mut value = self.expression(unit, expression, state, &node.source);
        value.add_control(gate);

        match target {
            Expr::Index { base, indices } => {
                destination = self.symbols.endpoint(unit, base);

                for index in indices {
                    value.add_control(&self.expression(unit, index, state, &node.source));
                }

                value.constants = None;
                strong = false;
            }

            Expr::Member { base, name }
                if name.parse::<u8>().is_ok()
                    && destination
                        .as_ref()
                        .is_some_and(|s| s.ty.base.eq_ignore_ascii_case("BOOL")) =>
            {
                destination = self.symbols.endpoint(unit, base);
                value.constants = None;
                strong = false;
            }

            _ => {}
        }

        if let Some(storage) = destination {
            if storage
                .ty
                .target
                .as_ref()
                .is_some_and(|id| self.model.units.iter().any(|u| &u.id == id))
                || storage.ty.indirection.is_some()
            {
            } else {
                let key = self.cell(storage);
                self.write(state, key, value, strong);
            }
        }
    }

    pub(super) fn record_fields(
        &self,
        ty: &TypeRef,
        depth: usize,
    ) -> Option<Vec<(Vec<String>, TypeRef)>> {
        if depth >= 16 || !ty.dimensions.is_empty() || ty.indirection.is_some() {
            return None;
        }

        let ty = self.symbols.effective_type(ty)?;
        let record =
            self.model.types.iter().find(|t| {
                Some(&t.id) == ty.target.as_ref() && matches!(t.kind, TypeKind::Struct(_))
            })?;
        let mut fields = vec![];

        for field in record.fields() {
            if let Some(nested) = self.record_fields(&field.ty, depth + 1) {
                for (path, ty) in nested {
                    fields.push((
                        std::iter::once(field.name.clone()).chain(path).collect(),
                        ty,
                    ));
                }
            } else {
                fields.push((vec![field.name.clone()], field.ty.clone()));
            }
        }

        Some(fields)
    }
}
