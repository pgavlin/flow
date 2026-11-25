use std::num::NonZeroU32;

use super::*;

mod responsive;

fn add(builder: &mut FlowBuilder, kind: NodeKind) -> NodeId {
    builder.add_node(kind).unwrap()
}

fn text(builder: &mut FlowBuilder, value: &str) -> NodeId {
    builder.add_text(value).unwrap()
}

fn append(builder: &mut FlowBuilder, parent: NodeId, child: NodeId) {
    builder.append(parent, child).unwrap();
}

fn append_text(builder: &mut FlowBuilder, parent: NodeId, value: &str) {
    let child = text(builder, value);
    append(builder, parent, child);
}

fn paragraph(builder: &mut FlowBuilder, value: &str) -> NodeId {
    let paragraph = add(builder, NodeKind::Paragraph);
    append_text(builder, paragraph, value);
    paragraph
}

fn append_paragraph(builder: &mut FlowBuilder, parent: NodeId, value: &str) {
    let child = paragraph(builder, value);
    append(builder, parent, child);
}

fn one() -> NonZeroU32 {
    NonZeroU32::new(1).unwrap()
}

fn cell(kind: CellKind, column_span: u32, row_span: u32) -> NodeKind {
    NodeKind::Cell(Cell {
        kind,
        column_start: u32::MAX,
        column_span: NonZeroU32::new(column_span).unwrap(),
        row_span: NonZeroU32::new(row_span).unwrap(),
        scope: None,
        explicit_headers: None,
    })
}

fn finish_with_root(builder: &mut FlowBuilder) -> NodeId {
    add(builder, NodeKind::Document)
}

fn anchor(builder: &mut FlowBuilder, name: &str, node: NodeId) {
    builder.add_anchor(AnchorId::new(name), node).unwrap();
}

fn anchored_node(document: &FlowDocument, name: &str) -> (NodeId, TextPosition) {
    let target = document.anchor(&AnchorId::new(name)).unwrap().target;
    (target.node, target.position)
}

#[test]
fn golden_rules_cover_inline_flow_breaks_and_empty_nodes() {
    struct Golden {
        name: &'static str,
        build: fn() -> FlowDocument,
        expected: &'static str,
    }

    fn inline() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let first = text(&mut builder, "hé");
        let wrapper = add(
            &mut builder,
            NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Strong,
                value: None,
            }),
        );
        let second = text(&mut builder, "llo");
        append(&mut builder, wrapper, second);
        append(&mut builder, paragraph, first);
        append(&mut builder, paragraph, wrapper);
        append(&mut builder, root, paragraph);
        builder.finish().unwrap()
    }

    fn flow() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let first = paragraph(&mut builder, "a");
        let marker = add(
            &mut builder,
            NodeKind::LocationMarker(LocationMarker {
                kind: LocationMarkerKind::Other,
                label: None,
            }),
        );
        let second = paragraph(&mut builder, "b");
        append(&mut builder, root, first);
        append(&mut builder, root, marker);
        append(&mut builder, root, second);
        builder.finish().unwrap()
    }

    fn breaks() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let first = text(&mut builder, "a");
        let opportunity = add(
            &mut builder,
            NodeKind::BreakOpportunity(BreakOpportunity {
                hyphen_when_broken: true,
            }),
        );
        let hard_break = add(&mut builder, NodeKind::HardBreak);
        let second = text(&mut builder, "β");
        append(&mut builder, paragraph, first);
        append(&mut builder, paragraph, opportunity);
        append(&mut builder, paragraph, hard_break);
        append(&mut builder, paragraph, second);
        append(&mut builder, root, paragraph);
        builder.finish().unwrap()
    }

    for golden in [
        Golden {
            name: "inline and text literal",
            build: inline,
            expected: "héllo",
        },
        Golden {
            name: "flow and empty",
            build: flow,
            expected: "a\n\nb",
        },
        Golden {
            name: "hard and discretionary breaks",
            build: breaks,
            expected: "a\nβ",
        },
    ] {
        let document = (golden.build)();
        assert_eq!(document.text().as_str(), golden.expected, "{}", golden.name);
        assert_eq!(
            document.get_node(document.root()).unwrap().text_range,
            TextRange::new(TextOffset::ZERO, TextOffset::new(golden.expected.len())),
            "{} root range",
            golden.name
        );
    }
}

