use super::*;

#[derive(Debug, Clone, PartialEq)]
pub enum ControlExpr {
    Duration {
        nanoseconds: u64,
    },

    Literal {
        value: String,
    },

    Read {
        key: String,
        previous: bool,
    },

    Active {
        machine: String,
        states: Vec<String>,
    },

    Unary {
        operator: UnaryOp,
        operand: Box<Self>,
    },

    Binary {
        operator: BinaryOp,
        left: Box<Self>,
        right: Box<Self>,
    },

    Choose {
        condition: Box<Self>,
        yes: Box<Self>,
        no: Box<Self>,
    },

    Unsupported {
        text: String,
        operands: Vec<Self>,
    },
}

impl ControlExpr {
    pub fn constant(expression: &Expr) -> Option<Self> {
        Some(match expression {
            Expr::Duration { nanoseconds } => Self::Duration {
                nanoseconds: *nanoseconds,
            },

            Expr::Literal { value } => Self::Literal {
                value: value.clone(),
            },

            Expr::Unary { operator, operand }
                if matches!(operator, UnaryOp::Positive | UnaryOp::Negate | UnaryOp::Not) =>
            {
                Self::Unary {
                    operator: *operator,
                    operand: Box::new(Self::constant(operand)?),
                }
            }

            _ => return None,
        })
    }

    pub fn boolean(value: bool) -> Self {
        Self::Literal {
            value: if value { "true" } else { "false" }.into(),
        }
    }

    pub fn truth(&self) -> Option<bool> {
        match self {
            Self::Literal { value } if value.eq_ignore_ascii_case("true") => Some(true),
            Self::Literal { value } if value.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        }
    }

    pub fn binary(op: BinaryOp, left: Self, right: Self) -> Self {
        if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual) {
            if let Some(value) = right.truth() {
                return if value == (op == BinaryOp::Equal) {
                    left
                } else {
                    left.negated()
                };
            }

            if let Some(value) = left.truth() {
                return if value == (op == BinaryOp::Equal) {
                    right
                } else {
                    right.negated()
                };
            }
        }

        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let absorbing = op == BinaryOp::Or;

            if left.truth() == Some(absorbing) || right.truth() == Some(absorbing) {
                return Self::boolean(absorbing);
            }

            if left.truth() == Some(!absorbing) {
                return right;
            }

            if right.truth() == Some(!absorbing) || left == right {
                return left;
            }

            if op == BinaryOp::And {
                let mut terms = vec![];
                conjunctions(&left, &mut terms);
                conjunctions(&right, &mut terms);

                if terms
                    .iter()
                    .any(|term| terms.contains(&term.clone().negated()))
                {
                    return Self::boolean(false);
                }

                let equalities: Vec<_> = terms.iter().filter_map(equality).collect();

                for (i, (key, value)) in equalities.iter().enumerate() {
                    if equalities[i + 1..]
                        .iter()
                        .any(|(other, v)| other == key && v != value)
                    {
                        return Self::boolean(false);
                    }
                }

                terms.retain(|term| {
                    if let Self::Unary { operator, operand } = term
                        && *operator == UnaryOp::Not
                        && let Some((key, value)) = equality(operand)
                    {
                        return !equalities
                            .iter()
                            .any(|(other, v)| other == &key && v != &value);
                    }

                    true
                });

                let mut result = terms.remove(0);

                for term in terms {
                    result = Self::Binary {
                        operator: BinaryOp::And,
                        left: Box::new(result),
                        right: Box::new(term),
                    };
                }

                return result;
            }

