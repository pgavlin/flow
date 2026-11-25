#![cfg(feature = "mediawiki-html")]

use flow_document::formats::mediawiki_html::*;
use flow_document::model::flow::*;

fn nodes(document: &FlowDocument) -> impl Iterator<Item = &Node> {
    document
        .preorder(document.root())
        .map(|id| document.get_node(id).unwrap())
}

#[cfg(feature = "render")]
#[test]
fn apple_article_paginates_without_fetching_metadata_for_sized_images() {
    use flow_document::{
        pagination::{build_page_items, validate_paginated_layout},
        render::Renderer,
        settings::RenderSettings,
        viewer::{DocumentViewer, ViewerEffect},
    };
    use std::sync::Arc;
    let document = import(
        include_str!("fixtures/mediawiki/apple-1386470530.html"),
        MediaWikiOptions {
            source_url: Some("https://en.wikipedia.org/wiki/Apple".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert_eq!(
        nodes(&document)
            .filter(|node| matches!(node.kind, NodeKind::Image(_)))
            .count(),
        42
    );
    let mut viewer = DocumentViewer::new("Apple", Default::default());
    viewer.replace_document(Ok(document.clone()));
    assert!(
        !viewer
            .take_effects()
            .iter()
            .any(|effect| matches!(effect, ViewerEffect::RequestImageMetadata(_)))
    );
    let mut renderer = Renderer::new(RenderSettings {
        body_fonts: vec!["Literata".into()],
        code_fonts: vec!["Fira Code".into()],
        ..Default::default()
    });
    for font in [
        include_bytes!("fixtures/fonts/literata.ttf").as_slice(),
        include_bytes!("fixtures/fonts/literata-bold.ttf").as_slice(),
        include_bytes!("fixtures/fonts/literata-italic.ttf").as_slice(),
        include_bytes!("fixtures/fonts/literata-bold-italic.ttf").as_slice(),
        include_bytes!("fixtures/fonts/fira-code.ttf").as_slice(),
    ] {
        assert!(renderer.register_font(Arc::from(font)));
    }
    for width in [400, 632, 900] {
        let composed =
            renderer.compose_document(&document, renderer.effective_content_width(width));
        let input = build_page_items(&composed);
        for height in [240, 840] {
            let pages = renderer
                .paginate_composed_document(&composed, height)
                .unwrap();
            assert!(pages.pages.len() > 10);
            assert_eq!(validate_paginated_layout(&composed, &input, &pages), Ok(()));
        }
    }
}

#[test]
fn pinned_real_parsoid_article_preserves_sections_notes_math_and_images() {
    let source = include_str!("fixtures/mediawiki/pythagorean-theorem-1386629982.html");
    let result = import(
        source,
        MediaWikiOptions {
            source_url: Some("https://en.wikipedia.org/wiki/Pythagorean_theorem".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = &result.content.document;
    assert_eq!(
        result.metadata.database_title.as_deref(),
        Some("Pythagorean theorem")
    );
    assert!(document.text().as_str().contains("Pythagorean theorem"));
    assert_eq!(
        nodes(document)
            .filter(|node| matches!(node.kind, NodeKind::Math(_)))
            .count(),
        100
    );
    assert!(nodes(document).any(|node| matches!(node.kind, NodeKind::Section)));
    assert!(nodes(document).any(|node| matches!(&node.kind, NodeKind::Link(link) if matches!(link.relation, LinkRelation::NoteReference { .. }))));
    assert!(nodes(document).any(|node| matches!(&node.kind, NodeKind::Link(link) if link.relation == LinkRelation::NoteReturn)));
    assert!(nodes(document).any(|node| matches!(node.kind, NodeKind::Image(_))));
    assert!(!result.metadata.categories.is_empty());
    assert!(
        !result
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidDestination)
    );
}