#[test]
fn every_logical_text_rule_has_a_table_driven_golden() {
    struct Golden {
        rule: LogicalTextRule,
        build: fn() -> FlowDocument,
        document_text: &'static str,
        target_text: &'static str,
    }

    fn finish(mut builder: FlowBuilder, root: NodeId, target: NodeId) -> FlowDocument {
        anchor(&mut builder, "target", target);
        if target != root {
            builder.append(root, target).unwrap();
        }
        builder.finish().unwrap()
    }

    fn inline() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let target = paragraph(&mut builder, "x");
        finish(builder, root, target)
    }

    fn flow() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let first = paragraph(&mut builder, "a");
        let second = paragraph(&mut builder, "b");
        append(&mut builder, root, first);
        append(&mut builder, root, second);
        finish(builder, root, root)
    }

    fn table(target_kind: LogicalTextRule) -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let caption = add(&mut builder, NodeKind::TableCaption);
        append_paragraph(&mut builder, caption, "cap");
        append(&mut builder, table, caption);
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let row = add(&mut builder, NodeKind::Row);
        for value in ["a", "b"] {
            let cell = add(&mut builder, cell(CellKind::Data, 1, 1));
            append_paragraph(&mut builder, cell, value);
            append(&mut builder, row, cell);
        }
        append(&mut builder, group, row);
        append(&mut builder, table, group);
        append(&mut builder, root, table);
        let target = match target_kind {
            LogicalTextRule::TableRoot => table,
            LogicalTextRule::TableRows => group,
            LogicalTextRule::TableCells => row,
            _ => unreachable!(),
        };
        anchor(&mut builder, "target", target);
        builder.finish().unwrap()
    }

    fn table_root() -> FlowDocument {
        table(LogicalTextRule::TableRoot)
    }
    fn table_rows() -> FlowDocument {
        table(LogicalTextRule::TableRows)
    }
    fn table_cells() -> FlowDocument {
        table(LogicalTextRule::TableCells)
    }

    fn literal() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let target = text(&mut builder, "é");
        append(&mut builder, paragraph, target);
        append(&mut builder, root, paragraph);
        anchor(&mut builder, "target", target);
        builder.finish().unwrap()
    }

    fn hard_break() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let target = add(&mut builder, NodeKind::HardBreak);
        append(&mut builder, paragraph, target);
        append(&mut builder, root, paragraph);
        anchor(&mut builder, "target", target);
        builder.finish().unwrap()
    }

    fn opportunity() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let target = add(
            &mut builder,
            NodeKind::BreakOpportunity(BreakOpportunity {
                hyphen_when_broken: false,
            }),
        );
        append(&mut builder, paragraph, target);
        append(&mut builder, root, paragraph);
        anchor(&mut builder, "target", target);
        builder.finish().unwrap()
    }

    fn atomic() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let resource = builder
            .add_resource(Resource {
                identity: "i".into(),
                media_type: None,
            })
            .unwrap();
        let target = add(
            &mut builder,
            NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("alt").unwrap()),
                title: None,
                responsive: None,
                intrinsic_size: None,
            }),
        );
        finish(builder, root, target)
    }

    fn replacement() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let target = add(&mut builder, NodeKind::ThematicBreak);
        finish(builder, root, target)
    }

    fn empty() -> FlowDocument {
        let mut builder = FlowBuilder::new();
        let root = finish_with_root(&mut builder);
        let target = add(
            &mut builder,
            NodeKind::LocationMarker(LocationMarker {
                kind: LocationMarkerKind::Other,
                label: None,
            }),
        );
        finish(builder, root, target)
    }

    for golden in [
        Golden {
            rule: LogicalTextRule::InlineChildren,
            build: inline,
            document_text: "x",
            target_text: "x",
        },
        Golden {
            rule: LogicalTextRule::FlowChildren,
            build: flow,
            document_text: "a\n\nb",
            target_text: "a\n\nb",
        },
        Golden {
            rule: LogicalTextRule::TableRoot,
            build: table_root,
            document_text: "cap\n\na\tb",
            target_text: "cap\n\na\tb",
        },
        Golden {
            rule: LogicalTextRule::TableRows,
            build: table_rows,
            document_text: "cap\n\na\tb",
            target_text: "a\tb",
        },
        Golden {
            rule: LogicalTextRule::TableCells,
            build: table_cells,
            document_text: "cap\n\na\tb",
            target_text: "a\tb",
        },
        Golden {
            rule: LogicalTextRule::TextLiteral,
            build: literal,
            document_text: "é",
            target_text: "é",
        },
        Golden {
            rule: LogicalTextRule::HardBreak,
            build: hard_break,
            document_text: "\n",
            target_text: "\n",
        },
        Golden {
            rule: LogicalTextRule::BreakOpportunity,
            build: opportunity,
            document_text: "",
            target_text: "",
        },
        Golden {
            rule: LogicalTextRule::AtomicAlternative,
            build: atomic,
            document_text: "alt",
            target_text: "alt",
        },
        Golden {
            rule: LogicalTextRule::ObjectReplacement,
            build: replacement,
            document_text: "\u{fffc}",
            target_text: "\u{fffc}",
        },
        Golden {
            rule: LogicalTextRule::Empty,
            build: empty,
            document_text: "",
            target_text: "",
        },
    ] {
        let document = (golden.build)();
        let (target, position) = anchored_node(&document, "target");
        let target_node = document.get_node(target).unwrap();
        assert_eq!(target_node.kind.logical_text_rule(), golden.rule);
        assert_eq!(document.text().as_str(), golden.document_text);
        assert_eq!(
            document.get_text(target_node.text_range),
            Some(golden.target_text)
        );
        assert!(
            document
                .text()
                .as_str()
                .is_char_boundary(target_node.text_range.start.as_usize())
        );
        assert!(
            document
                .text()
                .as_str()
                .is_char_boundary(target_node.text_range.end.as_usize())
        );
        if !target_node.text_range.is_empty() {
            assert_eq!(position.affinity, Affinity::Downstream);
            assert_eq!(position.offset, target_node.text_range.start);
        }
    }
}

