use super::*;
use flow_document::{formats::markdown, model::flow::AnchorId, pixels::PixelBuffer};

fn key(value: &str) -> DocumentKey {
    DocumentKey::new(value).unwrap()
}
fn target(value: &str) -> PublicationTarget {
    PublicationTarget::document(key(value))
}
fn publication(revision: &str) -> Publication {
    Publication::new(PublicationDescription {
        key: PublicationKey::new("opaque: shelf / α").unwrap(),
        revision: PublicationRevision::new(revision).unwrap(),
        metadata: PublicationMetadata::default(),
        documents: ["one", "two", "side"]
            .into_iter()
            .map(|id| DocumentDescriptor {
                key: key(id),
                title: id.into(),
                fonts: Vec::new(),
            })
            .collect(),
        reading_order: ["one", "one", "two", "one"].into_iter().map(key).collect(),
        navigation: vec![crate::toc::PublicationNavigationEntry {
            label: "One".into(),
            target: target("one"),
            children: vec![crate::toc::PublicationNavigationEntry {
                label: "Two".into(),
                target: PublicationTarget {
                    document: key("two"),
                    anchor: Some(AnchorId::new("target")),
                },
                children: Vec::new(),
            }],
        }],
        resources: Vec::new(),
        start: None,
    })
    .unwrap()
}
fn import(source: &str) -> ImportedDocument {
    ImportedDocument::new(
        markdown::parse(source, Default::default())
            .unwrap()
            .document,
    )
}
fn document(id: &str) -> ImportedDocument {
    let source = match id {
        "one" => "# One\n\n[continue](two.md#target)\n\nFirst body.",
        "two" => "# Two\n\n<a id=\"target\"></a>Target body.",
        _ => "# Side\n\nAuxiliary document.",
    };
    let mut document = import(source);
    document.links.insert(
        DocumentReference::Relative("two.md".into()),
        LinkBinding::Document(target("two")),
    );
    document
}

/// A testing host: no EPUB, filesystem, format dispatch or async runtime.
fn service(session: &mut ReadingSession) -> Vec<SessionEffect> {
    let mut transcript = Vec::new();
    for _ in 0..100 {
        let effects = session.take_effects().unwrap();
        let mut more = false;
        for effect in &effects {
            match effect {
                SessionEffect::ResolvePublication { id, .. } => {
                    session.provide_publication(*id, publication("r1")).unwrap();
                    more = true;
                }
                SessionEffect::LoadDocument(request) => {
                    session
                        .provide_document(request.id, document(request.document.as_str()))
                        .unwrap();
                    more = true;
                }
                SessionEffect::ContinueWork => more = true,
                SessionEffect::LoadResource(_) | SessionEffect::ResolveLink { .. } => {
                    panic!("unexpected host work {effect:?}")
                }
                _ => (),
            }
        }
        transcript.extend(effects);
        if !more {
            return transcript;
        }
    }
    panic!("session produced unbounded work");
}
fn ready(session: &ReadingSession, navigation: NavigationId) {
    assert!(matches!(
        session.navigation_state(navigation),
        Some(NavigationState::Ready { .. })
    ));
}
fn loaded() -> (ReadingSession, PublicationHandle) {
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let handle = session.register_publication(publication("r1")).unwrap();
    let navigation = session.open_target(handle, target("one")).unwrap();
    service(&mut session);
    ready(&session, navigation);
    (session, handle)
}
fn pending_document(session: &mut ReadingSession) -> DocumentRequest {
    session
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadDocument(request) if request.purpose <= RequestPurpose::History => {
                Some(request)
            }
            _ => None,
        })
        .expect("foreground document request")
}

#[test]
fn generic_navigation_history_outline_and_auxiliary_documents() {
    let (mut session, handle) = loaded();
    assert_eq!(session.reading_order_position(), Some((1, 2)));
    let navigation = session
        .follow_destination(Destination::Document {
            document: DocumentReference::Relative("two.md".into()),
            anchor: Some(AnchorId::new("target")),
        })
        .unwrap()
        .unwrap();
    service(&mut session);
    ready(&session, navigation);
    assert_eq!(session.current_target().unwrap().document, key("two"));
    assert_eq!(session.reading_order_position(), Some((2, 2)));
    assert_eq!(
        session.publications().get(handle).unwrap().navigation()[0].children[0]
            .target
            .anchor
            .as_ref()
            .unwrap()
            .as_str(),
        "target"
    );
    let previous = session.go_back().unwrap().unwrap();
    service(&mut session);
    ready(&session, previous);
    assert_eq!(session.current_target().unwrap().document, key("one"));
    let next = session.go_forward().unwrap().unwrap();
    service(&mut session);
    ready(&session, next);
    assert_eq!(session.current_target().unwrap().document, key("two"));
    session.open_target(handle, target("side")).unwrap();
    service(&mut session);
    assert_eq!(session.reading_order_position(), None);
    assert_eq!(session.move_reading_order(1).unwrap(), None);
}

