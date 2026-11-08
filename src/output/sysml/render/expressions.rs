use super::*;
use crate::ir::Expr;

fn predicate(expression: &Expr, inputs: &[StateInput]) -> Doc {
    fn convert(e: &Expr) -> crate::analysis::ControlExpr {
        use crate::analysis::ControlExpr as C;

        match e {
            Expr::Duration { nanoseconds } => C::Duration {
                nanoseconds: *nanoseconds,
            },

            Expr::Literal { value } => C::Literal {
                value: value.clone(),
            },

            Expr::Reference { name } => C::Read {
                key: name.clone(),
                previous: false,
            },

            Expr::Unary { operator, operand } => C::Unary {
                operator: *operator,
                operand: Box::new(convert(operand)),
            },

            Expr::Binary {
                operator,
                left,
                right,
            } => C::Binary {
                operator: *operator,
                left: Box::new(convert(left)),
                right: Box::new(convert(right)),
            },

            Expr::Call { callee, arguments } if callee.text() == "$choose" => C::Choose {
                condition: Box::new(convert(&arguments[0].value)),
                yes: Box::new(convert(&arguments[1].value)),
                no: Box::new(convert(&arguments[2].value)),
            },

            _ => unreachable!("validated state predicate"),
        }
    }

    expression_doc(&convert(expression), 0, inputs)
}

pub(super) fn direct_formula(e: &crate::analysis::ControlExpr, inputs: &[StateInput]) -> String {
    expression_doc(e, 0, inputs).pretty(usize::MAX).to_string()
}

pub(super) fn write_equation(
    w: &mut Writer,
    header: &str,
    e: &crate::analysis::ControlExpr,
    inputs: &[StateInput],
) {
    write_value(w, header, expression_doc(e, 0, inputs));
}

pub(super) fn write_value(w: &mut Writer, header: &str, value: Doc) {
    w.document(
        Doc::text(format!("{header} ="))
            .append(Doc::line().append(value).nest(4))
            .append(";")
            .group(),
    );
}

pub(super) fn write_condition(w: &mut Writer, e: &Expr, inputs: &[StateInput]) {
    w.document(Doc::text("if ").append(predicate(e, inputs)).group());
}

fn expression_doc(e: &crate::analysis::ControlExpr, parent: u8, inputs: &[StateInput]) -> Doc {
    use crate::analysis::ControlExpr::*;

    let (precedence, doc) = match e {
        Duration { nanoseconds } => (10, Doc::text(duration(*nanoseconds))),

        Literal { value } => (
            10,
            Doc::text(
                if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
                    value.to_ascii_lowercase()
                } else if value.contains("::") && !value.starts_with('"') {
                    super::qualified(value)
                } else {
                    value.clone()
                },
            ),
        ),

        Read { key, .. } => (
            10,
            Doc::text(
                if inputs
                    .iter()
                    .any(|i| i.name == *key && i.item_type.is_some())
                {
                    format!("{}.value", name(key))
                } else {
                    name(key)
                },
            ),
        ),

        Unary { operator, operand } => (
            8,
            Doc::text(format!(
                "{} ",
                if *operator == UnaryOp::Not {
                    "not"
                } else {
                    operator.symbol()
                }
            ))
            .append(expression_doc(operand, 8, inputs)),
        ),

        Binary {
            operator,
            left,
            right,
        } => {
            let (op, p) = match operator {
                BinaryOp::Or => ("or", 1),
                BinaryOp::Xor => ("xor", 2),
                BinaryOp::And => ("and", 3),
                BinaryOp::Equal => ("==", 4),
                BinaryOp::NotEqual => ("!=", 4),
                BinaryOp::Less
                | BinaryOp::Greater
                | BinaryOp::LessEqual
                | BinaryOp::GreaterEqual => (operator.symbol(), 4),
                BinaryOp::Add | BinaryOp::Subtract => (operator.symbol(), 6),
                _ => (operator.symbol(), 7),
            };

            (
                p,
                expression_doc(left, p, inputs)
                    .append(
                        Doc::line()
                            .append(Doc::text(format!("{op} ")))
                            .append(expression_doc(right, p + 1, inputs))
                            .nest(4),
                    )
                    .group(),
            )
        }

        Choose { condition, yes, no } => (
            0,
            Doc::text("if ")
                .append(expression_doc(condition, 0, inputs))
                .append(
                    Doc::line()
                        .append("? ")
                        .append(expression_doc(yes, 0, inputs))
                        .nest(4),
                )
                .append(
                    Doc::line()
                        .append("else ")
                        .append(expression_doc(no, 0, inputs)),
                )
                .group(),
        ),

        _ => unreachable!("projected control expression"),
    };

    if precedence < parent {
        Doc::text("(").append(doc).append(")")
    } else {
        doc
    }
}

fn duration(nanoseconds: u64) -> String {
    let seconds = nanoseconds / 1_000_000_000;
    let fraction = nanoseconds % 1_000_000_000;

    let value = if fraction == 0 {
        seconds.to_string()
    } else {
        format!("{seconds}.{fraction:09}")
            .trim_end_matches('0')
            .to_string()
    };

    format!("{value} [SI::s]")
}
