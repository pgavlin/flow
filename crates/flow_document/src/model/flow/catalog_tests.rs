use std::sync::Arc;

use super::*;

fn add(builder: &mut FlowBuilder, kind: NodeKind) -> NodeId {
    builder.add_node(kind).unwrap()
}

#[test]
fn catalogs_finalize_with_verified_origins_styles_and_anchors() {
    let source_text = "hé";
    let revision = SourceDigest::for_text(source_text);
    let mut builder = FlowBuilder::new();
    let source = builder
        .add_source_artifact(
            SourceArtifactDescriptor {
                identity: Some(DocumentReference::Relative("chapter.md".into())),
                revision: revision.clone(),
                media_type: Some("text/markdown".into()),
            },
            source_text,
        )
        .unwrap();
    builder.set_resource_base(Some("book/".into()));

    let imported = builder
        .add_origin(Origin::Markdown {
            source,
            source_range: 0..source_text.len(),
        })
        .unwrap();
    let generated = builder
        .add_origin(Origin::Generated {
            reason: GeneratedReason::AnonymousParagraph,
            related: Some(imported),
        })
        .unwrap();

    let style = StyleInput::semantic(SemanticStyleKey::Heading);
    let style_id = builder.intern_style_input(style.clone()).unwrap();
    assert_eq!(builder.intern_style_input(style).unwrap(), style_id);

    let root = add(&mut builder, NodeKind::Document);
    let paragraph = add(&mut builder, NodeKind::Paragraph);
    let text = builder.add_text(source_text).unwrap();
    builder.set_origin(paragraph, generated).unwrap();
    builder.set_origin(text, imported).unwrap();
    builder.set_style_input(text, style_id).unwrap();
    builder
        .add_origin_span(
            text,
            0..source_text.len(),
            imported,
            Some(0..source_text.len()),
        )
        .unwrap();
    builder
        .add_origin_span(
            text,
            1..source_text.len(),
            imported,
            Some(1..source_text.len()),
        )
        .unwrap();
    builder
        .add_anchor_with_origin(AnchorId::new("target"), text, Some(imported))
        .unwrap();
    builder.append(paragraph, text).unwrap();
    builder.append(root, paragraph).unwrap();

    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), source_text);
    assert_eq!(document.resource_base(), Some("book/"));
    assert_eq!(document.source_artifacts().len(), 1);
    assert_eq!(document.style_inputs().len(), 2);
    assert_eq!(document.origins().len(), 2);
    assert_eq!(
        document.get_style_input(style_id).unwrap().semantic_key(),
        Some(SemanticStyleKey::Heading)
    );
    assert_eq!(
        document.get_origin(imported),
        Some(&Origin::Markdown {
            source,
            source_range: 0..3
        })
    );
    assert_eq!(
        document.anchor(&AnchorId::new("target")).unwrap().origin,
        Some(imported)
    );
    assert_eq!(document.origin_spans().len(), 2);
    assert_eq!(
        document.origin_spans()[0].logical,
        TextRange::new(TextOffset::new(0), TextOffset::new(3))
    );
    assert_eq!(
        document.origin_spans()[1].logical,
        TextRange::new(TextOffset::new(1), TextOffset::new(3))
    );
}

#[test]
fn source_revisions_and_utf8_ranges_are_validated_without_retention() {
    let mut builder = FlowBuilder::new();
    let mismatch = builder.add_source_artifact(
        SourceArtifactDescriptor {
            identity: None,
            revision: SourceDigest::for_text("other"),
            media_type: None,
        },
        "hé",
    );
    assert_eq!(mismatch, Err(BuildError::SourceDigestMismatch));

    let source = builder.add_source("hé", None, None).unwrap();
    assert_eq!(
        builder.add_origin(Origin::Markdown {
            source,
            source_range: 0..2,
        }),
        Err(BuildError::InvalidSourceRange {
            artifact: source,
            range: 0..2,
        })
    );

    let root = add(&mut builder, NodeKind::Document);
    let paragraph = add(&mut builder, NodeKind::Paragraph);
    let text = builder.add_text("é").unwrap();
    let origin = builder
        .add_origin(Origin::Markdown {
            source,
            source_range: 1..3,
        })
        .unwrap();
    builder
        .add_origin_span(text, 0..1, origin, Some(1..3))
        .unwrap();
    builder.append(paragraph, text).unwrap();
    builder.append(root, paragraph).unwrap();
    assert!(matches!(
        builder.finish(),
        Err(BuildError::InvalidOriginSpan { node, range })
            if node == text && range == (0..1)
    ));
}

#[test]
fn retained_sources_are_separate_and_digest_addressed() {
    let source: Arc<str> = Arc::from("source");
    let revision = SourceDigest::for_text(&source);
    let mut retained = RetainedSourceBundle::default();
    retained.insert(revision.clone(), source).unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained.get(&revision), Some("source"));
    assert!(retained.get(&SourceDigest::for_text("different")).is_none());
    assert_eq!(
        retained.insert(revision, Arc::from("mismatch")),
        Err(RetainedSourceError)
    );
}

#[test]
fn duplicate_anchors_are_rejected_without_importer_recovery() {
    let mut builder = FlowBuilder::new();
    let root = add(&mut builder, NodeKind::Document);
    let anchor = AnchorId::new("same");
    builder.add_anchor(anchor.clone(), root).unwrap();
    assert_eq!(
        builder.add_anchor(anchor.clone(), root),
        Err(BuildError::DuplicateAnchor { anchor })
    );
}