            if op == BinaryOp::Or {
                let mut a = vec![];
                let mut b = vec![];
                conjunctions(&left, &mut a);
                conjunctions(&right, &mut b);

                let common: Vec<_> = a.iter().filter(|e| b.contains(e)).cloned().collect();
                a.retain(|e| !common.contains(e));
                b.retain(|e| !common.contains(e));

                if a.len() == 1 && b.len() == 1 && a[0] == b[0].clone().negated() {
                    return common
                        .into_iter()
                        .reduce(|a, b| Self::binary(BinaryOp::And, a, b))
                        .unwrap_or(Self::boolean(true));
                }
            }
        }

        if let (Self::Literal { value: a }, Self::Literal { value: b }) = (&left, &right)
            && let (Ok(a), Ok(b)) = (a.parse::<i128>(), b.parse::<i128>())
        {
            let result = match op {
                BinaryOp::Equal => Some(a == b),
                BinaryOp::NotEqual => Some(a != b),
                BinaryOp::Greater => Some(a > b),
                BinaryOp::Less => Some(a < b),
                BinaryOp::GreaterEqual => Some(a >= b),
                BinaryOp::LessEqual => Some(a <= b),
                _ => None,
            };

            if let Some(result) = result {
                return Self::boolean(result);
            }
        }

        Self::Binary {
            operator: op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn negated(self) -> Self {
        if let Some(truth) = self.truth() {
            return Self::boolean(!truth);
        }

        match self {
            Self::Unary {
                operator: UnaryOp::Not,
                operand,
            } => *operand,

            operand => Self::Unary {
                operator: UnaryOp::Not,
                operand: Box::new(operand),
            },
        }
    }

    pub fn choose(condition: Self, yes: Self, no: Self) -> Self {
        match condition.truth() {
            Some(true) => return yes,
            Some(false) => return no,
            None => {}
        }

        if yes == no {
            return yes;
        }

        if yes.truth() == Some(true) && no.truth() == Some(false) {
            return condition;
        }

        if yes.truth() == Some(false) && no.truth() == Some(true) {
            return condition.negated();
        }

        Self::Choose {
            condition: Box::new(condition),
            yes: Box::new(yes),
            no: Box::new(no),
        }
    }

    pub fn visit(&self, f: &mut impl FnMut(&Self)) {
        f(self);

        match self {
            Self::Unary { operand, .. } => operand.visit(f),

            Self::Binary { left, right, .. } => {
                left.visit(f);
                right.visit(f);
            }

            Self::Choose { condition, yes, no } => {
                condition.visit(f);
                yes.visit(f);
                no.visit(f);
            }

            Self::Unsupported { operands, .. } => {
                for operand in operands {
                    operand.visit(f);
                }
            }

            _ => {}
        }
    }
}

fn conjunctions(expression: &ControlExpr, terms: &mut Vec<ControlExpr>) {
    if let ControlExpr::Binary {
        operator,
        left,
        right,
    } = expression
        && *operator == BinaryOp::And
    {
        conjunctions(left, terms);
        conjunctions(right, terms);
        return;
    }

    terms.push(expression.clone());
}

fn equality(expression: &ControlExpr) -> Option<(ControlExpr, i128)> {
    if let ControlExpr::Binary {
        operator,
        left,
        right,
    } = expression
        && *operator == BinaryOp::Equal
        && let ControlExpr::Literal { value } = right.as_ref()
    {
        return Some((*left.clone(), value.parse().ok()?));
    }

    None
}

