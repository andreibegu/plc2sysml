use crate::analysis::SignalBoundary;
mod index;

use crate::ir::*;
use anyhow::{Context, Result, bail};
pub use index::Index;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringContext {
    pub physical_model: PathBuf,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_definition: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_name: Option<String>,

    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub source: String,
    pub targets: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<IoRole>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub source_name: String,
    pub storage: StorageRef,
    pub name: String,
    pub item_type: Option<String>,
    pub value_type: Option<String>,
    pub role: IoRole,
    pub targets: Vec<SignalEndpoint>,
}

#[derive(Debug, Clone)]
pub struct SignalEndpoint {
    pub physical: String,
    pub features: Vec<String>,
}

impl SignalEndpoint {
    pub fn path(&self) -> Vec<String> {
        self.physical
            .split('.')
            .map(String::from)
            .chain(self.features.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoRole {
    Command,
    Feedback,
}

#[derive(Debug)]
pub struct LoadedContext {
    pub physical_model: PathBuf,
    pub root_definition: String,
    pub system_name: String,
    pub channels: Vec<Channel>,

    pub index: Index,
    pub physical_transfers: BTreeSet<(String, String)>,
}

impl LoadedContext {
    pub fn boundaries(&self) -> Vec<SignalBoundary> {
        self.channels
            .iter()
            .map(|channel| SignalBoundary {
                storage: channel.storage.clone(),
                direction: match channel.role {
                    IoRole::Feedback => IoDirection::Input,
                    IoRole::Command => IoDirection::Output,
                },
            })
            .collect()
    }

    pub fn load(path: &Path, model: &Program) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
        let spec: EngineeringContext =
            serde_json::from_slice(&bytes).context("Invalid engineering context")?;

        let physical_model = if spec.physical_model.is_relative() {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&spec.physical_model)
        } else {
            spec.physical_model
        };

        let index = Index::load(&physical_model)?;

        let root_definition = match spec.root_definition {
            Some(root) => root,
            None => index.root_definition()?,
        };

        index.require_definition(&root_definition, "part")?;

        let system_name = spec
            .system_name
            .unwrap_or_else(|| format!("{}Model", root_definition.rsplit("::").next().unwrap()));

        if system_name.trim().is_empty() {
            bail!("system_name must not be empty");
        }

        let mut channels = vec![];
        let mut accesses = None::<crate::analysis::Accesses>;
        let mut seen = BTreeSet::new();

        for binding in spec.bindings {
            let storage = storage(model, &binding.source)?;

            if !seen.insert(storage.key()) {
                bail!("Duplicate source binding {}", binding.source);
            }

            let variable =
                variable(model, &storage).context("Binding must identify declared storage")?;

            if storage.path.last().is_some_and(|p| p.parse::<u8>().is_ok()) {
                bail!(
                    "{}: map the containing word; bit reads are analyzed within the code",
                    binding.source
                );
            }

            if !storage.ty.dimensions.is_empty()
                || storage.ty.indirection.is_some()
                || storage.ty.target.as_ref().is_some_and(|id| {
                    model
                        .types
                        .iter()
                        .any(|t| &t.id == id && matches!(t.kind, TypeKind::Struct(_)))
                })
            {
                bail!("{} must identify a scalar I/O channel", binding.source);
            }

            let address = variable
                .address
                .as_deref()
                .unwrap_or("")
                .to_ascii_uppercase();

            let hardware_role = match IoDirection::from_address(&address) {
                Some(IoDirection::Input) => Some(IoRole::Feedback),
                Some(IoDirection::Output) => Some(IoRole::Command),
                None => None,
            };

            if hardware_role.zip(binding.role).is_some_and(|(a, b)| a != b) {
                bail!("{}: role conflicts with hardware address", binding.source);
            }

            if binding.targets.is_empty() {
                bail!(
                    "{} needs a physical part or explicit signal endpoint target",
                    binding.source
                );
            }

            let mut physical_role = None;

            for target in &binding.targets {
                if index.resolve_path(&root_definition, target).is_ok() {
                    continue;
                }

                let direction = index.signal_direction(&root_definition, target)?;
                let candidate = endpoint_role(direction.as_deref());

                if physical_role.zip(candidate).is_some_and(|(a, b)| a != b) {
                    bail!(
                        "{}: targets declare conflicting signal directions",
                        binding.source
                    );
                }

                physical_role = physical_role.or(candidate);
            }

            if physical_role
                .zip(hardware_role.or(binding.role))
                .is_some_and(|(a, b)| a != b)
            {
                bail!(
                    "{}: role or hardware address conflicts with physical signal direction",
                    binding.source
                );
            }

            let mut declared_role = hardware_role.or(physical_role).or(binding.role);

            if declared_role.is_none() {
                let mut candidates = BTreeSet::new();

                for target in &binding.targets {
                    if let Some(direction) = index.signal_direction(&root_definition, target)? {
                        candidates.insert(direction);
                    }
                }

                if candidates.len() == 1 {
                    declared_role = endpoint_role(candidates.iter().next().map(String::as_str));
                }
            }

            let role = if let Some(role) = declared_role {
                role
            } else {
                let usage = accesses.get_or_insert_with(|| crate::analysis::io_accesses(model));
                let usage = usage.one_sided(&storage).with_context(|| format!(
                    "{}: direction is unknown or code use is mixed/by-reference; specify role or map an explicit in/out endpoint", binding.source
                ))?;

                match usage {
                    crate::analysis::CodeUse::WriteOnly => IoRole::Command,

                    crate::analysis::CodeUse::ReadOnly => IoRole::Feedback,
                }
            };

            let mut item_type = None;
            let mut targets = vec![];
            let mut target_names = BTreeSet::new();

            for target in &binding.targets {
                if !target_names.insert(target) {
                    bail!("Duplicate target {target}");
                }

                let expected = if role == IoRole::Command { "in" } else { "out" };

                let endpoint = index.signal_endpoint(&root_definition, target, expected)?;

                if endpoint.direction != expected {
                    bail!("{target}: parameter direction conflicts with the resolved I/O role");
                }

                if let Some(ty) = &endpoint.item_type {
                    index.require_definition(ty, "item")?;
                }

                if item_type
                    .as_ref()
                    .is_some_and(|ty| *ty != endpoint.item_type)
                {
                    bail!("{} has targets with different item types", binding.source);
                }

                item_type = Some(endpoint.item_type);
                targets.push(SignalEndpoint {
                    physical: endpoint.physical,
                    features: endpoint.features,
                });
            }

            if binding
                .name
                .as_ref()
                .is_some_and(|name| name.trim().is_empty())
            {
                bail!(
                    "{}: an explicit signal label must not be empty",
                    binding.source
                );
            }

            let payload = value_type(&storage.ty).map(String::from);
            channels.push(Channel {
                source_name: binding.source,
                storage,
                name: binding.name.unwrap_or_else(|| words(&variable.name)),
                item_type: item_type.unwrap(),
                value_type: payload,
                role,
                targets,
            });
        }

        let physical_transfers = index.physical_transfers(&root_definition)?;

        Ok(Self {
            physical_model,
            root_definition,
            system_name,
            channels,
            index,
            physical_transfers,
        })
    }
}

fn endpoint_role(direction: Option<&str>) -> Option<IoRole> {
    match direction {
        Some("in") => Some(IoRole::Command),
        Some("out") => Some(IoRole::Feedback),
        _ => None,
    }
}

fn value_type(ty: &TypeRef) -> Option<&'static str> {
    match ty.base.to_ascii_uppercase().as_str() {
        "BOOL" | "BIT" => Some("Boolean"),

        "BYTE" | "WORD" | "DWORD" | "LWORD" | "SINT" | "USINT" | "INT" | "UINT" | "DINT"
        | "UDINT" | "LINT" | "ULINT" => Some("Integer"),

        "REAL" | "LREAL" => Some("Real"),
        "STRING" | "WSTRING" | "CHAR" | "WCHAR" => Some("String"),
        _ => None,
    }
}

