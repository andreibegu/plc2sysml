mod lookup;
mod references;

use lookup::Lookup;
pub(crate) use references::{Symbols, instance_storage, parameters};

use crate::ir::*;
use std::collections::BTreeSet;

pub fn program(mut model: Program) -> anyhow::Result<Program> {
    let mut errors = vec![];
    structure(&mut model, &mut errors);
    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("\n"));
    }

    model.bindings = references::bind(&model);
    let errors = crate::ir::validate(&model);
    if !errors.is_empty() {
        anyhow::bail!("Invalid program: {}", errors.join("; "));
    }
    Ok(model)
}

struct Symbol {
    id: String,
    name: String,
    owner: Option<String>,
    category: Category,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Unit(UnitKind),
    Type,
}

struct Index {
    symbols: Vec<Symbol>,
}

impl Index {
    fn new(model: &Program) -> Self {
        let mut symbols: Vec<_> = model
            .units
            .iter()
            .map(|u| Symbol {
                id: u.id.clone(),
                name: u.name.clone(),
                owner: u.owner.clone(),
                category: Category::Unit(u.kind),
            })
            .collect();

        symbols.extend(model.types.iter().map(|t| Symbol {
            id: t.id.clone(),
            name: t.name.clone(),
            owner: None,
            category: Category::Type,
        }));

        Self { symbols }
    }

    fn lookup(&self, name: &str, accepts: impl Fn(Category) -> bool) -> Resolution {
        choose(
            self.symbols
                .iter()
                .filter(|s| {
                    s.owner.is_none() && s.name.eq_ignore_ascii_case(name) && accepts(s.category)
                })
                .map(|s| s.id.as_str()),
        )
    }
}

enum Resolution {
    Found(String),
    Missing,
    Ambiguous,
}

impl Resolution {
    fn target(self) -> Option<String> {
        match self {
            Self::Found(id) => Some(id),
            Self::Missing | Self::Ambiguous => None,
        }
    }
}

fn choose<'a>(mut candidates: impl Iterator<Item = &'a str>) -> Resolution {
    match (candidates.next(), candidates.next()) {
        (None, _) => Resolution::Missing,
        (Some(id), None) => Resolution::Found(id.into()),
        _ => Resolution::Ambiguous,
    }
}

fn structure(model: &mut Program, errors: &mut Vec<String>) {
    let index = Index::new(model);
    for unit in &mut model.units {
        for variable in &mut unit.variables {
            resolve_type(&index, &mut variable.ty);
        }

        if let Some(ty) = &mut unit.return_type {
            resolve_type(&index, ty);
        }

        unit.base_type = unit.extends.as_ref().and_then(|name| {
            index
                .lookup(name, |category| {
                    matches!(
                        category,
                        Category::Unit(UnitKind::FunctionBlock | UnitKind::Interface)
                    )
                })
                .target()
        });
    }

    for ty in &mut model.types {
        match &mut ty.kind {
            TypeKind::Alias(base) => resolve_type(&index, base),
            TypeKind::Struct(fields) => {
                for field in fields {
                    resolve_type(&index, &mut field.ty);
                }
            }
            TypeKind::Enum { base, .. } => {
                if let Some(base) = base {
                    resolve_type(&index, base);
                }
            }
        }
    }

    for task in &mut model.tasks {
        for entry in &mut task.programs {
            entry.target = index
                .lookup(&entry.name, |category| {
                    category == Category::Unit(UnitKind::Program)
                })
                .target();
        }
    }

    check_duplicates(model, errors);
}

fn resolve_type(index: &Index, ty: &mut TypeRef) {
    ty.target = if is_builtin_type(&ty.base) {
        None
    } else {
        index
            .lookup(&ty.base, |category| {
                matches!(
                    category,
                    Category::Type | Category::Unit(UnitKind::FunctionBlock | UnitKind::Interface)
                )
            })
            .target()
    };
}

pub fn is_builtin_type(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "BOOL"
            | "BIT"
            | "BYTE"
            | "WORD"
            | "DWORD"
            | "LWORD"
            | "SINT"
            | "USINT"
            | "INT"
            | "UINT"
            | "DINT"
            | "UDINT"
            | "LINT"
            | "ULINT"
            | "REAL"
            | "LREAL"
            | "STRING"
            | "WSTRING"
            | "TIME"
            | "LTIME"
            | "DATE"
            | "LDATE"
            | "TOD"
            | "TIME_OF_DAY"
            | "LTOD"
            | "DT"
            | "DATE_AND_TIME"
            | "LDT"
            | "CHAR"
            | "WCHAR"
            | "ANY"
            | "ANY_INT"
            | "ANY_NUM"
            | "ANY_REAL"
            | "ANY_BIT"
            | "ANY_STRING"
    )
}

pub(crate) fn builtin_call(name: &str) -> bool {
    let n = name.to_ascii_uppercase();

    n.split_once("_TO_")
        .is_some_and(|(from, to)| is_builtin_type(from) && is_builtin_type(to))
        || is_builtin_type(&n)
        || matches!(
            n.as_str(),
            "MOVE"
                | "ABS"
                | "AND"
                | "OR"
                | "XOR"
                | "NOT"
                | "EQ"
                | "NE"
                | "GE"
                | "GT"
                | "LE"
                | "LT"
                | "ADD"
                | "SUB"
                | "MUL"
                | "DIV"
                | "MOD"
                | "SQRT"
                | "LN"
                | "LOG"
                | "EXP"
                | "EXPT"
                | "SIN"
                | "COS"
                | "TAN"
                | "ASIN"
                | "ACOS"
                | "ATAN"
                | "ATAN2"
                | "MIN"
                | "MAX"
                | "LIMIT"
                | "SEL"
                | "MUX"
                | "SHL"
                | "SHR"
                | "ROL"
                | "ROR"
                | "LEN"
                | "LEFT"
                | "RIGHT"
                | "MID"
                | "CONCAT"
                | "INSERT"
                | "DELETE"
                | "REPLACE"
                | "FIND"
                | "TRUNC"
                | "ADR"
                | "SIZEOF"
                | "LOWER_BOUND"
                | "UPPER_BOUND"
        )
}

fn check_duplicates(model: &Program, errors: &mut Vec<String>) {
    let mut names = BTreeSet::new();

    for u in &model.units {
        let key = (u.owner.as_deref(), u.name.to_ascii_lowercase());

        if !names.insert(key) {
            errors.push(format!(
                "{}:{}: Duplicate unit {}",
                u.source.path, u.source.line, u.name
            ));
        }

        let mut vars = BTreeSet::new();

        for v in &u.variables {
            if !vars.insert(v.name.to_lowercase()) {
                errors.push(format!(
                    "{}:{}: Duplicate variable {}",
                    v.source.path, v.source.line, v.name
                ));
            }
        }
    }

    let mut types = BTreeSet::new();

    for t in &model.types {
        if !types.insert(t.name.to_ascii_lowercase()) {
            errors.push(format!(
                "{}:{}: Duplicate type {}",
                t.source.path, t.source.line, t.name
            ));
        }
    }
}

pub(crate) fn source_storage(
    model: &Program,
    unit: &Unit,
    expression: &Expr,
) -> Option<StorageRef> {
    Lookup::new(model).endpoint(unit, expression)
}
