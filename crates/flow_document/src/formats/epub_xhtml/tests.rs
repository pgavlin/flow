use super::*;
use crate::formats::xhtml::{XhtmlParseError, parse_with_options};
use crate::model::flow::{NodeKind, StyleContextKey, TextAlignment};

#[test]
fn import_rejects_fragments_and_requires_an_xhtml_document_body() {
    for source in [
        "<p>A fragment.</p>",
        "<html><body><p>Missing namespace.</p></body></html>",
        "<html xmlns='http://www.w3.org/1999/xhtml'><head/></html>",
        "<html xmlns='http://www.w3.org/1999/xhtml'><body xmlns='urn:other'/></html>",
        "<html xmlns='http://www.w3.org/1999/xhtml'><body/></html><p>Second root.</p>",
    ] {
        assert!(
            matches!(
                import(source, EpubXhtmlOptions::default()),
                Err(XhtmlImportError::Prepare(XhtmlPrepareError::Profile(_)))
            ),
            "{source}"
        );
    }
}

#[test]
fn lowering_uses_the_validated_body_not_a_same_named_descendant() {
    let source = "<html xmlns='http://www.w3.org/1999/xhtml'><head><title>Not body text</title><other:body xmlns:other='urn:other'>Also not body text</other:body></head><body><p>Readable body.</p></body></html>";
    let outcome = import(source, EpubXhtmlOptions::default()).unwrap();
    assert_eq!(outcome.document.text().as_str(), "Readable body.");
}

#[test]
fn stylesheet_discovery_preserves_document_order_without_fetching_resources() {
    let source = "<html xmlns='http://www.w3.org/1999/xhtml'><head><link rel='stylesheet' href='first.css'/><link rel='icon' href='icon.png'/><link rel='alternate STYLESHEET' href='second.css'/><link rel='stylesheet'/></head><body><p>Text.</p></body></html>";
    assert_eq!(
        stylesheet_hrefs(source).unwrap(),
        ["first.css", "second.css"]
    );
}

#[test]
fn import_preserves_resource_base_identity_and_optional_source_retention() {
    use crate::model::flow::{DocumentReference, SourceRetention};
    let source = "<html xmlns='http://www.w3.org/1999/xhtml'><head/><body><p>Retained body.</p></body></html>";
    let outcome = import(
        source,
        EpubXhtmlOptions {
            resource_base: Some("epub://test/chapter.xhtml".into()),
            identity: Some(DocumentReference::PublicationItem("chapter.xhtml".into())),
            retention: SourceRetention::Retain,
            ..EpubXhtmlOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        outcome.document.resource_base(),
        Some("epub://test/chapter.xhtml")
    );
    assert_eq!(outcome.retained_sources.unwrap().len(), 1);
    assert!(outcome.document.origins().len() >= 2);
}

#[test]
fn epub_policy_explicitly_preserves_supported_math_and_diagram_extensions() {
    use crate::model::flow::{MathDisplay, NodeKind, PreformattedKind};
    let source = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><p><span data-tex="a+b">discard generated text</span></p><span class="katex-display" data-tex="x"/><pre><code class="language-mermaid">graph TD
  One --&gt; Two</code></pre></body></html>"#;
    let outcome = import(source, EpubXhtmlOptions::default()).unwrap();
    let kinds = outcome
        .document
        .preorder(outcome.document.root())
        .map(|id| &outcome.document.get_node(id).unwrap().kind)
        .collect::<Vec<_>>();
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Math(math) if math.display == MathDisplay::Inline && math.source == "a+b")));
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Math(math) if math.display == MathDisplay::Block && math.source == "x")));
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Preformatted(code) if code.content_kind == PreformattedKind::MermaidSource)));
    assert!(!outcome.document.text().as_str().contains("generated"));
}

#[test]
fn epub_profile_accepts_external_xhtml_doctypes_without_entity_definitions() {
    let source = br#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head/><body><p>text</p></body></html>"#;
    let prepared = prepare_with_options(
        source,
        XhtmlParseOptions {
            allow_doctype: true,
            ..Default::default()
        },
        &EpubXhtmlProfile,
    )
    .unwrap();
    assert!(prepared.tree.nodes().len() > 4);
    assert!(matches!(
        parse_with_options(
            b"<!DOCTYPE html [<!ENTITY word 'text'>]><html/>",
            XhtmlParseOptions {
                allow_doctype: true,
                ..Default::default()
            }
        ),
        Err(XhtmlParseError::Malformed { .. })
    ));
}

#[test]
fn lowers_complete_epub_xhtml_with_cascaded_author_styles() {
    let source = r#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Not canonical</title><link rel="stylesheet" href="book.css"/></head><body><div><p class="CN">CHAPTER ONE</p><p class="TX">Readable prose.</p></div></body></html>"#;
    let outcome = import(
            source,
            EpubXhtmlOptions {
                stylesheets: vec![r#"body { font-family: "Book Serif", serif; widows: 3; orphans: 4; } p.CN { font-size: 2em; text-align: center; margin-top: 10%; page-break-after: avoid; } p.TX { line-height: 1.4; text-indent: 1.2em; }"#.into()],
                ..EpubXhtmlOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        outcome.document.text().as_str(),
        "CHAPTER ONE\n\nReadable prose."
    );
    let chapter = outcome
        .document
        .preorder(outcome.document.root())
        .find(|id| {
            let node = outcome.document.get_node(*id).unwrap();
            matches!(node.kind, NodeKind::Paragraph)
                && outcome.document.get_text(node.text_range) == Some("CHAPTER ONE")
        })
        .unwrap();
    let styles = outcome.document.resolve_styles(StyleContextKey::default());
    let style = styles
        .get_computed_style(styles.style_for_node(chapter).unwrap())
        .unwrap();
    assert_eq!(style.font_size_per_mille(), 2_000);
    assert_eq!(
        style.font_family(),
        crate::model::flow::EffectiveFontFamily::Named("Book Serif".into())
    );
    assert_eq!(style.text_alignment(), Some(TextAlignment::Center));
    assert_eq!(style.break_after(), crate::model::flow::AuthorBreak::Avoid);
    assert_eq!(style.widows(), 3);
    assert_eq!(style.orphans(), 4);
    assert_eq!(
        style.margin_top(),
        Some(crate::model::flow::CssLength::Percent(10_000))
    );
}
