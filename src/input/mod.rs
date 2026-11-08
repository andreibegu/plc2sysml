mod cfc;
mod codesys;
mod ladder;
pub mod physical;
mod sfc;
pub(crate) mod st;

use crate::ir::{Program, Source, UnitKind, Variable, id};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::{fs, path::Path};
use walkdir::WalkDir;

pub fn read(path: &Path) -> Result<Program> {
    let input = path
        .canonicalize()
        .with_context(|| format!("Cannot open {}", path.display()))?;

    let root = if input.is_dir() {
        input.as_path()
    } else {
        input.parent().context("Input has no parent directory")?
    };

    let mut model = Program::default();
    let mut links = ImportLinks::default();
    let mut errors = vec![];
    let mut paths = vec![];

    if input.is_file() {
        paths.push(input.clone());
    } else {
        for entry in WalkDir::new(&input)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();

                !matches!(
                    name.as_ref(),
                    ".git"
                        | ".svn"
                        | ".idea"
                        | ".vscode"
                        | "target"
                        | "node_modules"
                        | "__pycache__"
                )
            })
        {
            match entry {
                Ok(entry) if entry.file_type().is_symlink() => {}

                Ok(entry) if entry.file_type().is_file() => paths.push(entry.path().into()),
                Ok(_) => {}

                Err(error) => source_error(&mut errors, error.to_string(), Source::default()),
            }
        }
    }

    paths.sort();

    for path in paths {
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();

        if !matches!(
            extension.as_str(),
            "st" | "xml" | "export" | "project" | "library"
        ) {
            continue;
        }

        let relative = relative(root, &path)?;

        let source = Source {
            path: relative.clone(),
            line: 1,
            column: 1,
            end_line: 1,
            ..Default::default()
        };

        if matches!(extension.as_str(), "project" | "library") {
            source_error(
                &mut errors,
                "Export the CODESYS project to native XML or ST before transformation",
                source,
            );
            continue;
        }

        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,

            Err(error) => {
                source_error(&mut errors, format!("{error:#}"), source);
                continue;
            }
        };

        match decode(&bytes) {
            Ok(text) => {
                if extension == "st" {
                    let units = st::parse_document(
                        &st::Document::plain(&relative, &text),
                        &mut model,
                        &mut errors,
                    );
                    model.units.extend(units);
                } else {
                    codesys::read_xml(&relative, &text, &mut model, &mut links, &mut errors);
                }
            }

            Err(error) => source_error(&mut errors, error.to_string(), source),
        }
    }

    if model.units.is_empty() && model.types.is_empty() {
        source_error(
            &mut errors,
            "No code declarations found in the input",
            Source::default(),
        );
    }

    model.units.sort_by(|a, b| {
        (a.source.group(), &a.name, &a.id).cmp(&(b.source.group(), &b.name, &b.id))
    });
    model.types.sort_by(|a, b| {
        (a.source.group(), &a.name, &a.id).cmp(&(b.source.group(), &b.name, &b.id))
    });
    if !errors.is_empty() {
        bail!(
            "{}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    links.attach(&mut model);
    codesys::add_interfaces(&mut model);
    Ok(model)
}

fn relative(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root)?;

    let s = relative
        .to_str()
        .context("Source paths must be valid UTF-8")?;

    if s.contains('\\') {
        bail!("Source path contains a backslash: {s}");
    }

    Ok(s.replace(std::path::MAIN_SEPARATOR, "/"))
}

fn decode(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if !(bytes.len() - 2).is_multiple_of(2) {
            bail!("Incomplete UTF-16 code unit");
        }

        let little = bytes[0] == 0xff;
        let units: Vec<_> = bytes[2..]
            .chunks_exact(2)
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect();

        return String::from_utf16(&units).context("Invalid UTF-16");
    }

    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);

    String::from_utf8(bytes.to_vec()).context("Expected UTF-8 or UTF-16 with a byte-order mark")
}

#[derive(Debug, Clone)]
pub(crate) struct SourceError {
    pub message: String,
    pub source: Source,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.source.path, self.source.line, self.message
        )
    }
}

pub(crate) fn source_error(
    errors: &mut Vec<SourceError>,
    message: impl Into<String>,
    source: Source,
) {
    errors.push(SourceError {
        message: message.into(),
        source,
    });
}

#[derive(Default)]
struct ImportLinks {
    native_ids: BTreeMap<String, String>,
    parents: BTreeMap<String, String>,
    runtime: Vec<(String, Vec<Variable>)>,
}

impl ImportLinks {
    fn attach(self, program: &mut Program) {
        let owners: BTreeSet<_> = program
            .units
            .iter()
            .filter(|u| {
                matches!(
                    u.kind,
                    UnitKind::Program | UnitKind::FunctionBlock | UnitKind::Interface
                )
            })
            .map(|u| u.id.clone())
            .collect();
        let named: Vec<_> = program
            .units
            .iter()
            .filter(|u| owners.contains(&u.id))
            .map(|u| (u.name.clone(), u.id.clone()))
            .collect();
        for unit in &mut program.units {
            if unit.owner.is_some()
                || !matches!(
                    unit.kind,
                    UnitKind::Action | UnitKind::Method | UnitKind::Property
                )
            {
                continue;
            }
            unit.owner = self
                .parents
                .get(&unit.id)
                .and_then(|parent| self.native_ids.get(parent))
                .filter(|owner| owners.contains(*owner) && **owner != unit.id)
                .cloned();
            if unit.owner.is_none() {
                let hint = if unit.source.path.ends_with(".st") {
                    st::owner_hint(&unit.source.path)
                } else {
                    unit.source
                        .group()
                        .rsplit('/')
                        .next()
                        .filter(|n| !n.is_empty())
                        .map(String::from)
                };
                if let Some(hint) = hint {
                    let mut matches = named
                        .iter()
                        .filter(|(name, _)| name.eq_ignore_ascii_case(&hint));
                    if let Some((_, owner)) = matches.next()
                        && matches.next().is_none()
                    {
                        unit.owner = Some(owner.clone());
                    }
                }
            }
        }
        for (source, variables) in self.runtime {
            let owner = program
                .units
                .iter()
                .find(|u| u.id == source)
                .and_then(|u| u.owner.clone())
                .unwrap_or(source);
            if let Some(unit) = program.units.iter_mut().find(|u| u.id == owner) {
                for mut variable in variables {
                    if unit.variable(&variable.name).is_none() {
                        variable.id =
                            id(&["variable", &unit.id, &variable.name.to_ascii_lowercase()]);
                        unit.variables.push(variable);
                    }
                }
            }
        }
    }
}
