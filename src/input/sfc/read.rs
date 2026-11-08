use super::MAX_CONFIGURATIONS;
use super::{ACTION, JUMP, QUALIFIER, SEQUENCE, TEXT, TRANSITION, attribute, elements};
use super::{Arc, SfcModel};
use crate::input::codesys::{STEP, XmlContext, expression, named, value};
use crate::ir::*;
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};

const BRANCH: &str = "{e221b778-bb8f-44ba-8a6a-f796287d4e62}";
const MACRO: &str = "{ab40cf8e-1fe8-43b9-aac3-2242e32c7f9e}";
const PARALLEL: &str = "23bdaa98-72ec-41f7-817b-9dede5697086";
const INITIAL: &str = "6844a48e-46c2-4cc8-a185-a478f3b99cc0";
const EXCLUDED: &str = "01580b27-6378-448b-8ecb-0e4b795b58d6";
const MIN_TIME: &str = "cacf1a68-236e-47c2-b7b1-1cf9199718cb";

#[derive(Clone)]
struct Frontier {
    steps: BTreeSet<String>,
    condition: Expr,
    source: Source,
}

struct Reader<'a, 'b> {
    context: &'a XmlContext<'b>,
    names: BTreeMap<String, String>,
    arcs: Vec<Arc>,
}

impl SfcModel {
    pub fn read(
        context: &XmlContext<'_>,
        root: Node<'_, '_>,
        settings: Node<'_, '_>,
    ) -> Result<Self, String> {
        let sequence = root
            .descendants()
            .find(|n| n.attribute("Type") == Some(SEQUENCE))
            .ok_or("SFC has no root sequence")?;
        let steps: Vec<_> = root
            .descendants()
            .filter(|n| n.attribute("Type") == Some(STEP))
            .collect();
        let initial: Vec<_> = steps
            .iter()
            .filter(|s| attribute(**s, INITIAL).eq_ignore_ascii_case("TRUE"))
            .collect();

        let [initial] = initial.as_slice() else {
            return Err("SFC must have exactly one initial step".into());
        };

        let mut topology = Reader {
            context,
            names: BTreeMap::new(),
            arcs: vec![],
        };

        let mut labels = BTreeSet::new();

        for step in &steps {
            let label = attribute(*step, TEXT);

            if label.is_empty() || !labels.insert(label.to_ascii_lowercase()) {
                return Err("SFC step names are empty or ambiguous".into());
            }

            if !attribute(*step, MIN_TIME).trim().is_empty() {
                return Err("SFC minimum step times need a timed execution model".into());
            }

            topology.names.insert(value(*step, "Id"), label);
        }

        topology.walk(&elements(sequence), vec![], 0)?;

        let mut model = Self {
            names: topology.names,
            arcs: topology.arcs,
            initial: value(**initial, "Id"),
            activities: vec![],
            reset: None,
            observer: None,
            runtime: vec![],
        };

        for step in &steps {
            for action in elements(*step)
                .into_iter()
                .filter(|n| n.attribute("Type") == Some(ACTION))
            {
                model.activities.push(StateActivity {
                    step: attribute(*step, TEXT),
                    target: expression(&attribute(action, TEXT), &context.source(action)),
                    kind: match attribute(action, QUALIFIER).to_ascii_uppercase().as_str() {
                        "N" => ActivityKind::WhileActive,
                        "S" => ActivityKind::Set,
                        "R" => ActivityKind::Reset,
                        _ => ActivityKind::Unsupported,
                    },
                    source: context.source(action),
                });
            }

            let action = attribute(*step, "700a583f-b4d4-43e4-8c14-629c7cd3bec8");

            if !action.trim().is_empty() {
                model.activities.push(StateActivity {
                    step: attribute(*step, TEXT),
                    target: expression(&action, &context.source(*step)),
                    kind: ActivityKind::StepAction,
                    source: context.source(*step),
                });
            }
        }

        for flags in settings
            .descendants()
            .filter(|n| n.has_tag_name("Dictionary2") && n.attribute("Name") == Some("Flags"))
        {
            for entry in flags.children().filter(Node::is_element) {
                let used = entry
                    .descendants()
                    .find(|n| n.attribute("Name") == Some("Use"))
                    .is_some_and(|n| n.text().is_some_and(|v| v.eq_ignore_ascii_case("TRUE")));

                if !used {
                    continue;
                }

                let flag = entry
                    .children()
                    .find(|n| n.has_tag_name("Key"))
                    .and_then(|n| n.children().find(Node::is_element))
                    .and_then(|n| n.text())
                    .unwrap_or("");

                let (name, ty) = match flag {
                    "Reset" => {
                        model.reset = Some(Expr::Reference {
                            name: "SFCReset".into(),
                        });
                        ("SFCReset", "BOOL")
                    }

                    "CurrentStep" => {
                        model.observer = Some(Expr::Reference {
                            name: "SFCCurrentStep".into(),
                        });
                        ("SFCCurrentStep", "STRING")
                    }

                    _ => {
                        return Err(format!(
                            "Enabled SFC control flag requires additional execution semantics: {flag}"
                        ));
                    }
                };

                let declared = entry
                    .descendants()
                    .find(|n| n.attribute("Name") == Some("Declare"))
                    .is_some_and(|n| n.text().is_some_and(|v| v.eq_ignore_ascii_case("TRUE")));

                if !declared {
                    continue;
                }

                let source = context.source(entry);
                model.runtime.push(Variable {
                    id: id(&[
                        "sfc_runtime",
                        &source.path,
                        source.origin.as_deref().unwrap_or(""),
                        name,
                    ]),
                    name: name.into(),
                    kind: VariableKind::Local,
                    constant: false,
                    retention: Retention::Automatic,
                    ty: TypeRef::scalar(ty),
                    initial: None,
                    address: None,
                    source,
                });
            }
        }

        Ok(model)
    }
}

