use super::*;
use flow_document::viewer::ImagePixels;

fn single_model(name: &str, font: bool) -> Publication {
    let mut description = publication("r1").description().clone();
    description.key = PublicationKey::new(name).unwrap();
    description.navigation.clear();
    description.documents.retain(|doc| doc.key == key("one"));
    description.reading_order = vec![key("one")];
    if font {
        let resource = ResourceKey::new("body font").unwrap();
        description.documents[0].fonts.push(resource.clone());
        description.resources.push(ResourceDescriptor {
            key: resource,
            media_type: Some("font/ttf".into()),
        });
    }
    Publication::new(description).unwrap()
}
fn metadata_request(session: &mut ReadingSession) -> ResourceRequest {
    session
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadResource(request) if request.kind == ResourceKind::ImageMetadata => {
                Some(request)
            }
            _ => None,
        })
        .expect("metadata request")
}
fn font_data() -> FontResource {
    flow_document::test_fonts::charter_regular()
}

#[test]
fn required_fonts_block_activation_and_reject_wrong_representations() {
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let handle = session
        .register_publication(single_model("font book", true))
        .unwrap();
    let navigation = session.open_target(handle, target("one")).unwrap();
    let requests = session.take_effects().unwrap();
    let document_request = requests
        .iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadDocument(request) => Some(request),
            _ => None,
        })
        .unwrap();
    let font_request = requests
        .iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadResource(request) => Some(request),
            _ => None,
        })
        .unwrap();
    session
        .provide_document(document_request.id, import("# Font document\n\nBody."))
        .unwrap();
    assert_eq!(
        session.navigation_state(navigation),
        Some(&NavigationState::Pending)
    );
    assert!(!session.viewer().has_document());
    assert!(matches!(
        session.provide_resource(
            font_request.id,
            PreparedResource::ImageMetadata {
                width: 1,
                height: 1
            }
        ),
        Err(ReadingSessionError::ResponseMismatch)
    ));
    session
        .provide_resource(font_request.id, PreparedResource::Font(font_data()))
        .unwrap();
    ready(&session, navigation);
    assert!(session.viewer().has_document());
    assert_eq!(
        session
            .scoped_fonts(&session.active.as_ref().unwrap().key)
            .len(),
        1
    );
}

#[test]
fn font_environments_do_not_accumulate_across_publications() {
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    for (name, resource) in [
        (
            "first font environment",
            flow_document::test_fonts::charter_regular(),
        ),
        (
            "second font environment",
            flow_document::test_fonts::charter_bold(),
        ),
    ] {
        let handle = session
            .register_publication(single_model(name, true))
            .unwrap();
        let navigation = session.open_target(handle, target("one")).unwrap();
        for effect in session.take_effects().unwrap() {
            match effect {
                SessionEffect::LoadDocument(request) => session
                    .provide_document(request.id, import("# Font document\n\nBody."))
                    .unwrap(),
                SessionEffect::LoadResource(request) => session
                    .provide_resource(request.id, PreparedResource::Font(resource.clone()))
                    .unwrap(),
                _ => (),
            }
        }
        ready(&session, navigation);
        assert_eq!(
            session
                .scoped_fonts(&session.active.as_ref().unwrap().key)
                .len(),
            1
        );
    }
    let back = session.go_back().unwrap().unwrap();
    ready(&session, back);
    assert_eq!(
        session
            .scoped_fonts(&session.active.as_ref().unwrap().key)
            .len(),
        1
    );
}

#[test]
fn late_metadata_is_scoped_and_cancelled_image_responses_are_rejected() {
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let first = session
        .register_publication(single_model("first images", false))
        .unwrap();
    session.open_target(first, target("one")).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, import("![same source](cover.png)"))
        .unwrap();
    let old_image = metadata_request(&mut session);
    let second = session
        .register_publication(single_model("second images", false))
        .unwrap();
    session.open_target(second, target("one")).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, import("![same source](cover.png)"))
        .unwrap();
    let new_image = metadata_request(&mut session);
    assert_ne!(old_image.publication, new_image.publication);
    assert_eq!(old_image.resource, new_image.resource);
    assert!(matches!(
        session.provide_resource(
            old_image.id,
            PreparedResource::ImageMetadata {
                width: 4,
                height: 8
            }
        ),
        Err(ReadingSessionError::StaleRequest)
    ));
    session
        .provide_resource(
            new_image.id,
            PreparedResource::ImageMetadata {
                width: 8,
                height: 4,
            },
        )
        .unwrap();
    session.prepare_continuous_scene(320, 480);
    let mut buffer = PixelBuffer::new(320, 480, [0; 3]).unwrap();
    session
        .render_into(&mut buffer.surface_mut(), 320, 480, 1.0)
        .unwrap();
    let pixels = session
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadResource(request)
                if matches!(request.kind, ResourceKind::ImagePixels { .. }) =>
            {
                Some(request)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(pixels.publication, new_image.publication);
    session
        .provide_resource(
            pixels.id,
            PreparedResource::ImagePixels(
                ImagePixels::from_rgba(8, 4, vec![255; 8 * 4 * 4]).unwrap(),
            ),
        )
        .unwrap();
}