impl SymbolicAnalysis<'_> {
    pub(super) fn expression(
        &mut self,
        scope: &Scope,
        expression: &Expr,
        values: &BTreeMap<String, ControlExpr>,
    ) -> ControlExpr {
        if let Some(storage) = self.storage(scope, expression) {
            let key = self.cell(storage);
            return values.get(&key).cloned().unwrap_or(ControlExpr::Read {
                key,
                previous: false,
            });
        }

        if let Some(value) = self.symbols.enum_value(scope.unit, &expression.text()) {
            return ControlExpr::Literal {
                value: value.to_string(),
            };
        }

        match expression {
            Expr::Duration { nanoseconds } => ControlExpr::Duration {
                nanoseconds: *nanoseconds,
            },

            Expr::Snapshot { value } => {
                if let Some(storage) = self.storage(scope, value) {
                    ControlExpr::Read {
                        key: self.cell(storage),
                        previous: true,
                    }
                } else {
                    ControlExpr::Unsupported {
                        text: expression.text(),
                        operands: vec![],
                    }
                }
            }

            Expr::Literal { value } => ControlExpr::Literal {
                value: value.clone(),
            },

            Expr::Unary { operator, operand }
                if matches!(operator, UnaryOp::Not | UnaryOp::Negate | UnaryOp::Positive) =>
            {
                let operand = self.expression(scope, operand, values);

                if *operator == UnaryOp::Not {
                    operand.negated()
                } else {
                    ControlExpr::Unary {
                        operator: *operator,
                        operand: Box::new(operand),
                    }
                }
            }

            Expr::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(scope, left, values);
                let right = self.expression(scope, right, values);
                ControlExpr::binary(*operator, left, right)
            }

            Expr::Call { callee, arguments } => self.function(scope, callee, arguments, values),

            Expr::Unknown { operands } => ControlExpr::Unsupported {
                text: expression.text(),
                operands: operands
                    .iter()
                    .map(|operand| self.expression(scope, operand, values))
                    .collect(),
            },

            Expr::Unary { operand, .. } => ControlExpr::Unsupported {
                text: expression.text(),
                operands: vec![self.expression(scope, operand, values)],
            },

            _ => ControlExpr::Unsupported {
                text: expression.text(),
                operands: vec![],
            },
        }
    }

    fn function(
        &mut self,
        scope: &Scope,
        callee: &Expr,
        arguments: &[Argument],
        values: &BTreeMap<String, ControlExpr>,
    ) -> ControlExpr {
        let operator = callee.text().to_ascii_uppercase();
        let mut args = vec![];

        for a in arguments.iter().filter(|a| {
            a.direction != ArgumentDirection::Output
                && a.name
                    .as_deref()
                    .is_none_or(|n| !n.eq_ignore_ascii_case("EN"))
        }) {
            args.push(self.expression(scope, &a.value, values));
        }

        let binary = match operator.as_str() {
            "AND" => Some(BinaryOp::And),
            "OR" => Some(BinaryOp::Or),
            "XOR" => Some(BinaryOp::Xor),
            "EQ" => Some(BinaryOp::Equal),
            "NE" => Some(BinaryOp::NotEqual),
            "GT" => Some(BinaryOp::Greater),
            "GE" => Some(BinaryOp::GreaterEqual),
            "LT" => Some(BinaryOp::Less),
            "LE" => Some(BinaryOp::LessEqual),
            "ADD" => Some(BinaryOp::Add),
            "SUB" => Some(BinaryOp::Subtract),
            "MUL" => Some(BinaryOp::Multiply),
            "DIV" => Some(BinaryOp::Divide),
            _ => None,
        };

        if let Some(binary) = binary {
            if matches!(
                binary,
                BinaryOp::Equal
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
            ) && args.len() >= 2
            {
                return args
                    .windows(2)
                    .map(|a| ControlExpr::binary(binary, a[0].clone(), a[1].clone()))
                    .reduce(|a, b| ControlExpr::binary(BinaryOp::And, a, b))
                    .unwrap();
            }
            if args.len() >= 2
                && (!matches!(
                    binary,
                    BinaryOp::Subtract | BinaryOp::Divide | BinaryOp::NotEqual
                ) || args.len() == 2)
            {
                return args
                    .into_iter()
                    .reduce(|a, b| ControlExpr::binary(binary, a, b))
                    .unwrap();
            }
        }

        match (operator.as_str(), args.as_slice()) {
            ("NOT", [arg]) => arg.clone().negated(),
            ("MOVE", [arg]) => arg.clone(),

            ("SEL", [condition, no, yes]) => {
                ControlExpr::choose(condition.clone(), yes.clone(), no.clone())
            }

            ("ABS", [arg]) => ControlExpr::choose(
                ControlExpr::binary(
                    BinaryOp::Less,
                    arg.clone(),
                    ControlExpr::Literal { value: "0".into() },
                ),
                ControlExpr::Unary {
                    operator: UnaryOp::Negate,
                    operand: Box::new(arg.clone()),
                },
                arg.clone(),
            ),

            _ => {
                if let Some(unit) = self.symbols.callable_name(scope.unit, callee)
                    && matches!(unit.kind, UnitKind::Function | UnitKind::Method)
                {
                    self.call(scope, callee, arguments);

                    let instance = self
                        .symbols
                        .call_instance(scope.unit, unit, callee, &scope.unit.source)
                        .map(|s| instance_storage(scope.unit, scope.instance.as_ref(), &s));
                    let called = Scope {
                        unit,
                        instance: instance.clone(),
                        name: scope.name.clone(),
                    };

                    let params: Vec<_> = parameters(unit, arguments);

                    for (argument, parameter) in arguments.iter().zip(params) {
                        if let Some(parameter) = parameter
                            && parameter.kind == VariableKind::Input
                            && let Some(storage) = self.storage(
                                &called,
                                &Expr::Reference {
                                    name: parameter.name.clone(),
                                },
                            )
                        {
                            let key = self.cell(storage);
                            let value = self.expression(scope, &argument.value, values);
                            self.capture(key, value);
                        }
                    }

                    if let Some(ty) = &unit.return_type {
                        let storage = instance_storage(
                            unit,
                            instance.as_ref(),
                            &StorageRef {
                                unit: unit.id.clone(),
                                path: vec!["$return".into()],
                                ty: ty.clone(),
                                source: unit.source.clone(),
                            },
                        );
                        let key = self.cell(storage);
                        return ControlExpr::Read {
                            key,
                            previous: false,
                        };
                    }
                }

                ControlExpr::Unsupported {
                    text: format!("{}()", callee.text()),
                    operands: args,
                }
            }
        }
    }
}

