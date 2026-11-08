use super::{Body, Expr, id};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub units: Vec<Unit>,
    pub types: Vec<TypeDef>,
    pub tasks: Vec<Task>,
    pub bindings: Bindings,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,
    pub cyclic: bool,
    pub programs: Vec<TaskEntry>,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub name: String,
    pub target: Option<String>,
}

impl Program {
    pub fn variable(&self, storage: &StorageRef) -> Option<&Variable> {
        let root = self.units.iter().find(|u| u.id == storage.unit)?;
        let mut variable = root.variable(storage.path.first()?)?;

        for member in &storage.path[1..] {
            let ty = self.effective_type(&variable.ty)?;
            let target = ty.target.as_ref()?;
            variable = if let Some(unit) = self.units.iter().find(|u| &u.id == target) {
                unit.variable(member)?
            } else {
                self.types
                    .iter()
                    .find(|t| &t.id == target)?
                    .fields()
                    .iter()
                    .find(|v| v.name.eq_ignore_ascii_case(member))?
            };
        }

        Some(variable)
    }

    pub fn effective_type<'t>(&'t self, ty: &'t TypeRef) -> Option<&'t TypeRef> {
        let mut current = ty;
        let mut seen = BTreeSet::new();
        while let Some(target) = &current.target {
            if !seen.insert(target) {
                return None;
            }
            let Some(alias) = self
                .types
                .iter()
                .find(|t| &t.id == target && matches!(t.kind, TypeKind::Alias(_)))
            else {
                break;
            };
            if !current.dimensions.is_empty() || current.indirection.is_some() {
                break;
            }
            current = alias.base()?;
        }
        Some(current)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Unknown,
    Program,
    FunctionBlock,
    Function,
    GlobalVariables,
    Method,
    Action,
    Interface,
    Property,
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub intrinsic: Option<Intrinsic>,
    pub id: String,
    pub name: String,
    pub kind: UnitKind,
    pub owner: Option<String>,
    pub extends: Option<String>,
    pub base_type: Option<String>,
    pub implements: Vec<String>,
    pub return_type: Option<TypeRef>,
    pub variables: Vec<Variable>,
    pub bodies: Vec<Body>,
    pub source: Source,
}

impl Unit {
    pub fn variable(&self, name: &str) -> Option<&Variable> {
        self.variables
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(name))
    }

