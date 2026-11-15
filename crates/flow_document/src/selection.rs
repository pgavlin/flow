pub use crate::model::{Affinity, TextPosition};
use crate::model::{
    TextRange,
    flow::{FlowDocument, NodeKind},
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub anchor: TextPosition,
    pub active: TextPosition,
}

impl Selection {
    pub const fn collapsed(position: TextPosition) -> Self {
        Self {
            anchor: position,
            active: position,
        }
    }

    pub fn range(self) -> TextRange {
        TextRange {
            start: self.anchor.offset.min(self.active.offset),
            end: self.anchor.offset.max(self.active.offset),
        }
    }

    pub fn is_collapsed(self) -> bool {
        self.anchor.offset == self.active.offset
    }
}

#[cfg(any(feature = "render", test))]
mod recovery;
#[cfg(feature = "render")]
pub(crate) use recovery::StableFlowSelection;
#[cfg(any(feature = "render", test))]
pub(crate) use recovery::{capture_flow_selection, recover_flow_selection};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FlowSelectionUnitKind {
    Paragraph,
    Heading(u8),
    Term,
    Summary,
    Preformatted,
    Cell,
    Table,
    Atomic,
    #[cfg(any(feature = "render", test))]
    Separator,
}

#[derive(Clone, Copy, Debug)]
struct FlowSelectionUnit {
    kind: FlowSelectionUnitKind,
    range: TextRange,
    atomic: bool,
}

pub fn selected_text(document: &FlowDocument, selection: Selection) -> Option<String> {
    let selection = normalize_flow_selection(document, selection)?;
    let range = selection.range();
    document
        .text()
        .as_str()
        .get(range.start..range.end)
        .map(ToOwned::to_owned)
}

pub fn selection_at(
    document: &FlowDocument,
    position: TextPosition,
    granularity: SelectionGranularity,
) -> Option<Selection> {
    validate_flow_position(document, position)?;
    let range = match granularity {
        SelectionGranularity::Character => {
            return normalize_flow_selection(document, Selection::collapsed(position));
        }
        SelectionGranularity::Word => flow_word_range_at(document, position)?,
        SelectionGranularity::Block => flow_block_range_at(document, position)?,
    };
    normalize_flow_selection(document, selection_from_range(range))
}

fn validate_flow_position(document: &FlowDocument, position: TextPosition) -> Option<TextPosition> {
    let text = document.text().as_str();
    (position.offset <= text.len() && text.is_char_boundary(position.offset)).then_some(position)
}

fn normalize_flow_selection(
    document: &FlowDocument,
    mut selection: Selection,
) -> Option<Selection> {
    validate_flow_position(document, selection.anchor)?;
    validate_flow_position(document, selection.active)?;
    if selection.is_collapsed() {
        if let Some(atom) = flow_units(document)
            .filter(|unit| unit.atomic)
            .find(|atom| {
                atom.range.start < selection.anchor.offset
                    && selection.anchor.offset < atom.range.end
            })
        {
            let offset = match selection.anchor.affinity {
                Affinity::Upstream => atom.range.start,
                Affinity::Downstream => atom.range.end,
            };
            let affinity = match selection.anchor.affinity {
                Affinity::Upstream => Affinity::Downstream,
                Affinity::Downstream => Affinity::Upstream,
            };
            selection.anchor.offset = offset;
            selection.anchor.affinity = affinity;
            selection.active = selection.anchor;
        }
        return Some(selection);
    }
    let forward = selection.anchor.offset <= selection.active.offset;
    let mut start = selection.anchor.offset.min(selection.active.offset);
    let mut end = selection.anchor.offset.max(selection.active.offset);
    let mut normalize_start = false;
    let mut normalize_end = false;
    for atom in flow_units(document).filter(|unit| unit.atomic) {
        if atom.range.start < end && start < atom.range.end {
            let exact_atom = start == atom.range.start && end == atom.range.end;
            normalize_start |= exact_atom
                || (atom.range.start < start && start < atom.range.end)
                || (start == atom.range.start && end < atom.range.end);
            normalize_end |= exact_atom
                || (atom.range.start < end && end < atom.range.end)
                || (atom.range.start < start && end == atom.range.end);
            start = start.min(atom.range.start);
            end = end.max(atom.range.end);
        }
    }
    if forward {
        selection.anchor.offset = start;
        selection.active.offset = end;
        if normalize_start {
            selection.anchor.affinity = Affinity::Downstream;
        }
        if normalize_end {
            selection.active.affinity = Affinity::Upstream;
        }
    } else {
        selection.anchor.offset = end;
        selection.active.offset = start;
        if normalize_end {
            selection.anchor.affinity = Affinity::Upstream;
        }
        if normalize_start {
            selection.active.affinity = Affinity::Downstream;
        }
    }
    Some(selection)
}