#[test]
fn publication_pagination_waits_for_intrinsic_metadata_and_rebuilds_after_response() {
    let mut settings = Settings::default();
    settings.reading_mode = ReadingMode::Paginated;
    let mut session = ReadingSession::new(settings);
    session.resize((320, 480));
    let handle = session
        .register_publication(single_model("image pagination", false))
        .unwrap();
    session.open_target(handle, target("one")).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, import("![figure](cover.png)\n\nAfter figure."))
        .unwrap();
    let image = metadata_request(&mut session);
    session.prepare_publication_pagination().unwrap();
    assert!(!session.publication_pagination_complete());
    assert_eq!(session.publication_paginated_page_metrics(), None);
    session
        .provide_resource(
            image.id,
            PreparedResource::ImageMetadata {
                width: 100,
                height: 1000,
            },
        )
        .unwrap();
    for _ in 0..4 {
        session.take_effects().unwrap();
    }
    assert!(session.publication_pagination_complete());
    let mut expected = DocumentViewer::new("expected", session.settings.clone());
    expected.register_image("cover.png", None, 100, 1000, vec![0; 100 * 1000 * 4]);
    expected.replace_document(Ok(import("![figure](cover.png)\n\nAfter figure.").document));
    assert_eq!(
        session.publication_page_index().unwrap(),
        &[Some(
            expected.prepare_pagination(320, 480).unwrap().1 as u32
        )]
    );
}

#[test]
fn image_metadata_invalidates_only_its_document_page_counts() {
    let (mut session, handle) = loaded();
    let pages = vec![Some(3); session.model(handle).unwrap().reading_order().len()];
    assert!(session.install_publication_page_index(pages.clone()));
    let other = session
        .register_publication(single_model("other book", false))
        .unwrap();
    let other_key = DocumentCacheKey {
        publication: other,
        document: key("one"),
    };
    session.invalidate_document_page_counts(&other_key);
    assert_eq!(session.publication_page_index().unwrap(), pages);

    let scope = DocumentCacheKey {
        publication: handle,
        document: key("one"),
    };
    session.invalidate_document_page_counts(&scope);
    for (document, pages) in session
        .model(handle)
        .unwrap()
        .reading_order()
        .iter()
        .zip(session.publication_page_index().unwrap())
    {
        assert_eq!(
            *pages,
            if document == &key("one") {
                None
            } else {
                Some(3)
            }
        );
    }
}

#[test]
fn publication_image_pagination_loads_documents_linearly() {
    fn drain(session: &mut ReadingSession, images: bool, counts: &mut [usize; 3]) {
        for _ in 0..50_000 {
            let effects = session.take_effects().unwrap();
            if effects.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    SessionEffect::LoadDocument(request) => {
                        let slot = match request.purpose {
                            RequestPurpose::Foreground => 0,
                            RequestPurpose::Pagination => 1,
                            RequestPurpose::Prefetch => 2,
                            _ => panic!("unexpected purpose"),
                        };
                        counts[slot] += 1;
                        let source = if images {
                            format!(
                                "# Chapter\n\n![image](image-{}.png)",
                                request.document.as_str()
                            )
                        } else {
                            "# Chapter\n\nA short paragraph.".into()
                        };
                        session
                            .provide_document(request.id, import(&source))
                            .unwrap();
                    }
                    SessionEffect::LoadResource(request) => {
                        assert_eq!(request.kind, ResourceKind::ImageMetadata);
                        session
                            .provide_resource(
                                request.id,
                                PreparedResource::ImageMetadata {
                                    width: 64,
                                    height: 64,
                                },
                            )
                            .unwrap();
                    }
                    _ => (),
                }
            }
        }
        panic!("pagination did not settle");
    }

    for images in [false, true] {
        for count in [10, 50, 100] {
            let mut description = single_model("linear pagination", false)
                .description()
                .clone();
            description.reading_order = (0..count).map(|i| key(&format!("chapter-{i}"))).collect();
            description.documents = description
                .reading_order
                .iter()
                .map(|key| DocumentDescriptor {
                    key: key.clone(),
                    title: "Chapter".into(),
                    fonts: vec![],
                })
                .collect();
            let model = Publication::new(description).unwrap();
            let target = model.start().unwrap();
            let mut session = ReadingSession::new(Settings {
                reading_mode: ReadingMode::Paginated,
                ..Default::default()
            });
            assert!(session.register_font_resource(font_data()));
            session.resize((600, 800));
            let handle = session.register_publication(model).unwrap();
            session.open_target(handle, target).unwrap();
            let mut counts = [0; 3];
            drain(&mut session, images, &mut counts);
            session.prepare_publication_pagination().unwrap();
            drain(&mut session, images, &mut counts);
            assert!(session.publication_pagination_complete());
            eprintln!("images={images}, chapters={count}: {counts:?}");
            assert!(
                counts.iter().sum::<usize>() <= 2 * count,
                "excessive document loads: {counts:?}"
            );
        }
    }
}