impl Reader<'_, '_> {
    fn walk(
        &mut self,
        items: &[Node<'_, '_>],
        mut frontier: Vec<Frontier>,
        depth: usize,
    ) -> Result<Vec<Frontier>, String> {
        if depth > 64 {
            return Err("SFC nesting exceeds the recovery bound".into());
        }

        for node in items {
            if attribute(*node, EXCLUDED).eq_ignore_ascii_case("TRUE") {
                return Err("Excluded SFC elements are not recovered".into());
            }

            match node.attribute("Type") {
                Some(MACRO) => {
                    let segment = named(*node, "Segment").ok_or("SFC macro has no segment")?;
                    frontier = self.walk(&elements(segment), frontier, depth + 1)?;
                }

                Some(STEP) => {
                    let next = BTreeSet::from([value(*node, "Id")]);
                    self.connect(&frontier, &next);
                    frontier = vec![Frontier {
                        steps: next,
                        condition: Expr::boolean(true),
                        source: self.context.source(*node),
                    }];
                }

                Some(TRANSITION) => {
                    if named(*node, "EmbeddedTransition")
                        .is_some_and(|n| n.tag_name().name() != "Null")
                    {
                        return Err(
                            "Embedded SFC transition implementations are not recovered".into()
                        );
                    }

                    let text = attribute(*node, TEXT);

                    if text.trim().is_empty() {
                        return Err("SFC transition condition is empty".into());
                    }

                    let condition = expression(&text, &self.context.source(*node));

                    if matches!(condition, Expr::Unknown { .. }) {
                        return Err(format!("Unsupported SFC condition: {text}"));
                    }

                    for incoming in &mut frontier {
                        incoming.condition = incoming.condition.clone().conjoin(condition.clone());
                        incoming.source = self.context.source(*node);
                    }
                }

                Some(JUMP) => {
                    let label = attribute(*node, TEXT);
                    let destination = self
                        .names
                        .iter()
                        .find(|(_, n)| n.eq_ignore_ascii_case(&label))
                        .map(|(id, _)| id.clone())
                        .ok_or_else(|| format!("SFC jump target is missing: {label}"))?;
                    self.connect(&frontier, &BTreeSet::from([destination]));
                    frontier.clear();
                }

                Some(BRANCH) => {
                    let branches = elements(*node);

                    if branches.is_empty()
                        || branches
                            .iter()
                            .any(|n| n.attribute("Type") != Some(SEQUENCE))
                    {
                        return Err("SFC branch has unsupported children".into());
                    }

                    if attribute(*node, PARALLEL).eq_ignore_ascii_case("TRUE") {
                        let lists: Vec<_> = branches.iter().map(|n| elements(*n)).collect();

                        if lists.iter().any(|nodes| {
                            nodes
                                .first()
                                .is_none_or(|n| n.attribute("Type") != Some(STEP))
                        }) {
                            return Err(
                                "Parallel SFC branches must begin with explicit steps".into()
                            );
                        }

                        let start: BTreeSet<_> =
                            lists.iter().map(|nodes| value(nodes[0], "Id")).collect();
                        self.connect(&frontier, &start);

                        let mut joined = vec![Frontier {
                            steps: BTreeSet::new(),
                            condition: Expr::boolean(true),
                            source: self.context.source(*node),
                        }];

                        for nodes in lists {
                            let ends = self.walk(
                                &nodes[1..],
                                vec![Frontier {
                                    steps: BTreeSet::from([value(nodes[0], "Id")]),
                                    condition: Expr::boolean(true),
                                    source: self.context.source(nodes[0]),
                                }],
                                depth + 1,
                            )?;

                            if ends.is_empty() {
                                continue;
                            }

                            let mut combined = vec![];

                            for a in &joined {
                                for b in &ends {
                                    combined.push(Frontier {
                                        steps: a.steps.union(&b.steps).cloned().collect(),
                                        condition: a.condition.clone().conjoin(b.condition.clone()),
                                        source: b.source.clone(),
                                    });
                                }
                            }

                            if combined.len() > MAX_CONFIGURATIONS {
                                return Err(
                                    "SFC join alternatives exceed the recovery bound".into()
                                );
                            }

                            joined = combined;
                        }

                        frontier = joined.into_iter().filter(|f| !f.steps.is_empty()).collect();
                    } else {
                        let mut alternatives = vec![];
                        let mut previous = Expr::boolean(false);

                        for branch in branches {
                            let nodes = elements(branch);
                            let first = nodes
                                .first()
                                .filter(|n| n.attribute("Type") == Some(TRANSITION))
                                .ok_or("Alternative SFC branches must begin with a transition")?;
                            let guard =
                                expression(&attribute(*first, TEXT), &self.context.source(*first));
                            let incoming = frontier
                                .iter()
                                .cloned()
                                .map(|mut f| {
                                    f.condition = f.condition.conjoin(previous.clone().negated());
                                    f
                                })
                                .collect();

                            alternatives.extend(self.walk(&nodes, incoming, depth + 1)?);
                            previous = previous.disjoin(guard);
                        }

                        frontier = alternatives;
                    }
                }

                _ => return Err("Unknown SFC topology element".into()),
            }

            if frontier.len() > MAX_CONFIGURATIONS {
                return Err("SFC alternatives exceed the recovery bound".into());
            }
        }

        Ok(frontier)
    }

    fn connect(&mut self, incoming: &[Frontier], to: &BTreeSet<String>) {
        for f in incoming {
            if !f.steps.is_empty() && f.condition.as_boolean() != Some(false) {
                self.arcs.push(Arc {
                    from: f.steps.clone(),
                    to: to.clone(),
                    condition: f.condition.clone(),
                    source: f.source.clone(),
                });
            }
        }
    }
}