const MAX_ATOMS: usize = 8;
const MAX_NODES: usize = 512;

impl ControlExpr {
    pub fn reduced_boolean(&self) -> Self {
        let mut unsupported = false;
        self.visit(&mut |e| unsupported |= matches!(e, Self::Unsupported { .. }));

        if unsupported {
            return self.clone();
        }

        let mut atoms = vec![];
        let mut remaining = MAX_NODES;

        if !collect(self, &mut atoms, &mut remaining) {
            return self.clone();
        }

        let table: Vec<_> = (0..1usize << atoms.len())
            .map(|mask| evaluate(self, &atoms, mask))
            .collect();
        let result = expression(&atoms, &table, 0, 0);

        if size(&result) <= size(self) {
            result
        } else {
            self.clone()
        }
    }
}

fn collect(e: &ControlExpr, atoms: &mut Vec<ControlExpr>, remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }

    *remaining -= 1;

    use ControlExpr::*;

    if e.truth().is_some() {
        return true;
    }

    match e {
        Unary { operator, operand } if *operator == UnaryOp::Not => {
            collect(operand, atoms, remaining)
        }

        Binary {
            operator: BinaryOp::And | BinaryOp::Or | BinaryOp::Xor,
            left,
            right,
        } => collect(left, atoms, remaining) && collect(right, atoms, remaining),

        Choose { condition, yes, no } => {
            collect(condition, atoms, remaining)
                && collect(yes, atoms, remaining)
                && collect(no, atoms, remaining)
        }

        _ => {
            if !atoms.contains(e) {
                atoms.push(e.clone());
            }

            atoms.len() <= MAX_ATOMS
        }
    }
}

fn evaluate(e: &ControlExpr, atoms: &[ControlExpr], mask: usize) -> bool {
    if let Some(value) = e.truth() {
        return value;
    }

    use ControlExpr::*;

    match e {
        Unary { operator, operand } if *operator == UnaryOp::Not => !evaluate(operand, atoms, mask),

        Binary {
            operator,
            left,
            right,
        } if matches!(operator, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor) => {
            let a = evaluate(left, atoms, mask);
            let b = evaluate(right, atoms, mask);

            match operator {
                BinaryOp::And => a && b,
                BinaryOp::Or => a || b,
                _ => a ^ b,
            }
        }

        Choose { condition, yes, no } => evaluate(
            if evaluate(condition, atoms, mask) {
                yes
            } else {
                no
            },
            atoms,
            mask,
        ),

        _ => {
            mask & (1
                << atoms
                    .iter()
                    .position(|atom| atom == e)
                    .expect("collected atom"))
                != 0
        }
    }
}

fn expression(atoms: &[ControlExpr], table: &[bool], level: usize, mask: usize) -> ControlExpr {
    if level == atoms.len() {
        return ControlExpr::boolean(table[mask]);
    }

    let no = expression(atoms, table, level + 1, mask);
    let yes = expression(atoms, table, level + 1, mask | (1 << level));

    if yes == no {
        return yes;
    }

    let condition = atoms[level].clone();

    match (yes.truth(), no.truth()) {
        (_, Some(false)) => ControlExpr::binary(BinaryOp::And, condition, yes),
        (Some(false), _) => ControlExpr::binary(BinaryOp::And, condition.negated(), no),
        (_, Some(true)) => ControlExpr::binary(BinaryOp::Or, condition.negated(), yes),
        (Some(true), _) => ControlExpr::binary(BinaryOp::Or, condition, no),
        _ => ControlExpr::choose(condition, yes, no),
    }
}

fn size(e: &ControlExpr) -> usize {
    let mut count = 0;
    e.visit(&mut |_| count += 1);
    count
}
