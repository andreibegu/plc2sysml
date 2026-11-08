use crate::input::codesys::{XmlContext, commented, expression, named, statement, value};
use crate::ir::*;
use roxmltree::Node;

#[derive(Debug)]
pub(crate) struct LadderModel {
    networks: Vec<Network>,
}

#[derive(Debug)]
struct Network {
    calls: Vec<LocatedBlock>,
    transfers: Vec<Transfer>,
}

#[derive(Debug)]
struct LocatedBlock {
    block: Block,
    source: Source,
    offset: usize,
}

#[derive(Debug)]
struct Transfer {
    value: LadderValue,
    targets: Vec<(LadderValue, Source, usize)>,
}

#[derive(Debug)]
struct Block {
    kind: String,
    instance: String,
    arguments: Vec<BlockArgument>,
    outputs: Vec<Output>,
}

#[derive(Debug)]
struct BlockArgument {
    name: Option<String>,
    direction: ArgumentDirection,
    value: LadderValue,
}

#[derive(Debug)]
struct Output {
    name: String,
    returned: bool,
}

#[derive(Debug)]
enum LadderValue {
    Operand(String),
    Block(Box<Block>),
    Modified { value: Box<Self> },
    Unknown(String),
}

impl LadderModel {
    pub fn read(context: &XmlContext<'_>, root: Node<'_, '_>) -> Self {
        let networks = root
            .descendants()
            .filter(|n| n.attribute("Name") == Some("NetworkItems"))
            .map(|network| {
                let mut calls = vec![];
                let mut transfers = vec![];

                for node in network.descendants().filter(|n| !commented(*n)) {
                    if node
                        .ancestors()
                        .find(|n| n.attribute("Name") == Some("NetworkItems"))
                        != Some(network)
                    {
                        continue;
                    }

                    let call_type = value(node, "CallType");

                    if !value(node, "BoxType").is_empty()
                        && (!instance(node).is_empty()
                            || matches!(call_type.as_str(), "Action" | "Method" | "Function"))
                    {
                        calls.push(LocatedBlock {
                            block: read_block(node, 0),
                            source: context.source(node),
                            offset: node.range().start,
                        });
                    }

                    if let Some(rhs) = named(node, "RValue") {
                        let value = read_value(rhs, 0);
                        let targets = named(node, "OutputItems")
                            .and_then(|n| named(n, "OutputItems"))
                            .into_iter()
                            .flat_map(|n| n.children().filter(Node::is_element))
                            .map(|n| (read_value(n, 0), context.source(n), n.range().start))
                            .collect();
                        transfers.push(Transfer { value, targets });
                    }
                }

                Network { calls, transfers }
            })
            .collect();

        Self { networks }
    }
}

fn read_block(node: Node<'_, '_>, depth: usize) -> Block {
    let mut arguments = vec![];

    for (list_key, param_key, direction) in [
        ("InputItems", "InputParam", ArgumentDirection::Input),
        ("OutputItems", "OutputParam", ArgumentDirection::Output),
    ] {
        let names = parameter_names(node, param_key);
        let list = named(node, list_key).and_then(|n| {
            if list_key == "OutputItems" {
                named(n, "OutputItems")
            } else {
                Some(n)
            }
        });

        for (i, item) in list
            .into_iter()
            .flat_map(|n| n.children().filter(Node::is_element))
            .enumerate()
        {
            arguments.push(BlockArgument {
                name: names.get(i).filter(|s| !s.is_empty()).cloned(),
                direction,
                value: read_value(item, depth + 1),
            });
        }
    }

    let destinations: Vec<_> = named(node, "OutputItems")
        .and_then(|n| named(n, "OutputItems"))
        .into_iter()
        .flat_map(|n| n.children().filter(Node::is_element))
        .collect();

    let outputs = parameter_names(node, "OutputParam")
        .into_iter()
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, name)| Output {
            name,
            returned: destinations
                .get(i)
                .is_some_and(|n| n.tag_name().name() == "Null"),
        })
        .collect();

    Block {
        kind: value(node, "BoxType"),
        instance: instance(node),
        arguments,
        outputs,
    }
}

