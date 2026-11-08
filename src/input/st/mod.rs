use crate::input::{SourceError, source_error};
mod declarations;
pub(crate) mod expressions;
pub(crate) mod lexer;
mod statements;

use crate::ir::*;
pub use declarations::type_ref;
use expressions as expr;
use lexer::{Token, TokenKind, lex};

pub fn parse_document(
    doc: &Document,
    model: &mut Program,
    errors: &mut Vec<SourceError>,
) -> Vec<Unit> {
    let mut p = Parser::new(doc, errors);
    let mut units = vec![];

    while !p.eof() {
        if p.at("TYPE") {
            p.pos += 1;

            while !p.eof() && !p.at("END_TYPE") {
                if let Some(t) = p.type_def() {
                    model.types.push(t);
                } else {
                    if !p.eof() {
                        p.pos += 1;
                    }
                }
            }

            p.expect("END_TYPE");
            p.eat(";");
            continue;
        }

        let start = p.pos;

        let kind = if p.eat("PROGRAM") {
            Some(UnitKind::Program)
        } else if p.eat("FUNCTION_BLOCK") {
            Some(UnitKind::FunctionBlock)
        } else if p.eat("FUNCTION") {
            Some(UnitKind::Function)
        } else if p.eat("METHOD") {
            Some(UnitKind::Method)
        } else if p.eat("ACTION") {
            Some(UnitKind::Action)
        } else if p.eat("INTERFACE") {
            Some(UnitKind::Interface)
        } else if p.eat("PROPERTY") {
            Some(UnitKind::Property)
        } else if p.at("VAR_GLOBAL") {
            Some(UnitKind::GlobalVariables)
        } else {
            None
        };

        if let Some(kind) = kind {
            let name = if kind == UnitKind::GlobalVariables {
                filename_name(&doc.path)
            } else {
                while p.at_any(&[
                    "PUBLIC",
                    "PRIVATE",
                    "PROTECTED",
                    "INTERNAL",
                    "FINAL",
                    "ABSTRACT",
                ]) {
                    p.pos += 1;
                }

                p.word().unwrap_or_else(|| {
                    p.error("Missing declaration name");
                    filename_name(&doc.path)
                })
            };

            let mut unit = Unit::new(&name, kind, p.source(start, p.pos));

            if p.eat(":") {
                let from = p.pos;

                while !p.eof()
                    && !p.is_var()
                    && !p.at_any(&["EXTENDS", "IMPLEMENTS"])
                    && p.tokens[p.pos].start < p.line_end(start)
                {
                    p.pos += 1;
                }

                unit.return_type = Some(type_ref(&p.raw(from, p.pos)));
            }

            if p.eat("EXTENDS") {
                unit.extends = p.qualified_word();
            }

            if p.eat("IMPLEMENTS") {
                loop {
                    if let Some(n) = p.qualified_word() {
                        unit.implements.push(n);
                    }

                    if !p.eat(",") {
                        break;
                    }
                }
            }

            while p.is_var() {
                let vars = p.variables(&unit.id);
                unit.variables.extend(vars);
            }

            let body_start = p.pos;
            let body = p.sequence(
                &[
                    "END_PROGRAM",
                    "END_FUNCTION_BLOCK",
                    "END_FUNCTION",
                    "END_METHOD",
                    "END_ACTION",
                    "END_INTERFACE",
                    "END_PROPERTY",
                    "PROGRAM",
                    "FUNCTION_BLOCK",
                    "FUNCTION",
                    "METHOD",
                    "PROPERTY",
                    "INTERFACE",
                    "TYPE",
                ],
                false,
                0,
            );

            if p.pos > body_start {
                unit.bodies.push(
                    p.body_model(body_start, p.pos, body)
                        .lower(&unit.id, "implementation"),
                );
            }

            if p.at_any(&[
                "END_PROGRAM",
                "END_FUNCTION_BLOCK",
                "END_FUNCTION",
                "END_METHOD",
                "END_ACTION",
                "END_INTERFACE",
                "END_PROPERTY",
            ]) {
                p.pos += 1;
                p.eat(";");
            }

            units.push(unit);
        } else {
            let name = filename_name(&doc.path);
            let mut unit = Unit::new(&name, UnitKind::Action, p.source(start, start + 1));

            let stmts = p.sequence(&[], false, 0);
            unit.bodies.push(
                p.body_model(start, p.pos, stmts)
                    .lower(&unit.id, "implementation"),
            );
            units.push(unit);
        }

        if p.pos == start {
            p.pos += 1;
        }
    }

    units
}

pub fn read_body(doc: &Document, ds: &mut Vec<SourceError>) -> StModel {
    let mut p = Parser::new(doc, ds);
    let stmts = p.sequence(&[], false, 0);
    p.body_model(0, p.pos, stmts)
}

pub fn filename_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let name = name
        .strip_suffix(".st")
        .or_else(|| name.strip_suffix(".ST"))
        .unwrap_or(name);

    name.rsplit('.').next().unwrap_or(name).into()
}

pub(super) fn owner_hint(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;

    if let Some((owner, _)) = file.trim_end_matches(".st").split_once('.') {
        return Some(owner.into());
    }

    path.rsplit_once('/')
        .and_then(|(p, _)| p.rsplit('/').next())
        .map(String::from)
}

struct Parser<'a> {
    doc: &'a Document,
    tokens: Vec<Token>,
    pos: usize,
    ds: &'a mut Vec<SourceError>,
}