#[test]
fn repeated_references_are_skipped_in_both_directions_and_totals() {
    let (mut session, _) = loaded();
    let next = session.move_reading_order(1).unwrap().unwrap();
    service(&mut session);
    ready(&session, next);
    assert_eq!(session.current_target().unwrap().document, key("two"));
    assert!(session.move_reading_order(1).unwrap().is_none());
    let previous = session.move_reading_order(-1).unwrap().unwrap();
    service(&mut session);
    ready(&session, previous);
    assert_eq!(session.current_target().unwrap().document, key("one"));
    assert!(session.history_is_empty());
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    service(&mut session);
    assert!(session.publication_pagination_complete());
    assert_eq!(session.publication_page_index().unwrap().len(), 2);
    assert_eq!(session.publication_paginated_page_metrics(), Some((1, 2)));
}

#[test]
fn asynchronous_navigation_is_transactional_and_rejects_cancelled_responses() {
    let (mut session, handle) = loaded();
    session.set_cache_limits(0, 0);
    let before = session.current_target().cloned();
    let navigation = session.open_target(handle, target("side")).unwrap();
    let request = pending_document(&mut session);
    assert_eq!(
        session.navigation_state(navigation),
        Some(&NavigationState::Pending)
    );
    assert_eq!(session.current_target(), before.as_ref());
    assert!(session.back.is_empty());
    assert!(session.cancel_navigation(navigation));
    assert!(matches!(
        session.provide_document(request.id, document("side")),
        Err(ReadingSessionError::StaleRequest)
    ));
    assert_eq!(session.current_target(), before.as_ref());
    let newer = session.open_target(handle, target("side")).unwrap();
    let old = pending_document(&mut session);
    let newest = session.open_target(handle, target("two")).unwrap();
    let wanted = pending_document(&mut session);
    assert_ne!(newer, newest);
    assert!(matches!(
        session.provide_document(old.id, document("side")),
        Err(ReadingSessionError::StaleRequest)
    ));
    session
        .provide_document(wanted.id, document("two"))
        .unwrap();
    ready(&session, newest);
    assert_eq!(session.back.len(), 1);
}

#[test]
fn failed_navigation_is_retryable_without_history_damage() {
    let (mut session, handle) = loaded();
    let navigation = session.open_target(handle, target("side")).unwrap();
    let request = pending_document(&mut session);
    session
        .fail_request(request.id, LoadFailure::new("host permission denied"))
        .unwrap();
    assert!(matches!(
        session.navigation_state(navigation),
        Some(NavigationState::Failed(_))
    ));
    assert_eq!(session.current_target().unwrap().document, key("one"));
    assert!(session.back.is_empty());
    let retry = session.retry_navigation(navigation).unwrap();
    let request = pending_document(&mut session);
    session
        .provide_document(request.id, document("side"))
        .unwrap();
    ready(&session, retry);
    assert_eq!(session.back.len(), 1);
}

#[test]
fn missing_publication_and_wrong_revision_are_host_work_not_uri_routing() {
    let mut session = ReadingSession::new(Settings::default());
    let navigation = session
        .open_publication(publication("r1").key().clone())
        .unwrap();
    let effects = session.take_effects().unwrap();
    let id = effects
        .iter()
        .find_map(|effect| match effect {
            SessionEffect::ResolvePublication { id, .. } => Some(*id),
            _ => None,
        })
        .unwrap();
    let mut wrong = publication("r1").description().clone();
    wrong.key = PublicationKey::new("wrong").unwrap();
    assert!(matches!(
        session.provide_publication(id, Publication::new(wrong).unwrap()),
        Err(ReadingSessionError::ResponseMismatch)
    ));
    session.provide_publication(id, publication("r1")).unwrap();
    service(&mut session);
    ready(&session, navigation);
    assert_eq!(session.current_target().unwrap().document, key("one"));
}

