use super::*;
use crate::{
    formats::markdown,
    pixels::PixelBuffer,
    render::{InteractionState, Renderer},
    settings::Settings,
    test_fonts,
    viewer::DocumentViewer,
};

fn document(text: &str) -> crate::model::flow::FlowDocument {
    markdown::parse(text, Default::default()).unwrap().document
}
fn compose(
    renderer: &mut Renderer,
    context: &mut FontContext,
    text: &str,
) -> crate::layout::ComposedDocument {
    renderer.with_font_context(context, |renderer| {
        renderer.compose_document(&document(text), 240.0)
    })
}

#[test]
fn constructors_are_lazy_and_environments_outlive_viewers() {
    let mut context = FontContext::default();
    let renderer = Renderer::new(Settings::default());
    assert!(renderer.fonts.backend.borrow().is_none());
    drop(renderer);
    assert_eq!(context.stats().environment_builds, 0);
    for _ in 0..3 {
        let mut viewer = DocumentViewer::new(
            "independent item",
            Settings {
                reading_mode: crate::settings::ReadingMode::Paginated,
                ..Settings::default()
            },
        );
        viewer.replace_document(Ok(document("Repeated paragraph words.")));
        assert!(
            viewer
                .with_font_context(&mut context, |viewer| viewer.prepare_pagination(320, 480))
                .is_some()
        );
    }
    let stats = context.stats();
    assert_eq!(stats.environment_builds, 1);
    assert_eq!(stats.environments, 1);
    assert_eq!(stats.environment_hits, 2);
    assert!(stats.fonts.metric_hits > 0);
}

#[test]
fn environment_identity_is_content_and_order_not_document_scope() {
    let mut context = FontContext::default();
    for namespace in ["one", "two"] {
        let mut renderer = Renderer::new(Settings::default());
        renderer.set_resource_namespace(Some(namespace.into()));
        assert!(renderer.set_font_resources_with_context(
            vec![test_fonts::charter_regular(), test_fonts::charter_bold()],
            &mut context
        ));
        compose(&mut renderer, &mut context, "Same font environment.");
    }
    assert_eq!(context.stats().environment_builds, 1);
    assert_eq!(context.stats().fonts.registrations, 2);
    let mut renderer = Renderer::new(Settings::default());
    assert!(renderer.set_font_resources_with_context(
        vec![test_fonts::charter_bold(), test_fonts::charter_regular()],
        &mut context
    ));
    assert_eq!(context.stats().environment_builds, 2);
}

#[test]
fn preferences_and_policy_are_isolated_but_sizes_are_not_environments() {
    let mut context = FontContext::default();
    let mut renderer = Renderer::new(Settings::default());
    compose(&mut renderer, &mut context, "Policy.");
    renderer.set_font_size(22.0);
    compose(&mut renderer, &mut context, "Policy.");
    assert_eq!(context.stats().environment_builds, 1);
    let mut settings = renderer.settings().clone();
    settings.body_fonts = vec!["Charter".into()];
    renderer.set_settings(settings.clone());
    compose(&mut renderer, &mut context, "Policy.");
    settings.author_fonts = !settings.author_fonts;
    renderer.set_settings(settings.clone());
    compose(&mut renderer, &mut context, "Policy.");
    settings.code_fonts = vec!["Fira Code".into()];
    renderer.set_settings(settings);
    compose(&mut renderer, &mut context, "Policy.");
    assert_eq!(context.stats().environment_builds, 4);
}

#[test]
fn failed_registration_preserves_cached_and_private_state() {
    let mut context = FontContext::default();
    let mut renderer = Renderer::new(Settings::default());
    let before = compose(&mut renderer, &mut context, "A stable paragraph.");
    let digest = renderer.persistent_layout_digest();
    let stats = context.stats();
    let invalid = FontResource::from_bytes(std::sync::Arc::from(b"invalid font".as_slice()));
    assert!(!renderer.set_font_resources_with_context(vec![invalid], &mut context));
    assert_eq!(renderer.persistent_layout_digest(), digest);
    assert_eq!(context.stats().environment_builds, stats.environment_builds);
    let after = compose(&mut renderer, &mut context, "A stable paragraph.");
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[test]
fn scoped_backend_returns_on_unwinding() {
    let mut context = FontContext::default();
    let mut renderer = Renderer::new(Settings::default());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        renderer.with_font_context(&mut context, |renderer| {
            renderer.compose_document(&document("Warm before panic."), 240.0);
            panic!("synthetic operation failure");
        });
    }));
    assert!(panic.is_err());
    assert_eq!(context.stats().environments, 1);
    assert!(renderer.fonts.backend.borrow().is_none());
    compose(&mut renderer, &mut context, "Warm before panic.");
    assert_eq!(context.stats().environment_builds, 1);
    assert!(context.stats().fonts.metric_hits > 0);
}

#[test]
fn configuration_changes_inside_a_scope_do_not_corrupt_other_environments() {
    let mut context = FontContext::default();
    let mut renderer = Renderer::new(Settings::default());
    let first = compose(&mut renderer, &mut context, "Isolated.");
    renderer.with_font_context(&mut context, |renderer| {
        let mut settings = renderer.settings().clone();
        settings.body_fonts = vec!["Fira Code".into()];
        renderer.set_settings(settings);
        renderer.compose_document(&document("Changed."), 240.0);
    });
    let mut other = Renderer::new(Settings::default());
    let restored = compose(&mut other, &mut context, "Isolated.");
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(restored).unwrap()
    );
}

