use super::*;

impl SymbolicAnalysis<'_> {
    pub(super) fn standard_values(&mut self, scope: &Scope) -> BTreeMap<String, ControlExpr> {
        let Some(kind) = scope.unit.intrinsic else {
            return BTreeMap::new();
        };

        use Intrinsic::*;

        let mut read = |name: &str, previous: bool| {
            let storage = self
                .storage(scope, &Expr::Reference { name: name.into() })
                .expect("standard port");
            let key = self.cell(storage);

            if previous {
                self.result
                    .cells
                    .get_mut(&key)
                    .expect("standard cell")
                    .invocation_memory = true;
            }

            ControlExpr::Read { key, previous }
        };

        let equations = match kind {
            RisingEdge | FallingEdge => {
                let clock = read("CLK", false);
                let old = read("CLK", true);

                vec![(
                    "Q",
                    if kind == RisingEdge {
                        ControlExpr::binary(BinaryOp::And, clock, old.negated())
                    } else {
                        ControlExpr::binary(BinaryOp::And, clock.negated(), old)
                    },
                )]
            }

            ResetDominant | SetDominant => {
                let set = read(if kind == SetDominant { "SET1" } else { "SET" }, false);
                let reset = read(
                    if kind == SetDominant {
                        "RESET"
                    } else {
                        "RESET1"
                    },
                    false,
                );
                let old = read("Q1", true);

                vec![(
                    "Q1",
                    if kind == SetDominant {
                        ControlExpr::binary(
                            BinaryOp::Or,
                            set,
                            ControlExpr::binary(BinaryOp::And, reset.negated(), old),
                        )
                    } else {
                        ControlExpr::binary(
                            BinaryOp::And,
                            reset.negated(),
                            ControlExpr::binary(BinaryOp::Or, set, old),
                        )
                    },
                )]
            }

            OnDelay | OffDelay | Pulse => {
                let input = read("IN", false);
                let elapsed = read("ET", false);
                let preset = read("PT", false);

                let value = match kind {
                    OnDelay => ControlExpr::binary(
                        BinaryOp::And,
                        input,
                        ControlExpr::binary(BinaryOp::GreaterEqual, elapsed, preset),
                    ),

                    OffDelay => ControlExpr::binary(
                        BinaryOp::Or,
                        input,
                        ControlExpr::binary(
                            BinaryOp::And,
                            read("Q", true),
                            ControlExpr::binary(BinaryOp::Less, elapsed, preset),
                        ),
                    ),

                    Pulse => {
                        let trigger =
                            ControlExpr::binary(BinaryOp::And, input, read("IN", true).negated());

                        ControlExpr::binary(
                            BinaryOp::And,
                            ControlExpr::binary(BinaryOp::Or, read("Q", true), trigger),
                            ControlExpr::binary(BinaryOp::Less, elapsed, preset),
                        )
                    }

                    _ => unreachable!(),
                };

                vec![("Q", value)]
            }

            CountUp => {
                let count = read("CV", true);
                let limit = read("PV", false);
                let edge = ControlExpr::binary(
                    BinaryOp::And,
                    read("CU", false),
                    read("CU", true).negated(),
                );
                let increment = ControlExpr::binary(
                    BinaryOp::And,
                    edge,
                    ControlExpr::binary(BinaryOp::Less, count.clone(), limit.clone()),
                );

                let next = ControlExpr::choose(
                    read("RESET", false),
                    ControlExpr::Literal { value: "0".into() },
                    ControlExpr::choose(
                        increment,
                        ControlExpr::binary(
                            BinaryOp::Add,
                            count.clone(),
                            ControlExpr::Literal { value: "1".into() },
                        ),
                        count,
                    ),
                );

                vec![
                    ("CV", next.clone()),
                    (
                        "Q",
                        ControlExpr::binary(BinaryOp::GreaterEqual, next, limit),
                    ),
                ]
            }
        };

        let mut values = BTreeMap::new();

        for (name, equation) in equations {
            let storage = self
                .storage(scope, &Expr::Reference { name: name.into() })
                .expect("standard output");
            values.insert(self.cell(storage), equation);
        }

        for &(input, output) in kind.dependencies() {
            let input = self
                .storage(scope, &Expr::Reference { name: input.into() })
                .expect("standard input");
            let input = self.cell(input);
            let output = self
                .storage(
                    scope,
                    &Expr::Reference {
                        name: output.into(),
                    },
                )
                .expect("standard output");
            let output = self.cell(output);
            self.record_dependencies(
                &output,
                &ControlExpr::Read {
                    key: input,
                    previous: false,
                },
            );
        }

        values
    }

    pub(super) fn standard(&mut self, scope: &Scope) {
        for (key, value) in self.standard_values(scope) {
            if !self.captured.contains(&key) {
                self.define(key, value);
            }
        }
    }
}