impl<'a> Parser<'a> {
    fn new(doc: &'a Document, ds: &'a mut Vec<SourceError>) -> Self {
        let n = ds.len();
        let tokens = lex(&doc.text, &doc.path, ds);

        for d in &mut ds[n..] {
            if doc.origin.is_some() {
                d.source.logical_line = Some(d.source.line);
                d.source.origin = doc.origin.clone();
                d.source.line = doc
                    .physical_lines
                    .get(d.source.line - 1)
                    .copied()
                    .unwrap_or(d.source.line);
            }
        }

        Self {
            doc,
            tokens,
            pos: 0,
            ds,
        }
    }

    fn eof(&self) -> bool {
        self.tokens
            .get(self.pos)
            .is_none_or(|token| token.kind == TokenKind::Eof)
    }

    fn at(&self, s: &str) -> bool {
        self.tokens.get(self.pos).is_some_and(|token| token.is(s))
    }

    fn at_any(&self, ss: &[&str]) -> bool {
        ss.iter().any(|s| self.at(s))
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.at(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> Option<String> {
        if self
            .tokens
            .get(self.pos)
            .is_some_and(|token| token.kind == TokenKind::Word)
        {
            let s = self.tokens[self.pos].text.clone();
            self.pos += 1;
            Some(s)
        } else {
            None
        }
    }

    fn qualified_word(&mut self) -> Option<String> {
        let mut s = self.word()?;

        while self.eat(".") {
            s.push('.');
            s.push_str(&self.word()?);
        }

        Some(s)
    }

    fn raw(&self, a: usize, b: usize) -> String {
        if a >= b {
            return String::new();
        }

        self.doc.text[self.tokens[a].start..self.tokens[b - 1].end].into()
    }

    fn source(&self, a: usize, b: usize) -> Source {
        self.doc.source(
            self.tokens[a.min(self.tokens.len() - 1)].start,
            if b > a {
                self.tokens[(b - 1).min(self.tokens.len() - 1)].end
            } else {
                self.tokens[a.min(self.tokens.len() - 1)].end
            },
        )
    }

    fn error(&mut self, message: &str) {
        source_error(self.ds, message, self.source(self.pos, self.pos + 1));
    }

    fn expect(&mut self, s: &str) {
        if !self.eat(s) {
            self.error(&format!("Expected {s}"));
        }
    }

    fn line_end(&self, start: usize) -> usize {
        let a = self.tokens[start].start;
        self.doc.text[a..]
            .find('\n')
            .map(|n| a + n)
            .unwrap_or(self.doc.text.len())
    }

    fn is_var(&self) -> bool {
        self.at_any(&[
            "VAR",
            "VAR_INPUT",
            "VAR_OUTPUT",
            "VAR_IN_OUT",
            "VAR_GLOBAL",
            "VAR_TEMP",
            "VAR_STAT",
            "VAR_INST",
            "VAR_EXTERNAL",
            "VAR_CONFIG",
            "VAR_ACCESS",
        ])
    }

    fn scan_until(&mut self, stops: &[&str]) {
        let mut depth = 0i32;

        while !self.eof() {
            if depth == 0 && self.at_any(stops) {
                break;
            }

            if self.at_any(&["(", "["]) {
                depth += 1;
            }

            if self.at_any(&[")", "]"]) {
                depth -= 1;
            }

            self.pos += 1;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    pub path: String,
    pub text: String,
    pub origin: Option<String>,
    pub physical_lines: Vec<usize>,
}

impl Document {
    pub fn plain(path: &str, text: &str) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
            origin: None,
            physical_lines: vec![],
        }
    }

    pub fn source(&self, start: usize, end: usize) -> Source {
        let mut s = Source::at(&self.path, &self.text, start, end);

        if self.origin.is_some() {
            s.logical_line = Some(s.line);
            s.origin = self.origin.clone();
            s.line = self
                .physical_lines
                .get(s.line - 1)
                .copied()
                .unwrap_or(s.line);
            s.end_line = self
                .physical_lines
                .get(s.end_line - 1)
                .copied()
                .unwrap_or(s.line);
        }

        s
    }
}

#[derive(Debug)]
pub struct StModel {
    pub statements: Vec<Statement>,
    pub source: Source,
}

impl StModel {
    pub fn lower(self, unit: &str, name: &str) -> Body {
        Body {
            id: id(&["behavior", unit, name]),
            name: name.into(),
            statements: self.statements,
            execution: Execution::Sequential,
            source: self.source,
        }
    }
}

pub(super) fn unknown(text: impl AsRef<str>) -> Expr {
    Expr::Unknown {
        operands: unknown_operands(text.as_ref()),
    }
}

pub(super) fn unknown_operands(text: &str) -> Vec<Expr> {
    let tokens = lex(text, "", &mut vec![]);
    let mut names = std::collections::BTreeSet::new();
    for (i, token) in tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TokenKind::Word)
    {
        let mut name = token.text.clone();
        let mut next = i + 1;
        while tokens.get(next).is_some_and(|t| t.is("."))
            && tokens
                .get(next + 1)
                .is_some_and(|t| t.kind == TokenKind::Word)
        {
            name.push('.');
            name.push_str(&tokens[next + 1].text);
            next += 2;
        }
        names.insert(name);
    }
    names
        .into_iter()
        .map(|name| Expr::Reference { name })
        .collect()
}
