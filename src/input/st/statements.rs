use super::*;

impl Parser<'_> {
    fn expression_until(&mut self, stops: &[&str]) -> Expr {
        let a = self.pos;
        self.scan_until(stops);
        self.expression(a, self.pos)
    }

    fn expression(&mut self, a: usize, b: usize) -> Expr {
        expr::parse(&self.tokens[a..b])
    }

    pub(super) fn body_model(&self, a: usize, b: usize, statements: Vec<Statement>) -> StModel {
        StModel {
            statements,
            source: self.source(a, b),
        }
    }

    fn labels_end(&self) -> Option<usize> {
        let mut i = self.pos;
        let mut expect_value = true;
        let mut seen = false;

        while i < self.tokens.len() {
            let t = &self.tokens[i];

            if t.is(":") {
                return if seen && !expect_value { Some(i) } else { None };
            }

            if t.is(";") || t.is(":=") || t.is("(") || t.is("THEN") || t.kind == TokenKind::Eof {
                return None;
            }

            if t.is(",") || t.is("..") {
                if expect_value {
                    return None;
                }

                expect_value = true;
            } else if t.is(".") || t.is("-") || t.is("+") {
            } else if t.kind == TokenKind::Word || t.kind == TokenKind::Literal {
                seen = true;
                expect_value = false;
            } else {
                return None;
            }

            i += 1;
        }

        None
    }

    pub(super) fn sequence(
        &mut self,
        stops: &[&str],
        case_arm: bool,
        depth: usize,
    ) -> Vec<Statement> {
        let mut result = vec![];

        while !(self.eof() || self.at_any(stops) || case_arm && self.labels_end().is_some()) {
            let a = self.pos;

            if depth >= 64 {
                self.scan_until(&[";"]);
                self.eat(";");
                source_error(
                    self.ds,
                    "Statement nesting exceeds 64",
                    self.source(a, self.pos),
                );
                result.push(self.stmt(
                    a,
                    StatementKind::Unknown {
                        operands: super::unknown_operands(&self.raw(a, self.pos)),
                    },
                ));
                continue;
            }

            let kind = if self.eat("IF") {
                let condition = self.expression_until(&["THEN"]);
                self.expect("THEN");

                let body = self.sequence(&["ELSIF", "ELSE", "END_IF"], false, depth + 1);
                let mut branches = vec![Branch {
                    test: condition,
                    body,
                }];

                while self.eat("ELSIF") {
                    let condition = self.expression_until(&["THEN"]);
                    self.expect("THEN");
                    let body = self.sequence(&["ELSIF", "ELSE", "END_IF"], false, depth + 1);
                    branches.push(Branch {
                        test: condition,
                        body,
                    });
                }

                let otherwise = if self.eat("ELSE") {
                    self.sequence(&["END_IF"], false, depth + 1)
                } else {
                    vec![]
                };

                self.expect("END_IF");
                self.eat(";");

                StatementKind::If {
                    branches,
                    otherwise,
                }
            } else if self.eat("CASE") {
                let selector = self.expression_until(&["OF"]);
                self.expect("OF");

                let mut arms = vec![];

                while !self.eof() && !self.at_any(&["ELSE", "END_CASE"]) {
                    let Some(end) = self.labels_end() else {
                        self.error("Expected CASE labels");
                        self.pos += 1;
                        continue;
                    };

                    let labels = self.tokens[self.pos..end]
                        .split(|t| t.is(","))
                        .map(|ts| {
                            if let Some(range) = ts.iter().position(|t| t.is("..")) {
                                CaseLabel::Range {
                                    lower: expr::parse(&ts[..range]),
                                    upper: expr::parse(&ts[range + 1..]),
                                }
                            } else {
                                CaseLabel::Value(expr::parse(ts))
                            }
                        })
                        .collect();

                    self.pos = end + 1;

                    let body = self.sequence(&["ELSE", "END_CASE"], true, depth + 1);
                    arms.push(Branch { test: labels, body });
                }

                let otherwise = if self.eat("ELSE") {
                    self.sequence(&["END_CASE"], false, depth + 1)
                } else {
                    vec![]
                };

                self.expect("END_CASE");
                self.eat(";");

                StatementKind::Case {
                    selector,
                    arms,
                    otherwise,
                }
            } else if self.eat("FOR") {
                let variable = self.word().unwrap_or_default();
                self.expect(":=");

                let start = self.expression_until(&["TO"]);
                self.expect("TO");

                let end = self.expression_until(&["BY", "DO"]);

                let step = if self.eat("BY") {
                    Some(self.expression_until(&["DO"]))
                } else {
                    None
                };

                self.expect("DO");

                let body = self.sequence(&["END_FOR"], false, depth + 1);
                self.expect("END_FOR");
                self.eat(";");

                StatementKind::Loop {
                    control: LoopControl::Range {
                        variable: Expr::Reference { name: variable },
                        start,
                        end,
                        step,
                    },
                    body,
                }
            } else if self.eat("WHILE") {
                let condition = self.expression_until(&["DO"]);
                self.expect("DO");
                let body = self.sequence(&["END_WHILE"], false, depth + 1);
                self.expect("END_WHILE");
                self.eat(";");

                StatementKind::Loop {
                    control: LoopControl::Condition {
                        condition,
                        test: LoopTest::Before,
                    },
                    body,
                }
            } else if self.eat("REPEAT") {
                let body = self.sequence(&["UNTIL"], false, depth + 1);
                self.expect("UNTIL");
                let condition = self.expression_until(&["END_REPEAT"]);
                self.expect("END_REPEAT");
                self.eat(";");

                StatementKind::Loop {
                    control: LoopControl::Condition {
                        condition,
                        test: LoopTest::After,
                    },
                    body,
                }
            } else if self.eat("RETURN") {
                self.expect(";");
                StatementKind::Return
            } else if self.eat("EXIT") {
                self.expect(";");
                StatementKind::Exit
            } else if self.eat("CONTINUE") {
                self.expect(";");
                StatementKind::Continue
            } else if self.eat(";") {
                continue;
            } else {
                self.scan_until(&[
                    ";",
                    "END_IF",
                    "ELSIF",
                    "ELSE",
                    "END_CASE",
                    "END_FOR",
                    "END_WHILE",
                    "UNTIL",
                    "END_REPEAT",
                ]);

                let b = self.pos;
                self.expect(";");

                let mut nesting = 0;
                let assignment = self.tokens[a..b].iter().position(|token| {
                    if token.is("(") || token.is("[") {
                        nesting += 1;
                    }

                    if token.is(")") || token.is("]") {
                        nesting -= 1;
                    }

                    nesting == 0 && token.is(":=")
                });

                if let Some(n) = assignment {
                    let target = self.expression(a, a + n);
                    let value = self.expression(a + n + 1, b);
                    StatementKind::Assign { target, value }
                } else {
                    let expression = self.expression(a, b);

                    if matches!(expression, Expr::Call { .. }) {
                        StatementKind::Call { expression }
                    } else {
                        StatementKind::Unknown {
                            operands: super::unknown_operands(&self.raw(a, self.pos)),
                        }
                    }
                }
            };

            if self.pos == a {
                self.pos += 1;
            }

            result.push(self.stmt(a, kind));
        }

        result
    }

    fn stmt(&self, a: usize, kind: StatementKind) -> Statement {
        Statement {
            id: id(&[
                "statement",
                &self.doc.path,
                self.doc.origin.as_deref().unwrap_or(""),
                &self.tokens[a].start.to_string(),
            ]),
            source: self.source(a, self.pos),
            kind,
        }
    }
}
