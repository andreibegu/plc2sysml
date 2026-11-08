use crate::{
    input::{SourceError, source_error},
    ir::Source,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Literal,
    Symbol,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn is(&self, s: &str) -> bool {
        self.text.eq_ignore_ascii_case(s)
    }
}

pub fn lex(text: &str, path: &str, ds: &mut Vec<SourceError>) -> Vec<Token> {
    let mut tokens = vec![];
    let mut i = 0;

    while i < text.len() {
        let c = text[i..].chars().next().unwrap();

        if c.is_whitespace() || c == '\u{feff}' {
            i += c.len_utf8();
            continue;
        }

        let start = i;

        if text[i..].starts_with("//") {
            i = text[i..].find('\n').map(|n| i + n).unwrap_or(text.len());
            continue;
        }

        if text[i..].starts_with("(*") {
            i += 2;

            let mut depth = 1;

            while i < text.len() && depth > 0 {
                if text[i..].starts_with("(*") {
                    depth += 1;
                    i += 2;
                } else if text[i..].starts_with("*)") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += text[i..].chars().next().unwrap().len_utf8();
                }
            }

            if depth > 0 {
                source_error(
                    ds,
                    "Unclosed block comment",
                    Source::at(path, text, start, i),
                );
            }

            continue;
        }

        if c == '{' {
            let mut quote = None;
            i += 1;

            while i < text.len() {
                let ch = text[i..].chars().next().unwrap();
                i += ch.len_utf8();

                if ch == '$' && quote.is_some() && i < text.len() {
                    i += text[i..].chars().next().unwrap().len_utf8();
                    continue;
                }

                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() && (ch == '\'' || ch == '"') {
                    quote = Some(ch);
                } else if ch == '}' && quote.is_none() {
                    break;
                }
            }

            if !text[start..i].ends_with('}') {
                source_error(
                    ds,
                    "Unclosed CODESYS attribute",
                    Source::at(path, text, start, i),
                );
            }

            continue;
        }

        let kind;

        if c == '\'' || c == '"' {
            kind = TokenKind::Literal;

            let (end, closed) = quoted_end(text, i);
            i = end;

            if !closed {
                source_error(
                    ds,
                    "Unclosed string literal",
                    Source::at(path, text, start, i),
                );
            }
        } else if c.is_alphabetic() || c == '_' {
            kind = TokenKind::Word;
            i += c.len_utf8();

            while i < text.len() {
                let x = text[i..].chars().next().unwrap();

                if x.is_alphanumeric() || x == '_' {
                    i += x.len_utf8();
                } else {
                    break;
                }
            }

            if text[i..].starts_with('#') {
                i += 1;

                if text[i..].starts_with(['\'', '"']) {
                    let (end, closed) = quoted_end(text, i);
                    i = end;

                    if !closed {
                        source_error(
                            ds,
                            "Unclosed typed string literal",
                            Source::at(path, text, start, i),
                        );
                    }
                } else {
                    let prefix = text[start..i - 1].to_ascii_uppercase();
                    let date = matches!(
                        prefix.as_str(),
                        "D" | "DATE" | "LDATE" | "DT" | "DATE_AND_TIME" | "LDT" | "LDATE_AND_TIME"
                    );
                    let temporal = date
                        || matches!(
                            prefix.as_str(),
                            "T" | "TIME"
                                | "LTIME"
                                | "TOD"
                                | "TIME_OF_DAY"
                                | "LTOD"
                                | "LTIME_OF_DAY"
                        );

                    i = number_end(text, i, temporal, date);
                }

                tokens.push(Token {
                    kind: TokenKind::Literal,
                    text: text[start..i].into(),
                    start,
                    end: i,
                });
                continue;
            }
        } else if c.is_ascii_digit() {
            kind = TokenKind::Literal;
            i = number_end(text, i, false, false);
        } else {
            kind = TokenKind::Symbol;

            if [":=", "=>", "..", "<=", ">=", "<>", "**", "?=", "S=", "R="]
                .iter()
                .any(|x| text[i..].starts_with(x))
            {
                i += 2;
            } else {
                i += c.len_utf8();
            }
        }

        tokens.push(Token {
            kind,
            text: text[start..i].into(),
            start,
            end: i,
        });
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        text: String::new(),
        start: i,
        end: i,
    });

    tokens
}

fn quoted_end(text: &str, start: usize) -> (usize, bool) {
    let delimiter = text.as_bytes()[start];
    let mut i = start + 1;

    while i < text.len() {
        let c = text[i..].chars().next().expect("character boundary");
        i += c.len_utf8();

        if c == '$' && i < text.len() {
            i += text[i..]
                .chars()
                .next()
                .expect("character boundary")
                .len_utf8();
        } else if c == char::from(delimiter) {
            if text.as_bytes().get(i) == Some(&delimiter) {
                i += 1;
            } else {
                return (i, true);
            }
        }
    }

    (i, false)
}

fn number_end(text: &str, start: usize, temporal: bool, date: bool) -> usize {
    let mut i = start;

    while let Some(&c) = text.as_bytes().get(i) {
        let sign = matches!(c, b'+' | b'-')
            && (i == start
                || date && c == b'-'
                || !text[start..i].contains('#')
                    && matches!(text.as_bytes().get(i.saturating_sub(1)), Some(b'e' | b'E')));

        if c.is_ascii_alphanumeric()
            || matches!(c, b'_' | b'#')
            || c == b'.' && !text[i..].starts_with("..")
            || temporal && c == b':'
            || sign
        {
            i += 1;
        } else {
            break;
        }
    }

    i
}