fn read_value(node: Node<'_, '_>, depth: usize) -> LadderValue {
    if depth > 128 {
        return LadderValue::Unknown("deep ladder expression".into());
    }

    let result = if !value(node, "BoxType").is_empty() {
        LadderValue::Block(Box::new(read_block(node, depth + 1)))
    } else {
        let operand = value(node, "Operand");

        if !operand.trim().is_empty() {
            LadderValue::Operand(operand)
        } else if let Some(child) = named(node, "Operand") {
            read_value(child, depth + 1)
        } else if let Some(child) = named(node, "Input").filter(|n| n.tag_name().name() != "Null") {
            read_value(child, depth + 1)
        } else if !value(node, "VarId").is_empty() {
            let id = value(node, "VarId");
            let producers: Vec<_> = node
                .ancestors()
                .find(|n| n.attribute("Name") == Some("NetworkItems"))
                .into_iter()
                .flat_map(|n| n.descendants())
                .filter(|n| *n != node && value(*n, "VarId") == id)
                .filter_map(|n| named(n, "Input"))
                .filter(|n| n.tag_name().name() != "Null")
                .collect();

            if let [producer] = producers.as_slice() {
                read_value(*producer, depth + 1)
            } else {
                LadderValue::Unknown(format!("unresolved native value {id}"))
            }
        } else {
            LadderValue::Unknown(String::new())
        }
    };

    let flags = named(node, "Flags")
        .map(|n| value(n, "Flags"))
        .unwrap_or_default();

    if !flags.is_empty() && flags != "0" {
        LadderValue::Modified {
            value: Box::new(result),
        }
    } else {
        result
    }
}

fn parameter_names(node: Node<'_, '_>, key: &str) -> Vec<String> {
    named(node, key)
        .and_then(|n| named(n, "Names"))
        .into_iter()
        .flat_map(|n| n.children().filter(Node::is_element))
        .map(|n| n.text().unwrap_or("").to_string())
        .collect()
}

fn instance(node: Node<'_, '_>) -> String {
    named(node, "Instance")
        .map(|n| value(n, "Operand"))
        .unwrap_or_default()
}

impl LadderModel {
    pub fn lower(&self, behavior: &Body) -> Vec<Statement> {
        let mut statements = vec![];

        for network in &self.networks {
            for call in &network.calls {
                statements.push(statement(
                    behavior,
                    &call.source,
                    call.offset,
                    StatementKind::Call {
                        expression: call.block.call(&call.source),
                    },
                ));
            }
        }

        for network in &self.networks {
            for transfer in &network.transfers {
                for (target, source, offset) in &transfer.targets {
                    let target = target.lower(source);

                    if matches!(
                        target,
                        Expr::Reference { .. } | Expr::Member { .. } | Expr::Index { .. }
                    ) {
                        statements.push(statement(
                            behavior,
                            source,
                            *offset,
                            StatementKind::Assign {
                                target,
                                value: transfer.value.lower(source),
                            },
                        ));
                    }
                }
            }
        }

        statements
    }
}

impl Block {
    fn call(&self, source: &Source) -> Expr {
        let callee = if self.instance.is_empty() {
            &self.kind
        } else {
            &self.instance
        };

        let arguments = self
            .arguments
            .iter()
            .filter_map(|arg| {
                if matches!(&arg.value, LadderValue::Operand(text) | LadderValue::Unknown(text) if text.trim().is_empty()) {
                    return None;
                }
                let value = arg.value.lower(source);

                Some(Argument {
                    name: arg.name.clone(),
                    direction: arg.direction,
                    value,
                })
            })
            .collect();

        Expr::Call {
            callee: Box::new(expression(callee, source)),
            arguments,
        }
    }

    fn result(&self, source: &Source) -> Expr {
        if self.instance.is_empty() {
            return self.call(source);
        }

        let returned: Vec<_> = self.outputs.iter().filter(|o| o.returned).collect();

        let output = if let [output] = returned.as_slice() {
            Some(*output)
        } else if let [output] = self.outputs.as_slice() {
            Some(output)
        } else {
            None
        };

        if let Some(output) = output {
            expression(&format!("{}.{}", self.instance, output.name), source)
        } else {
            Expr::Unknown { operands: vec![] }
        }
    }
}

impl LadderValue {
    fn lower(&self, source: &Source) -> Expr {
        match self {
            Self::Operand(text) => expression(text, source),
            Self::Block(block) => block.result(source),

            Self::Modified { value, .. } => Expr::Unknown {
                operands: vec![value.lower(source)],
            },

            Self::Unknown(text) => super::st::unknown(text.clone()),
        }
    }
}
