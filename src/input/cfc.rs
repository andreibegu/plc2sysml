use crate::input::codesys::{XmlContext, commented, expression, named, statement, value};
use crate::ir::*;
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub(crate) struct CfcModel {
    blocks: Vec<Block>,
    operands: BTreeMap<String, Expr>,
    outputs: Vec<Output>,
    wires: BTreeMap<String, BTreeSet<String>>,
    owners: BTreeMap<String, (usize, usize)>,
}

#[derive(Debug)]
struct Pin {
    id: String,
    name: String,
    negated: bool,
}

#[derive(Debug)]
struct Block {
    kind: String,
    instance: String,
    inputs: Vec<Pin>,
    outputs: Vec<Pin>,
    source: Source,
    offset: usize,
}

#[derive(Debug)]
struct Output {
    pin: Pin,
    target: Expr,
    source: Source,
    offset: usize,
}

impl CfcModel {
    pub fn read(context: &XmlContext<'_>, root: Node<'_, '_>) -> Self {
        let mut model = Self {
            blocks: vec![],
            operands: BTreeMap::new(),
            outputs: vec![],
            wires: BTreeMap::new(),
            owners: BTreeMap::new(),
        };

        for node in root.descendants().filter(|n| !commented(*n)) {
            let inputs = list(node, "Inputs");
            let outputs = list(node, "Outputs");

            if !inputs.is_empty() || !outputs.is_empty() {
                let texts: Vec<_> = list(node, "Texts")
                    .iter()
                    .map(|n| value(*n, "Text"))
                    .collect();
                let count = inputs.len() + outputs.len();

                if texts.len() != count + 1 && texts.len() != count + 2 {
                    continue;
                }

                let input_count = inputs.len();
                let inputs: Vec<_> = inputs
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| pin(n, &texts[i]))
                    .collect();

                let outputs: Vec<_> = outputs
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| pin(n, &texts[input_count + i]))
                    .collect();

                for (position, pin) in outputs.iter().enumerate() {
                    model
                        .owners
                        .insert(pin.id.clone(), (model.blocks.len(), position));
                }

                model.blocks.push(Block {
                    kind: texts[count].clone(),
                    instance: texts.get(count + 1).cloned().unwrap_or_default(),
                    inputs,
                    outputs,
                    source: context.source(node),
                    offset: node.range().start,
                });
            }

            if let Some(output) = named(node, "Output") {
                let text = value(named(node, "Text").unwrap_or(node), "Text");

                if !text.trim().is_empty() {
                    let pin = pin(output, "");
                    model.operands.insert(
                        pin.id.clone(),
                        pin.apply(expression(&text, &context.source(node))),
                    );
                }
            }

            if let Some(input) = named(node, "Input").filter(|_| named(node, "Text").is_some()) {
                let text = value(named(node, "Text").unwrap(), "Text");

                if !text.trim().is_empty() {
                    model.outputs.push(Output {
                        pin: pin(input, ""),
                        target: expression(&text, &context.source(node)),
                        source: context.source(node),
                        offset: node.range().start,
                    });
                }
            }
        }

        for node in root.descendants() {
            let from = value(node, "SourcePinId");
            let to = value(node, "DestPinId");

            if !from.is_empty() && !to.is_empty() {
                model.wires.entry(to).or_default().insert(from);
            }
        }

        model
    }
}

fn pin(node: Node<'_, '_>, name: &str) -> Pin {
    Pin {
        id: value(node, "Id"),
        name: name.into(),
        negated: value(node, "Negated").eq_ignore_ascii_case("true"),
    }
}

fn list<'a, 'i>(node: Node<'a, 'i>, key: &str) -> Vec<Node<'a, 'i>> {
    named(node, key)
        .and_then(|n| named(n, "InnerList"))
        .into_iter()
        .flat_map(|n| n.children().filter(Node::is_element))
        .collect()
}

impl CfcModel {
    pub fn lower(&self, behavior: &Body) -> Vec<Statement> {
        let mut statements = vec![];

        for block in &self.blocks {
            if block.instance.is_empty() {
                continue;
            }

            let arguments = block
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(i, pin)| {
                    let value = self.connected(&pin.id, &mut BTreeSet::new(), 0)?;

                    Some(Argument {
                        name: Some(if pin.name.is_empty() {
                            format!("input_{}", i + 1)
                        } else {
                            pin.name.clone()
                        }),
                        direction: ArgumentDirection::Input,
                        value: pin.apply(value),
                    })
                })
                .collect();

            statements.push(statement(
                behavior,
                &block.source,
                block.offset,
                StatementKind::Call {
                    expression: Expr::Call {
                        callee: Box::new(expression(&block.instance, &block.source)),
                        arguments,
                    },
                },
            ));
        }

        for output in &self.outputs {
            if let Some(value) = self.connected(&output.pin.id, &mut BTreeSet::new(), 0) {
                statements.push(statement(
                    behavior,
                    &output.source,
                    output.offset,
                    StatementKind::Assign {
                        target: output.target.clone(),
                        value: output.pin.apply(value),
                    },
                ));
            }
        }

        statements
    }

    fn connected(&self, pin: &str, visited: &mut BTreeSet<String>, depth: usize) -> Option<Expr> {
        let sources = self.wires.get(pin)?;

        if sources.len() != 1 {
            return Some(Expr::Unknown { operands: vec![] });
        }

        Some(self.output(sources.first()?, visited, depth))
    }

    fn output(&self, pin: &str, visited: &mut BTreeSet<String>, depth: usize) -> Expr {
        if depth > 128 || !visited.insert(pin.into()) {
            return Expr::Unknown { operands: vec![] };
        }

        let result = if let Some(expr) = self.operands.get(pin) {
            expr.clone()
        } else if let Some((index, position)) = self.owners.get(pin) {
            let block = &self.blocks[*index];
            let outpin = &block.outputs[*position];

            let expr = if !block.instance.is_empty() && !outpin.name.is_empty() {
                expression(
                    &format!("{}.{}", block.instance, outpin.name),
                    &block.source,
                )
            } else {
                let arguments = block
                    .inputs
                    .iter()
                    .filter_map(|input| {
                        let value = self.connected(&input.id, visited, depth + 1)?;

                        Some(Argument {
                            name: (!input.name.is_empty()).then(|| input.name.clone()),
                            direction: ArgumentDirection::Input,
                            value: input.apply(value),
                        })
                    })
                    .collect();

                Expr::Call {
                    callee: Box::new(Expr::Reference {
                        name: block.kind.clone(),
                    }),
                    arguments,
                }
            };

            outpin.apply(expr)
        } else {
            Expr::Unknown { operands: vec![] }
        };

        visited.remove(pin);

        result
    }
}

impl Pin {
    fn apply(&self, expr: Expr) -> Expr {
        if self.negated {
            Expr::Unary {
                operator: UnaryOp::Not,
                operand: Box::new(expr),
            }
        } else {
            expr
        }
    }
}
