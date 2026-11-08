use super::{BinaryOp, Expr, Source};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Body {
    pub id: String,
    pub name: String,
    pub statements: Vec<Statement>,
    pub execution: Execution,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum Execution {
    Sequential,
    Unspecified,
    StateMachine(Box<StateGraph>),
}

impl Body {
    pub fn graph(&self) -> Option<&StateGraph> {
        match &self.execution {
            Execution::StateMachine(graph) => Some(graph),
            _ => None,
        }
    }

    pub fn is_ordered(&self) -> bool {
        matches!(self.execution, Execution::Sequential)
    }

    pub fn visit_expressions(&self, f: &mut impl FnMut(&Expr)) {
        for statement in &self.statements {
            statement.visit_expressions(f);
        }

        if let Some(graph) = self.graph() {
            for transition in &graph.transitions {
                transition.condition.visit(f);
            }
            for activity in &graph.activities {
                activity.target.visit(f);
            }
            for expression in graph
                .selector
                .iter()
                .chain(&graph.reset)
                .chain(graph.observation.iter().map(|o| &o.variable))
            {
                expression.visit(f);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct StateGraph {
    pub name: String,
    pub selector: Option<Expr>,
    pub observation: Option<StateObservation>,
    pub initial: String,
    pub states: Vec<ControlState>,
    pub transitions: Vec<StateTransition>,
    pub reset: Option<Expr>,
    pub activities: Vec<StateActivity>,
}

#[derive(Debug, Clone)]
pub struct StateObservation {
    pub variable: Expr,
    pub labels: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct StateActivity {
    pub step: String,
    pub target: Expr,
    pub kind: ActivityKind,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActivityKind {
    WhileActive,
    Set,
    Reset,
    StepAction,
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct ControlState {
    pub name: String,
    pub active_steps: Vec<String>,
    pub selector_value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StateTransition {
    pub from: String,
    pub to: String,
    pub condition: Expr,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct Statement {
    pub id: String,
    pub source: Source,
    pub kind: StatementKind,
}

#[derive(Debug, Clone)]
pub enum StatementKind {
    MayAffect {
        condition: Expr,
        target: Expr,
    },

    Assign {
        target: Expr,
        value: Expr,
    },

    Call {
        expression: Expr,
    },

    If {
        branches: Vec<Branch>,
        otherwise: Vec<Statement>,
    },

    Case {
        selector: Expr,
        arms: Vec<Branch<Vec<CaseLabel>>>,
        otherwise: Vec<Statement>,
    },

    Loop {
        control: LoopControl,
        body: Vec<Statement>,
    },

    Return,
    Exit,
    Continue,

    Unknown {
        operands: Vec<Expr>,
    },
}

impl Statement {
    pub fn visit(&self, f: &mut impl FnMut(&Self)) {
        f(self);

        match &self.kind {
            StatementKind::If {
                branches,
                otherwise,
            } => {
                for branch in branches {
                    for statement in &branch.body {
                        statement.visit(f);
                    }
                }
                for statement in otherwise {
                    statement.visit(f);
                }
            }
            StatementKind::Case {
                arms, otherwise, ..
            } => {
                for arm in arms {
                    for statement in &arm.body {
                        statement.visit(f);
                    }
                }
                for statement in otherwise {
                    statement.visit(f);
                }
            }
            StatementKind::Loop { body, .. } => {
                for statement in body {
                    statement.visit(f);
                }
            }
            _ => {}
        }
    }

    pub fn visit_expressions(&self, f: &mut impl FnMut(&Expr)) {
        self.visit(&mut |statement| match &statement.kind {
            StatementKind::Assign { target, value }
            | StatementKind::MayAffect {
                target,
                condition: value,
            } => {
                target.visit(f);
                value.visit(f);
            }
            StatementKind::Call { expression } => expression.visit(f),
            StatementKind::If { branches, .. } => {
                for branch in branches {
                    branch.test.visit(f);
                }
            }
            StatementKind::Case { selector, arms, .. } => {
                selector.visit(f);
                for arm in arms {
                    for label in &arm.test {
                        match label {
                            CaseLabel::Value(value) => value.visit(f),
                            CaseLabel::Range { lower, upper } => {
                                lower.visit(f);
                                upper.visit(f);
                            }
                        }
                    }
                }
            }
            StatementKind::Loop { control, .. } => match control {
                LoopControl::Condition { condition, .. } => condition.visit(f),
                LoopControl::Range {
                    variable,
                    start,
                    end,
                    step,
                } => {
                    for expression in [Some(variable), Some(start), Some(end), step.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        expression.visit(f);
                    }
                }
            },
            StatementKind::Unknown { operands } => {
                for operand in operands {
                    operand.visit(f);
                }
            }
            _ => {}
        });
    }
}

#[derive(Debug, Clone)]
pub enum LoopControl {
    Condition {
        condition: Expr,
        test: LoopTest,
    },
    Range {
        variable: Expr,
        start: Expr,
        end: Expr,
        step: Option<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopTest {
    Before,
    After,
}

#[derive(Debug, Clone)]
pub struct Branch<T = Expr> {
    pub test: T,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub enum CaseLabel {
    Value(Expr),
    Range { lower: Expr, upper: Expr },
}

impl CaseLabel {
    pub fn condition(&self, selector: &Expr) -> Expr {
        let compare = |operator, value: &Expr| Expr::Binary {
            operator,
            left: Box::new(selector.clone()),
            right: Box::new(value.clone()),
        };
        match self {
            Self::Value(value) => compare(BinaryOp::Equal, value),
            Self::Range { lower, upper } => {
                compare(BinaryOp::GreaterEqual, lower).conjoin(compare(BinaryOp::LessEqual, upper))
            }
        }
    }
}
