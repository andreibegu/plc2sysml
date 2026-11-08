use crate::input::st;
use crate::input::{ImportLinks, SourceError, source_error};
use crate::ir::{Program, Source};
use roxmltree::{Document, Node, ParsingOptions};

pub fn read_xml(
    path: &str,
    text: &str,
    model: &mut Program,
    links: &mut ImportLinks,
    errors: &mut Vec<SourceError>,
) {
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: 1_000_000,
        ..Default::default()
    };

    let xml = match Document::parse_with_options(text, options) {
        Ok(xml) => xml,

        Err(error) => {
            let pos = error.pos();
            source_error(
                errors,
                error.to_string(),
                Source {
                    path: path.into(),
                    line: pos.row as usize,
                    column: pos.col as usize,
                    end_line: pos.row as usize,
                    ..Default::default()
                },
            );
            return;
        }
    };

    let context = XmlContext::new(path, text);

    if xml.root_element().tag_name().name() == "ExportFile" {
        super::read(&context, xml.root_element(), model, links, errors);
    }
}

pub(crate) struct XmlContext<'a> {
    pub(super) path: &'a str,
    text: &'a str,
    line_starts: Vec<usize>,
}

impl XmlContext<'_> {
    fn new<'a>(path: &'a str, text: &'a str) -> XmlContext<'a> {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
        );

        XmlContext {
            path,
            text,
            line_starts,
        }
    }

    pub(crate) fn source(&self, node: Node<'_, '_>) -> Source {
        let range = node.range();
        let line = self
            .line_starts
            .partition_point(|offset| *offset <= range.start);

        Source {
            path: self.path.into(),
            line,
            column: self.text[self.line_starts[line - 1]..range.start]
                .chars()
                .count()
                + 1,
            end_line: self
                .line_starts
                .partition_point(|offset| *offset <= range.end),
            origin: Some(selector(node)),
            logical_line: None,
        }
    }

    pub(crate) fn st_document(&self, node: Node<'_, '_>) -> st::Document {
        let lines: Vec<_> = node
            .descendants()
            .filter(|n| n.is_element() && n.attribute("Name") == Some("Text"))
            .collect();

        st::Document {
            path: self.path.into(),
            text: lines
                .iter()
                .map(|n| n.text().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n"),
            origin: Some(selector(node)),
            physical_lines: lines.iter().map(|n| self.source(*n).line).collect(),
        }
    }
}

fn selector(node: Node<'_, '_>) -> String {
    let mut parts = vec![];

    for n in node.ancestors().filter(Node::is_element) {
        let index = n
            .prev_siblings()
            .skip(1)
            .filter(|s| s.is_element() && s.tag_name() == n.tag_name())
            .count()
            + 1;
        parts.push(format!("{}[{}]", n.tag_name().name(), index));
    }

    parts.reverse();

    format!("/{}", parts.join("/"))
}

pub(crate) fn named<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children()
        .find(|n| n.is_element() && n.attribute("Name") == Some(name))
}

pub(crate) fn value(node: Node<'_, '_>, name: &str) -> String {
    named(node, name)
        .and_then(|n| n.text())
        .unwrap_or("")
        .into()
}