#[test]
fn atomic_alternatives_and_object_replacements_are_canonical() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let resource = builder
        .add_resource(Resource {
            identity: "image.png".into(),
            media_type: Some("image/png".into()),
        })
        .unwrap();

    let meaningful = add(
        &mut builder,
        NodeKind::Image(Image {
            resource,
            alternative: ImageAlternative::Text(NonEmptyText::new("alt").unwrap()),
            title: None,
            responsive: None,
            intrinsic_size: None,
        }),
    );
    let decorative = add(
        &mut builder,
        NodeKind::Image(Image {
            resource,
            alternative: ImageAlternative::Decorative,
            title: None,
            responsive: None,
            intrinsic_size: None,
        }),
    );
    let missing = add(
        &mut builder,
        NodeKind::Image(Image {
            resource,
            alternative: ImageAlternative::Missing,
            title: None,
            responsive: None,
            intrinsic_size: None,
        }),
    );
    let math = add(
        &mut builder,
        NodeKind::Math(Math {
            notation: MathNotation::Tex,
            source: "x^2".into(),
            display: MathDisplay::Inline,
        }),
    );
    let titled = add(
        &mut builder,
        NodeKind::UnsupportedObject(UnsupportedObject {
            placement: Placement::Inline,
            media_kind: MediaKind::Video,
            title: Some("movie".into()),
            size_hint: None,
            activation: None,
            reason: UnsupportedReason::NoNativePresenter,
        }),
    );
    let placeholder = add(
        &mut builder,
        NodeKind::UnsupportedObject(UnsupportedObject {
            placement: Placement::Inline,
            media_kind: MediaKind::Unknown,
            title: None,
            size_hint: None,
            activation: None,
            reason: UnsupportedReason::NoNativePresenter,
        }),
    );
    let fallback = add(
        &mut builder,
        NodeKind::UnsupportedObject(UnsupportedObject {
            placement: Placement::Inline,
            media_kind: MediaKind::Audio,
            title: Some("ignored".into()),
            size_hint: None,
            activation: None,
            reason: UnsupportedReason::NoNativePresenter,
        }),
    );
    let fallback_text = text(&mut builder, "fallback");
    append(&mut builder, fallback, fallback_text);

    let paragraph = add(&mut builder, NodeKind::Paragraph);
    for child in [
        meaningful,
        decorative,
        missing,
        math,
        titled,
        placeholder,
        fallback,
    ] {
        append(&mut builder, paragraph, child);
    }
    append(&mut builder, root, paragraph);
    let rule = add(&mut builder, NodeKind::ThematicBreak);
    append(&mut builder, root, rule);

    let titled_flow = add(
        &mut builder,
        NodeKind::UnsupportedObject(UnsupportedObject {
            placement: Placement::Flow,
            media_kind: MediaKind::Iframe,
            title: Some("empty fallback".into()),
            size_hint: None,
            activation: None,
            reason: UnsupportedReason::NoNativePresenter,
        }),
    );
    let empty_table = add(
        &mut builder,
        NodeKind::Table(Table {
            column_count: one(),
        }),
    );
    let empty_group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
    let empty_row = add(&mut builder, NodeKind::Row);
    let empty_cell_1 = add(&mut builder, cell(CellKind::Data, 1, 1));
    let empty_cell_2 = add(&mut builder, cell(CellKind::Data, 1, 1));
    append(&mut builder, empty_row, empty_cell_1);
    append(&mut builder, empty_row, empty_cell_2);
    append(&mut builder, empty_group, empty_row);
    append(&mut builder, empty_table, empty_group);
    append(&mut builder, titled_flow, empty_table);
    append(&mut builder, root, titled_flow);
    anchor(&mut builder, "suppressed-table", empty_table);

    for (name, node) in [
        ("meaningful", meaningful),
        ("decorative", decorative),
        ("missing", missing),
        ("math", math),
        ("titled", titled),
        ("placeholder", placeholder),
        ("fallback", fallback),
        ("rule", rule),
    ] {
        anchor(&mut builder, name, node);
    }

    let document = builder.finish().unwrap();
    assert_eq!(
        document.text().as_str(),
        "alt\u{fffc}x^2movie\u{fffc}fallback\n\n\u{fffc}\n\nempty fallback"
    );
    let (suppressed_table, suppressed_position) = anchored_node(&document, "suppressed-table");
    assert!(
        document
            .get_node(suppressed_table)
            .unwrap()
            .text_range
            .is_empty()
    );
    assert_eq!(suppressed_position.affinity, Affinity::Downstream);

    let (decorative, position) = anchored_node(&document, "decorative");
    assert!(document.get_node(decorative).unwrap().text_range.is_empty());
    assert_eq!(position.offset, TextOffset::new(3));
    assert_eq!(position.affinity, Affinity::Downstream);

    for (name, expected) in [
        ("meaningful", "alt"),
        ("missing", "\u{fffc}"),
        ("math", "x^2"),
        ("titled", "movie"),
        ("placeholder", "\u{fffc}"),
        ("fallback", "fallback"),
        ("rule", "\u{fffc}"),
    ] {
        let (node, _) = anchored_node(&document, name);
        assert_eq!(
            document
                .get_text(document.get_node(node).unwrap().text_range)
                .unwrap(),
            expected,
            "{name}"
        );
    }
}