#[test]
fn prefetch_promotion_coalesces_work_and_bounds_history_and_cache() {
    let mut session = ReadingSession::new(Settings::default());
    session.set_cache_limits(1, 0);
    let handle = session.register_publication(publication("r1")).unwrap();
    session.open_target(handle, target("one")).unwrap();
    let first = pending_document(&mut session);
    session.provide_document(first.id, document("one")).unwrap();
    let request = session
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::LoadDocument(request) if request.purpose == RequestPurpose::Prefetch => {
                Some(request)
            }
            _ => None,
        })
        .unwrap();
    let navigation = session.open_target(handle, target("two")).unwrap();
    let effects = session.take_effects().unwrap();
    assert!(effects.iter().any(
        |effect| matches!(effect, SessionEffect::Reprioritize { id, .. } if *id == request.id)
    ));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, SessionEffect::LoadDocument(_)))
    );
    session
        .provide_document(request.id, document("two"))
        .unwrap();
    ready(&session, navigation);
    session.open_target(handle, target("side")).unwrap();
    service(&mut session);
    assert!(session.document_cache.len() <= 1);
    assert!(session.back.iter().all(|entry| entry.viewer.is_none()));
    let back = session.go_back().unwrap().unwrap();
    service(&mut session);
    ready(&session, back);
    assert_eq!(session.current_target().unwrap().document, key("two"));
    assert!(session.document_cache.len() <= 1);
}

#[test]
fn retained_history_reuses_real_composition_identity() {
    let (mut session, handle) = loaded();
    let before = session.viewer().continuous_layout().unwrap() as *const _;
    session.open_target(handle, target("two")).unwrap();
    service(&mut session);
    session.go_back().unwrap();
    service(&mut session);
    assert_eq!(
        session.viewer().continuous_layout().unwrap() as *const _,
        before
    );
}

#[test]
fn settings_rebuild_counts_but_paint_changes_retain_composition() {
    let (mut session, _) = loaded();
    let before = session.viewer().continuous_layout().unwrap() as *const _;
    session.set_dark_mode(true);
    session.prepare_continuous_scene(320, 480);
    assert_eq!(
        session.viewer().continuous_layout().unwrap() as *const _,
        before
    );
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    service(&mut session);
    assert!(session.publication_pagination_complete());
    session.set_font_size(26.0);
    assert!(session.publication_page_index().is_none());
    session.prepare_publication_pagination().unwrap();
    service(&mut session);
    let actual = session.publication_page_index().unwrap();
    for (index, id) in ["one", "two"].into_iter().enumerate() {
        let mut expected = DocumentViewer::new(id, session.settings().clone());
        expected.replace_document(Ok(document(id).document));
        assert_eq!(
            actual[index],
            Some(expected.prepare_pagination(320, 480).unwrap().1 as u32)
        );
    }
    session.set_max_width(300.0);
    assert!(session.publication_page_index().is_none());
}

#[test]
fn restoration_uses_canonical_text_identity_across_source_and_runtime_changes() {
    let (session, _) = loaded();
    let saved = session.persistent_location().unwrap();
    let mut restored = ReadingSession::new(Settings::default());
    restored.resize((640, 720));
    let handle = restored.register_publication(publication("r2")).unwrap();
    let navigation = restored.restore_location(&saved).unwrap();
    service(&mut restored);
    assert_eq!(restored.current_publication(), Some(handle));
    assert_eq!(
        restored.navigation_state(navigation),
        Some(&NavigationState::Ready {
            restored: Some(RestoreOutcome::Exact)
        })
    );
    let mut changed = ReadingSession::new(Settings::default());
    changed.resize((320, 480));
    changed.set_background_request_limit(0);
    changed.register_publication(publication("r1")).unwrap();
    let navigation = changed.restore_location(&saved).unwrap();
    let request = pending_document(&mut changed);
    let source = format!(
        "New introductory text.\n\n{}",
        document("one").document.text().as_str()
    );
    changed
        .provide_document(request.id, import(&source))
        .unwrap();
    assert_eq!(
        changed.navigation_state(navigation),
        Some(&NavigationState::Ready {
            restored: Some(RestoreOutcome::Recovered)
        })
    );
}

