use crate::model::{
    TextRange,
    flow::{FlowDocument, NodeId, NodeKind},
};
use std::ops::Range;

#[derive(Clone, Debug)]
struct TextSpan {
    rendered: Range<usize>,
    range: TextRange,
}

#[derive(Clone, Debug, Default)]
struct SearchUnit {
    _node: Option<NodeId>,
    text: String,
    spans: Vec<TextSpan>,
}

impl SearchUnit {
    fn push(&mut self, text: &str, range: TextRange) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.spans.push(TextSpan {
            rendered: start..self.text.len(),
            range,
        });
    }

    fn matches(&self, query: &str, output: &mut Vec<TextRange>) {
        let (folded_text, folded_chars) = fold_with_mapping(&self.text);
        let folded_query = query.to_lowercase();
        for (folded_start, _) in folded_text.match_indices(&folded_query) {
            let folded_end = folded_start + folded_query.len();
            let first_mapping =
                folded_chars.partition_point(|mapping| mapping.folded.end <= folded_start);
            let after_mapping =
                folded_chars.partition_point(|mapping| mapping.folded.start < folded_end);
            let Some(mappings) = folded_chars.get(first_mapping..after_mapping) else {
                continue;
            };
            let Some((first, rest)) = mappings.split_first() else {
                continue;
            };
            let start = first.original.start;
            let end = rest
                .last()
                .map_or(first.original.end, |last| last.original.end);
            let first_span = self
                .spans
                .partition_point(|span| span.rendered.end <= start);
            let mut matching = self.spans[first_span..]
                .iter()
                .take_while(|span| span.rendered.start < end)
                .map(|span| {
                    let rendered_len = span.rendered.end - span.rendered.start;
                    let source_len = span.range.end - span.range.start;
                    if rendered_len == source_len {
                        TextRange {
                            start: span.range.start + start.max(span.rendered.start)
                                - span.rendered.start,
                            end: span.range.start + end.min(span.rendered.end)
                                - span.rendered.start,
                        }
                    } else {
                        span.range
                    }
                });
            let Some(mut source) = matching.next() else {
                continue;
            };
            for span in matching {
                source.start = source.start.min(span.start);
                source.end = source.end.max(span.end);
            }
            output.push(source);
        }
    }
}

#[derive(Clone, Debug)]
struct FoldMapping {
    folded: Range<usize>,
    original: Range<usize>,
}

fn fold_with_mapping(text: &str) -> (String, Vec<FoldMapping>) {
    let mut folded = String::new();
    let mut mapping = Vec::new();
    for (original_start, character) in text.char_indices() {
        let folded_start = folded.len();
        folded.extend(character.to_lowercase());
        mapping.push(FoldMapping {
            folded: folded_start..folded.len(),
            original: original_start..original_start + character.len_utf8(),
        });
    }
    (folded, mapping)
}

#[derive(Clone, Debug, Default)]
pub struct SearchIndex {
    units: Vec<SearchUnit>,
}

impl SearchIndex {
    pub fn new(document: &FlowDocument) -> Self {
        let mut units = Vec::new();
        let mut stack = vec![document.root()];
        while let Some(id) = stack.pop() {
            let node = document
                .get_node(id)
                .expect("finalized flow search traversal retains valid identities");
            match node.kind {
                NodeKind::Paragraph
                | NodeKind::Heading(_)
                | NodeKind::Preformatted(_)
                | NodeKind::Term
                | NodeKind::Summary => {
                    append_flow_units(document, id, &mut units);
                }
                NodeKind::Row => push_flow_row_units(document, id, &mut units),
                NodeKind::Image(_) | NodeKind::Math(_) | NodeKind::ThematicBreak => {
                    push_atomic_flow_unit(document, id, &mut units);
                }
                NodeKind::UnsupportedObject(_) if !has_readable_flow_fallback(document, node) => {
                    push_atomic_flow_unit(document, id, &mut units);
                }
                _ => stack.extend(node.children.iter().rev().copied()),
            }
        }
        Self { units }
    }

    pub fn find(&self, query: &str) -> Vec<TextRange> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for unit in &self.units {
            unit.matches(query, &mut matches);
        }
        matches.sort_by_key(|source| (source.start, source.end));
        matches.dedup();
        matches
    }
}

fn append_flow_units(document: &FlowDocument, root: NodeId, units: &mut Vec<SearchUnit>) {
    let mut current = SearchUnit {
        _node: Some(root),
        ..SearchUnit::default()
    };
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = document
            .get_node(id)
            .expect("finalized flow search units retain valid identities");
        match node.kind {
            NodeKind::Text | NodeKind::HardBreak => {
                if let Some(text) = document.get_text(node.text_range) {
                    current.push(text, text_range(node.text_range));
                }
            }
            NodeKind::Image(_) | NodeKind::Math(_) | NodeKind::ThematicBreak => {
                flush_flow_unit(&mut current, units);
                push_atomic_flow_unit(document, id, units);
            }
            NodeKind::UnsupportedObject(_) if !has_readable_flow_fallback(document, node) => {
                flush_flow_unit(&mut current, units);
                push_atomic_flow_unit(document, id, units);
            }
            _ => stack.extend(node.children.iter().rev().copied()),
        }
    }
    flush_flow_unit(&mut current, units);
}