#[test]
fn empty_positions_and_anchor_affinities_are_deterministic() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let marker = |builder: &mut FlowBuilder, name: &str| {
        let node = add(
            builder,
            NodeKind::LocationMarker(LocationMarker {
                kind: LocationMarkerKind::Other,
                label: None,
            }),
        );
        anchor(builder, name, node);
        node
    };

    let leading = marker(&mut builder, "leading");
    let first = paragraph(&mut builder, "a");
    let interstitial = marker(&mut builder, "interstitial");
    let second = paragraph(&mut builder, "b");
    let trailing = marker(&mut builder, "trailing");
    for child in [leading, first, interstitial, second, trailing] {
        append(&mut builder, root, child);
    }

    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), "a\n\nb");
    for (name, offset, affinity) in [
        ("leading", 0, Affinity::Downstream),
        ("interstitial", 3, Affinity::Downstream),
        ("trailing", 4, Affinity::Upstream),
    ] {
        let (node, position) = anchored_node(&document, name);
        assert_eq!(
            document.get_node(node).unwrap().text_range,
            TextRange::new(TextOffset::new(offset), TextOffset::new(offset))
        );
        assert_eq!(
            position,
            TextPosition::new(TextOffset::new(offset), affinity)
        );
    }
}

#[test]
fn a_completely_empty_nested_document_uses_zero_downstream() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let flow = add(&mut builder, NodeKind::Flow);
    let paragraph = add(&mut builder, NodeKind::Paragraph);
    let opportunity = add(
        &mut builder,
        NodeKind::BreakOpportunity(BreakOpportunity {
            hyphen_when_broken: false,
        }),
    );
    append(&mut builder, paragraph, opportunity);
    append(&mut builder, flow, paragraph);
    append(&mut builder, root, flow);
    for (name, node) in [("root", root), ("flow", flow), ("leaf", opportunity)] {
        anchor(&mut builder, name, node);
    }

    let document = builder.finish().unwrap();
    assert!(document.text().is_empty());
    for name in ["root", "flow", "leaf"] {
        let (node, position) = anchored_node(&document, name);
        assert!(document.get_node(node).unwrap().text_range.is_empty());
        assert_eq!(
            position,
            TextPosition::new(TextOffset::ZERO, Affinity::Downstream)
        );
    }
}