#[test]
fn cancelled_images_are_requested_again_after_history_restoration() {
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let first = session
        .register_publication(single_model("first pending image", false))
        .unwrap();
    session.open_target(first, target("one")).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, import("![image](cover.png)"))
        .unwrap();
    let cancelled = metadata_request(&mut session);
    let second = session
        .register_publication(single_model("second pending image", false))
        .unwrap();
    session.open_target(second, target("one")).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, import("![image](cover.png)"))
        .unwrap();
    metadata_request(&mut session);
    session.go_back().unwrap().unwrap();
    let restored = metadata_request(&mut session);
    assert_eq!(restored.publication, cancelled.publication);
    assert_ne!(restored.id, cancelled.id);
    session
        .provide_resource(
            restored.id,
            PreparedResource::ImageMetadata {
                width: 8,
                height: 4,
            },
        )
        .unwrap();
}

#[test]
fn failed_background_metadata_is_retryable_without_replacing_the_view() {
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let handle = session.register_publication(publication("r1")).unwrap();
    session.open_target(handle, target("two")).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, document("two"))
        .unwrap();
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    let request = session
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadDocument(request) if request.document == key("one") => Some(request),
            _ => None,
        })
        .unwrap();
    session
        .provide_document(request.id, import("![background](cover.png)"))
        .unwrap();
    let image = metadata_request(&mut session);
    session
        .fail_request(image.id, LoadFailure::new("temporary image failure"))
        .unwrap();
    assert!(session.publication_pagination_failure().is_some());
    assert_eq!(session.current_target().unwrap().document, key("two"));
    session.retry_failed_resources();
    let retried = metadata_request(&mut session);
    session
        .provide_resource(
            retried.id,
            PreparedResource::ImageMetadata {
                width: 4,
                height: 4,
            },
        )
        .unwrap();
    for _ in 0..4 {
        session.take_effects().unwrap();
    }
    assert!(session.publication_pagination_complete());
    assert_eq!(session.current_target().unwrap().document, key("two"));
}

#[test]
fn image_request_backpressure_does_not_drop_work() {
    let mut session = ReadingSession::new(Settings::default());
    let handle = session
        .register_publication(single_model("many images", false))
        .unwrap();
    session.open_target(handle, target("one")).unwrap();
    let request = pending_document(&mut session);
    let source = (0..40)
        .map(|index| format!("![figure](image-{index}.png)\n\n"))
        .collect::<String>();
    session
        .provide_document(request.id, import(&source))
        .unwrap();
    let mut supplied = HashSet::new();
    for _ in 0..10 {
        let requests = session.take_effects().unwrap();
        assert!(session.requests.len() <= 32);
        for effect in requests {
            if let SessionEffect::LoadResource(request) = effect {
                assert_eq!(request.kind, ResourceKind::ImageMetadata);
                supplied.insert(request.resource);
                session
                    .provide_resource(
                        request.id,
                        PreparedResource::ImageMetadata {
                            width: 4,
                            height: 4,
                        },
                    )
                    .unwrap();
            }
        }
        if supplied.len() == 40 {
            break;
        }
    }
    assert_eq!(supplied.len(), 40);
}

#[test]
fn invalid_font_failure_and_retry_preserve_the_displayed_document() {
    let (mut session, _) = loaded();
    let handle = session
        .register_publication(single_model("bad font", true))
        .unwrap();
    let navigation = session.open_target(handle, target("one")).unwrap();
    let effects = session.take_effects().unwrap();
    let resource = effects
        .iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadResource(request) => Some(request),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        session.provide_resource(
            resource.id,
            PreparedResource::Font(FontResource::from_bytes(Arc::from([])))
        ),
        Err(ReadingSessionError::InvalidResource)
    ));
    session
        .fail_request(resource.id, LoadFailure::new("invalid font"))
        .unwrap();
    assert!(matches!(
        session.navigation_state(navigation),
        Some(NavigationState::Failed(_))
    ));
    assert_ne!(session.current_publication(), Some(handle));
    assert!(session.viewer().has_document());
}
