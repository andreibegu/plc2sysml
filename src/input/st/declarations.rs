use super::*;

impl Parser<'_> {
    pub(super) fn variables(&mut self, owner: &str) -> Vec<Variable> {
        let kind = match self.tokens[self.pos].text.to_ascii_uppercase().as_str() {
            "VAR" => VariableKind::Local,
            "VAR_INPUT" => VariableKind::Input,
            "VAR_OUTPUT" => VariableKind::Output,
            "VAR_IN_OUT" => VariableKind::InOut,
            "VAR_GLOBAL" => VariableKind::Global,
            "VAR_TEMP" => VariableKind::Temporary,
            "VAR_STAT" => VariableKind::Static,
            "VAR_INST" => VariableKind::Instance,
            "VAR_EXTERNAL" => VariableKind::External,
            "VAR_CONFIG" => VariableKind::Configuration,
            "VAR_ACCESS" => VariableKind::Access,
            _ => unreachable!("variable section checked by parser"),
        };
        self.pos += 1;

        let mut constant = false;
        let mut retention = Retention::Automatic;

        while self.at_any(&["CONSTANT", "RETAIN", "PERSISTENT", "NON_RETAIN"]) {
            match self.tokens[self.pos].text.to_ascii_uppercase().as_str() {
                "CONSTANT" => constant = true,
                "RETAIN" => retention = Retention::Retained,
                "PERSISTENT" => retention = Retention::Persistent,
                "NON_RETAIN" => retention = Retention::NonRetained,
                _ => unreachable!(),
            }
            self.pos += 1;
        }

        let mut vars = vec![];

        while !self.eof() && !self.at("END_VAR") {
            let a = self.pos;
            self.scan_until(&[";", "END_VAR"]);

            let b = self.pos;

            if !self.eat(";") {
                self.error("Variable declaration is missing a semicolon");
            }

            vars.extend(self.declaration(owner, kind, constant, retention, a, b));

            if self.pos == a {
                self.pos += 1;
            }
        }

        self.expect("END_VAR");

        vars
    }

    fn declaration(
        &mut self,
        owner: &str,
        kind: VariableKind,
        constant: bool,
        retention: Retention,
        a: usize,
        b: usize,
    ) -> Vec<Variable> {
        if a == b {
            return vec![];
        }

        let ts = &self.tokens[a..b];

        let Some(colon) = ts.iter().position(|t| t.is(":")) else {
            source_error(
                self.ds,
                "Cannot parse declaration; source is preserved in its source file",
                self.source(a, b),
            );
            return vec![];
        };

        let at = ts[..colon].iter().position(|t| t.is("AT"));
        let names_end = at.unwrap_or(colon);
        let address = at.map(|n| {
            self.raw(a + n + 1, a + colon)
                .split_whitespace()
                .collect::<String>()
        });
        let mut depth = 0;
        let mut init = None;

        for (n, t) in ts.iter().enumerate().skip(colon + 1) {
            if t.is("(") || t.is("[") {
                depth += 1;
            }

            if t.is(")") || t.is("]") {
                depth -= 1;
            }

            if depth == 0 && t.is(":=") {
                init = Some(n);
                break;
            }
        }

        let ty = type_ref(&self.raw(a + colon + 1, a + init.unwrap_or(ts.len())));
        let initial = init.map(|n| expr::parse(&self.tokens[a + n + 1..b]));
        let source = self.source(a, b);
        let mut result = vec![];

        for t in &ts[..names_end] {
            if t.is(",") {
                continue;
            }

            if t.kind != TokenKind::Word {
                source_error(self.ds, "Unsupported variable declarator", source.clone());
                continue;
            }

            result.push(Variable {
                id: id(&["variable", owner, &t.text.to_lowercase()]),
                name: t.text.clone(),
                kind,
                constant,
                retention,
                ty: ty.clone(),
                initial: initial.clone(),
                address: address.clone(),
                source: source.clone(),
            });
        }

        result
    }

    pub(super) fn type_def(&mut self) -> Option<TypeDef> {
        let a = self.pos;
        let name = self.word()?;

        if !self.eat(":") {
            self.error("Expected colon after type name");
            return None;
        }

        let id = id(&["type", &self.doc.path, &name]);
        let kind = if self.eat("STRUCT") {
            let mut fields = vec![];
            while !self.eof() && !self.at_any(&["END_STRUCT", "END_TYPE"]) {
                let start = self.pos;
                self.scan_until(&[";", "END_STRUCT", "END_TYPE"]);
                let end = self.pos;
                self.eat(";");
                fields.extend(self.declaration(
                    &id,
                    VariableKind::Field,
                    false,
                    Retention::Automatic,
                    start,
                    end,
                ));
                if self.pos == start {
                    break;
                }
            }
            self.expect("END_STRUCT");
            self.eat(";");
            TypeKind::Struct(fields)
        } else if self.eat("(") {
            let mut literals = vec![];
            while !self.eof() && !self.at(")") {
                let start = self.pos;
                let name = self.word()?;
                let value = if self.eat(":=") {
                    let begin = self.pos;
                    self.scan_until(&[",", ")"]);
                    Some(self.raw(begin, self.pos))
                } else {
                    None
                };
                literals.push(EnumValue {
                    name,
                    value,
                    source: self.source(start, self.pos),
                });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")");
            let base = if self.eat(":") {
                let begin = self.pos;
                self.scan_until(&[";", "END_TYPE"]);
                Some(type_ref(&self.raw(begin, self.pos)))
            } else {
                None
            };
            self.eat(";");
            TypeKind::Enum { base, literals }
        } else {
            let begin = self.pos;
            self.scan_until(&[";", "END_TYPE"]);
            let base = type_ref(&self.raw(begin, self.pos));
            self.eat(";");
            TypeKind::Alias(base)
        };
        let ty = TypeDef {
            id,
            name,
            kind,
            source: self.source(a, self.pos),
        };

        Some(ty)
    }
}