#[test]
fn empty_table_cells_and_rows_use_structural_slots() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let table = add(
        &mut builder,
        NodeKind::Table(Table {
            column_count: one(),
        }),
    );
    let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
    let first_row = add(&mut builder, NodeKind::Row);
    let first_cell = add(&mut builder, cell(CellKind::Data, 1, 1));
    let second_cell = add(&mut builder, cell(CellKind::Data, 1, 1));
    append(&mut builder, first_row, first_cell);
    append(&mut builder, first_row, second_cell);
    let second_row = add(&mut builder, NodeKind::Row);
    let third_cell = add(&mut builder, cell(CellKind::Data, 1, 1));
    append(&mut builder, second_row, third_cell);
    append(&mut builder, group, first_row);
    append(&mut builder, group, second_row);
    append(&mut builder, table, group);
    append(&mut builder, root, table);
    for (name, node) in [
        ("cell-1", first_cell),
        ("cell-2", second_cell),
        ("row-2", second_row),
        ("cell-3", third_cell),
    ] {
        anchor(&mut builder, name, node);
    }

    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), "\t\n");
    for (name, offset, affinity) in [
        ("cell-1", 0, Affinity::Downstream),
        ("cell-2", 1, Affinity::Downstream),
        ("row-2", 2, Affinity::Upstream),
        ("cell-3", 2, Affinity::Upstream),
    ] {
        let (node, position) = anchored_node(&document, name);
        assert_eq!(
            document.get_node(node).unwrap().text_range,
            TextRange::new(TextOffset::new(offset), TextOffset::new(offset))
        );
        assert_eq!(position.offset, TextOffset::new(offset));
        assert_eq!(position.affinity, affinity, "{name}");
    }
}

#[test]
fn anchors_remap_from_draft_order_to_final_preorder() {
    let mut builder = FlowBuilder::new();
    let marker = add(
        &mut builder,
        NodeKind::LocationMarker(LocationMarker {
            kind: LocationMarkerKind::Other,
            label: None,
        }),
    );
    anchor(&mut builder, "marker", marker);
    let root = finish_with_root(&mut builder);
    let content = paragraph(&mut builder, "x");
    append(&mut builder, root, content);
    append(&mut builder, root, marker);

    let document = builder.finish().unwrap();
    let (final_marker, position) = anchored_node(&document, "marker");
    assert_ne!(final_marker, marker);
    assert!(matches!(
        document.get_node(final_marker).unwrap().kind,
        NodeKind::LocationMarker(_)
    ));
    assert_eq!(
        position,
        TextPosition::new(TextOffset::new(1), Affinity::Upstream)
    );
}

#[test]
fn nested_empty_subtree_between_content_binds_downstream() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let first = paragraph(&mut builder, "a");
    let empty_flow = add(&mut builder, NodeKind::Flow);
    let empty_paragraph = add(&mut builder, NodeKind::Paragraph);
    let opportunity = add(
        &mut builder,
        NodeKind::BreakOpportunity(BreakOpportunity {
            hyphen_when_broken: false,
        }),
    );
    append(&mut builder, empty_paragraph, opportunity);
    append(&mut builder, empty_flow, empty_paragraph);
    let second = paragraph(&mut builder, "b");
    append(&mut builder, root, first);
    append(&mut builder, root, empty_flow);
    append(&mut builder, root, second);
    for (name, node) in [
        ("flow", empty_flow),
        ("paragraph", empty_paragraph),
        ("opportunity", opportunity),
    ] {
        anchor(&mut builder, name, node);
    }

    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), "a\n\nb");
    for name in ["flow", "paragraph", "opportunity"] {
        let (node, position) = anchored_node(&document, name);
        assert_eq!(
            document.get_node(node).unwrap().text_range,
            TextRange::new(TextOffset::new(3), TextOffset::new(3))
        );
        assert_eq!(position.affinity, Affinity::Downstream);
    }
}