    pub fn new(name: &str, kind: UnitKind, source: Source) -> Self {
        Self {
            intrinsic: None,
            id: id(&[
                "unit",
                &source.path,
                source.origin.as_deref().unwrap_or(""),
                name,
            ]),
            name: name.into(),
            kind,
            owner: None,
            extends: None,
            base_type: None,
            implements: vec![],
            return_type: None,
            variables: vec![],
            bodies: vec![],
            source,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TypeKind {
    Alias(TypeRef),
    Struct(Vec<Variable>),
    Enum {
        base: Option<TypeRef>,
        literals: Vec<EnumValue>,
    },
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub id: String,
    pub name: String,
    pub kind: TypeKind,
    pub source: Source,
}

impl TypeDef {
    pub fn base(&self) -> Option<&TypeRef> {
        match &self.kind {
            TypeKind::Alias(base) => Some(base),
            TypeKind::Enum { base, .. } => base.as_ref(),
            TypeKind::Struct(_) => None,
        }
    }

    pub fn fields(&self) -> &[Variable] {
        match &self.kind {
            TypeKind::Struct(fields) => fields,
            _ => &[],
        }
    }

    pub fn literals(&self) -> &[EnumValue] {
        match &self.kind {
            TypeKind::Enum { literals, .. } => literals,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub name: String,
    pub value: Option<String>,
    pub source: Source,
}

#[derive(Debug, Clone, Default)]
pub struct TypeRef {
    pub base: String,
    pub dimensions: Vec<ArrayBound>,
    pub indirection: Option<Indirection>,
    pub subrange: Option<IntegerRange>,
    pub target: Option<String>,
}

impl TypeRef {
    pub fn same_type(&self, other: &Self) -> bool {
        let base_matches = match (&self.target, &other.target) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.base.eq_ignore_ascii_case(&other.base),
            _ => false,
        };
        base_matches
            && self.dimensions == other.dimensions
            && self.indirection == other.indirection
            && self.subrange == other.subrange
    }

    pub fn duration_bounds(&self) -> Option<(u64, u64)> {
        if self.indirection.is_some() || !self.dimensions.is_empty() {
            return None;
        }

        match self.base.to_ascii_uppercase().as_str() {
            "TIME" => Some((0, u32::MAX as u64 * 1_000_000)),
            "LTIME" => Some((0, u64::MAX)),
            _ => None,
        }
    }

    pub fn scalar(name: impl Into<String>) -> Self {
        Self {
            base: name.into(),
            ..Self::default()
        }
    }

    pub fn integer_bounds(&self) -> Option<(i128, i128)> {
        if self.indirection.is_some() || !self.dimensions.is_empty() {
            return None;
        }

        let (bits, signed) = match self.base.to_ascii_uppercase().as_str() {
            "SINT" => (8, true),
            "INT" => (16, true),
            "DINT" => (32, true),
            "LINT" => (64, true),
            "BYTE" | "USINT" => (8, false),
            "WORD" | "UINT" => (16, false),
            "DWORD" | "UDINT" => (32, false),
            "LWORD" | "ULINT" => (64, false),
            _ => return None,
        };

        let (mut min, mut max) = if signed {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (0, (1i128 << bits) - 1)
        };

        if let Some(range) = &self.subrange
            && range.lower <= range.upper
            && range.lower >= min
            && range.upper <= max
        {
            min = range.lower;
            max = range.upper;
        }

        Some((min, max))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArrayBound {
    pub lower: Expr,
    pub upper: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indirection {
    Pointer,
    Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerRange {
    pub lower: i128,
    pub upper: i128,
}

#[derive(Debug, Clone)]
pub struct Variable {
    pub id: String,
    pub name: String,
    pub kind: VariableKind,
    pub constant: bool,
    pub retention: Retention,
    pub ty: TypeRef,
    pub initial: Option<Expr>,
    pub address: Option<String>,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableKind {
    Local,
    Input,
    Output,
    InOut,
    Global,
    Temporary,
    Static,
    Instance,
    External,
    Configuration,
    Access,
    Field,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Retention {
    #[default]
    Automatic,
    Retained,
    Persistent,
    NonRetained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoDirection {
    Input,
    Output,
}

impl IoDirection {
    pub fn from_address(address: &str) -> Option<Self> {
        match address.get(..2).map(str::to_ascii_uppercase).as_deref() {
            Some("%I") => Some(Self::Input),
            Some("%Q") => Some(Self::Output),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StorageRef {
    pub unit: String,
    pub path: Vec<String>,
    pub ty: TypeRef,
    pub source: Source,
}

impl StorageRef {
    pub fn key(&self) -> String {
        format!("{}::{}", self.unit, self.path.join(".")).to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReferenceBinding {
    pub storage: Option<StorageRef>,
    pub callable: Option<String>,
    pub constant: Option<i128>,
}

#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub(crate) references: BTreeMap<(String, String), ReferenceBinding>,
}

impl Bindings {
    pub fn get(&self, owner: &str, name: &str) -> Option<&ReferenceBinding> {
        self.references
            .get(&(owner.into(), name.to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub origin: Option<String>,
    pub logical_line: Option<usize>,
}

impl Source {
    pub fn group(&self) -> &str {
        self.path
            .rsplit_once('/')
            .map(|(group, _)| group)
            .unwrap_or("")
    }

    pub fn at(path: &str, text: &str, start: usize, end: usize) -> Self {
        let start = text.floor_char_boundary(start.min(text.len()));
        let end = text.floor_char_boundary(end.max(start).min(text.len()));
        let prefix = &text[..start];

        Self {
            path: path.into(),
            line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
            end_line: text[..end].bytes().filter(|b| *b == b'\n').count() + 1,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    RisingEdge,
    FallingEdge,
    OnDelay,
    OffDelay,
    Pulse,
    ResetDominant,
    SetDominant,
    CountUp,
}

impl Intrinsic {
    pub fn ports(self) -> &'static [(&'static str, &'static str, VariableKind)] {
        match self {
            Self::RisingEdge | Self::FallingEdge => &[
                ("CLK", "BOOL", VariableKind::Input),
                ("Q", "BOOL", VariableKind::Output),
            ],

            Self::OnDelay | Self::OffDelay | Self::Pulse => &[
                ("IN", "BOOL", VariableKind::Input),
                ("PT", "TIME", VariableKind::Input),
                ("Q", "BOOL", VariableKind::Output),
                ("ET", "TIME", VariableKind::Output),
            ],

            Self::ResetDominant => &[
                ("SET", "BOOL", VariableKind::Input),
                ("RESET1", "BOOL", VariableKind::Input),
                ("Q1", "BOOL", VariableKind::Output),
            ],

            Self::SetDominant => &[
                ("SET1", "BOOL", VariableKind::Input),
                ("RESET", "BOOL", VariableKind::Input),
                ("Q1", "BOOL", VariableKind::Output),
            ],

            Self::CountUp => &[
                ("CU", "BOOL", VariableKind::Input),
                ("RESET", "BOOL", VariableKind::Input),
                ("PV", "WORD", VariableKind::Input),
                ("Q", "BOOL", VariableKind::Output),
                ("CV", "WORD", VariableKind::Output),
            ],
        }
    }

    pub fn dependencies(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::RisingEdge | Self::FallingEdge => &[("CLK", "Q")],

            Self::OnDelay | Self::OffDelay | Self::Pulse => {
                &[("IN", "Q"), ("PT", "Q"), ("IN", "ET"), ("PT", "ET")]
            }

            Self::ResetDominant => &[("SET", "Q1"), ("RESET1", "Q1")],
            Self::SetDominant => &[("SET1", "Q1"), ("RESET", "Q1")],

            Self::CountUp => &[
                ("CU", "CV"),
                ("RESET", "CV"),
                ("PV", "CV"),
                ("CU", "Q"),
                ("RESET", "Q"),
                ("PV", "Q"),
            ],
        }
    }
}