#[test]
fn missing_model_restore_and_removed_document_are_explicit() {
    let (session, _) = loaded();
    let saved = session.persistent_location().unwrap();
    let mut restored = ReadingSession::new(Settings::default());
    restored.resize((320, 480));
    let navigation = restored.restore_location(&saved).unwrap();
    service(&mut restored);
    ready(&restored, navigation);
    let mut input = publication("r2").description().clone();
    input.documents.retain(|doc| doc.key != saved.document);
    input.reading_order.retain(|doc| doc != &saved.document);
    input.navigation.clear();
    let mut missing = ReadingSession::new(Settings::default());
    missing
        .register_publication(Publication::new(input).unwrap())
        .unwrap();
    let navigation = missing.restore_location(&saved).unwrap();
    assert!(matches!(
        missing.navigation_state(navigation),
        Some(NavigationState::Failed(_))
    ));
    assert!(!missing.viewer().has_document());
}

#[test]
fn saved_anchor_does_not_override_the_actual_position_and_fallback_is_reported() {
    let (session, _) = loaded();
    let mut saved = session.persistent_location().unwrap();
    saved.anchor_hint = Some(AnchorId::new("missing"));
    let mut restored = ReadingSession::new(Settings::default());
    restored.resize((320, 480));
    restored.register_publication(publication("r1")).unwrap();
    let navigation = restored.restore_location(&saved).unwrap();
    service(&mut restored);
    assert_eq!(
        restored.navigation_state(navigation),
        Some(&NavigationState::Ready {
            restored: Some(RestoreOutcome::Exact)
        })
    );
    saved.text_fingerprint = TextFingerprint::of_document(&import("different text").document);
    saved.position.context_before = "unrecoverable".into();
    saved.position.context_after = "evidence".into();
    let navigation = restored.restore_location(&saved).unwrap();
    service(&mut restored);
    assert_eq!(
        restored.navigation_state(navigation),
        Some(&NavigationState::Ready {
            restored: Some(RestoreOutcome::DocumentStartFallback)
        })
    );
}

#[test]
fn saved_locations_have_only_a_compatible_semantic_order() {
    let (mut session, handle) = loaded();
    let first = session.persistent_location().unwrap();
    session.open_target(handle, target("two")).unwrap();
    service(&mut session);
    let second = session.persistent_location().unwrap();
    assert_eq!(
        session.compare_persistent_locations(&first, &second),
        Some(Ordering::Less)
    );
    let mut incompatible = first.clone();
    incompatible.text_fingerprint = second.text_fingerprint;
    assert_eq!(
        session.compare_persistent_locations(&first, &incompatible),
        None
    );
    incompatible = second;
    incompatible.publication_revision = PublicationRevision::new("another").unwrap();
    assert_eq!(
        session.compare_persistent_locations(&first, &incompatible),
        None
    );
}

