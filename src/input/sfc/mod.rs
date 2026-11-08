use crate::input::codesys::{named, value};
use crate::ir::*;
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod effects;
mod read;

pub(crate) use effects::recover;

const SEQUENCE: &str = "{f1db7f6f-4774-46a6-ad9b-a94414f875fc}";
const TRANSITION: &str = "{4ecb29e8-a90a-4646-84ac-2ff54215ab29}";
const JUMP: &str = "{7e39ac27-154f-419a-a255-6a6cf37d5ed0}";
const ACTION: &str = "{a94bb75d-5fb3-40ad-9d01-41d5b980784d}";
const TEXT: &str = "38391c6d-6d4a-42f8-8ee7-9f45e5adafa8";
const MAX_CONFIGURATIONS: usize = 128;
const QUALIFIER: &str = "e174fc0d-80b0-4a9e-a530-ca239c249a50";

fn attribute(node: Node<'_, '_>, key: &str) -> String {
    named(node, "Attributes")
        .into_iter()
        .flat_map(|n| n.children().filter(Node::is_element))
        .find(|n| value(*n, "AttributeId") == key)
        .and_then(|n| named(n, "Value"))
        .map(|n| value(n, "Value"))
        .unwrap_or_default()
}

fn elements<'a, 'i>(node: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    named(node, "ElementList")
        .and_then(|n| named(n, "InnerList"))
        .into_iter()
        .flat_map(|n| n.children().filter(Node::is_element))
        .collect()
}

#[derive(Debug)]
pub(super) struct Arc {
    pub(super) from: BTreeSet<String>,
    pub(super) to: BTreeSet<String>,
    pub(super) condition: Expr,
    pub(super) source: Source,
}

#[derive(Debug)]
pub(crate) struct SfcModel {
    pub(super) names: BTreeMap<String, String>,
    pub(super) arcs: Vec<Arc>,
    pub(super) initial: String,
    pub(super) activities: Vec<StateActivity>,
    pub(super) reset: Option<Expr>,
    pub(super) observer: Option<Expr>,
    pub(crate) runtime: Vec<Variable>,
}

impl SfcModel {
    pub fn lower(self) -> Result<StateGraph, String> {
        let mut graph = self.expand(self.initial.clone())?;
        graph.activities = self.activities;
        graph.reset = self.reset;
        graph.observation = self.observer.map(|variable| {
            let mut labels = std::collections::BTreeMap::new();

            for label in self.names.values() {
                let positions: Vec<_> = graph
                    .states
                    .iter()
                    .filter(|s| s.active_steps.contains(label))
                    .collect();

                if !positions.is_empty() && positions.iter().all(|s| s.active_steps.len() == 1) {
                    labels.insert(
                        label.clone(),
                        positions.iter().map(|s| s.name.clone()).collect(),
                    );
                }
            }

            StateObservation { variable, labels }
        });

        Ok(graph)
    }

    pub(super) fn expand(&self, initial: String) -> Result<StateGraph, String> {
        let initial = BTreeSet::from([initial]);
        let mut pending = VecDeque::from([initial.clone()]);
        let mut seen = BTreeSet::from([initial.clone()]);
        let label = |steps: &BTreeSet<String>| {
            steps
                .iter()
                .map(|s| self.names[s].as_str())
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let mut transitions = vec![];

        while let Some(active) = pending.pop_front() {
            let applicable: Vec<_> = self
                .arcs
                .iter()
                .filter(|a| a.from.is_subset(&active))
                .collect();

            for (i, a) in applicable.iter().enumerate() {
                if applicable[i + 1..]
                    .iter()
                    .any(|b| a.from.is_disjoint(&b.from))
                {
                    return Err(
                        "Concurrent progressing SFC regions require synchronized scan semantics"
                            .into(),
                    );
                }

                let next: BTreeSet<_> = active.difference(&a.from).chain(&a.to).cloned().collect();

                if next == active {
                    continue;
                }

                transitions.push(StateTransition {
                    from: label(&active),
                    to: label(&next),
                    condition: a.condition.clone(),
                    source: a.source.clone(),
                });

                if seen.insert(next.clone()) {
                    if seen.len() > MAX_CONFIGURATIONS {
                        return Err(
                            "SFC active-step configurations exceed the recovery bound".into()
                        );
                    }

                    pending.push_back(next);
                }
            }
        }

        Ok(StateGraph {
            activities: vec![],
            reset: None,
            name: "Sequence position".into(),
            selector: None,
            observation: None,
            initial: label(&initial),
            states: seen
                .into_iter()
                .map(|steps| ControlState {
                    name: label(&steps),
                    active_steps: steps.iter().map(|s| self.names[s].clone()).collect(),
                    selector_value: None,
                })
                .collect(),
            transitions,
        })
    }
}