#[test]
fn empty_row_groups_follow_table_caption_and_group_slots() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let table = add(
        &mut builder,
        NodeKind::Table(Table {
            column_count: one(),
        }),
    );
    let caption = add(&mut builder, NodeKind::TableCaption);
    append_paragraph(&mut builder, caption, "cap");
    append(&mut builder, table, caption);
    for index in 0..2 {
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let row = add(&mut builder, NodeKind::Row);
        let cell = add(&mut builder, cell(CellKind::Data, 1, 1));
        append(&mut builder, row, cell);
        append(&mut builder, group, row);
        append(&mut builder, table, group);
        anchor(&mut builder, &format!("group-{index}"), group);
    }
    append(&mut builder, root, table);

    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), "cap\n\n\n");
    for (name, offset) in [("group-0", 5), ("group-1", 6)] {
        let (group, position) = anchored_node(&document, name);
        assert!(document.get_node(group).unwrap().text_range.is_empty());
        assert_eq!(position.offset, TextOffset::new(offset));
        assert_eq!(position.affinity, Affinity::Upstream);
    }
}

#[test]
fn markdown_contract_example_has_expected_canonical_text() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let quotation = add(&mut builder, NodeKind::Quotation);
    let list = add(
        &mut builder,
        NodeKind::List(List {
            kind: ListKind::Ordered,
            start: 3,
            reversed: false,
        }),
    );
    let item = add(&mut builder, NodeKind::ListItem(ListItem { ordinal: None }));
    let first_paragraph = add(&mut builder, NodeKind::Paragraph);
    let strong = add(
        &mut builder,
        NodeKind::InlineSemantic(InlineSemantic {
            role: InlineRole::Strong,
            value: None,
        }),
    );
    append_text(&mut builder, strong, "Read");
    append(&mut builder, first_paragraph, strong);
    append_text(&mut builder, first_paragraph, " this.");
    let note_link = add(
        &mut builder,
        NodeKind::Link(Link {
            content: LinkContent::Phrasing,
            destination: Destination::Local(AnchorId::new("fn-n")),
            relation: LinkRelation::NoteReference {
                return_anchor: Some(AnchorId::new("fnref-n")),
            },
            title: None,
        }),
    );
    let superscript = add(
        &mut builder,
        NodeKind::InlineSemantic(InlineSemantic {
            role: InlineRole::Superscript,
            value: None,
        }),
    );
    append_text(&mut builder, superscript, "1");
    append(&mut builder, note_link, superscript);
    anchor(&mut builder, "fnref-n", note_link);
    append(&mut builder, first_paragraph, note_link);
    append(&mut builder, item, first_paragraph);

    let nested = add(
        &mut builder,
        NodeKind::List(List {
            kind: ListKind::Unordered,
            start: 1,
            reversed: false,
        }),
    );
    let task = add(&mut builder, NodeKind::ListItem(ListItem { ordinal: None }));
    let task_paragraph = add(&mut builder, NodeKind::Paragraph);
    let checkbox = add(
        &mut builder,
        NodeKind::Checkbox(Checkbox {
            state: CheckboxState::Checked,
        }),
    );
    append(&mut builder, task_paragraph, checkbox);
    append_text(&mut builder, task_paragraph, "carefully");
    append(&mut builder, task, task_paragraph);
    append(&mut builder, nested, task);
    append(&mut builder, item, nested);
    append(&mut builder, list, item);
    append(&mut builder, quotation, list);

    let note = add(&mut builder, NodeKind::Section);
    builder
        .set_semantics(
            note,
            Semantics {
                roles: vec![SemanticRole::Known(KnownRole::Footnote)],
                ..Semantics::default()
            },
        )
        .unwrap();
    append_paragraph(&mut builder, note, "A note.");
    anchor(&mut builder, "fn-n", note);
    append(&mut builder, quotation, note);
    append(&mut builder, root, quotation);

    let document = builder.finish().unwrap();
    assert_eq!(
        document.text().as_str(),
        "Read this.1\n\ncarefully\n\nA note."
    );
    let checkbox = document
        .preorder(document.root())
        .find(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Checkbox(_)))
        .unwrap();
    assert!(document.get_node(checkbox).unwrap().text_range.is_empty());
    let (definition, _) = anchored_node(&document, "fn-n");
    assert!(matches!(
        &document.get_node(definition).unwrap().kind,
        NodeKind::Section
    ));
    assert_eq!(
        document.get_node(definition).unwrap().semantics.roles,
        vec![SemanticRole::Known(KnownRole::Footnote)]
    );
    let (reference, _) = anchored_node(&document, "fnref-n");
    assert!(matches!(
        &document.get_node(reference).unwrap().kind,
        NodeKind::Link(Link {
            relation: LinkRelation::NoteReference { .. },
            ..
        })
    ));
}

