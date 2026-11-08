use std::collections::BTreeSet;
mod xml;

use super::{cfc, ladder, sfc};
pub(super) use xml::read_xml;
pub(crate) use xml::{XmlContext, named, value};
pub(crate) const STEP: &str = "{86ee92a2-d863-4136-be27-82b36d84211c}";

use crate::{
    input::{SourceError, source_error, st},
    ir::*,
};
use roxmltree::Node;

pub(super) fn read(
    context: &XmlContext<'_>,
    root: Node<'_, '_>,
    model: &mut Program,
    links: &mut super::ImportLinks,
    errors: &mut Vec<SourceError>,
) {
    for entry in root
        .descendants()
        .filter(|n| named(*n, "MetaObject").is_some() && named(*n, "Object").is_some())
    {
        let meta = named(entry, "MetaObject").unwrap();
        let object = named(entry, "Object").unwrap();
        let name = value(meta, "Name");

        if let Some(programs) = named(object, "PouList") {
            model.tasks.push(Task {
                name,
                cyclic: value(object, "Kindoftask").eq_ignore_ascii_case("Cyclic"),
                programs: programs
                    .children()
                    .filter(Node::is_element)
                    .map(|p| value(p, "Name"))
                    .filter(|n| !n.is_empty())
                    .map(|name| TaskEntry { name, target: None })
                    .collect(),
                source: context.source(object),
            });
            continue;
        }

        if named(object, "TaskConfigurationList").is_some() {
            continue;
        }

        let declarations_before = model.types.len();
        let declaration = named(object, "Interface").or_else(|| named(object, "Declaration"));

        let mut units = if let Some(interface) = declaration {
            let doc = context.st_document(interface);

            if doc.text.trim().is_empty() {
                vec![]
            } else {
                st::parse_document(&doc, model, errors)
            }
        } else {
            vec![]
        };

        if units.is_empty() {
            if model.types.len() > declarations_before {
                continue;
            }

            if named(object, "Implementation").is_none() && declaration.is_none() {
                continue;
            }

            let kind = if context.path.ends_with(".action.xml")
                || object.attribute("Type") == Some("{8ac092e5-3128-4e26-9e7e-11016c6684f2}")
            {
                UnitKind::Action
            } else if context.path.ends_with(".method.xml") {
                UnitKind::Method
            } else {
                UnitKind::Unknown
            };

            units.push(Unit::new(&name, kind, context.source(meta)));
        }

        for unit in &mut units {
            if unit.kind == UnitKind::GlobalVariables {
                unit.name = name.clone();
            }

            links
                .native_ids
                .insert(value(meta, "Guid"), unit.id.clone());
            links
                .parents
                .insert(unit.id.clone(), value(meta, "ParentGuid"));
            if let Some(implementation) = named(object, "Implementation") {
                if implementation
                    .descendants()
                    .any(|n| n.attribute("Name") == Some("TextLines"))
                {
                    let doc = context.st_document(implementation);
                    let body = st::read_body(&doc, errors);
                    unit.bodies.push(body.lower(&unit.id, "implementation"));
                } else {
                    let mut variables = vec![];
                    unit.bodies.push(graphical(
                        context,
                        implementation,
                        &unit.id,
                        errors,
                        meta,
                        &mut variables,
                    ));
                    links.runtime.push((unit.id.clone(), variables));
                }
            }
        }

        model.units.extend(units);
    }
}

enum GraphicalLanguage {
    Sfc,
    LadderFbd,
    Cfc,
    Unknown,
}

fn graphical(
    context: &XmlContext<'_>,
    root: Node<'_, '_>,
    owner: &str,
    ds: &mut Vec<SourceError>,
    settings: Node<'_, '_>,
    variables: &mut Vec<Variable>,
) -> Body {
    let language = if root
        .descendants()
        .any(|n| n.attribute("Type") == Some(STEP))
    {
        GraphicalLanguage::Sfc
    } else if root
        .descendants()
        .any(|n| n.attribute("Name") == Some("NetworkList"))
    {
        GraphicalLanguage::LadderFbd
    } else if root
        .descendants()
        .any(|n| n.attribute("Name") == Some("SourcePinId"))
    {
        GraphicalLanguage::Cfc
    } else {
        GraphicalLanguage::Unknown
    };

    let mut behavior = Body {
        id: id(&["behavior", owner, "implementation"]),
        name: "implementation".into(),
        statements: vec![],
        execution: Execution::Unspecified,
        source: context.source(root),
    };

    behavior.execution = if matches!(language, GraphicalLanguage::Sfc) {
        let graph = sfc::SfcModel::read(context, root, settings)
            .and_then(|mut chart| {
                variables.append(&mut chart.runtime);
                chart.lower()
            })
            .ok()
            .map(Box::new);
        behavior.statements = sfc::recover(context, root, &behavior);
        match graph {
            Some(graph) => Execution::StateMachine(graph),
            None => Execution::Unspecified,
        }
    } else {
        validate_network(context, root, ds);
        behavior.statements = match language {
            GraphicalLanguage::LadderFbd => {
                ladder::LadderModel::read(context, root).lower(&behavior)
            }
            GraphicalLanguage::Cfc => cfc::CfcModel::read(context, root).lower(&behavior),
            _ => vec![],
        };
        Execution::Unspecified
    };

    behavior
}