fn flow_units(document: &FlowDocument) -> impl Iterator<Item = FlowSelectionUnit> + '_ {
    std::iter::successors(Some(document.root()), |node| document.following(*node)).filter_map(
        |node| {
            let value = document.get_node(node)?;
            let (kind, atomic) = match &value.kind {
                NodeKind::Paragraph => (FlowSelectionUnitKind::Paragraph, false),
                NodeKind::Heading(heading) => {
                    (FlowSelectionUnitKind::Heading(heading.level.get()), false)
                }
                NodeKind::Term => (FlowSelectionUnitKind::Term, false),
                NodeKind::Summary => (FlowSelectionUnitKind::Summary, false),
                NodeKind::Preformatted(_) => (FlowSelectionUnitKind::Preformatted, false),
                NodeKind::Cell(_) => (FlowSelectionUnitKind::Cell, false),
                NodeKind::Table(_) => (FlowSelectionUnitKind::Table, false),
                NodeKind::Image(_) | NodeKind::Math(_) | NodeKind::ThematicBreak => {
                    (FlowSelectionUnitKind::Atomic, true)
                }
                NodeKind::UnsupportedObject(_)
                    if !value.children.iter().any(|child| {
                        document
                            .get_node(*child)
                            .is_some_and(|child| !child.text_range.is_empty())
                    }) =>
                {
                    (FlowSelectionUnitKind::Atomic, true)
                }
                _ => return None,
            };
            let range = TextRange {
                start: value.text_range.start.as_usize(),
                end: value.text_range.end.as_usize(),
            };
            if atomic && range.start == range.end {
                return None;
            }
            Some(FlowSelectionUnit {
                kind,
                range,
                atomic,
            })
        },
    )
}

fn flow_unit_at(
    document: &FlowDocument,
    position: TextPosition,
    include_tables: bool,
) -> Option<FlowSelectionUnit> {
    flow_units(document)
        .filter(|unit| include_tables || unit.kind != FlowSelectionUnitKind::Table)
        .filter(|unit| {
            unit.range.contains(position.offset)
                || (position.affinity == Affinity::Upstream && unit.range.end == position.offset)
                || (position.affinity == Affinity::Downstream
                    && unit.range.start == position.offset)
        })
        .min_by_key(|unit| {
            let table_priority =
                usize::from(include_tables && unit.kind != FlowSelectionUnitKind::Table);
            (
                table_priority,
                unit.range.end.saturating_sub(unit.range.start),
            )
        })
}

fn flow_word_range_at(document: &FlowDocument, position: TextPosition) -> Option<TextRange> {
    let unit = flow_unit_at(document, position, false)?;
    if unit.atomic {
        return (unit.range.start < unit.range.end).then_some(unit.range);
    }
    let text = document.text().as_str();
    let mut scope = unit.range;
    if unit.kind == FlowSelectionUnitKind::Preformatted {
        let segment = text.get(scope.start..scope.end)?;
        let local = position
            .offset
            .saturating_sub(scope.start)
            .min(segment.len());
        let line_start = segment[..local].rfind('\n').map_or(0, |index| index + 1);
        let line_end = segment[local..]
            .find('\n')
            .map_or(segment.len(), |index| local + index);
        scope = TextRange {
            start: scope.start + line_start,
            end: scope.start + line_end,
        };
    }
    let segment = text.get(scope.start..scope.end)?;
    segment.unicode_word_indices().find_map(|(start, word)| {
        let range = TextRange {
            start: scope.start + start,
            end: scope.start + start + word.len(),
        };
        let selected = range.contains(position.offset)
            || (position.affinity == Affinity::Upstream && range.end == position.offset)
            || (position.affinity == Affinity::Downstream && range.start == position.offset);
        selected.then_some(range)
    })
}