#[test]
fn external_and_unresolved_links_remain_host_policy() {
    let (mut session, _) = loaded();
    session
        .follow_destination(Destination::External("file:///not-an-engine-loader".into()))
        .unwrap();
    assert!(
        session
            .take_effects()
            .unwrap()
            .contains(&SessionEffect::OpenExternal(
                "file:///not-an-engine-loader".into()
            ))
    );
    let id = session
        .follow_destination(Destination::Document {
            document: DocumentReference::Host("custom reference".into()),
            anchor: None,
        })
        .unwrap()
        .unwrap();
    let request = session
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::ResolveLink { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    session
        .provide_link_target(
            request,
            ResolvedLink::External("https://example.test".into()),
        )
        .unwrap();
    assert_eq!(
        session.navigation_state(id),
        Some(&NavigationState::External)
    );
    assert_eq!(session.current_target().unwrap().document, key("one"));
}

#[test]
fn resource_bindings_are_validated_before_commit() {
    let (mut session, handle) = loaded();
    let navigation = session.open_target(handle, target("side")).unwrap();
    let request = pending_document(&mut session);
    let mut invalid = document("side");
    invalid.links.insert(
        DocumentReference::Relative("bad".into()),
        LinkBinding::Document(target("absent")),
    );
    assert!(session.provide_document(request.id, invalid).is_err());
    assert_eq!(session.current_target().unwrap().document, key("one"));
    session
        .provide_document(request.id, document("side"))
        .unwrap();
    ready(&session, navigation);
}

#[test]
fn background_pagination_reuses_resident_history_without_materialization_requests() {
    let (mut session, handle) = loaded();
    session.open_target(handle, target("two")).unwrap();
    service(&mut session);
    session.set_cache_limits(0, 4);
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    let transcript = service(&mut session);
    assert!(session.publication_pagination_complete());
    assert!(
        !transcript
            .iter()
            .any(|effect| matches!(effect, SessionEffect::LoadDocument(_)))
    );
    assert_eq!(session.current_target().unwrap().document, key("two"));
}

/// Drive a synthetic publication without source files or link bindings.
fn service_synthetic_chapters(session: &mut ReadingSession) -> Vec<SessionEffect> {
    let mut transcript = Vec::new();
    for _ in 0..400 {
        let effects = session.take_effects().unwrap();
        let mut more = false;
        for effect in &effects {
            match effect {
                SessionEffect::LoadDocument(request) => {
                    session
                        .provide_document(request.id, import("Synthetic chapter text."))
                        .unwrap();
                    more = true;
                }
                SessionEffect::ContinueWork => more = true,
                SessionEffect::LoadResource(_)
                | SessionEffect::ResolvePublication { .. }
                | SessionEffect::ResolveLink { .. } => {
                    panic!("unexpected host work {effect:?}")
                }
                _ => (),
            }
        }
        transcript.extend(effects);
        if !more {
            return transcript;
        }
    }
    panic!("session produced unbounded synthetic chapter work");
}

#[test]
fn background_pagination_waits_for_image_metadata_before_composing() {
    let (mut session, handle) = loaded();
    // Supply a background job directly to isolate metadata scheduling from
    // foreground rendering and prefetch. No actual image bytes are needed.
    session.set_background_request_limit(3);
    session.set_reading_mode(ReadingMode::Paginated);
    assert!(session.install_publication_page_index(vec![Some(1), None]));
    session
        .accept_pagination_document(
            DocumentCacheKey {
                publication: handle,
                document: key("two"),
            },
            CachedDocument {
                imported: import("Text. ![a](a.png) ![b](b.png) ![c](c.png)"),
                semantic_key: None,
            },
        )
        .unwrap();
    session.advance_pagination().unwrap();
    let requests = session
        .effects
        .iter()
        .filter_map(|effect| match effect {
            SessionEffect::LoadResource(request) if request.kind == ResourceKind::ImageMetadata => {
                Some(request.id)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    let mut premature_compositions = Vec::new();
    for remaining in (1..=3).rev() {
        let job = session
            .publication_pagination
            .as_ref()
            .unwrap()
            .job
            .as_ref()
            .unwrap();
        assert_eq!(job.waiting_metadata.len(), remaining);
        premature_compositions.push(job.viewer.active_composition().is_some());
        assert!(!session.publication_pagination_complete());
        session
            .provide_resource(
                requests[3 - remaining],
                PreparedResource::ImageMetadata {
                    width: 100,
                    height: 100,
                },
            )
            .unwrap();
    }
    // Completion must still work once all dimensions are available.
    assert!(session.publication_pagination_complete());
    assert_eq!(session.current_target().unwrap().document, key("one"));
    assert_eq!(
        premature_compositions,
        vec![false; 3],
        "background pagination composed with unresolved image dimensions"
    );
}

#[test]
fn background_pagination_does_not_reimport_prefetched_neighbors() {
    let mut description = publication("r1").description().clone();
    description.documents = (0..77)
        .map(|index| DocumentDescriptor {
            key: key(&format!("chapter-{index}")),
            title: String::new(),
            fonts: Vec::new(),
        })
        .collect();
    description.reading_order = description
        .documents
        .iter()
        .map(|document| document.key.clone())
        .collect();
    description.navigation.clear();
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let handle = session
        .register_publication(Publication::new(description).unwrap())
        .unwrap();
    session.open_target(handle, target("chapter-0")).unwrap();
    let mut transcript = service_synthetic_chapters(&mut session);
    // The adjacent chapter is already prefetched before pagination starts.
    assert!(session.document_cache.contains_key(&DocumentCacheKey {
        publication: handle,
        document: key("chapter-1"),
    }));
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    transcript.extend(service_synthetic_chapters(&mut session));
    assert!(session.publication_pagination_complete());
    assert_eq!(session.publication_page_index().unwrap().len(), 77);
    assert_eq!(session.current_target().unwrap().document, key("chapter-0"));
    let neighbor_prefetches = transcript.iter().filter(|effect| matches!(effect,
        SessionEffect::LoadDocument(request)
            if request.purpose == RequestPurpose::Prefetch && request.document == key("chapter-1")
    )).count();
    assert_eq!(
        neighbor_prefetches, 1,
        "pagination evicted and reimported the prefetched neighbor"
    );
    let fonts = session.font_context_stats();
    assert_eq!(
        fonts.environment_builds, 1,
        "all pagination viewers must reuse the foreground font environment"
    );
    assert!(fonts.environment_hits >= 77);
    assert!(fonts.fonts.metric_hits > 0);
    assert!(fonts.fonts.plan_hits > 0);
}

#[test]
fn host_font_cache_policy_changes_preserve_the_publication_index() {
    let (mut session, _) = loaded();
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    service(&mut session);
    let pages = session.publication_page_index().unwrap().to_vec();
    session.set_font_context_options(flow_document::font_context::FontContextOptions {
        max_environments: 0,
        cache_bytes: 0,
        plans_per_face: 0,
        max_faces_per_environment: 1,
    });
    assert!(session.publication_pagination_complete());
    assert_eq!(session.publication_page_index(), Some(pages.as_slice()));
    assert!(session.prepare_pagination(320, 480).is_some());
    assert_eq!(session.font_context_stats().cache_bytes, 0);
    assert_eq!(session.font_context_stats().environments, 0);
}

#[test]
fn rendering_uses_the_coordinated_host_surface_path() {
    let (mut session, _) = loaded();
    let mut buffer = PixelBuffer::new(320, 480, [255; 3]).unwrap();
    session
        .render_into(&mut buffer.surface_mut(), 320, 480, 1.0)
        .unwrap();
    assert!(
        buffer
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != [255; 3])
    );
}

#[test]
fn page_index_installs_validate_shape_and_dependency_identity() {
    use crate::cache_codec::DependencyDigest;
    let (mut session, _) = loaded();
    // Cached indexes and their dependency identity are available even while
    // reading continuously; loading one does not require a pagination job.
    assert!(
        session
            .persistent_publication_index_digest(DependencyDigest([1; 32]))
            .is_some()
    );
    assert!(session.install_publication_page_index(vec![Some(1), Some(2)]));
    assert_eq!(
        session.publication_page_index(),
        Some(&[Some(1), Some(2)][..])
    );
    assert!(session.publication_pagination_complete());
    session.set_reading_mode(ReadingMode::Paginated);
    assert!(!session.install_publication_page_index(vec![Some(0), Some(1)]));
    assert!(!session.install_publication_page_index(vec![Some(1)]));
    assert!(session.install_publication_page_index(vec![Some(1), None]));
    assert!(!session.publication_pagination_complete());
    let original = session
        .persistent_publication_index_digest(DependencyDigest([1; 32]))
        .unwrap();
    assert_ne!(
        Some(original),
        session.persistent_publication_index_digest(DependencyDigest([2; 32]))
    );
    session.set_font_size(26.0);
    assert_ne!(
        Some(original),
        session.persistent_publication_index_digest(DependencyDigest([1; 32]))
    );
}

#[cfg(feature = "epub")]
mod epub_tests;
#[test]
fn registered_revision_selection_wins_over_a_saved_source_revision() {
    let (mut session, _) = loaded();
    let saved = session.persistent_location().unwrap();
    let selected = session.register_publication(publication("r2")).unwrap();
    session.restore_location(&saved).unwrap();
    let request = pending_document(&mut session);
    assert_eq!(request.revision.as_str(), "r2");
    session
        .provide_document(request.id, document("one"))
        .unwrap();
    assert_eq!(session.current_publication(), Some(selected));
}

#[test]
fn cancelling_unresolved_work_rejects_cross_session_and_stale_ids() {
    let mut first = ReadingSession::new(Settings::default());
    let id = first
        .open_publication(publication("r1").key().clone())
        .unwrap();
    let request = first
        .take_effects()
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            SessionEffect::ResolvePublication { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    let mut second = ReadingSession::new(Settings::default());
    second
        .open_publication(publication("r1").key().clone())
        .unwrap();
    assert!(matches!(
        second.provide_publication(request, publication("r1")),
        Err(ReadingSessionError::StaleRequest)
    ));
    assert!(first.cancel_navigation(id));
    assert!(matches!(
        first.provide_publication(request, publication("r1")),
        Err(ReadingSessionError::StaleRequest)
    ));
}

mod resource_tests;