#[test]
fn flow_semantic_xhtml_contract_example_has_expected_canonical_text() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let resource = builder
        .add_resource(Resource {
            identity: "/assets/abc".into(),
            media_type: Some("image/png".into()),
        })
        .unwrap();
    let figure = add(&mut builder, NodeKind::Figure);
    let image = add(
        &mut builder,
        NodeKind::Image(Image {
            resource,
            alternative: ImageAlternative::Text(NonEmptyText::new("Quarterly revenue").unwrap()),
            title: None,
            responsive: None,
            intrinsic_size: Some(IntrinsicSize {
                width: PositiveCssPixels::new(640.0).unwrap(),
                height: PositiveCssPixels::new(360.0).unwrap(),
            }),
        }),
    );
    anchor(&mut builder, "chart", figure);
    append(&mut builder, figure, image);
    let caption = add(&mut builder, NodeKind::Caption);
    let caption_paragraph = add(&mut builder, NodeKind::Paragraph);
    append_text(&mut builder, caption_paragraph, "Revenue in ");
    let strong = add(
        &mut builder,
        NodeKind::InlineSemantic(InlineSemantic {
            role: InlineRole::Strong,
            value: None,
        }),
    );
    append_text(&mut builder, strong, "2026");
    append(&mut builder, caption_paragraph, strong);
    append_text(&mut builder, caption_paragraph, ".");
    append(&mut builder, caption, caption_paragraph);
    append(&mut builder, figure, caption);
    append(&mut builder, root, figure);

    let table = add(
        &mut builder,
        NodeKind::Table(Table {
            column_count: NonZeroU32::new(2).unwrap(),
        }),
    );
    let table_caption = add(&mut builder, NodeKind::TableCaption);
    append_paragraph(&mut builder, table_caption, "Results");
    append(&mut builder, table, table_caption);
    for (group_kind, rows) in [
        (
            RowGroup::Head,
            vec![vec![
                (CellKind::Header, "Name", 1),
                (CellKind::Header, "Count", 1),
            ]],
        ),
        (
            RowGroup::Body,
            vec![
                vec![(CellKind::Data, "One", 2), (CellKind::Data, "42", 1)],
                vec![(CellKind::Data, "43", 1)],
            ],
        ),
    ] {
        let group = add(&mut builder, NodeKind::RowGroup(group_kind));
        for cells in rows {
            let row = add(&mut builder, NodeKind::Row);
            for (kind, value, row_span) in cells {
                let cell_node = add(&mut builder, cell(kind, 1, row_span));
                append_paragraph(&mut builder, cell_node, value);
                append(&mut builder, row, cell_node);
            }
            append(&mut builder, group, row);
        }
        append(&mut builder, table, group);
    }
    anchor(&mut builder, "results", table);
    append(&mut builder, root, table);

    let document = builder.finish().unwrap();
    assert_eq!(
        document.text().as_str(),
        "Quarterly revenue\n\nRevenue in 2026.\n\nResults\n\nName\tCount\nOne\t42\n43"
    );
    let (figure, _) = anchored_node(&document, "chart");
    assert!(matches!(
        document.get_node(figure).unwrap().kind,
        NodeKind::Figure
    ));
    let (table, _) = anchored_node(&document, "results");
    assert!(matches!(
        document.get_node(table).unwrap().kind,
        NodeKind::Table(Table { column_count }) if column_count.get() == 2
    ));
    let body_cells = document
        .preorder(table)
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Cell(cell) if cell.kind == CellKind::Data => Some(cell.column_start),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(body_cells, vec![0, 1, 1]);
}

