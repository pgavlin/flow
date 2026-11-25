use super::*;
use crate::{
    cache_codec::{CacheCodecLimits, DependencyDigest, decode_semantic, encode_semantic},
    pixels::PixelBuffer,
    render::{InteractionState, PageSelection, PaintOptions, Renderer},
    settings::RenderSettings,
};

#[test]
fn sized_images_settle_without_metadata_fetches_or_recomposition() {
    use crate::viewer::{DocumentViewer, ImagePixels, ViewerEffect};
    let document = import(
        "<p><img src='a.png' width='80' height='40' alt='A'><img src='b.png' width='60' height='30' alt='B'><img src='a.png' width='40' height='20' alt='Smaller A'></p>",
        MediaWikiOptions { input: MediaWikiInput::BodyFragment, expected_version: Some(MediaWikiHtmlVersion::V2_8_0), ..Default::default() }
    ).unwrap().content.document;
    let mut viewer = DocumentViewer::new("images", Default::default());
    viewer.replace_document(Ok(document));
    assert!(
        !viewer
            .take_effects()
            .iter()
            .any(|effect| matches!(effect, ViewerEffect::RequestImageMetadata(_)))
    );
    let mut pixels = PixelBuffer::new(400, 240, [255; 3]).unwrap();
    let mut composition = None;
    let mut supplied = 0;
    let mut settled = false;
    for _ in 0..5 {
        viewer
            .render_into(&mut pixels.surface_mut(), 400, 240, 1.0)
            .unwrap();
        let current = viewer.active_composition().unwrap() as *const _;
        if let Some(previous) = composition {
            assert_eq!(previous, current);
        }
        composition = Some(current);
        let effects = viewer.take_effects();
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, ViewerEffect::RequestImageMetadata(_)))
        );
        let requests: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| match effect {
                ViewerEffect::RequestImagePixels(request) => Some(request),
                _ => None,
            })
            .collect();
        if requests.is_empty() {
            settled = true;
            break;
        }
        for request in requests {
            let (width, height) = (
                request.desired_width.unwrap(),
                request.desired_height.unwrap(),
            );
            let image =
                ImagePixels::from_rgba(width, height, vec![255; (width * height * 4) as usize])
                    .unwrap();
            assert!(viewer.provide_image_pixels(request, image));
            supplied += 1;
        }
    }
    assert!(supplied >= 2);
    assert!(
        settled,
        "visible sized images must not keep requesting one another"
    );
}

#[test]
fn imported_notes_math_images_and_generated_labels_render_and_cache() {
    let source = r##"<section><h2 id="heading">Article</h2><p>Text
<sup typeof="mw:Extension/ref" id="ref"><a href="#note">[1]</a></sup>
<a rel="mw:ExtLink" class="external autonumber" href="https://example.test"></a>
<span typeof="mw:Extension/math" data-mw='{"name":"math","attrs":{},"body":{"extsrc":"x^2"}}'>fallback</span></p>
<figure><img src="diagram.png" srcset="diagram-2.png 2x" alt="Diagram"><figcaption>Caption</figcaption></figure>
<div typeof="mw:Extension/references"><ol><li id="note"><span class="mw-cite-backlink"><a href="#ref">return</a></span>Note body</li></ol></div></section>"##;
    let document = import(
        source,
        MediaWikiOptions {
            input: MediaWikiInput::BodyFragment,
            expected_version: Some(MediaWikiHtmlVersion::V2_8_0),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    let digest = DependencyDigest::for_bytes(source.as_bytes());
    let limits = CacheCodecLimits::default();
    let bytes = encode_semantic(&document, digest, limits).unwrap();
    let restored = decode_semantic(&bytes, digest, limits).unwrap();
    assert_eq!(bytes, encode_semantic(&restored, digest, limits).unwrap());
    assert!(restored.origins().any(|(_, origin)| matches!(
        origin,
        Origin::Generated {
            reason: GeneratedReason::LinkLabel,
            ..
        }
    )));
    let renderer = Renderer::new(RenderSettings::default());
    let composition = renderer.compose_document(&restored, 420.0);
    let pages = renderer
        .paginate_composed_document(&composition, 500)
        .unwrap();
    let mut pixels = PixelBuffer::new(420, 500, [255; 3]).unwrap();
    renderer
        .paint_paginated_page_into(
            &mut pixels.surface_mut(),
            &composition,
            &pages,
            (420, 500),
            PageSelection {
                requested_page: 0,
                text_anchor: None,
            },
            PaintOptions {
                scale: 1.0,
                interaction: InteractionState::default(),
            },
        )
        .unwrap();
}
