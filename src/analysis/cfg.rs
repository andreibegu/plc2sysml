use crate::ir::*;
use petgraph::{
    algo::dominators::simple_fast,
    graphmap::DiGraphMap,
    visit::{Dfs, Walker},
};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub(crate) enum Operation {
    Exit,
    Assign(Expr, Expr),
    Call(Expr),
    Branch(Expr),
    MayAffect(Expr, Expr),
    Unknown,
    Advance(Expr),
}

#[derive(Debug, Clone)]
pub(crate) struct FlowNode {
    pub operation: Operation,
    pub source: Source,
    pub successors: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct ControlFlowGraph {
    pub nodes: Vec<FlowNode>,
    pub entry: usize,
    pub controls: Vec<BTreeSet<usize>>,
}

impl ControlFlowGraph {
    pub(crate) fn new(behavior: &Body) -> Self {
        let mut graph = Self {
            nodes: vec![FlowNode {
                operation: Operation::Exit,
                source: behavior.source.clone(),
                successors: vec![],
            }],
            entry: 0,
            controls: vec![],
        };

        graph.entry = graph.sequence(&behavior.statements, 0, 0, 0);
        graph.compute_control_dependencies();

        graph
    }

    fn add(
        &mut self,
        operation: Operation,
        statement: &Statement,
        successors: Vec<usize>,
    ) -> usize {
        let index = self.nodes.len();
        self.nodes.push(FlowNode {
            operation,
            source: statement.source.clone(),
            successors,
        });

        index
    }

    fn sequence(
        &mut self,
        statements: &[Statement],
        mut next: usize,
        leave: usize,
        repeat: usize,
    ) -> usize {
        for statement in statements.iter().rev() {
            next = match &statement.kind {
                StatementKind::Assign { target, value } => self.add(
                    Operation::Assign(target.clone(), value.clone()),
                    statement,
                    vec![next],
                ),

                StatementKind::Call { expression } => {
                    self.add(Operation::Call(expression.clone()), statement, vec![next])
                }

                StatementKind::MayAffect { condition, target } => self.add(
                    Operation::MayAffect(condition.clone(), target.clone()),
                    statement,
                    vec![next],
                ),

                StatementKind::Return => 0,
                StatementKind::Exit => leave,
                StatementKind::Continue => repeat,

                StatementKind::Unknown { .. } => {
                    self.add(Operation::Unknown, statement, vec![next])
                }

                StatementKind::If {
                    branches,
                    otherwise,
                } => {
                    let mut alternative = self.sequence(otherwise, next, leave, repeat);

                    for branch in branches.iter().rev() {
                        let body = self.sequence(&branch.body, next, leave, repeat);
                        alternative = self.add(
                            Operation::Branch(branch.test.clone()),
                            statement,
                            vec![body, alternative],
                        );
                    }

                    alternative
                }

                StatementKind::Case {
                    selector,
                    arms,
                    otherwise,
                } => {
                    let mut alternative = self.sequence(otherwise, next, leave, repeat);

                    for arm in arms.iter().rev() {
                        let body = self.sequence(&arm.body, next, leave, repeat);
                        let condition = arm
                            .test
                            .iter()
                            .map(|label| label.condition(selector))
                            .reduce(Expr::disjoin)
                            .unwrap();

                        alternative = self.add(
                            Operation::Branch(condition),
                            statement,
                            vec![body, alternative],
                        );
                    }

                    alternative
                }

                StatementKind::Loop {
                    control: LoopControl::Condition { condition, test },
                    body,
                } => {
                    let branch = self.add(Operation::Branch(condition.clone()), statement, vec![]);
                    let body = self.sequence(body, branch, next, branch);
                    let is_repeat = *test == LoopTest::After;
                    self.nodes[branch].successors = if is_repeat {
                        vec![next, body]
                    } else {
                        vec![body, next]
                    };

                    if is_repeat { body } else { branch }
                }

                StatementKind::Loop {
                    control:
                        LoopControl::Range {
                            variable,
                            start,
                            end,
                            step,
                        },
                    body,
                } => {
                    let condition = Expr::Unknown {
                        operands: [Some(start.clone()), Some(end.clone()), step.clone()]
                            .into_iter()
                            .flatten()
                            .collect(),
                    };

                    let branch = self.add(Operation::Branch(condition), statement, vec![]);
                    let increment = self.add(
                        Operation::Advance(variable.clone()),
                        statement,
                        vec![branch],
                    );
                    let body = self.sequence(body, increment, next, increment);
                    self.nodes[branch].successors = vec![body, next];

                    self.add(
                        Operation::Assign(variable.clone(), start.clone()),
                        statement,
                        vec![branch],
                    )
                }
            };
        }

        next
    }

    fn compute_control_dependencies(&mut self) {
        let mut reverse = DiGraphMap::<usize, ()>::new();

        for (index, node) in self.nodes.iter().enumerate() {
            reverse.add_node(index);

            for successor in &node.successors {
                reverse.add_edge(*successor, index, ());
            }
        }

        let dominators = simple_fast(&reverse, 0);
        let post: Vec<BTreeSet<_>> = (0..self.nodes.len())
            .map(|index| dominators.dominators(index).into_iter().flatten().collect())
            .collect();
        let mut controls = vec![BTreeSet::new(); self.nodes.len()];

        for (branch, node) in self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.successors.len() == 2)
        {
            for successor in &node.successors {
                for controlled in post[*successor].difference(&post[branch]) {
                    controls[*controlled].insert(branch);
                }
            }
        }

        let relation = DiGraphMap::<usize, ()>::from_edges(
            controls
                .iter()
                .enumerate()
                .flat_map(|(node, parents)| parents.iter().map(move |parent| (node, *parent))),
        );

        self.controls = controls
            .into_iter()
            .map(|parents| {
                parents
                    .into_iter()
                    .flat_map(|parent| Dfs::new(&relation, parent).iter(&relation))
                    .collect()
            })
            .collect();
    }
}

impl FlowNode {
    pub(super) fn effect(statement: &Statement) -> Self {
        let operation = match &statement.kind {
            StatementKind::Assign { target, value } => {
                Operation::Assign(target.clone(), value.clone())
            }
            StatementKind::Call { expression } => Operation::Call(expression.clone()),
            StatementKind::MayAffect { condition, target } => {
                Operation::MayAffect(condition.clone(), target.clone())
            }
            _ => Operation::Unknown,
        };
        Self {
            operation,
            source: statement.source.clone(),
            successors: vec![],
        }
    }
}
