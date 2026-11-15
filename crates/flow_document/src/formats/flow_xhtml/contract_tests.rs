//! Behavioral coverage for the synthetic flow semantic XHTML fixture corpus.

use super::{XhtmlOptions, fixtures, import};
use crate::model::flow::*;

fn document(fixture: fixtures::Fixture) -> FlowDocument {
    let outcome = import(fixture.source, XhtmlOptions::default())
        .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
    assert_eq!(
        outcome
            .diagnostics
            .iter()
            .map(|item| &item.kind)
            .collect::<Vec<_>>(),
        fixture.diagnostics.iter().collect::<Vec<_>>(),
        "unexpected diagnostics for {}",
        fixture.name,
    );
    outcome.document
}

fn nodes(document: &FlowDocument) -> impl Iterator<Item = &Node> {
    document
        .preorder(document.root())
        .map(|id| document.get_node(id).unwrap())
}

fn at<'a>(document: &'a FlowDocument, anchor: &str) -> &'a Node {
    let id = document.anchor(&AnchorId::new(anchor)).unwrap().target.node;
    document.get_node(id).unwrap()
}

fn text<'a>(document: &'a FlowDocument, anchor: &str) -> &'a str {
    document.get_text(at(document, anchor).text_range).unwrap()
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "Test host inventories on-disk fixtures"
)]
fn every_fixture_is_registered_and_checked_at_both_supported_versions() {
    fn inventory(directory: &std::path::Path, names: &mut Vec<String>, root: &std::path::Path) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                inventory(&path, names, root);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "xhtml")
            {
                names.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flow-semantic-xhtml");
    let mut disk = Vec::new();
    inventory(&root, &mut disk, &root);
    let mut registered = fixtures::all()
        .map(|fixture| fixture.name.to_owned())
        .collect::<Vec<_>>();
    disk.sort();
    registered.sort();
    assert_eq!(
        disk, registered,
        "every fixture must participate in the test suite"
    );

    for fixture in fixtures::all() {
        let legacy = document(*fixture);
        let current = import(
            fixture.source,
            XhtmlOptions {
                canonical_version: super::CURRENT_FLOW_SEMANTIC_XHTML_VERSION,
                ..XhtmlOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            legacy.text().as_str(),
            current.document.text().as_str(),
            "{}",
            fixture.name
        );
        assert_eq!(
            current
                .diagnostics
                .iter()
                .map(|item| &item.kind)
                .collect::<Vec<_>>(),
            fixture.diagnostics.iter().collect::<Vec<_>>(),
            "{}",
            fixture.name
        );
        assert!(nodes(&legacy).count() > 1, "{}", fixture.name);
    }
}

#[test]
fn serialization_decodes_entities_and_cdata_without_reading_metadata() {
    let document = document(fixtures::SERIALIZATION);
    assert_eq!(
        text(&document, "entities"),
        "A & B < C > D \"quoted\" 'single' \u{a0} λ."
    );
    assert_eq!(text(&document, "cdata"), "Literal <tag> & text.");
    assert!(!document.text().as_str().contains("Comments"));
}

#[test]
fn namespace_and_xml_language_are_preserved() {
    let namespaced = document(fixtures::NAMESPACE);
    assert_eq!(namespaced.text().as_str(), "Namespaced prose.");
    assert_eq!(
        at(&namespaced, "namespace").semantics.language,
        LanguageOverride::Tag(LanguageTag::parse("en").unwrap())
    );
}

#[test]
fn blocks_keep_all_heading_levels_containers_quotes_and_breaks() {
    let document = document(fixtures::BLOCKS);
    let levels = nodes(&document)
        .filter_map(|node| match node.kind {
            NodeKind::Heading(heading) => Some(heading.level.get()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(levels, [1, 2, 3, 4, 5, 6]);
    for anchor in [
        "group", "section", "article", "main", "header", "footer", "aside", "nav", "address",
    ] {
        assert!(
            matches!(at(&document, anchor).kind, NodeKind::Flow),
            "{anchor}"
        );
    }
    assert_eq!(
        nodes(&document)
            .filter(|node| matches!(node.kind, NodeKind::Quotation))
            .count(),
        2
    );
    assert!(matches!(
        at(&document, "break").kind,
        NodeKind::ThematicBreak
    ));
    assert!(document.anchor(&AnchorId::new("heading-1")).is_some());
    assert!(
        document.anchor(&AnchorId::new("heading-two")).is_none(),
        "no generated heading IDs"
    );
    assert_eq!(
        text(&document, "quote"),
        "Outer quotation.\n\nInner quotation.\n\nQuotation ending."
    );
}

#[test]
fn inline_roles_and_time_metadata_remain_semantic() {
    let document = document(fixtures::INLINE);
    let semantic = nodes(&document)
        .filter_map(|node| match &node.kind {
            NodeKind::InlineSemantic(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        semantic.iter().map(|value| value.role).collect::<Vec<_>>(),
        [
            InlineRole::Emphasis,
            InlineRole::Strong,
            InlineRole::Code,
            InlineRole::Subscript,
            InlineRole::Superscript,
            InlineRole::Abbreviation,
            InlineRole::Citation,
            InlineRole::Quotation,
            InlineRole::Mark,
            InlineRole::Time,
        ]
    );
    assert_eq!(
        semantic.last().unwrap().value.as_deref(),
        Some("2030-04-05")
    );
    assert!(matches!(at(&document, "neutral").kind, NodeKind::Inline));
    assert_eq!(
        document.text().as_str(),
        "Plain emphasis importance code H2O x2 ABC A made-up book “A quotation” highlight fifth day.\nAfter the break."
    );
}

#[test]
fn metadata_preserves_language_overrides_resets_and_direction() {
    let document = document(fixtures::METADATA);
    for (anchor, language) in [
        ("english", "en"),
        ("arabic", "ar"),
        ("latin", "en"),
        ("xml-language", "ja"),
        ("precedence", "fr"),
    ] {
        assert_eq!(
            at(&document, anchor).semantics.language,
            LanguageOverride::Tag(LanguageTag::parse(language).unwrap()),
            "{anchor}"
        );
    }
    assert_eq!(
        at(&document, "inherited").semantics.language,
        LanguageOverride::Inherit
    );
    assert_eq!(
        at(&document, "unknown").semantics.language,
        LanguageOverride::Unknown
    );
    let styles = document.resolve_styles(StyleContextKey::default());
    for (anchor, direction) in [
        ("english", BaseDirection::LeftToRight),
        ("inherited", BaseDirection::LeftToRight),
        ("arabic", BaseDirection::RightToLeft),
        ("latin", BaseDirection::LeftToRight),
        ("automatic", BaseDirection::Auto),
    ] {
        let id = document.anchor(&AnchorId::new(anchor)).unwrap().target.node;
        let style = styles
            .get_computed_style(styles.style_for_node(id).unwrap())
            .unwrap();
        assert_eq!(style.bidi().direction, direction, "{anchor}");
    }
}

#[test]
fn whitespace_collapses_across_inline_boundaries_but_preserves_nonbreaking_space() {
    let document = document(fixtures::WHITESPACE);
    assert_eq!(text(&document, "collapsed"), "one two three four five\nsix");
    assert_eq!(text(&document, "nonbreaking"), "a\u{a0}b");
    assert_eq!(text(&document, "inline-code"), "a b c");
}

#[test]
fn links_distinguish_local_relative_and_external_destinations() {
    let document = document(fixtures::LINKS);
    for anchor in ["local-link", "encoded-link"] {
        let NodeKind::Link(link) = &at(&document, anchor).kind else {
            panic!("{anchor}");
        };
        assert_eq!(
            link.destination,
            Destination::Local(AnchorId::new("link-target"))
        );
    }
    for (anchor, path, target) in [
        ("relative-link", "chapter.xhtml", None),
        ("relative-anchor", "chapter.xhtml", Some("part 2")),
    ] {
        let NodeKind::Link(link) = &at(&document, anchor).kind else {
            panic!("{anchor}");
        };
        assert_eq!(
            link.destination,
            Destination::Document {
                document: DocumentReference::Relative(path.into()),
                anchor: target.map(AnchorId::new),
            }
        );
    }
    for (anchor, uri) in [
        ("external-link", "https://example.test/search?a=1&b=2"),
        ("mail-link", "mailto:author@example.test"),
    ] {
        let NodeKind::Link(link) = &at(&document, anchor).kind else {
            panic!("{anchor}");
        };
        assert_eq!(link.destination, Destination::External(uri.into()));
        assert_eq!(link.relation, LinkRelation::Ordinary);
    }
    assert!(matches!(at(&document, "linkless").kind, NodeKind::Inline));
    let image_link = at(&document, "image-link");
    assert!(matches!(image_link.kind, NodeKind::Link(_)));
    assert!(matches!(
        document.get_node(image_link.children[0]).unwrap().kind,
        NodeKind::Image(_)
    ));
}

#[test]
fn lists_preserve_numbering_and_nested_items() {
    let document = document(fixtures::LISTS);
    assert!(matches!(
        at(&document, "descending").kind,
        NodeKind::List(List {
            kind: ListKind::Ordered,
            start: 5,
            reversed: true
        })
    ));
    assert!(matches!(
        at(&document, "default-list").kind,
        NodeKind::List(List {
            start: 1,
            reversed: false,
            ..
        })
    ));
    assert!(matches!(
        at(&document, "negative-list").kind,
        NodeKind::List(List { start: -2, .. })
    ));
    assert!(matches!(
        at(&document, "first-item").kind,
        NodeKind::ListItem(ListItem { ordinal: None })
    ));
    assert!(matches!(
        at(&document, "reset-item").kind,
        NodeKind::ListItem(ListItem { ordinal: Some(2) })
    ));
    assert!(matches!(
        at(&document, "nested").kind,
        NodeKind::List(List {
            kind: ListKind::Unordered,
            ..
        })
    ));
    assert!(text(&document, "reset-item").contains("Nested point."));
}

#[test]
fn definitions_keep_multiple_terms_definitions_and_paragraphs() {
    let document = document(fixtures::DEFINITIONS);
    assert!(matches!(
        at(&document, "definitions").kind,
        NodeKind::DefinitionList
    ));
    let terms = nodes(&document)
        .filter(|node| matches!(node.kind, NodeKind::Term))
        .count();
    let definitions = nodes(&document)
        .filter(|node| matches!(node.kind, NodeKind::Definition))
        .count();
    assert_eq!((terms, definitions), (3, 3));
    assert!(
        document
            .text()
            .as_str()
            .contains("A second definition paragraph.")
    );
    assert!(
        document
            .text()
            .as_str()
            .contains("An alternative definition.")
    );
}

#[test]
fn preformatted_text_preserves_spacing_language_selection_and_diagram_source() {
    let document = document(fixtures::PREFORMATTED);
    for (anchor, language, kind) in [
        ("plain", None, PreformattedKind::PlainText),
        ("rust", Some("rust"), PreformattedKind::SourceCode),
        ("unspecified", None, PreformattedKind::SourceCode),
        (
            "first-language",
            Some("python"),
            PreformattedKind::SourceCode,
        ),
        ("mermaid", Some("mermaid"), PreformattedKind::MermaidSource),
    ] {
        let NodeKind::Preformatted(value) = &at(&document, anchor).kind else {
            panic!("{anchor}");
        };
        assert_eq!(value.language.as_deref(), language, "{anchor}");
        assert_eq!(value.content_kind, kind, "{anchor}");
    }
    assert_eq!(text(&document, "plain"), "left    right\nup      down");
    assert_eq!(
        text(&document, "rust"),
        "fn sample() {\n  let limit = 2 < 3;\n}"
    );
    assert_eq!(text(&document, "unspecified"), "x < y && y > z");
    assert_eq!(text(&document, "mermaid"), "graph TD\n  Seed --> Leaf");
}

#[test]
fn images_keep_alternatives_dimensions_and_deduplicate_resource_identities() {
    let document = document(fixtures::IMAGES);
    let image = |anchor| match &at(&document, anchor).kind {
        NodeKind::Image(image) => image,
        _ => panic!("expected image"),
    };
    assert!(matches!(
        image("meaningful").alternative,
        ImageAlternative::Text(_)
    ));
    let size = image("meaningful").intrinsic_size.unwrap();
    assert_eq!((size.width.get(), size.height.get()), (640.0, 360.0));
    assert_eq!(
        image("decorative").alternative,
        ImageAlternative::Decorative
    );
    assert_eq!(image("meaningful").resource, image("reused").resource);
    assert_eq!(image("reused").intrinsic_size.unwrap().width.get(), 12.5);
    assert!(image("remote").intrinsic_size.is_none());
    assert_eq!(document.resources().len(), 3);
    assert_eq!(
        document
            .get_resource(image("remote").resource)
            .unwrap()
            .identity,
        "https://images.example.test/grid.png"
    );
}

#[test]
fn figures_preserve_caption_position_and_block_content() {
    let document = document(fixtures::FIGURES);
    let after = at(&document, "caption-after");
    let before = at(&document, "caption-before");
    assert!(matches!(after.kind, NodeKind::Figure));
    assert!(matches!(
        document.get_node(after.children[0]).unwrap().kind,
        NodeKind::Image(_)
    ));
    assert!(matches!(
        document.get_node(after.children[1]).unwrap().kind,
        NodeKind::Caption
    ));
    assert!(matches!(
        document.get_node(before.children[0]).unwrap().kind,
        NodeKind::Caption
    ));
    assert_eq!(
        text(&document, "after-caption"),
        "A plan of the plots.\n\nEach plot is fictional."
    );
}

#[test]
fn tables_keep_groups_header_semantics_spans_and_implicit_body_rows() {
    let document = document(fixtures::TABLES);
    let grouped = at(&document, "grouped");
    assert!(
        matches!(grouped.kind, NodeKind::Table(Table { column_count }) if column_count.get() == 3)
    );
    let groups = grouped
        .children
        .iter()
        .map(|id| &document.get_node(*id).unwrap().kind)
        .collect::<Vec<_>>();
    assert!(matches!(
        groups.as_slice(),
        [
            NodeKind::TableCaption,
            NodeKind::RowGroup(RowGroup::Head),
            NodeKind::RowGroup(RowGroup::Body),
            NodeKind::RowGroup(RowGroup::Foot)
        ]
    ));
    let cells = nodes(&document)
        .filter_map(|node| match &node.kind {
            NodeKind::Cell(cell) => Some(cell),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cells[0].scope, Some(HeaderScope::Column));
    assert_eq!(cells[3].kind, CellKind::Header);
    assert_eq!(cells[3].row_span.get(), 2);
    assert_eq!(cells[6].column_start, 1, "skip the active row span");
    assert_eq!(cells[9].column_span.get(), 2);
    assert!(
        matches!(at(&document, "direct").kind, NodeKind::Table(Table { column_count }) if column_count.get() == 2)
    );
    assert!(text(&document, "grouped").contains("North\t12\tmm\n13\tmm"));
    assert_eq!(text(&document, "direct"), "Left\tRight");
}

#[test]
fn math_uses_authoritative_tex_not_generated_presentation() {
    let document = document(fixtures::MATH);
    let math = nodes(&document)
        .filter_map(|node| match &node.kind {
            NodeKind::Math(math) => Some(math),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(math.len(), 3);
    assert_eq!(
        (math[0].source.as_str(), math[0].display),
        ("a+b", MathDisplay::Inline)
    );
    assert_eq!(
        (math[1].source.as_str(), math[1].display),
        ("x < y", MathDisplay::Inline)
    );
    assert_eq!(
        (math[2].source.as_str(), math[2].display),
        ("\\frac{a}{b}", MathDisplay::Block)
    );
    assert!(math.iter().all(|math| math.notation == MathNotation::Tex));
    assert_eq!(
        document.text().as_str(),
        "A sum a+b and a comparison x < y.\n\n\\frac{a}{b}"
    );
    assert!(!document.text().as_str().contains("generated"));
}

#[test]
fn disclosures_keep_initial_state_summary_and_nested_body() {
    let document = document(fixtures::DISCLOSURES);
    for (anchor, initially_open) in [("open", true), ("closed", false)] {
        let node = at(&document, anchor);
        assert!(
            matches!(node.kind, NodeKind::Disclosure(Disclosure { initially_open: value }) if value == initially_open)
        );
        assert!(matches!(
            document.get_node(node.children[0]).unwrap().kind,
            NodeKind::Summary
        ));
    }
    assert_eq!(text(&document, "open-summary"), "Open summary");
    assert!(text(&document, "closed").contains("Body list item."));
}

#[test]
fn media_keeps_activation_precedence_size_hints_titles_and_block_fallbacks() {
    let document = document(fixtures::MEDIA);
    let media = |anchor| match &at(&document, anchor).kind {
        NodeKind::UnsupportedObject(object) => object,
        _ => panic!("expected media"),
    };
    assert_eq!(media("frame").media_kind, MediaKind::Iframe);
    assert_eq!(media("frame").title.as_deref(), Some("Sample frame"));
    assert_eq!(media("frame").size_hint.unwrap().width.get(), 560.0);
    assert_eq!(
        media("frame").activation,
        Some(Destination::External(
            "https://media.example.test/frame".into()
        ))
    );
    for (anchor, kind, path) in [
        ("audio", MediaKind::Audio, "media/direct.ogg"),
        ("video", MediaKind::Video, "media/first.webm"),
    ] {
        assert_eq!(media(anchor).media_kind, kind);
        assert_eq!(
            media(anchor).activation,
            Some(Destination::Document {
                document: DocumentReference::Relative(path.into()),
                anchor: None,
            })
        );
        assert_eq!(media(anchor).reason, UnsupportedReason::NoNativePresenter);
    }
    assert_eq!(media("video").size_hint.unwrap().height.get(), 180.0);
    assert_eq!(
        document.text().as_str(),
        "Sample frame\n\nAudio fallback.\n\nVideo fallback."
    );
    assert!(!document.text().as_str().contains("captions"));
}

#[test]
fn regression_table_spans_are_clamped_and_place_cells_after_active_spans() {
    let document = document(fixtures::TABLE_SPANS);
    let widths = nodes(&document)
        .filter_map(|node| match node.kind {
            NodeKind::Table(table) => Some(table.column_count.get()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(widths, [2, 3]);
    let cells = nodes(&document)
        .filter_map(|node| match &node.kind {
            NodeKind::Cell(cell) => Some(cell),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cells[0].row_span.get(), 1);
    assert_eq!(cells[4].column_start, 1);
    assert_eq!(cells[4].column_span.get(), 2);
}

#[test]
fn regression_orphan_table_parts_keep_reading_order_and_images_without_a_table() {
    let document = document(fixtures::ORPHAN_TABLE_PARTS);
    assert!(!nodes(&document).any(|node| matches!(node.kind, NodeKind::Table(_))));
    assert!(nodes(&document).any(|node| matches!(node.kind, NodeKind::Image(_))));
    let text = document.text().as_str();
    assert!(text.find("A detached cell.").unwrap() < text.find("Another detached cell.").unwrap());
}

#[test]
fn regression_root_code_gets_a_preformatted_wrapper_without_losing_source() {
    let document = document(fixtures::ROOT_CODE);
    assert!(nodes(&document).any(|node| matches!(
        &node.kind,
        NodeKind::Preformatted(Preformatted {
            content_kind: PreformattedKind::MermaidSource,
            ..
        })
    )));
    assert_eq!(document.text().as_str(), "graph TD\n  Button --> Lamp");
}

#[test]
fn regression_invalid_metadata_keeps_first_anchor_and_distinct_missing_image_alternative() {
    let document = document(fixtures::INVALID_METADATA);
    assert_eq!(text(&document, "duplicate"), "First target.");
    assert_eq!(
        at(&document, "duplicate").semantics.language,
        LanguageOverride::Inherit
    );
    for anchor in ["missing-alt", "invalid-size", "incomplete-size"] {
        let NodeKind::Image(image) = &at(&document, anchor).kind else {
            panic!("{anchor}");
        };
        assert!(image.intrinsic_size.is_none());
        if anchor == "missing-alt" {
            assert_eq!(image.alternative, ImageAlternative::Missing);
        } else {
            assert!(matches!(image.alternative, ImageAlternative::Text(_)));
        }
    }
}

#[test]
fn regression_anonymous_paragraphs_and_wrapped_phrasing_preserve_content() {
    let document = document(fixtures::ANONYMOUS_PARAGRAPHS);
    assert_eq!(
        document.text().as_str(),
        "Leading text with emphasis.\n\nContainer text.\n\nAn explicit paragraph.\n\nTrailing container text.\n\nA wrapped summary.\n\nA disclosure body.\n\nA wrapped term.\n\nA definition."
    );
}

#[test]
fn regression_unsupported_attributes_do_not_add_presentation_or_semantics() {
    let document = document(fixtures::IGNORED_ATTRIBUTES);
    assert_eq!(
        text(&document, "attributes"),
        "Unstyled text. Not mathematics. An undated label."
    );
    assert!(!nodes(&document).any(|node| matches!(node.kind, NodeKind::Math(_))));
    let NodeKind::InlineSemantic(time) = &at(&document, "undated").kind else {
        panic!("expected time");
    };
    assert_eq!(time.role, InlineRole::Time);
    assert_eq!(time.value, None);
    let NodeKind::Link(link) = &at(&document, "plain-link").kind else {
        panic!("expected link");
    };
    assert_eq!(link.title, None);
    assert_eq!(link.relation, LinkRelation::Ordinary);
    for anchor in ["unit-size", "negative-size", "infinite-size", "nan-size"] {
        let NodeKind::Image(image) = &at(&document, anchor).kind else {
            panic!("{anchor}");
        };
        assert_eq!(image.title, None);
        assert!(image.intrinsic_size.is_none(), "{anchor}");
    }
    assert!(!document.text().as_str().contains("should_not_run"));
}

#[test]
fn structurally_invalid_documents_are_not_made_valid_by_well_formed_xml() {
    for source in [
        "<ul/>",
        "<dl><dd><p>A definition without a term.</p></dd></dl>",
        "<dl><dt>An undefined term.</dt></dl>",
        "<table/>",
        "<p><a href='#one'>Outer <a href='#two'>inner</a></a></p>",
        "<figure><figcaption><p>First.</p></figcaption><figcaption><p>Second.</p></figcaption></figure>",
        "<p><span class='katex-display' data-tex='x'/></p>",
    ] {
        assert!(
            crate::formats::xhtml::parse(source.as_bytes()).is_ok(),
            "{source}"
        );
        assert!(
            matches!(
                import(source, XhtmlOptions::default()),
                Err(super::XhtmlImportError::Build(_))
            ),
            "expected a semantic grammar error for {source}"
        );
    }
}

#[test]
fn resource_base_is_supplied_outside_the_document() {
    let outcome = import(
        fixtures::LINKS.source,
        XhtmlOptions {
            resource_base: Some("https://books.example.test/volume/".into()),
            ..XhtmlOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        outcome.document.resource_base(),
        Some("https://books.example.test/volume/")
    );
    assert_eq!(
        outcome.document.resources().next().unwrap().1.identity,
        "images/grid.png"
    );
}

#[test]
fn whole_documents_combine_structures_and_keep_local_links_resolvable() {
    for fixture in fixtures::EXAMPLES {
        let document = document(*fixture);
        let mut structures = std::collections::BTreeSet::new();
        for node in nodes(&document) {
            let category = match &node.kind {
                NodeKind::Heading(_) => "heading",
                NodeKind::List(_) => "list",
                NodeKind::Table(_) => "table",
                NodeKind::Figure => "figure",
                NodeKind::Math(_) => "math",
                NodeKind::Preformatted(_) => "preformatted",
                NodeKind::DefinitionList => "definitions",
                NodeKind::Disclosure(_) => "disclosure",
                NodeKind::UnsupportedObject(_) => "media",
                _ => continue,
            };
            structures.insert(category);
        }
        assert!(structures.len() >= 4, "{}: {structures:?}", fixture.name);
        for node in nodes(&document) {
            if let NodeKind::Link(Link {
                destination: Destination::Local(anchor),
                ..
            }) = &node.kind
            {
                assert!(
                    document.anchor(anchor).is_some(),
                    "{}: {anchor:?}",
                    fixture.name
                );
            }
        }
        assert!(document.text().as_str().len() > 200, "{}", fixture.name);
    }
}

#[cfg(feature = "render")]
#[test]
fn whole_documents_compose_paginate_and_paint_with_explicit_fixture_fonts() {
    use crate::{
        render::{InteractionState, Renderer},
        settings::RenderSettings,
    };
    let renderer = Renderer::new(RenderSettings::default());
    for fixture in fixtures::EXAMPLES {
        let document = document(*fixture);
        for (width, height) in [(320, 480), (640, 800)] {
            let rendered = renderer
                .render_page(
                    &document,
                    (width, height),
                    0,
                    None,
                    0.85,
                    InteractionState::default(),
                )
                .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
            assert!(!rendered.pages.pages.is_empty(), "{}", fixture.name);
            assert!(!rendered.layout.blocks.is_empty(), "{}", fixture.name);
            let page_count = rendered.pages.pages.len();
            for page_index in 0..page_count {
                let page = renderer
                    .render_page(
                        &document,
                        (width, height),
                        page_index,
                        None,
                        0.85,
                        InteractionState::default(),
                    )
                    .unwrap_or_else(|error| panic!("{} page {page_index}: {error}", fixture.name));
                assert_eq!(page.page_index, page_index);
                assert!(
                    page.surface
                        .pixels()
                        .chunks_exact(4)
                        .any(|pixel| pixel[..3] != [255, 255, 255]),
                    "{} page {page_index} should paint visible content",
                    fixture.name
                );
            }
            let continuous = renderer
                .render_full_document(&document, 0.85)
                .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
            assert!(continuous.height() > 0);
        }
    }
}
