use crate::{
    input::st::lexer::{Token, TokenKind},
    ir::*,
};

pub fn parse(tokens: &[Token]) -> Expr {
    let mut p = ExprParser {
        tokens,
        pos: 0,
        depth: 0,
    };

    match p.expression(0) {
        Some(e) if p.pos == tokens.len() => e,

        _ => super::unknown(
            tokens
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
    }
}

struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
}

impl ExprParser<'_> {
    fn peek(&self, s: &str) -> bool {
        self.tokens.get(self.pos).is_some_and(|t| t.is(s))
    }

    fn take(&mut self, s: &str) -> bool {
        if self.peek(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expression(&mut self, min: u8) -> Option<Expr> {
        if self.depth >= 64 {
            return None;
        }

        self.depth += 1;

        let mut lhs = if self.peek("-")
            || self.peek("+")
            || self.peek("NOT")
            || self.peek("ADR") && self.tokens.get(self.pos + 1).is_none_or(|t| !t.is("("))
        {
            let op = match self
                .tokens
                .get(self.pos)?
                .text
                .to_ascii_uppercase()
                .as_str()
            {
                "+" => UnaryOp::Positive,
                "-" => UnaryOp::Negate,
                "NOT" => UnaryOp::Not,
                "ADR" => UnaryOp::AddressOf,
                _ => return None,
            };
            self.pos += 1;

            Expr::Unary {
                operator: op,
                operand: Box::new(self.expression(15)?),
            }
        } else if self.take("(") {
            let e = self.expression(0)?;

            if !self.take(")") {
                return None;
            }

            e
        } else {
            let t = self.tokens.get(self.pos)?;
            self.pos += 1;

            match t.kind {
                TokenKind::Word if !t.is("TRUE") && !t.is("FALSE") => Expr::Reference {
                    name: t.text.clone(),
                },

                TokenKind::Literal | TokenKind::Word => match parse_duration(&t.text) {
                    Some(nanoseconds) => Expr::Duration { nanoseconds },

                    None => Expr::Literal {
                        value: t.text.clone(),
                    },
                },

                _ => return None,
            }
        };

        loop {
            if self.take(".") {
                let name = self.tokens.get(self.pos)?.text.clone();
                self.pos += 1;
                lhs = Expr::Member {
                    base: Box::new(lhs),
                    name,
                };
                continue;
            }

            if self.take("^") {
                lhs = Expr::Unary {
                    operator: UnaryOp::Dereference,
                    operand: Box::new(lhs),
                };
                continue;
            }

            if self.take("[") {
                let mut indices = vec![];

                loop {
                    indices.push(self.expression(0)?);

                    if self.take("]") {
                        break;
                    }

                    if !self.take(",") {
                        return None;
                    }
                }

                lhs = Expr::Index {
                    base: Box::new(lhs),
                    indices,
                };
                continue;
            }

            if self.take("(") {
                let mut arguments = vec![];

                if !self.take(")") {
                    loop {
                        let mut name = None;
                        let mut direction = ArgumentDirection::Input;

                        if self
                            .tokens
                            .get(self.pos + 1)
                            .is_some_and(|t| t.is(":=") || t.is("=>"))
                        {
                            name = Some(self.tokens.get(self.pos)?.text.clone());
                            self.pos += 1;

                            if self.take("=>") {
                                direction = ArgumentDirection::Output;
                            } else {
                                self.take(":=");
                            }
                        }

                        arguments.push(Argument {
                            name,
                            direction,
                            value: self.expression(0)?,
                        });

                        if self.take(")") {
                            break;
                        }

                        if !self.take(",") {
                            return None;
                        }
                    }
                }

                lhs = Expr::Call {
                    callee: Box::new(lhs),
                    arguments,
                };
                continue;
            }

            let Some(t) = self.tokens.get(self.pos) else {
                break;
            };

            let op = t.text.to_uppercase();

            let (operator, left, right) = match op.as_str() {
                "OR" => (BinaryOp::Or, 1, 2),
                "OR_ELSE" => (BinaryOp::OrElse, 1, 2),
                "XOR" => (BinaryOp::Xor, 3, 4),
                "AND" => (BinaryOp::And, 5, 6),
                "AND_THEN" => (BinaryOp::AndThen, 5, 6),
                "=" => (BinaryOp::Equal, 7, 8),
                "<>" => (BinaryOp::NotEqual, 7, 8),
                "<" => (BinaryOp::Less, 9, 10),
                ">" => (BinaryOp::Greater, 9, 10),
                "<=" => (BinaryOp::LessEqual, 9, 10),
                ">=" => (BinaryOp::GreaterEqual, 9, 10),
                "+" => (BinaryOp::Add, 11, 12),
                "-" => (BinaryOp::Subtract, 11, 12),
                "*" => (BinaryOp::Multiply, 13, 14),
                "/" => (BinaryOp::Divide, 13, 14),
                "MOD" => (BinaryOp::Modulo, 13, 14),
                "**" => (BinaryOp::Power, 16, 15),
                _ => break,
            };

            if left < min {
                break;
            }

            self.pos += 1;
            lhs = Expr::Binary {
                operator,
                left: Box::new(lhs),
                right: Box::new(self.expression(right)?),
            };
        }

        self.depth -= 1;

        Some(lhs)
    }
}

fn parse_duration(value: &str) -> Option<u64> {
    let (prefix, raw) = value.split_once('#')?;

    let long = match prefix.to_ascii_uppercase().as_str() {
        "T" | "TIME" => false,
        "LT" | "LTIME" => true,
        _ => return None,
    };

    let raw = raw.replace('_', "").to_ascii_lowercase();
    let mut remaining = raw.as_str();
    let mut nanoseconds = 0u128;
    let mut previous_scale = u128::MAX;

    while !remaining.is_empty() {
        let number_end = remaining.find(|c: char| !c.is_ascii_digit() && c != '.')?;
        let number = &remaining[..number_end];
        remaining = &remaining[number_end..];

        let (unit, scale) = [
            ("ms", 1_000_000u128),
            ("us", 1_000),
            ("ns", 1),
            ("d", 86_400_000_000_000),
            ("h", 3_600_000_000_000),
            ("m", 60_000_000_000),
            ("s", 1_000_000_000),
        ]
        .into_iter()
        .find(|(unit, _)| remaining.starts_with(unit))?;

        if scale >= previous_scale {
            return None;
        }

        previous_scale = scale;

        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));

        if whole.is_empty() || fraction.len() > 9 || !fraction.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }

        let divisor = 10u128.checked_pow(fraction.len().try_into().ok()?)?;

        let fractional = if fraction.is_empty() {
            0
        } else {
            fraction.parse::<u128>().ok()?.checked_mul(scale)?
        };

        if !fractional.is_multiple_of(divisor) {
            return None;
        }

        nanoseconds = nanoseconds
            .checked_add(whole.parse::<u128>().ok()?.checked_mul(scale)?)?
            .checked_add(fractional / divisor)?;
        remaining = &remaining[unit.len()..];

        if !fraction.is_empty() && !remaining.is_empty() {
            return None;
        }
    }

    let (_, max) =
        crate::ir::TypeRef::scalar(if long { "LTIME" } else { "TIME" }).duration_bounds()?;

    if raw.is_empty()
        || nanoseconds > max as u128
        || !long && !nanoseconds.is_multiple_of(1_000_000)
    {
        return None;
    }

    nanoseconds.try_into().ok()
}
