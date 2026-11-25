use super::*;

fn fixture(responsive: ResponsiveImage) -> (FlowBuilder, NodeId) {
    let mut builder = FlowBuilder::new();
    let root = add(&mut builder, NodeKind::Document);
    let resource = builder
        .add_resource(Resource {
            identity: "fallback.png".into(),
            media_type: None,
        })
        .unwrap();
    let image = add(
        &mut builder,
        NodeKind::Image(Image {
            resource,
            alternative: ImageAlternative::Text(NonEmptyText::new("Photo").unwrap()),
            title: None,
            intrinsic_size: None,
            responsive: Some(responsive),
        }),
    );
    append(&mut builder, root, image);
    (builder, image)
}

fn choices(descriptors: Vec<ImageCandidateDescriptor>) -> ResponsiveImage {
    ResponsiveImage {
        sources: Vec::new(),
        fallback: Some(ImageCandidateSet {
            candidates: descriptors
                .into_iter()
                .map(|descriptor| ImageCandidate {
                    resource: ResourceId::from_index(0),
                    descriptor,
                })
                .collect(),
            sizes: Some("100vw".into()),
        }),
    }
}

#[test]
fn rejects_empty_mixed_duplicate_and_nonpositive_candidates() {
    use ImageCandidateDescriptor::{Density, Width};
    for descriptors in [
        vec![],
        vec![Density(0.0)],
        vec![Density(-1.0)],
        vec![Density(f32::NAN)],
        vec![Density(f32::INFINITY)],
        vec![Density(1.0), Density(1.0)],
        vec![Width(one()), Width(one())],
        vec![Width(one()), Density(2.0)],
    ] {
        let (builder, image) = fixture(choices(descriptors));
        assert_eq!(
            builder.finish().unwrap_err(),
            BuildError::InvalidImageCandidates { node: image }
        );
    }
    let (builder, image) = fixture(ResponsiveImage {
        sources: Vec::new(),
        fallback: None,
    });
    assert_eq!(
        builder.finish().unwrap_err(),
        BuildError::InvalidImageCandidates { node: image }
    );
}

#[test]
fn validates_resource_references_in_both_source_and_fallback_sets() {
    for conditional in [false, true] {
        let mut responsive = choices(vec![ImageCandidateDescriptor::Density(1.0)]);
        let invalid = ResourceId::from_index(99);
        responsive.fallback.as_mut().unwrap().candidates[0].resource = invalid;
        if conditional {
            responsive.sources.push(ImageSource {
                media: Some("(min-width: 800px)".into()),
                media_type: Some("image/avif".into()),
                candidates: responsive.fallback.take().unwrap(),
            });
        }
        let (builder, image) = fixture(responsive);
        assert_eq!(
            builder.finish().unwrap_err(),
            BuildError::InvalidResource {
                node: image,
                resource: invalid
            }
        );
    }
}

#[test]
fn alternatives_do_not_change_canonical_text_or_candidate_order() {
    let responsive = choices(vec![
        ImageCandidateDescriptor::Density(2.0),
        ImageCandidateDescriptor::Density(1.0),
    ]);
    let (builder, _) = fixture(responsive.clone());
    let document = builder.finish().unwrap();
    assert_eq!(document.text().as_str(), "Photo");
    let image = document
        .preorder(document.root())
        .find_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Image(image) => Some(image),
            _ => None,
        })
        .unwrap();
    assert_eq!(image.responsive.as_ref(), Some(&responsive));
}