fn validate_network(context: &XmlContext<'_>, root: Node<'_, '_>, ds: &mut Vec<SourceError>) {
    let mut ids = std::collections::BTreeSet::new();

    for n in root.descendants().filter(|n| named(*n, "Id").is_some()) {
        if !ids.insert(value(n, "Id")) {
            source_error(ds, "Duplicate native network ID", context.source(n));
        }
    }
}

pub(crate) fn expression(text: &str, source: &Source) -> Expr {
    let tokens = crate::input::st::lexer::lex(text, &source.path, &mut vec![]);
    crate::input::st::expressions::parse(&tokens[..tokens.len().saturating_sub(1)])
}

pub(crate) fn statement(
    behavior: &Body,
    source: &Source,
    offset: usize,
    kind: StatementKind,
) -> Statement {
    let text = match &kind {
        StatementKind::Call { expression } => expression.text(),
        StatementKind::Assign { target, value } => format!("{} := {}", target.text(), value.text()),
        _ => unreachable!(),
    };

    Statement {
        id: id(&["native_network", &behavior.id, &offset.to_string(), &text]),
        source: source.clone(),
        kind,
    }
}

pub(crate) fn commented(node: Node<'_, '_>) -> bool {
    node.ancestors()
        .any(|n| value(n, "OutCommented").eq_ignore_ascii_case("true"))
}

pub(crate) fn add_interfaces(model: &mut Program) {
    let referenced: BTreeSet<_> = model
        .units
        .iter()
        .flat_map(|u| u.variables.iter().map(|v| v.ty.base.clone()))
        .chain(
            model
                .types
                .iter()
                .flat_map(|t| t.fields().iter().map(|v| v.ty.base.clone())),
        )
        .collect();

    for ty in referenced {
        if model.units.iter().any(|u| u.name.eq_ignore_ascii_case(&ty))
            || model.types.iter().any(|u| u.name.eq_ignore_ascii_case(&ty))
        {
            continue;
        }

        let upper = ty.to_ascii_uppercase();
        let simple = upper.strip_prefix("STANDARD.").unwrap_or(&upper);

        let Some(kind) = intrinsic(&ty) else {
            continue;
        };

        let mut unit = Unit::new(
            &ty,
            UnitKind::FunctionBlock,
            Source {
                path: "<Standard library>".into(),
                origin: Some(format!("CODESYS Standard::{simple}")),
                ..Default::default()
            },
        );

        unit.intrinsic = Some(kind);

        for &(name, ty, kind) in kind.ports() {
            unit.variables.push(Variable {
                id: id(&["standard_parameter", &unit.id, name]),
                name: name.into(),
                kind,
                constant: false,
                retention: Retention::Automatic,
                ty: TypeRef::scalar(ty),
                initial: None,
                address: None,
                source: unit.source.clone(),
            });
        }

        model.units.push(unit);
    }
}

fn intrinsic(name: &str) -> Option<Intrinsic> {
    let upper = name.to_ascii_uppercase();

    Some(match upper.strip_prefix("STANDARD.").unwrap_or(&upper) {
        "R_TRIG" => Intrinsic::RisingEdge,
        "F_TRIG" => Intrinsic::FallingEdge,
        "TON" => Intrinsic::OnDelay,
        "TOF" => Intrinsic::OffDelay,
        "TP" => Intrinsic::Pulse,
        "RS" => Intrinsic::ResetDominant,
        "SR" => Intrinsic::SetDominant,
        "CTU" => Intrinsic::CountUp,
        _ => return None,
    })
}