fn flush_flow_unit(current: &mut SearchUnit, units: &mut Vec<SearchUnit>) {
    if current.text.is_empty() {
        return;
    }
    let node = current._node;
    units.push(std::mem::replace(
        current,
        SearchUnit {
            _node: node,
            ..SearchUnit::default()
        },
    ));
}

fn push_flow_row_units(document: &FlowDocument, id: NodeId, units: &mut Vec<SearchUnit>) {
    let Some(row) = document.get_node(id) else {
        return;
    };
    let row_range = text_range(row.text_range);
    let mut atomics = Vec::new();
    let mut stack: Vec<_> = row.children.iter().rev().copied().collect();
    while let Some(child_id) = stack.pop() {
        let Some(child) = document.get_node(child_id) else {
            continue;
        };
        let atomic = matches!(
            child.kind,
            NodeKind::Image(_) | NodeKind::Math(_) | NodeKind::ThematicBreak
        ) || matches!(child.kind, NodeKind::UnsupportedObject(_))
            && !has_readable_flow_fallback(document, child);
        if atomic {
            if !child.text_range.is_empty() {
                atomics.push(child_id);
            }
        } else {
            stack.extend(child.children.iter().rev().copied());
        }
    }
    atomics.sort_by_key(|id| document.get_node(*id).unwrap().text_range.start);
    let mut cursor = row_range.start;
    for atomic in atomics {
        let range = text_range(document.get_node(atomic).unwrap().text_range);
        push_flow_range(
            document,
            id,
            TextRange {
                start: cursor,
                end: range.start,
            },
            units,
        );
        push_atomic_flow_unit(document, atomic, units);
        cursor = range.end;
    }
    push_flow_range(
        document,
        id,
        TextRange {
            start: cursor,
            end: row_range.end,
        },
        units,
    );
}

fn push_flow_range(
    document: &FlowDocument,
    id: NodeId,
    range: TextRange,
    units: &mut Vec<SearchUnit>,
) {
    if range.start >= range.end {
        return;
    }
    let Some(text) = document.text().as_str().get(range.start..range.end) else {
        return;
    };
    let mut unit = SearchUnit {
        _node: Some(id),
        ..SearchUnit::default()
    };
    unit.push(text, range);
    units.push(unit);
}

fn push_range_flow_unit(document: &FlowDocument, id: NodeId, units: &mut Vec<SearchUnit>) {
    let node = document
        .get_node(id)
        .expect("finalized semantic search units retain valid identities");
    let Some(text) = document.get_text(node.text_range) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    let mut unit = SearchUnit {
        _node: Some(id),
        ..SearchUnit::default()
    };
    unit.push(text, text_range(node.text_range));
    units.push(unit);
}

fn push_atomic_flow_unit(document: &FlowDocument, id: NodeId, units: &mut Vec<SearchUnit>) {
    push_range_flow_unit(document, id, units);
}

fn has_readable_flow_fallback(document: &FlowDocument, node: &crate::model::flow::Node) -> bool {
    node.children.iter().any(|child| {
        document
            .get_node(*child)
            .is_some_and(|child| !child.text_range.is_empty())
    })
}

