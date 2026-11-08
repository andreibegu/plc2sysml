mod expressions;
mod names;
mod states;

use super::*;
pub(crate) use names::identifier;
use names::{name, qualified};
use std::collections::{BTreeMap, BTreeSet};

type Doc = pretty::RcDoc<'static, ()>;
const LINE_WIDTH: usize = 100;

pub fn render(model: &SystemModel) -> anyhow::Result<BTreeMap<String, String>> {
    let mut files = model.structure.clone();
    states::render(model, &mut files)?;

    if !model.signal_types.is_empty() || !model.enum_types.is_empty() {
        let mut w = Writer::new();
        w.open(&format!("package {}", name(&model.signal_type_package)));

        for (ty, literals) in &model.enum_types {
            w.blank();
            w.open(&format!("enum def {}", name(ty)));

            for literal in literals {
                w.line(&format!("enum {};", name(literal)));
            }

            w.close();
        }

        for (ty, value_type) in &model.signal_types {
            w.blank();

            if let Some(value_type) = value_type {
                w.open(&format!("item def {}", name(ty)));
                w.typed(
                    "attribute value",
                    &format!("ScalarValues::{}", name(value_type)),
                    "",
                );
                w.close();
            } else {
                w.line(&format!("item def {};", name(ty)));
            }
        }

        w.close();
        files.insert("signals/types.sysml".into(), w.finish());
    }

    let mut root = Writer::new();
    root.open(&format!("package {}", name(&model.package)));
    root.open(&format!(
        "part def {} :> {}",
        name(&model.root_name),
        qualified(&model.root_definition)
    ));

    for equipment in &model.equipment {
        root.typed(
            &format!("part {}", name(&equipment.scope)),
            &format!(
                "{}::{}",
                name(&equipment.package),
                name(&equipment.definition)
            ),
            &format!(" :>> {}", name(&equipment.scope)),
        );
    }

    if !model.sequences.is_empty() {
        root.typed(
            "part control",
            &format!("{}Control::ControlLogic", name(&model.package)),
            "",
        );

        for sequence in &model.sequences {
            root.blank();

            for input in sequence.inputs.iter().filter(|i| i.item_type.is_some()) {
                for endpoint in &input.endpoints {
                    root.flow(
                        &feature_path(endpoint),
                        &format!("control.{}.{}", name(&sequence.usage), name(&input.name)),
                    );
                }
            }

            for output in &sequence.outputs {
                for endpoint in &output.endpoints {
                    root.flow(
                        &format!("control.{}.{}", name(&sequence.usage), name(&output.name)),
                        &feature_path(endpoint),
                    );
                }
            }
        }

        let mut control = Writer::new();
        control.open(&format!("package {}Control", name(&model.package)));
        control.line("item def ControlStep;");
        control.blank();
        control.open("part def ControlLogic");

        for sequence in &model.sequences {
            control.typed(
                &format!("part {}", name(&sequence.usage)),
                &format!("{}::Control", name(&sequence.package)),
                "",
            );
        }

        control.close();
        control.close();
        files.insert("behavior/control.sysml".into(), control.finish());
    }

    root.close();
    root.blank();

    let instance = model.root_instance_name();
    root.line(&format!(
        "part {} : {};",
        name(&instance),
        name(&model.root_name)
    ));
    root.close();
    files.insert("model.sysml".into(), root.finish());

    let mut paths = BTreeSet::new();

    for equipment in &model.equipment {
        let file = format!("signals/{}.sysml", names::filename(&equipment.package));

        if !paths.insert(file.to_ascii_lowercase()) {
            anyhow::bail!("Equipment names produce the same output path: {file}");
        }

        let mut w = Writer::new();
        w.open(&format!("package {}", name(&equipment.package)));
        w.open(&format!(
            "part def {} :> {}",
            name(&equipment.definition),
            qualified(&equipment.base)
        ));

        let mut tree = SignalTree::default();

        for channel in &equipment.channels {
            let mut node = &mut tree;

            for part in &channel.part {
                node = node.children.entry(part.clone()).or_default();
            }

            node.channels.push(channel);
        }

        write_channels(&mut w, &tree);
        w.close();
        w.close();
        files.insert(file, w.finish());

        let mut w = Writer::new();
        w.open(&format!("package {}Dependencies", name(&equipment.package)));
        w.line(&format!(
            "private import {}::{};",
            name(&model.package),
            name(&model.root_name)
        ));
        w.blank();

        let mut dependencies: Vec<_> = equipment.dependencies.iter().collect();
        dependencies.sort_by_key(|d| (&d.outputs[0][..d.outputs[0].len() - 1], &d.label));

        for dependency in dependencies {
            write_dependency(
                &mut w,
                dependency,
                &[model.root_name.clone(), equipment.scope.clone()],
            );
        }

        let mut cross: Vec<_> = equipment.cross_dependencies.iter().collect();
        cross.sort_by_key(|d| (&d.outputs[0][..d.outputs[0].len() - 1], &d.label));

        if !cross.is_empty() {
            w.blank();
        }

        for dependency in cross {
            write_dependency(&mut w, dependency, std::slice::from_ref(&model.root_name));
        }

        w.close();
        files.insert(
            format!("dependencies/{}.sysml", names::filename(&equipment.package)),
            w.finish(),
        );
    }

    Ok(files)
}

