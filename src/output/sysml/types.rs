use super::expressions::Projection;
use super::*;

impl Projection<'_> {
    pub(super) fn check_type(&self, expression: &ControlExpr, expected: &str) -> Option<()> {
        let actual = self.expression_type(expression)?;

        (actual == expected || actual == "Integer" && expected == "Real").then_some(())
    }

    pub(super) fn expression_type(&self, e: &ControlExpr) -> Option<&str> {
        use ControlExpr::*;

        match e {
            Literal { .. } if e.truth().is_some() => Some("Boolean"),
            Duration { .. } => Some("ISQ::DurationValue"),
            Literal { value } if value.starts_with('"') => Some("String"),
            Literal { value } if value.parse::<i128>().is_ok() => Some("Integer"),
            Literal { value } if value.parse::<f64>().is_ok() => Some("Real"),

            Literal { value } => self
                .enums
                .values()
                .find(|e| value.starts_with(&format!("{}::", e.name)))
                .map(|e| e.name.as_str()),

            Read { key, .. } => self
                .inputs
                .values()
                .find(|i| i.name == *key)
                .map(|i| i.value_type.as_str())
                .or_else(|| {
                    self.derived
                        .iter()
                        .find(|d| d.name == *key)
                        .map(|d| d.value_type.as_str())
                }),

            Unary { operator, operand } => {
                let ty = self.expression_type(operand)?;

                match (operator, ty) {
                    (UnaryOp::Not, "Boolean") => Some("Boolean"),
                    (UnaryOp::Positive | UnaryOp::Negate, "Integer" | "Real") => Some(ty),
                    _ => None,
                }
            }

            Binary {
                operator,
                left,
                right,
            } => {
                let a = self.expression_type(left)?;
                let b = self.expression_type(right)?;
                let numeric = matches!(a, "Integer" | "Real") && matches!(b, "Integer" | "Real");

                match operator {
                    BinaryOp::And | BinaryOp::Or | BinaryOp::Xor
                        if a == "Boolean" && b == "Boolean" =>
                    {
                        Some("Boolean")
                    }
                    BinaryOp::Equal | BinaryOp::NotEqual if a == b || numeric => Some("Boolean"),

                    BinaryOp::Less
                    | BinaryOp::Greater
                    | BinaryOp::LessEqual
                    | BinaryOp::GreaterEqual
                        if numeric || a == "ISQ::DurationValue" && a == b =>
                    {
                        Some("Boolean")
                    }

                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply if numeric => {
                        Some(if a == "Real" || b == "Real" {
                            "Real"
                        } else {
                            "Integer"
                        })
                    }

                    BinaryOp::Divide if numeric && (a == "Real" || b == "Real") => Some("Real"),
                    _ => None,
                }
            }

            Choose { condition, yes, no } => {
                self.check_type(condition, "Boolean")?;

                let a = self.expression_type(yes)?;
                let b = self.expression_type(no)?;

                if a == b {
                    Some(a)
                } else if matches!(a, "Integer" | "Real") && matches!(b, "Integer" | "Real") {
                    Some("Real")
                } else {
                    None
                }
            }

            _ => None,
        }
    }
}

pub(super) fn constant(expression: &Expr) -> Option<ControlExpr> {
    match expression {
        Expr::Duration { nanoseconds } => Some(ControlExpr::Duration {
            nanoseconds: *nanoseconds,
        }),

        Expr::Literal { value } => literal(value).map(|value| ControlExpr::Literal { value }),

        Expr::Unary { operator, operand }
            if matches!(operator, UnaryOp::Positive | UnaryOp::Negate | UnaryOp::Not) =>
        {
            Some(ControlExpr::Unary {
                operator: *operator,
                operand: Box::new(constant(operand)?),
            })
        }

        _ => None,
    }
}

pub(super) fn literal(value: &str) -> Option<String> {
    if value.eq_ignore_ascii_case("TRUE") || value.eq_ignore_ascii_case("FALSE") {
        return Some(value.to_ascii_lowercase());
    }

    let decimal = value.replace('_', "");

    if let Ok(value) = decimal.parse::<i128>() {
        return Some(value.to_string());
    }

    if decimal.parse::<f64>().is_ok_and(f64::is_finite) {
        return Some(decimal);
    }

    if let Some(text) = value.strip_prefix('\'').and_then(|s| s.strip_suffix('\''))
        && !text.contains(['$', '\''])
    {
        return serde_json::to_string(text).ok();
    }

    None
}

pub(super) fn value_type(ty: &TypeRef) -> Option<&'static str> {
    if ty.indirection.is_some() || !ty.dimensions.is_empty() {
        return None;
    }

    match ty.base.to_ascii_uppercase().as_str() {
        "BOOL" | "BIT" => Some("Boolean"),

        "BYTE" | "WORD" | "DWORD" | "LWORD" | "SINT" | "USINT" | "INT" | "UINT" | "DINT"
        | "UDINT" | "LINT" | "ULINT" => Some("Integer"),

        "REAL" | "LREAL" => Some("Real"),
        "STRING" | "WSTRING" => Some("String"),
        "TIME" | "LTIME" => Some("ISQ::DurationValue"),
        _ => None,
    }
}

pub(super) struct SourceEnum {
    pub name: String,
    pub values: BTreeMap<i128, String>,
}

pub(super) fn enums(project: &Program, package: &str) -> BTreeMap<String, SourceEnum> {
    let mut result = BTreeMap::new();
    let mut used = BTreeSet::from([
        "Signal".to_string(),
        "BooleanSignal".into(),
        "IntegerSignal".into(),
        "RealSignal".into(),
        "StringSignal".into(),
    ]);

    for ty in &project.types {
        if ty.literals().is_empty() {
            continue;
        }

        let mut values = BTreeMap::new();
        let mut next = 0i128;
        let mut names = BTreeSet::new();
        let mut supported = true;

        for literal in ty.literals() {
            let Some(number) = literal
                .value
                .as_ref()
                .map(|v| v.parse::<i128>().ok())
                .unwrap_or(Some(next))
            else {
                supported = false;
                break;
            };

            let name = identifier(&literal.name);

            if values.insert(number, name.clone()).is_some() || !names.insert(name) {
                supported = false;
                break;
            }

            let Some(following) = number.checked_add(1) else {
                supported = false;
                break;
            };

            next = following;
        }

        if !supported {
            continue;
        }

        let base = identifier(&ty.name);
        let mut name = base.clone();
        let mut suffix = 2;

        while !used.insert(name.clone()) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }

        result.insert(
            ty.id.clone(),
            SourceEnum {
                name: format!("{package}::{name}"),
                values,
            },
        );
    }

    result
}
