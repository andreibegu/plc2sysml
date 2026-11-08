#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnaryOp {
    Positive,
    Negate,
    Not,
    AddressOf,
    Dereference,
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Positive => "+",
            Self::Negate => "-",
            Self::Not => "NOT",
            Self::AddressOf => "ADR",
            Self::Dereference => "DEREF",
        }
    }
}

impl std::fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.symbol())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BinaryOp {
    Or,
    OrElse,
    Xor,
    And,
    AndThen,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Or => "OR",
            Self::OrElse => "OR_ELSE",
            Self::Xor => "XOR",
            Self::And => "AND",
            Self::AndThen => "AND_THEN",
            Self::Equal => "=",
            Self::NotEqual => "<>",
            Self::Less => "<",
            Self::Greater => ">",
            Self::LessEqual => "<=",
            Self::GreaterEqual => ">=",
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Modulo => "MOD",
            Self::Power => "**",
        }
    }
}

impl std::fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.symbol())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Duration {
        nanoseconds: u64,
    },

    Snapshot {
        value: Box<Expr>,
    },

    Literal {
        value: String,
    },

    Reference {
        name: String,
    },

    Unary {
        operator: UnaryOp,
        operand: Box<Expr>,
    },

    Binary {
        operator: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },

    Member {
        base: Box<Expr>,
        name: String,
    },

    Index {
        base: Box<Expr>,
        indices: Vec<Expr>,
    },

    Call {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
    },

    Unknown {
        operands: Vec<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    pub name: Option<String>,
    pub direction: ArgumentDirection,
    pub value: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentDirection {
    Input,
    Output,
}

impl Expr {
    pub fn visit(&self, f: &mut impl FnMut(&Self)) {
        f(self);

        match self {
            Self::Unary { operand, .. } | Self::Snapshot { value: operand } => operand.visit(f),
            Self::Binary { left, right, .. } => {
                left.visit(f);
                right.visit(f);
            }
            Self::Member { base, .. } => base.visit(f),
            Self::Index { base, indices } => {
                base.visit(f);
                for index in indices {
                    index.visit(f);
                }
            }
            Self::Call { callee, arguments } => {
                callee.visit(f);
                for argument in arguments {
                    argument.value.visit(f);
                }
            }
            Self::Unknown { operands } => {
                for operand in operands {
                    operand.visit(f);
                }
            }
            _ => {}
        }
    }

    pub fn boolean(value: bool) -> Self {
        Self::Literal {
            value: if value { "TRUE" } else { "FALSE" }.into(),
        }
    }

    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Literal { value } if value.eq_ignore_ascii_case("TRUE") => Some(true),
            Self::Literal { value } if value.eq_ignore_ascii_case("FALSE") => Some(false),
            _ => None,
        }
    }

    pub fn conjoin(self, other: Self) -> Self {
        match (self.as_boolean(), other.as_boolean()) {
            (Some(false), _) | (_, Some(false)) => Self::boolean(false),
            (Some(true), _) => other,
            (_, Some(true)) => self,
            _ if self == other => self,

            _ => Self::Binary {
                operator: BinaryOp::And,
                left: Box::new(self),
                right: Box::new(other),
            },
        }
    }

    pub fn negated(self) -> Self {
        if let Some(value) = self.as_boolean() {
            Self::boolean(!value)
        } else {
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
    }

    pub fn disjoin(self, other: Self) -> Self {
        match (self.as_boolean(), other.as_boolean()) {
            (Some(true), _) | (_, Some(true)) => Self::boolean(true),
            (Some(false), _) => other,
            (_, Some(false)) => self,
            _ if self == other => self,

            _ => Self::Binary {
                operator: BinaryOp::Or,
                left: Box::new(self),
                right: Box::new(other),
            },
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Duration { nanoseconds } => format!("{nanoseconds} ns"),
            Self::Snapshot { value } => format!("entry({})", value.text()),
            Self::Literal { value } => value.clone(),
            Self::Reference { name } => name.clone(),
            Self::Unknown { operands } => format!(
                "unknown({})",
                operands
                    .iter()
                    .map(Self::text)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),

            Self::Unary { operator, operand } => format!("{operator}({})", operand.text()),

            Self::Binary {
                operator,
                left,
                right,
            } => format!("({} {operator} {})", left.text(), right.text()),

            Self::Member { base, name } => format!("{}.{name}", base.text()),

            Self::Index { base, indices } => format!(
                "{}[{}]",
                base.text(),
                indices
                    .iter()
                    .map(Self::text)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),

            Self::Call { callee, arguments } => format!(
                "{}({})",
                callee.text(),
                arguments
                    .iter()
                    .map(|a| format!(
                        "{}{}{}",
                        a.name.as_deref().unwrap_or(""),
                        if a.name.is_some() {
                            if a.direction == ArgumentDirection::Output {
                                " => "
                            } else {
                                " := "
                            }
                        } else {
                            ""
                        },
                        a.value.text()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}