#[test]
fn canonical_finalization_handles_deep_trees_iteratively() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let paragraph = add(&mut builder, NodeKind::Paragraph);
    append(&mut builder, root, paragraph);
    let mut parent = paragraph;
    for _ in 0..10_000 {
        let inline = add(&mut builder, NodeKind::Inline);
        append(&mut builder, parent, inline);
        parent = inline;
    }
    append_text(&mut builder, parent, "deep");
    assert_eq!(builder.finish().unwrap().text().as_str(), "deep");
}

#[test]
fn epub_contract_example_has_expected_text_and_trailing_marker_affinity() {
    let mut builder = FlowBuilder::new();
    let root = finish_with_root(&mut builder);
    let section = add(&mut builder, NodeKind::Section);
    builder
        .set_semantics(
            section,
            Semantics {
                roles: vec![SemanticRole::Known(KnownRole::Chapter)],
                ..Semantics::default()
            },
        )
        .unwrap();
    anchor(&mut builder, "c1", section);
    let heading = add(
        &mut builder,
        NodeKind::Heading(Heading {
            level: HeadingLevel::H2,
        }),
    );
    append_text(&mut builder, heading, "الفصل");
    append(&mut builder, section, heading);

    let paragraph = add(&mut builder, NodeKind::Paragraph);
    let ruby = add(&mut builder, NodeKind::Ruby);
    let segment = add(&mut builder, NodeKind::RubySegment);
    let bases = add(&mut builder, NodeKind::RubyBaseLevel);
    let base = add(&mut builder, NodeKind::RubyBase);
    append_text(&mut builder, base, "漢");
    append(&mut builder, bases, base);
    append(&mut builder, segment, bases);
    let annotations = add(&mut builder, NodeKind::RubyAnnotationLevel);
    let left = add(&mut builder, NodeKind::RubyFallback);
    append_text(&mut builder, left, "(");
    append(&mut builder, annotations, left);
    let annotation = add(
        &mut builder,
        NodeKind::RubyAnnotation(RubyAnnotation { base_span: one() }),
    );
    append_text(&mut builder, annotation, "かん");
    append(&mut builder, annotations, annotation);
    let right = add(&mut builder, NodeKind::RubyFallback);
    append_text(&mut builder, right, ")");
    append(&mut builder, annotations, right);
    append(&mut builder, segment, annotations);
    append(&mut builder, ruby, segment);
    append(&mut builder, paragraph, ruby);
    append_text(&mut builder, paragraph, " ");
    let link = add(
        &mut builder,
        NodeKind::Link(Link {
            content: LinkContent::Phrasing,
            destination: Destination::Document {
                document: DocumentReference::PublicationItem("notes.xhtml".into()),
                anchor: Some(AnchorId::new("n1")),
            },
            relation: LinkRelation::NoteReference {
                return_anchor: None,
            },
            title: None,
        }),
    );
    append_text(&mut builder, link, "1");
    append(&mut builder, paragraph, link);
    append(&mut builder, section, paragraph);

    let marker = add(
        &mut builder,
        NodeKind::LocationMarker(LocationMarker {
            kind: LocationMarkerKind::PublicationPagebreak,
            label: Some("12".into()),
        }),
    );
    anchor(&mut builder, "p12", marker);
    append(&mut builder, section, marker);
    append(&mut builder, root, section);

    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), "الفصل\n\n漢(かん) 1");
    let (chapter, _) = anchored_node(&document, "c1");
    assert_eq!(
        document.get_node(chapter).unwrap().semantics.roles,
        vec![SemanticRole::Known(KnownRole::Chapter)]
    );
    assert!(
        document
            .preorder(chapter)
            .any(|id| matches!(document.get_node(id).unwrap().kind, NodeKind::Ruby))
    );
    let (marker, position) = anchored_node(&document, "p12");
    assert!(document.get_node(marker).unwrap().text_range.is_empty());
    assert_eq!(position.offset, document.text().len());
    assert_eq!(position.affinity, Affinity::Upstream);
}