fn feature_path(path: &[String]) -> String {
    path.iter().map(|p| name(p)).collect::<Vec<_>>().join(".")
}

#[derive(Default)]
struct SignalTree<'a> {
    channels: Vec<&'a SignalChannel>,
    children: BTreeMap<String, SignalTree<'a>>,
}

fn write_channels(w: &mut Writer, tree: &SignalTree<'_>) {
    let mut bounded = false;

    for (index, channel) in tree.channels.iter().enumerate() {
        if index > 0 && (bounded || channel.bounds.is_some()) {
            w.blank();
        }

        let declaration = format!(
            "{} item {} : {}",
            if channel.input { "in" } else { "out" },
            name(&channel.name),
            qualified(&channel.item_type)
        );

        if let Some((min, max)) = channel.bounds {
            w.open(&declaration);
            w.range("value", &min.to_string(), &max.to_string());
            w.close();
        } else {
            w.line(&format!("{declaration};"));
        }

        bounded = channel.bounds.is_some();
    }

    for (index, (part, children)) in tree.children.iter().enumerate() {
        if index > 0 || !tree.channels.is_empty() {
            w.blank();
        }

        w.open(&format!("part {} :>> {}", name(part), name(part)));
        write_channels(w, children);
        w.close();
    }
}

fn write_dependency(w: &mut Writer, dependency: &SignalDependency, prefix: &[String]) {
    w.line(&format!("dependency {}", name(&dependency.label)));
    write_references(w, "from", &dependency.outputs, prefix, "");
    write_references(w, "to", &dependency.inputs, prefix, ";");
    w.blank();
}

fn write_references(
    w: &mut Writer,
    keyword: &str,
    references: &[Vec<String>],
    prefix: &[String],
    end: &str,
) {
    for (i, reference) in references.iter().enumerate() {
        w.line(&format!(
            "    {} {}{}",
            if i == 0 { keyword } else { "    " },
            qualified(
                &prefix
                    .iter()
                    .chain(reference)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("::")
            ),
            if i + 1 < references.len() { "," } else { end }
        ));
    }
}

struct Writer {
    text: String,
    depth: usize,
}

impl Writer {
    fn new() -> Self {
        Self {
            text: String::new(),
            depth: 0,
        }
    }

    fn line(&mut self, text: &str) {
        self.text.push_str(&"    ".repeat(self.depth));
        self.text.push_str(text);
        self.text.push('\n');
    }

    fn document(&mut self, doc: Doc) {
        let text = doc
            .pretty(LINE_WIDTH.saturating_sub(self.depth * 4))
            .to_string();

        for line in text.lines() {
            self.line(line);
        }
    }

    fn range(&mut self, value: &str, min: &str, max: &str) {
        self.document(
            Doc::text("assert constraint {")
                .append(
                    Doc::line()
                        .append(Doc::text(format!("{value} >= {min}")))
                        .append(Doc::line())
                        .append(Doc::text(format!("and {value} <= {max}")))
                        .nest(4),
                )
                .append(Doc::line())
                .append("}")
                .group(),
        );
    }

    fn typed(&mut self, header: &str, ty: &str, suffix: &str) {
        self.document(
            Doc::text(format!("{header} :"))
                .append(
                    Doc::line()
                        .append(Doc::text(format!("{ty}{suffix}")))
                        .nest(4),
                )
                .append(";")
                .group(),
        );
    }

    fn flow(&mut self, from: &str, to: &str) {
        self.document(
            Doc::text(format!("flow from {from}"))
                .append(Doc::line().append(Doc::text(format!("to {to};"))).nest(4))
                .group(),
        );
    }

    fn open(&mut self, header: &str) {
        self.line(&format!("{header} {{"));
        self.depth += 1;
    }

    fn close(&mut self) {
        while self.text.ends_with("\n\n") {
            self.text.pop();
        }

        self.depth -= 1;
        self.line("}");
    }

    fn blank(&mut self) {
        if !self.text.ends_with("\n\n") && !self.text.ends_with("{\n") {
            self.text.push('\n');
        }
    }

    fn finish(self) -> String {
        self.text
    }
}
