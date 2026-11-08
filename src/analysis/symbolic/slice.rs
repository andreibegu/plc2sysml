use super::*;

impl ControlInstance {
    pub fn value_in_state(&self, expression: &ControlExpr, state: &str) -> Option<ControlExpr> {
        match expression {
            ControlExpr::Read {
                key,
                previous: false,
            } if self.selector.as_deref() == Some(key) => self
                .graph
                .states
                .iter()
                .find(|s| s.name == state)?
                .selector_value
                .clone()
                .map(|value| ControlExpr::Literal { value }),
            ControlExpr::Active { machine, states } if machine == &self.id => {
                Some(ControlExpr::boolean(states.iter().any(|s| s == state)))
            }
            _ => None,
        }
    }
}

const MAX_NODES: usize = 512;
const MAX_DEPTH: usize = 32;
const MAX_ORIGINS: usize = 256;

impl Analysis {
    pub fn slice(
        &self,
        key: &str,
        state: Option<(&ControlInstance, &str)>,
        stops: &BTreeSet<String>,
    ) -> ControlExpr {
        let expression = Slice {
            analysis: self,
            state,
            stops,
            active: BTreeSet::new(),
            remaining: MAX_NODES,
        }
        .read(key, false);

        if self
            .cells
            .get(key)
            .is_some_and(|c| c.storage.ty.base.eq_ignore_ascii_case("BOOL"))
        {
            expression.reduced_boolean()
        } else {
            expression
        }
    }

    pub fn depends(
        &self,
        key: &str,
        target: &str,
        state: Option<(&ControlInstance, &str)>,
    ) -> bool {
        let expression = self.slice(key, state, &BTreeSet::from([target.into()]));
        let mut pending = vec![expression];
        let mut seen = BTreeSet::new();

        while let Some(expression) = pending.pop() {
            let mut reads = vec![];
            expression.visit(&mut |e| {
                if let ControlExpr::Read { key, .. } = e {
                    reads.push(key.clone());
                }
            });

            for key in reads {
                if key == target {
                    return true;
                }

                if seen.len() < MAX_ORIGINS
                    && seen.insert(key.clone())
                    && let Some(cell) = self.cells.get(&key)
                {
                    for input in &cell.dependencies {
                        pending.push(self.slice(input, None, &BTreeSet::from([target.into()])));
                    }
                }
            }
        }

        false
    }
}

struct Slice<'a> {
    analysis: &'a Analysis,
    state: Option<(&'a ControlInstance, &'a str)>,
    stops: &'a BTreeSet<String>,
    active: BTreeSet<String>,
    remaining: usize,
}

impl Slice<'_> {
    fn read(&mut self, key: &str, previous: bool) -> ControlExpr {
        let read = ControlExpr::Read {
            key: key.into(),
            previous,
        };

        if let Some((machine, state)) = self.state
            && let Some(value) = machine.value_in_state(&read, state)
        {
            return value;
        }

        if previous
            || self.remaining == 0
            || self.active.len() >= MAX_DEPTH
            || self.stops.contains(key)
        {
            return read;
        }

        let Some(cell) = self.analysis.cells.get(key) else {
            return read;
        };

        if cell.sampled || !self.active.insert(key.into()) {
            return read;
        }

        let result = cell
            .expression
            .as_ref()
            .map(|e| self.expression(e))
            .unwrap_or(read);

        self.active.remove(key);

        result
    }

    fn expression(&mut self, e: &ControlExpr) -> ControlExpr {
        if self.remaining == 0 {
            return e.clone();
        }

        self.remaining -= 1;

        match e {
            ControlExpr::Read { key, previous } => self.read(key, *previous),

            ControlExpr::Active { .. } => self
                .state
                .and_then(|(machine, state)| machine.value_in_state(e, state))
                .unwrap_or_else(|| e.clone()),

            ControlExpr::Unary { operator, operand } => {
                let operand = self.expression(operand);

                if *operator == UnaryOp::Not {
                    operand.negated()
                } else {
                    ControlExpr::Unary {
                        operator: *operator,
                        operand: Box::new(operand),
                    }
                }
            }

            ControlExpr::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(left);

                if *operator == BinaryOp::And && left.truth() == Some(false)
                    || *operator == BinaryOp::Or && left.truth() == Some(true)
                {
                    return left;
                }

                ControlExpr::binary(*operator, left, self.expression(right))
            }

            ControlExpr::Choose { condition, yes, no } => {
                let condition = self.expression(condition);

                match condition.truth() {
                    Some(true) => self.expression(yes),
                    Some(false) => self.expression(no),

                    None => {
                        ControlExpr::choose(condition, self.expression(yes), self.expression(no))
                    }
                }
            }

            ControlExpr::Unsupported { text, operands } => ControlExpr::Unsupported {
                text: text.clone(),
                operands: operands.iter().map(|e| self.expression(e)).collect(),
            },

            _ => e.clone(),
        }
    }
}