fn flow_block_range_at(document: &FlowDocument, position: TextPosition) -> Option<TextRange> {
    if let Some(table) = flow_unit_at(document, position, true)
        && table.kind == FlowSelectionUnitKind::Table
    {
        return Some(table.range);
    }
    let unit = flow_unit_at(document, position, false)?;
    if unit.kind != FlowSelectionUnitKind::Preformatted {
        return Some(unit.range);
    }
    let text = document
        .text()
        .as_str()
        .get(unit.range.start..unit.range.end)?;
    let local = position
        .offset
        .saturating_sub(unit.range.start)
        .min(text.len());
    let start = text[..local].rfind('\n').map_or(0, |index| index + 1);
    let end = text[local..]
        .find('\n')
        .map_or(text.len(), |index| local + index);
    Some(TextRange {
        start: unit.range.start + start,
        end: unit.range.start + end,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionGranularity {
    Character,
    Word,
    Block,
}
fn selection_from_range(range: TextRange) -> Selection {
    Selection {
        anchor: TextPosition::new(range.start),
        active: TextPosition::new(range.end),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::markdown as flow_markdown;

    fn flow(source: &str) -> FlowDocument {
        flow_markdown::parse(source, Default::default())
            .unwrap()
            .document
    }

    #[test]
    fn flow_selection_extracts_exact_canonical_separators_and_rejects_invalid_utf8() {
        let document = flow("one\n\ntwo\n\n| A | B |\n|---|---|\n| C | D |");
        let text = document.text().as_str();
        let selection = Selection {
            anchor: TextPosition::new(0),
            active: TextPosition::new(text.len()),
        };
        assert_eq!(selected_text(&document, selection).unwrap(), text);
        assert!(text.contains("A\tB\nC\tD"));

        let unicode = flow("naïve");
        assert!(
            selected_text(
                &unicode,
                Selection {
                    anchor: TextPosition::new(3),
                    active: TextPosition::new(4),
                }
            )
            .is_none()
        );
    }

    #[test]
    fn flow_word_and_block_selection_use_canonical_semantic_units() {
        let document = flow(
            "pre**dic**table\n\n```text\nfirst line\nsecond line\n```\n\n| A | B |\n|---|---|\n| C | D |",
        );
        let text = document.text().as_str();
        let word = selection_at(
            &document,
            TextPosition::new(text.find("dic").unwrap() + 1),
            SelectionGranularity::Word,
        )
        .unwrap();
        assert_eq!(selected_text(&document, word).unwrap(), "predictable");

        let code = selection_at(
            &document,
            TextPosition::new(text.find("second").unwrap()),
            SelectionGranularity::Block,
        )
        .unwrap();
        assert_eq!(selected_text(&document, code).unwrap(), "second line");

        let table = selection_at(
            &document,
            TextPosition::new(text.find("C\tD").unwrap()),
            SelectionGranularity::Block,
        )
        .unwrap();
        assert_eq!(selected_text(&document, table).unwrap(), "A\tB\nC\tD");
    }

    #[test]
    fn flow_atomic_selections_snap_and_expand_to_complete_alternatives() {
        let document = flow("before ![quarterly chart](chart.png) after $alpha beta$");
        let text = document.text().as_str();
        let image_start = text.find("quarterly chart").unwrap();
        let image_end = image_start + "quarterly chart".len();
        let partial = Selection {
            anchor: TextPosition::new(image_start + 2),
            active: TextPosition::new(image_start + 5),
        };
        let normalized = normalize_flow_selection(&document, partial).unwrap();
        assert_eq!(
            normalized.range(),
            TextRange {
                start: image_start,
                end: image_end
            }
        );
        assert_eq!(normalized.anchor.affinity, Affinity::Downstream);
        assert_eq!(normalized.active.affinity, Affinity::Upstream);
        let enclosing = normalize_flow_selection(
            &document,
            Selection {
                anchor: TextPosition {
                    offset: 0,
                    affinity: Affinity::Upstream,
                },
                active: TextPosition::new(text.len()),
            },
        )
        .unwrap();
        assert_eq!(enclosing.anchor.affinity, Affinity::Upstream);
        assert_eq!(enclosing.active.affinity, Affinity::Downstream);
        let from_leading_edge = normalize_flow_selection(
            &document,
            Selection {
                anchor: TextPosition {
                    offset: image_start,
                    affinity: Affinity::Upstream,
                },
                active: TextPosition::new(image_start + 2),
            },
        )
        .unwrap();
        assert_eq!(from_leading_edge.anchor.affinity, Affinity::Downstream);
        assert_eq!(from_leading_edge.active.affinity, Affinity::Upstream);
        assert_eq!(
            selected_text(&document, partial).unwrap(),
            "quarterly chart"
        );

        let upstream = selection_at(
            &document,
            TextPosition {
                offset: image_start + 3,
                affinity: Affinity::Upstream,
            },
            SelectionGranularity::Character,
        )
        .unwrap();
        assert_eq!(upstream.anchor.offset, image_start);
        assert_eq!(upstream.anchor.affinity, Affinity::Downstream);
        let atomic_word = selection_at(
            &document,
            TextPosition::new(text.find("alpha").unwrap() + 1),
            SelectionGranularity::Word,
        )
        .unwrap();
        assert_eq!(selected_text(&document, atomic_word).unwrap(), "alpha beta");
    }

    #[test]
    fn flow_selection_ignores_decorative_atoms_and_selects_empty_code_lines() {
        let decorative = flow("a![](x)b");
        let word = selection_at(
            &decorative,
            TextPosition::new(1),
            SelectionGranularity::Word,
        )
        .unwrap();
        assert_eq!(selected_text(&decorative, word).unwrap(), "ab");

        let code = flow("```text\nfirst\n\nsecond\n```");
        let offset = code.text().as_str().find("\n\n").unwrap() + 1;
        let empty = selection_at(
            &code,
            TextPosition::new(offset),
            SelectionGranularity::Block,
        )
        .unwrap();
        assert!(empty.is_collapsed());
        assert_eq!(selected_text(&code, empty).unwrap(), "");

        let mut builder = crate::model::flow::FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        builder.append(root, paragraph).unwrap();
        let empty_document = builder.finish().unwrap();
        let empty_block = selection_at(
            &empty_document,
            TextPosition::new(0),
            SelectionGranularity::Block,
        )
        .unwrap();
        assert!(empty_block.is_collapsed());
    }

    #[test]
    fn flow_stable_selection_recovers_by_semantics_and_context() {
        let original = flow("# Heading\n\nselect this phrase\n\ntail");
        let start = original.text().as_str().find("this").unwrap();
        let stable = capture_flow_selection(
            &original,
            Selection {
                anchor: TextPosition {
                    offset: start,
                    affinity: Affinity::Upstream,
                },
                active: TextPosition::new(start + "this phrase".len()),
            },
        )
        .unwrap();
        let block_separator = original.text().as_str().find("\n\n").unwrap() + 1;
        let separator = capture_flow_selection(
            &original,
            Selection::collapsed(TextPosition::new(block_separator)),
        )
        .unwrap();
        let table_document = flow("| A | B |\n|---|---|\n| C | D |");
        let tab = table_document.text().as_str().find('\t').unwrap();
        assert!(
            capture_flow_selection(
                &table_document,
                Selection::collapsed(TextPosition::new(tab)),
            )
            .is_some()
        );

        let edited = flow("intro\n\n# Heading\n\nselect this phrase\n\ntail");
        let recovered_separator = recover_flow_selection(&edited, &separator).unwrap();
        assert!(matches!(
            edited.text().as_str().as_bytes()[recovered_separator.anchor.offset],
            b'\n' | b'\t'
        ));
        let recovered = recover_flow_selection(&edited, &stable).unwrap();
        assert_eq!(selected_text(&edited, recovered).unwrap(), "this phrase");
        assert_eq!(recovered.anchor.affinity, Affinity::Upstream);

        let unit_start = original.text().as_str().find("select").unwrap();
        let starts_at_boundary = capture_flow_selection(
            &original,
            Selection {
                anchor: TextPosition::new(unit_start),
                active: TextPosition::new(unit_start + "select".len()),
            },
        )
        .unwrap();
        let recovered_start = recover_flow_selection(&edited, &starts_at_boundary).unwrap();
        assert_eq!(selected_text(&edited, recovered_start).unwrap(), "select");

        let ambiguous = flow("select this phrase\n\nselect this phrase");
        assert!(recover_flow_selection(&ambiguous, &stable).is_none());
    }
}