fn text_range(range: crate::model::flow::TextRange) -> TextRange {
    TextRange {
        start: range.start.as_usize(),
        end: range.end.as_usize(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        formats::markdown::{MarkdownOptions, parse},
        model::flow::{
            Disclosure, FlowBuilder, MediaKind, NodeKind, Placement, SourceRetention,
            UnsupportedObject, UnsupportedReason,
        },
    };

    fn flow(source: &str) -> FlowDocument {
        parse(
            source,
            MarkdownOptions {
                resource_base: None,
                identity: None,
                retention: SourceRetention::Discard,
            },
        )
        .unwrap()
        .document
    }
    #[test]
    fn flow_search_uses_semantic_units_and_exact_canonical_ranges() {
        let document = flow(
            "one **bold** three\n\nbefore ![Quarterly revenue](chart.png) after\n\n$alpha beta$\n\n| Name | Count |\n| --- | ---: |\n| One | 1 |",
        );
        let index = SearchIndex::new(&document);

        let prose = index.find("one bold three");
        assert_eq!(prose.len(), 1);
        assert_eq!(
            document.text().get(crate::model::flow::TextRange::new(
                crate::model::flow::TextOffset::new(prose[0].start),
                crate::model::flow::TextOffset::new(prose[0].end),
            )),
            Some("one bold three")
        );
        let atomic = index.find("revenue");
        assert_eq!(atomic.len(), 1);
        assert_eq!(
            &document.text().as_str()[atomic[0].start..atomic[0].end],
            "revenue"
        );
        assert_eq!(index.find("alpha beta").len(), 1);
        assert_eq!(index.find("Name\tCount").len(), 1);
        assert_eq!(index.find("One\t1").len(), 1);
        assert!(index.find("Name\tCount\nOne").is_empty());
        assert!(index.find("before after").is_empty());
        assert!(index.units.iter().all(|unit| unit._node.is_some()));
    }

    #[test]
    fn flow_search_covers_specialized_phrasing_and_unsupported_fallbacks() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();

        let definitions = builder.add_node(NodeKind::DefinitionList).unwrap();
        let term = builder.add_node(NodeKind::Term).unwrap();
        let term_text = builder.add_text("searchable term").unwrap();
        builder.append(term, term_text).unwrap();
        let definition = builder.add_node(NodeKind::Definition).unwrap();
        let definition_paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let definition_text = builder.add_text("definition body").unwrap();
        builder
            .append(definition_paragraph, definition_text)
            .unwrap();
        builder.append(definition, definition_paragraph).unwrap();
        builder.append(definitions, term).unwrap();
        builder.append(definitions, definition).unwrap();
        builder.append(root, definitions).unwrap();

        let disclosure = builder
            .add_node(NodeKind::Disclosure(Disclosure {
                initially_open: false,
            }))
            .unwrap();
        let summary = builder.add_node(NodeKind::Summary).unwrap();
        let summary_text = builder.add_text("searchable summary").unwrap();
        builder.append(summary, summary_text).unwrap();
        let details = builder.add_node(NodeKind::Paragraph).unwrap();
        let details_text = builder.add_text("details body").unwrap();
        builder.append(details, details_text).unwrap();
        builder.append(disclosure, summary).unwrap();
        builder.append(disclosure, details).unwrap();
        builder.append(root, disclosure).unwrap();

        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let titled = builder
            .add_node(NodeKind::UnsupportedObject(UnsupportedObject {
                placement: Placement::Inline,
                media_kind: MediaKind::Video,
                title: Some("atomic title".into()),
                size_hint: None,
                activation: None,
                reason: UnsupportedReason::NoNativePresenter,
            }))
            .unwrap();
        let placeholder = builder
            .add_node(NodeKind::UnsupportedObject(UnsupportedObject {
                placement: Placement::Inline,
                media_kind: MediaKind::Unknown,
                title: None,
                size_hint: None,
                activation: None,
                reason: UnsupportedReason::NoNativePresenter,
            }))
            .unwrap();
        let fallback = builder
            .add_node(NodeKind::UnsupportedObject(UnsupportedObject {
                placement: Placement::Inline,
                media_kind: MediaKind::Audio,
                title: Some("ignored title".into()),
                size_hint: None,
                activation: None,
                reason: UnsupportedReason::NoNativePresenter,
            }))
            .unwrap();
        let fallback_text = builder.add_text("readable fallback").unwrap();
        builder.append(fallback, fallback_text).unwrap();
        builder.append(paragraph, titled).unwrap();
        builder.append(paragraph, placeholder).unwrap();
        builder.append(paragraph, fallback).unwrap();
        builder.append(root, paragraph).unwrap();

        let document = builder.finish().unwrap();
        let index = SearchIndex::new(&document);
        for query in [
            "searchable term",
            "searchable summary",
            "definition body",
            "details body",
            "atomic title",
            "readable fallback",
        ] {
            let matches = index.find(query);
            assert_eq!(matches.len(), 1, "missing {query}");
            assert_eq!(
                &document.text().as_str()[matches[0].start..matches[0].end],
                query
            );
        }
        assert!(index.find("ignored title").is_empty());
        let replacement = index.find("\u{fffc}");
        assert_eq!(replacement.len(), 1);
        assert_eq!(replacement[0].end - replacement[0].start, 3);
        assert!(index.units.windows(2).all(|units| {
            units[0].spans.first().unwrap().range.start
                <= units[1].spans.first().unwrap().range.start
        }));
    }

    #[test]
    fn flow_table_rows_preserve_separators_without_crossing_atomic_boundaries() {
        let document = flow("| A | B |\n|---|---|\n| ![chart](chart.png) after | C |");
        let index = SearchIndex::new(&document);

        assert_eq!(index.find("chart").len(), 1);
        assert_eq!(index.find("after\tC").len(), 1);
        assert!(index.find("chart after").is_empty());
    }

    #[test]
    fn flow_atomic_units_retain_repeated_exact_matches() {
        let document = flow("![word and word](chart.png)");
        let matches = SearchIndex::new(&document).find("word");

        assert_eq!(matches.len(), 2);
        assert_ne!(matches[0], matches[1]);
        assert!(
            matches
                .iter()
                .all(|range| { &document.text().as_str()[range.start..range.end] == "word" })
        );
    }

    #[test]
    fn flow_search_handles_many_matches_in_one_preformatted_unit() {
        let source = format!("```text\n{}\n```", "word ".repeat(10_000));
        let document = flow(&source);
        let matches = SearchIndex::new(&document).find("word");

        assert_eq!(matches.len(), 10_000);
        assert_eq!(
            &document.text().as_str()[matches[9_999].start..matches[9_999].end],
            "word"
        );
    }

    #[test]
    fn empty_query_has_no_matches() {
        assert!(SearchIndex::new(&flow("text")).find("").is_empty());
    }
}