pub fn type_ref(raw: &str) -> TypeRef {
    let tokens = lex(raw, "", &mut vec![]);
    let ts = &tokens[..tokens.len() - 1];
    let mut i = 0;
    let mut ty = TypeRef::default();

    while i < ts.len() {
        if ts[i].is("ARRAY") {
            i += 1;

            if i < ts.len() && ts[i].is("[") {
                i += 1;

                let mut a = i;

                while i < ts.len() && !ts[i].is("]") {
                    if ts[i].is(",") {
                        add_bound(&ts[a..i], &mut ty);
                        a = i + 1;
                    }

                    i += 1;
                }

                add_bound(&ts[a..i], &mut ty);
                i += usize::from(i < ts.len());
            }

            if i < ts.len() && ts[i].is("OF") {
                i += 1;
            }
        } else if ts[i].is("POINTER") || ts[i].is("REFERENCE") {
            ty.indirection = Some(if ts[i].is("POINTER") {
                Indirection::Pointer
            } else {
                Indirection::Reference
            });
            i += 1;

            if i < ts.len() && (ts[i].is("TO") || ts[i].is("OF")) {
                i += 1;
            }
        } else {
            break;
        }
    }

    let begin = i;

    if i < ts.len() && ts[i].kind == TokenKind::Word {
        i += 1;

        while i + 1 < ts.len() && ts[i].is(".") && ts[i + 1].kind == TokenKind::Word {
            i += 2;
        }
    }

    ty.base = ts[begin..i]
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("");

    if ty.base.is_empty() {
        ty.base = "UNKNOWN".into();
    }

    if i < ts.len() {
        ty.subrange = raw[ts[i].start..]
            .trim()
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .and_then(|s| s.split_once(".."))
            .and_then(|(a, b)| {
                Some(IntegerRange {
                    lower: a.trim().parse().ok()?,
                    upper: b.trim().parse().ok()?,
                })
            });
    }

    ty
}

fn add_bound(ts: &[Token], ty: &mut TypeRef) {
    if ts.is_empty() {
        return;
    }

    let n = ts.iter().position(|t| t.is(".."));
    let (lower, upper) = if let Some(n) = n {
        (expr::parse(&ts[..n]), expr::parse(&ts[n + 1..]))
    } else {
        (Expr::Literal { value: "0".into() }, expr::parse(ts))
    };

    ty.dimensions.push(ArrayBound { lower, upper });
}
