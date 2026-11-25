use super::*;
use crate::model::flow::{FlowDocument, Image, ImageCandidateDescriptor};

fn first_image(document: &FlowDocument) -> &Image {
    document
        .preorder(document.root())
        .find_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Image(image) => Some(image),
            _ => None,
        })
        .expect("an image survives extraction")
}

fn import_image(markup: &str) -> WebImportOutcome {
    import(
        &article(markup).replace("<head>", "<head><base href='../assets/'>"),
        WebOptions {
            source_url: Some("https://example.test/news/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn preserves_ordered_picture_sources_and_width_sizes() {
    let imported = import_image(
        "<p><picture><source type='image/avif' media='(min-width: 800px)' sizes='(max-width: 1000px) 90vw, 900px' srcset='wide.avif 800w, wider.avif 1600w'><source type='image/webp' srcset='small.webp 1x, big.webp 2x'><img src='fallback.jpg' srcset='small.jpg 400w, big.jpg 1200w' sizes='100vw' alt='Scene' title='A scene'></picture></p>",
    );
    let document = &imported.content.document;
    let image = first_image(document);
    assert_eq!(
        document.get_resource(image.resource).unwrap().identity,
        "https://example.test/assets/fallback.jpg"
    );
    assert_eq!(image.title.as_deref(), Some("A scene"));
    let responsive = image.responsive.as_ref().unwrap();
    assert_eq!(responsive.sources.len(), 2);
    let first = &responsive.sources[0];
    assert_eq!(first.media.as_deref(), Some("(min-width: 800px)"));
    assert_eq!(first.media_type.as_deref(), Some("image/avif"));
    assert_eq!(
        first.candidates.sizes.as_deref(),
        Some("(max-width: 1000px) 90vw, 900px")
    );
    let candidate = &first.candidates.candidates[1];
    assert_eq!(
        candidate.descriptor,
        ImageCandidateDescriptor::Width(std::num::NonZeroU32::new(1600).unwrap())
    );
    let resource = document.get_resource(candidate.resource).unwrap();
    assert_eq!(resource.identity, "https://example.test/assets/wider.avif");
    assert_eq!(resource.media_type.as_deref(), Some("image/avif"));
    assert_eq!(
        responsive.sources[1].media_type.as_deref(),
        Some("image/webp")
    );
    let fallback = responsive.fallback.as_ref().unwrap();
    assert_eq!(fallback.sizes.as_deref(), Some("100vw"));
    assert_eq!(fallback.candidates.len(), 2);
    assert!(
        imported.content.diagnostics.is_empty(),
        "{:?}",
        imported.content.diagnostics
    );
}

#[test]
fn density_sets_include_explicit_src_but_not_synthesized_fallbacks() {
    for (attributes, expected) in [
        ("src='normal.png' srcset='retina.png 2x'", vec![2.0, 1.0]),
        ("srcset='retina.png 2x'", vec![2.0]),
        (
            "src='unused.png' srcset='normal.png 1x, retina.png 2x'",
            vec![1.0, 2.0],
        ),
    ] {
        let imported = import_image(&format!("<p><img {attributes} alt='Photo'></p>"));
        let image = first_image(&imported.content.document);
        let candidates = &image
            .responsive
            .as_ref()
            .unwrap()
            .fallback
            .as_ref()
            .unwrap()
            .candidates;
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.descriptor)
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .map(ImageCandidateDescriptor::Density)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn preserves_valid_neighbors_of_invalid_and_disallowed_candidates() {
    let imported = import_image(
        "<p><img srcset='javascript:alert(1) 1x, zero.png 0w, bad.png nonsense, good.png 640w, duplicate.png 640w, mixed.png 2x, great.png 1280w' alt='Kept'></p>",
    );
    let document = &imported.content.document;
    let image = first_image(document);
    assert_eq!(
        document.get_resource(image.resource).unwrap().identity,
        "https://example.test/assets/good.png"
    );
    let candidates = &image
        .responsive
        .as_ref()
        .unwrap()
        .fallback
        .as_ref()
        .unwrap()
        .candidates;
    assert_eq!(candidates.len(), 2);
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("responsive image candidates"))
    );
    assert!(document.text().as_str().contains("Kept"));
}

#[test]
fn picture_without_img_src_still_has_a_deterministic_fallback() {
    let imported = import_image(
        "<p><picture><source srcset='photo.avif 1x, retina.avif 2x' type='image/avif'><img alt='Scene'></picture></p>",
    );
    let document = &imported.content.document;
    let image = first_image(document);
    assert_eq!(
        document.get_resource(image.resource).unwrap().identity,
        "https://example.test/assets/photo.avif"
    );
    let responsive = image.responsive.as_ref().unwrap();
    assert_eq!(responsive.sources.len(), 1);
    assert!(responsive.fallback.is_none());
}

#[test]
fn ordinary_images_and_invalid_sets_keep_single_resource_behavior() {
    for markup in [
        "<p><img src='normal.png' alt='Ordinary'></p>",
        "<p><img src='normal.png' srcset='bad.png 0x, other.png nonsense' alt='Ordinary'></p>",
    ] {
        let imported = import_image(markup);
        let document = &imported.content.document;
        let image = first_image(document);
        assert!(image.responsive.is_none());
        assert_eq!(
            document.get_resource(image.resource).unwrap().identity,
            "https://example.test/assets/normal.png"
        );
        assert!(document.text().as_str().contains("Ordinary"));
    }
}

#[test]
fn responsive_resources_remain_relative_without_a_host_url() {
    let imported = import(
        &article("<p><img srcset='small.png 1x, big.png 2x' alt='Photo'></p>"),
        Default::default(),
    )
    .unwrap();
    let document = &imported.content.document;
    let candidates = &first_image(document)
        .responsive
        .as_ref()
        .unwrap()
        .fallback
        .as_ref()
        .unwrap()
        .candidates;
    assert_eq!(
        document
            .get_resource(candidates[1].resource)
            .unwrap()
            .identity,
        "big.png"
    );
}

#[test]
fn usable_candidates_replace_a_disallowed_fallback_resource() {
    let imported =
        import_image("<p><img src='javascript:alert(1)' srcset='good.png 2x' alt='Photo'></p>");
    let document = &imported.content.document;
    let image = first_image(document);
    assert_eq!(
        document.get_resource(image.resource).unwrap().identity,
        "https://example.test/assets/good.png"
    );
    assert_eq!(
        image
            .responsive
            .as_ref()
            .unwrap()
            .fallback
            .as_ref()
            .unwrap()
            .candidates
            .len(),
        1
    );
}

#[test]
fn preserves_data_urls_and_lazy_image_candidates() {
    for markup in [
        "<p><img srcset='data:image/png;base64,AAAA 1x, big.png 2x' alt='Data'></p>",
        "<p><img data-src='normal.png' data-srcset='normal.png 1x, big.png 2x' alt='Lazy'></p>",
    ] {
        let imported = import_image(markup);
        let document = &imported.content.document;
        let candidates = &first_image(document)
            .responsive
            .as_ref()
            .unwrap()
            .fallback
            .as_ref()
            .unwrap()
            .candidates;
        assert_eq!(candidates.len(), 2);
        if markup.contains("base64") {
            assert_eq!(
                document
                    .get_resource(candidates[0].resource)
                    .unwrap()
                    .identity,
                "data:image/png;base64,AAAA"
            );
        }
    }
}

#[cfg(feature = "render")]
#[test]
fn responsive_choices_round_trip_through_semantic_cache() {
    use crate::cache_codec::{CacheCodecLimits, decode_semantic, encode_semantic};
    use flow_cache::DependencyDigest;
    let imported = import_image(
        "<p><picture><source media='(min-width: 800px)' type='image/avif' srcset='wide.avif 800w, wider.avif 1600w' sizes='80vw'><img src='normal.png' srcset='retina.png 2x' alt='Scene'></picture></p>",
    );
    let document = &imported.content.document;
    let dependency = DependencyDigest::for_bytes(b"responsive");
    let limits = CacheCodecLimits::default();
    let encoded = encode_semantic(document, dependency, limits).unwrap();
    let decoded = decode_semantic(&encoded, dependency, limits).unwrap();
    assert_eq!(first_image(&decoded), first_image(document));
    assert_eq!(decoded.text(), document.text());
    assert_eq!(
        encode_semantic(&decoded, dependency, limits).unwrap(),
        encoded
    );
}
