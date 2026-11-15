use std::collections::BTreeSet;

use super::*;
use crate::formats::xhtml::{XhtmlSourceNodeKind, parse};

#[test]
fn parses_every_synthetic_fixture_with_the_strict_profile() {
    for fixture in fixtures::all() {
        let prepared = prepare(fixture.source.as_bytes(), &StrictFlowProfile)
            .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
        assert!(prepared.tree.nodes().len() > 1, "{}", fixture.name);
        assert!(prepared.diagnostics.is_empty(), "{}", fixture.name);
    }
}

#[test]
fn contract_fixtures_cover_the_complete_element_vocabulary() {
    let mut actual = BTreeSet::new();
    for fixture in fixtures::CONTRACT {
        let tree = parse(fixture.source.as_bytes()).unwrap();
        for (_, node) in tree.nodes() {
            if let XhtmlSourceNodeKind::Element(element) = node.kind() {
                actual.insert(element.name().local().to_owned());
            }
        }
    }
    let expected = [
        "a",
        "abbr",
        "address",
        "article",
        "aside",
        "audio",
        "blockquote",
        "br",
        "caption",
        "cite",
        "code",
        "dd",
        "details",
        "div",
        "dl",
        "dt",
        "em",
        "figcaption",
        "figure",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "iframe",
        "img",
        "li",
        "main",
        "mark",
        "nav",
        "ol",
        "p",
        "pre",
        "q",
        "section",
        "source",
        "span",
        "strong",
        "sub",
        "summary",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "time",
        "tr",
        "track",
        "ul",
        "video",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn flow_rejects_doctypes_and_does_not_apply_author_css() {
    assert!(matches!(
        import("<!DOCTYPE p><p>Text.</p>", XhtmlOptions::default()),
        Err(XhtmlImportError::Prepare(XhtmlPrepareError::Parse(_)))
    ));
    let outcome = import(
        "<p id='plain' style='font-size: 3em; text-align: center'>Plain text.</p>",
        XhtmlOptions::default(),
    )
    .unwrap();
    let target = outcome
        .document
        .anchor(&crate::model::flow::AnchorId::new("plain"))
        .unwrap()
        .target
        .node;
    let styles = outcome
        .document
        .resolve_styles(crate::model::flow::StyleContextKey::default());
    let style = styles
        .get_computed_style(styles.style_for_node(target).unwrap())
        .unwrap();
    assert_eq!(style.font_size_per_mille(), 1_000);
    assert_eq!(style.text_alignment(), None);
}

#[test]
fn accepts_legacy_and_current_versions_but_rejects_unknown_future_contracts() {
    for canonical_version in [
        LEGACY_FLOW_SEMANTIC_XHTML_VERSION,
        CURRENT_FLOW_SEMANTIC_XHTML_VERSION,
    ] {
        import(
            "<p>known</p>",
            XhtmlOptions {
                canonical_version,
                ..XhtmlOptions::default()
            },
        )
        .unwrap();
    }
    assert!(matches!(
        import(
            "<p>future</p>",
            XhtmlOptions {
                canonical_version: CURRENT_FLOW_SEMANTIC_XHTML_VERSION + 1,
                ..XhtmlOptions::default()
            }
        ),
        Err(XhtmlImportError::UnsupportedVersion(_))
    ));
}

#[test]
fn source_retention_is_separate_from_xhtml_origins() {
    let outcome = import(
        "<p>retained</p>",
        XhtmlOptions {
            retention: SourceRetention::Retain,
            ..XhtmlOptions::default()
        },
    )
    .unwrap();
    assert_eq!(outcome.retained_sources.unwrap().len(), 1);
    assert!(outcome.document.origins().len() >= 2);
}