fn unique_unit<'a>(model: &'a Program, name: &str) -> Result<&'a Unit> {
    let candidates: Vec<_> = model
        .units
        .iter()
        .filter(|u| {
            u.name.eq_ignore_ascii_case(name)
                || format!("{}::{}", u.source.group(), u.name).eq_ignore_ascii_case(name)
        })
        .collect();

    if candidates.len() != 1 {
        bail!("Source unit {name} is missing or ambiguous; use its source-qualified name");
    }

    Ok(candidates[0])
}

fn storage(model: &Program, path: &str) -> Result<StorageRef> {
    let (unit, rest) = path
        .match_indices('.')
        .rev()
        .find_map(|(i, _)| {
            unique_unit(model, &path[..i])
                .ok()
                .map(|unit| (unit, &path[i + 1..]))
        })
        .with_context(|| {
            format!("Unresolved source owner in {path}; use Unit.variable or Unit.record.field")
        })?;
    let tokens = crate::input::st::lexer::lex(rest, "<engineering context>", &mut vec![]);
    let expr = crate::input::st::expressions::parse(&tokens[..tokens.len().saturating_sub(1)]);

    crate::resolve::source_storage(model, unit, &expr)
        .with_context(|| format!("Unresolved source {path}"))
}

fn variable<'a>(model: &'a Program, storage: &StorageRef) -> Option<&'a Variable> {
    model
        .units
        .iter()
        .find(|u| u.id == storage.unit)?
        .variable(storage.path.first()?)
}

pub fn template(model: &Program, physical_model: PathBuf) -> EngineeringContext {
    EngineeringContext {
        physical_model,
        root_definition: None,
        system_name: None,
        bindings: model
            .units
            .iter()
            .flat_map(|u| {
                u.variables
                    .iter()
                    .filter(|v| {
                        v.address
                            .as_ref()
                            .is_some_and(|a| IoDirection::from_address(a).is_some())
                    })
                    .map(move |v| Binding {
                        source: format!("{}.{}", u.name, v.name),
                        targets: vec!["TODO.physicalPart".into()],
                        name: None,
                        role: None,
                    })
            })
            .collect(),
    }
}

fn words(value: &str) -> String {
    let chars: Vec<_> = value.chars().collect();
    let mut result = String::new();

    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '_' | '-') {
            if !result.ends_with(' ') {
                result.push(' ');
            }

            continue;
        }

        if i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || chars[i - 1].is_uppercase()
                    && chars.get(i + 1).is_some_and(|c| c.is_lowercase()))
            && !result.ends_with(' ')
        {
            result.push(' ');
        }

        result.push(c);
    }

    result.split_whitespace().collect::<Vec<_>>().join(" ")
}