#[test]
fn caches_and_environment_count_are_bounded_and_trim_releases_them() {
    let options = FontContextOptions {
        max_environments: 2,
        cache_bytes: 4096,
        plans_per_face: 1,
        max_faces_per_environment: 1,
    };
    let mut context = FontContext::new(options);
    for index in 0..30 {
        let settings = Settings {
            rendering: RenderSettings {
                body_fonts: vec![format!("preference-{index}")],
                ..RenderSettings::default()
            },
            ..Settings::default()
        };
        let mut renderer = Renderer::new(settings);
        compose(
            &mut renderer,
            &mut context,
            &format!("Distinct text number {index}."),
        );
        assert!(context.stats().cache_bytes <= options.cache_bytes);
        assert!(context.stats().environments <= options.max_environments);
    }
    assert!(context.stats().environment_evictions > 0);
    context.trim();
    assert_eq!(context.stats().environments, 0);
    assert_eq!(context.stats().cache_bytes, 0);
}

#[test]
fn unseen_text_reuses_plans_and_matches_caching_disabled() {
    let mut hot = FontContext::default();
    let mut cold = FontContext::new(FontContextOptions {
        cache_bytes: 0,
        plans_per_face: 0,
        ..FontContextOptions::default()
    });
    let mut hot_renderer = Renderer::new(Settings::default());
    let mut cold_renderer = Renderer::new(Settings::default());
    for text in [
        "1234 5678 — !? …",
        "A first English paragraph.",
        "Entirely different vocabulary here.",
        "עברית العربية English",
        "Combining o\u{308} and emoji 😀.",
    ] {
        let cached = compose(&mut hot_renderer, &mut hot, text);
        let uncached = compose(&mut cold_renderer, &mut cold, text);
        assert_eq!(
            serde_json::to_value(cached).unwrap(),
            serde_json::to_value(uncached).unwrap()
        );
    }
    assert!(hot.stats().fonts.plan_hits > 0);
    assert_eq!(cold.stats().fonts.plan_hits, 0);
    assert_eq!(cold.stats().cache_bytes, 0);
}

#[test]
fn rasterization_is_renderer_local_even_when_the_environment_is_shared() {
    fn paint(renderer: &mut Renderer, context: &mut FontContext) -> PixelBuffer {
        renderer.with_font_context(context, |renderer| {
            renderer
                .render_page(
                    &document("Rasterization isolation."),
                    (320, 240),
                    0,
                    None,
                    1.0,
                    InteractionState::default(),
                )
                .unwrap()
                .surface
        })
    }
    let mut context = FontContext::default();
    let mut first = Renderer::new(Settings::default());
    let mut second = Renderer::new(Settings::default());
    let original = paint(&mut first, &mut context);
    let mut options = Settings::default().rasterization();
    options.regular_gamma = 0.4;
    options.emphasized_gamma = 0.5;
    second.set_rasterization(options);
    let changed = paint(&mut second, &mut context);
    let repeated = paint(&mut first, &mut context);
    assert_ne!(original.pixels(), changed.pixels());
    assert_eq!(original.pixels(), repeated.pixels());
    assert_eq!(context.stats().environment_builds, 1);
}

#[test]
fn trimming_context_releases_resource_backing_after_viewer_destruction() {
    let bytes: std::sync::Arc<[u8]> =
        std::sync::Arc::from(crate::test_fonts::fixture("charter.ttf"));
    let weak = std::sync::Arc::downgrade(&bytes);
    let resource = FontResource::from_bytes(bytes);
    let mut context = FontContext::default();
    {
        let mut renderer = Renderer::new(Settings::default());
        assert!(renderer.set_font_resources_with_context(vec![resource], &mut context));
        compose(&mut renderer, &mut context, "Retained backing.");
    }
    assert!(weak.upgrade().is_some());
    context.trim();
    assert!(weak.upgrade().is_none());
}

#[test]
fn host_can_resize_and_disable_caches_without_changing_layout_identity() {
    let mut context = FontContext::default();
    let mut renderer = Renderer::new(Settings::default());
    for family in ["Charter", "Fira Code", "Noto Sans"] {
        let mut settings = renderer.settings().clone();
        settings.body_fonts = vec![family.into()];
        renderer.set_settings(settings);
        compose(&mut renderer, &mut context, "Host policy.");
    }
    assert_eq!(context.stats().environments, 3);
    let digest = renderer.persistent_layout_digest();
    let built = context.stats().environment_builds;
    context.set_options(FontContextOptions {
        max_environments: 1,
        cache_bytes: 0,
        plans_per_face: 0,
        max_faces_per_environment: 1,
    });
    assert_eq!(context.stats().environments, 1);
    assert_eq!(context.stats().cache_bytes, 0);
    assert_eq!(context.stats().cached_plans, 0);
    assert!(context.stats().cached_faces <= 1);
    compose(&mut renderer, &mut context, "Host policy after resize.");
    assert_eq!(renderer.persistent_layout_digest(), digest);
    assert_eq!(context.stats().environment_builds, built);
    assert_eq!(context.stats().cache_bytes, 0);
    assert_eq!(context.stats().cached_plans, 0);

    context.set_options(FontContextOptions {
        max_environments: 0,
        ..context.options()
    });
    assert_eq!(context.stats().environments, 0);
    for _ in 0..2 {
        compose(&mut renderer, &mut context, "No retained environments.");
        assert_eq!(context.stats().environments, 0);
        assert_eq!(context.stats().cache_bytes, 0);
    }
    assert_eq!(context.stats().environment_builds, built + 2);
}

#[test]
fn contextual_types_keep_send_capability() {
    fn is_send<T: Send>() {}
    is_send::<FontContext>();
    is_send::<Renderer>();
    is_send::<DocumentViewer>();
}
