use crate::ir::{BinaryOp, UnaryOp};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Constant {
    Boolean(bool),
    Integer(i128),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Value {
    pub origins: BTreeSet<String>,
    pub constants: Option<BTreeSet<Constant>>,
}

impl Value {
    pub fn entry(key: &str) -> Self {
        Self {
            origins: BTreeSet::from([key.into()]),
            ..Self::default()
        }
    }

    pub fn literal(text: &str) -> Self {
        let text = text.trim().to_ascii_uppercase();

        let constant = match text.as_str() {
            "TRUE" => Some(Constant::Boolean(true)),
            "FALSE" => Some(Constant::Boolean(false)),
            _ => text.replace('_', "").parse().ok().map(Constant::Integer),
        };

        Self {
            constants: constant.map(|c| BTreeSet::from([c])),
            ..Self::default()
        }
    }

    pub fn join(&mut self, other: &Self) {
        self.origins.extend(other.origins.iter().cloned());

        self.constants = match (&self.constants, &other.constants) {
            (Some(a), Some(b)) => {
                let union: BTreeSet<_> = a.union(b).cloned().collect();
                (union.len() <= 32).then_some(union)
            }

            _ => None,
        };
    }

    pub fn add_control(&mut self, guard: &Self) {
        self.origins.extend(guard.origins.iter().cloned());
    }

    pub fn truth(&self) -> Option<bool> {
        let values = self.constants.as_ref()?;

        if values.is_empty() {
            return None;
        }

        if values.iter().all(|c| *c == Constant::Boolean(true)) {
            Some(true)
        } else if values.iter().all(|c| *c == Constant::Boolean(false)) {
            Some(false)
        } else {
            None
        }
    }

    pub fn unary(operator: UnaryOp, mut value: Self) -> Self {
        value.constants = value.constants.and_then(|values| {
            values
                .into_iter()
                .map(|c| match (operator, c) {
                    (UnaryOp::Not, Constant::Boolean(b)) => Some(Constant::Boolean(!b)),
                    (UnaryOp::Negate, Constant::Integer(n)) => {
                        n.checked_neg().map(Constant::Integer)
                    }
                    (UnaryOp::Positive, c) => Some(c),
                    _ => None,
                })
                .collect()
        });

        value
    }

    pub fn binary(operator: BinaryOp, left: Self, right: Self) -> Self {
        let absorbing = match operator {
            BinaryOp::And => Some(false),
            BinaryOp::Or => Some(true),
            _ => None,
        };

        if let Some(truth) = absorbing {
            if left.truth() == Some(truth) {
                return left;
            }

            if right.truth() == Some(truth) {
                return right;
            }
        }

        let constants = match (&left.constants, &right.constants) {
            (Some(a), Some(b)) if a.len() * b.len() <= 32 => a
                .iter()
                .flat_map(|a| b.iter().map(move |b| (a, b)))
                .map(|(a, b)| binary_constant(operator, a, b))
                .collect::<Option<BTreeSet<_>>>(),

            _ => None,
        };

        let mut value = left;
        value.join(&right);
        value.constants = constants;

        value
    }
}

fn binary_constant(operator: BinaryOp, a: &Constant, b: &Constant) -> Option<Constant> {
    use Constant::*;

    match (operator, a, b) {
        (BinaryOp::Equal, Boolean(a), Boolean(b)) => Some(Boolean(a == b)),
        (BinaryOp::Equal, Integer(a), Integer(b)) => Some(Boolean(a == b)),
        (BinaryOp::NotEqual, Boolean(a), Boolean(b)) => Some(Boolean(a != b)),
        (BinaryOp::NotEqual, Integer(a), Integer(b)) => Some(Boolean(a != b)),
        (BinaryOp::And, Boolean(a), Boolean(b)) => Some(Boolean(*a && *b)),
        (BinaryOp::Or, Boolean(a), Boolean(b)) => Some(Boolean(*a || *b)),
        (BinaryOp::Xor, Boolean(a), Boolean(b)) => Some(Boolean(*a != *b)),
        (BinaryOp::Less, Integer(a), Integer(b)) => Some(Boolean(a < b)),
        (BinaryOp::Greater, Integer(a), Integer(b)) => Some(Boolean(a > b)),
        (BinaryOp::LessEqual, Integer(a), Integer(b)) => Some(Boolean(a <= b)),
        (BinaryOp::GreaterEqual, Integer(a), Integer(b)) => Some(Boolean(a >= b)),

        _ => None,
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct State {
    pub values: BTreeMap<String, Value>,
    pub conditions: BTreeMap<usize, Value>,
    pub writes: BTreeSet<String>,
}

impl State {
    pub fn read(&self, key: &str) -> Value {
        self.values
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::entry(key))
    }

    pub fn join(&mut self, other: &Self) -> bool {
        let mut changed = false;
        let keys: BTreeSet<_> = self
            .values
            .keys()
            .chain(other.values.keys())
            .cloned()
            .collect();

        for key in keys {
            let mut value = self.read(&key);
            value.join(&other.read(&key));

            if self.values.get(&key) != Some(&value) {
                self.values.insert(key, value);
                changed = true;
            }
        }

        for (key, value) in &other.conditions {
            match self.conditions.entry(*key) {
                Entry::Occupied(mut entry) => {
                    let before = entry.get().clone();
                    entry.get_mut().join(value);
                    changed |= entry.get() != &before;
                }

                Entry::Vacant(entry) => {
                    entry.insert(value.clone());
                    changed = true;
                }
            }
        }

        let writes = self.writes.len();
        self.writes.extend(other.writes.iter().cloned());

        changed || writes != self.writes.len()
    }
}

type Constants = Vec<(String, BTreeSet<Constant>)>;
const LIMIT: usize = 8;

#[derive(Clone, Default)]
pub(super) struct Partitions {
    pub states: Vec<State>,
    merged: bool,
}

impl Partitions {
    pub fn insert(&mut self, state: State) -> bool {
        if self.merged {
            return self.states[0].join(&state);
        }

        let constants = environment(&state);

        if let Some(previous) = self
            .states
            .iter_mut()
            .find(|previous| environment(previous) == constants)
        {
            return previous.join(&state);
        }

        if self.states.len() < LIMIT {
            self.states.push(state);
            return true;
        }

        let mut joined = state;

        for previous in self.states.drain(..) {
            joined.join(&previous);
        }

        self.states.push(joined);
        self.merged = true;

        true
    }

    pub fn joined(&self) -> Option<State> {
        let mut states = self.states.iter();
        let mut joined = states.next()?.clone();

        for state in states {
            joined.join(state);
        }

        Some(joined)
    }
}

fn environment(state: &State) -> Constants {
    state
        .values
        .iter()
        .filter_map(|(key, value)| value.constants.clone().map(|c| (key.clone(), c)))
        .collect()
}
