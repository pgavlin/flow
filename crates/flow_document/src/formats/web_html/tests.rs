use super::*;
use crate::model::flow::{AnchorId, Destination, NodeKind, Origin};

fn article(extra: &str) -> String {
    format!("<!doctype html><html lang='en'><head><title>A useful article</title><meta name='author' content='A Writer'></head><body><nav>BOILERPLATE navigation</nav><article><h1>A useful article</h1>{}<h2>More details</h2>{extra}</article><footer>FOOTER navigation</footer></body></html>",
        "<p>This is a substantial article paragraph, with sentences, commas, and useful information for the reader. It is not navigation or advertising. We want to preserve the entire story and its ordinary reading structure.</p>".repeat(6))
}

#[test]
fn extracts_metadata_and_lowers_html5_dom_directly() {
    let source = article(
        "<p>A&nbsp;<b>bold</b> thought with <i>emphasis</i> and a<br>line break.</p><script>EVIL()</script>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/articles/story".into()),
            retention: SourceRetention::Retain,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(imported.metadata.title, "A useful article");
    assert_eq!(imported.metadata.byline.as_deref(), Some("A Writer"));
    let document = &imported.content.document;
    let text = document.text().as_str();
    assert!(
        text.contains("A\u{a0}bold thought with emphasis and a\nline break."),
        "{text}"
    );
    assert!(!text.contains("BOILERPLATE"));
    assert!(!text.contains("EVIL"));
    assert!(document.anchor(&AnchorId::new("more-details")).is_some());
    assert!(
        document
            .origins()
            .all(|(_, origin)| !matches!(origin, Origin::Xhtml { .. }))
    );
    assert!(document.origins().any(|(_, origin)| matches!(
        origin,
        Origin::Html {
            serialized_range: None,
            ..
        }
    )));
    let artifact = document.source_artifacts().next().unwrap().1;
    assert_eq!(artifact.media_type.as_deref(), Some("text/html"));
    assert_eq!(
        imported
            .content
            .retained_sources
            .unwrap()
            .get(&artifact.revision),
        Some(source.as_str())
    );
}

#[test]
fn resolves_resources_and_keeps_self_fragments_local() {
    let source = article(
        "<p><a href='https://example.test/story#details'>self</a> <a href='/next'>next</a> <a href='javascript:alert(1)'>unsafe</a></p><p><img src='images/photo.png' alt='A photo'></p><h2 id='details'>Detail</h2>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = imported.content.document;
    let links: Vec<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Link(link) => Some(&link.destination),
            _ => None,
        })
        .collect();
    assert!(
        links.contains(&&Destination::Local(AnchorId::new("details"))),
        "{links:?}"
    );
    assert!(links.contains(&&Destination::External("https://example.test/next".into())));
    assert!(
        !links.iter().any(
            |link| matches!(link, Destination::External(url) if url.starts_with("javascript:"))
        )
    );
    assert!(
        document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/images/photo.png")
    );
}

#[test]
fn ports_button_and_image_repairs_before_extraction() {
    let source = article(
        "<div class='wp-block-buttons'><div class='wp-block-button'><a class='wp-block-button__link' href='/full'>Read the complete story</a></div></div><p><a href='/gallery'><picture><source srcset='large.png'><img src='/photo.png' alt='Rescued photo'></picture></a></p>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = imported.content.document;
    assert!(document.text().as_str().contains("Read the complete story"));
    assert!(
        document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/photo.png")
    );
}

#[test]
fn rejects_unsupported_bases_and_limits_before_extraction() {
    assert!(matches!(
        import(
            "hello",
            WebOptions {
                source_url: Some("file:///tmp/article".into()),
                ..Default::default()
            }
        ),
        Err(WebImportError::InvalidUrl)
    ));
    assert!(matches!(
        import(&"x".repeat(MAX_SOURCE + 1), Default::default()),
        Err(WebImportError::TooComplex)
    ));
    let deeply_nested = format!(
        "{}text{}",
        "<div>".repeat(MAX_DEPTH + 1),
        "</div>".repeat(MAX_DEPTH + 1)
    );
    assert!(matches!(
        import(&deeply_nested, Default::default()),
        Err(WebImportError::TooComplex)
    ));
}

#[test]
fn separates_layout_tables_from_data_tables() {
    let source = article(
        "<table role='presentation'><tr><td><p>Layout prose.</p></td></tr></table><table><thead><tr><th>Label</th><th>Value</th></tr></thead><tbody><tr><td>Data</td><td>42</td></tr></tbody></table>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let tables = document
        .preorder(document.root())
        .filter(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Table(_)))
        .count();
    assert_eq!(tables, 1);
    assert!(document.text().as_str().contains("Layout prose."));
    assert!(document.text().as_str().contains("42"));
}

#[test]
fn document_base_resolves_images_without_loading_them() {
    let source = article(
        "<p><img src='photo.png' alt='Photo'><img src='javascript:bad()' alt='Rejected'></p>",
    )
    .replace(
        "<head>",
        "<head><base href='https://assets.example.test/root/'>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/article".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert_eq!(
        document.resource_base(),
        Some("https://assets.example.test/root/")
    );
    assert_eq!(document.resources().len(), 1);
    assert_eq!(
        document.resources().next().unwrap().1.identity,
        "https://assets.example.test/root/photo.png"
    );
}
