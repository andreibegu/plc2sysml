use super::{ACTION, JUMP, QUALIFIER, SEQUENCE, TEXT, TRANSITION, attribute, elements};
use crate::input::codesys::{STEP, XmlContext, expression, named, value};
use crate::ir::*;
use roxmltree::Node;

pub(crate) fn recover(
    context: &XmlContext<'_>,
    root: Node<'_, '_>,
    behavior: &Body,
) -> Vec<Statement> {
    let steps: Vec<_> = root
        .descendants()
        .filter(|n| n.attribute("Type") == Some(STEP))
        .collect();
    let mut statements = vec![];

    for step in &steps {
        let action = attribute(*step, "700a583f-b4d4-43e4-8c14-629c7cd3bec8");

        if !action.trim().is_empty() {
            let source = context.source(*step);
            statements.push(Statement {
                id: id(&["sfc_step_action", &behavior.id, &value(*step, "Id")]),
                source,
                kind: StatementKind::MayAffect {
                    condition: Expr::Literal {
                        value: "TRUE".into(),
                    },
                    target: expression(&action, &context.source(*step)),
                },
            });
        }
    }

    for sequence in root
        .descendants()
        .filter(|n| n.attribute("Type") == Some(SEQUENCE))
    {
        let items = elements(sequence);

        for (position, transition) in items
            .iter()
            .enumerate()
            .filter(|(_, n)| n.attribute("Type") == Some(TRANSITION))
        {
            if named(*transition, "EmbeddedTransition")
                .is_some_and(|n| n.tag_name().name() != "Null")
            {
                continue;
            }

            let text = attribute(*transition, TEXT);
            let source = context.source(*transition);
            let condition = expression(&text, &source);

            if text.trim().is_empty() || matches!(condition, Expr::Unknown { .. }) {
                continue;
            }

            let preceding = position
                .checked_sub(1)
                .and_then(|p| items.get(p))
                .copied()
                .filter(|n| n.attribute("Type") == Some(STEP));

            let following =
                items
                    .get(position + 1)
                    .copied()
                    .and_then(|n| match n.attribute("Type") {
                        Some(STEP) => Some(n),

                        Some(JUMP) => {
                            let target = attribute(n, TEXT);
                            let mut matches = steps
                                .iter()
                                .copied()
                                .filter(|s| attribute(*s, TEXT).eq_ignore_ascii_case(&target));
                            let found = matches.next()?;

                            matches.next().is_none().then_some(found)
                        }

                        _ => None,
                    });

            for (step, entering) in preceding
                .into_iter()
                .map(|s| (s, false))
                .chain(following.into_iter().map(|s| (s, true)))
            {
                for action in elements(step)
                    .into_iter()
                    .filter(|n| n.attribute("Type") == Some(ACTION))
                {
                    let qualifier = attribute(action, QUALIFIER).to_ascii_uppercase();

                    if qualifier != "N" && !(entering && matches!(qualifier.as_str(), "S" | "R")) {
                        continue;
                    }

                    let target_text = attribute(action, TEXT);
                    let target = expression(&target_text, &context.source(action));

                    if !matches!(target, Expr::Reference { .. } | Expr::Member { .. }) {
                        continue;
                    }

                    statements.push(Statement {
                        id: id(&[
                            "sfc_selection",
                            &behavior.id,
                            &value(*transition, "Id"),
                            &value(action, "Id"),
                            if entering { "entry" } else { "exit" },
                        ]),
                        source: source.clone(),
                        kind: StatementKind::MayAffect {
                            condition: condition.clone(),
                            target,
                        },
                    });
                }
            }
        }
    }

    statements
}
