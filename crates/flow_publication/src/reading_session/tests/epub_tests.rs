use super::*;
use crate::formats::epub::EpubPublication;
use flow_document::model::flow::{NodeKind, SourceRetention};
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn epub_fixture() -> Vec<u8> {
    epub_fixture_with_spine(r#"<itemref idref="one"/><itemref idref="two"/>"#)
}

fn epub_fixture_with_spine(spine: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file("mimetype", stored).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();
    let package = format!(
        r#"<package version="2.0"><metadata/><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx">{spine}</spine></package>"#
    );
    for (name, source) in [
        (
            "META-INF/container.xml",
            r#"<container><rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        ),
        ("OPS/package.opf", package.as_str()),
        (
            "OPS/one.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>One</title></head><body><p><a href="two.xhtml#target">Two</a></p></body></html>"#,
        ),
        (
            "OPS/two.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Two</title></head><body><p id="target">Target</p></body></html>"#,
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx><navMap><navPoint id="one" playOrder="1"><navLabel><text>One</text></navLabel><content src="one.xhtml"/><navPoint id="two" playOrder="2"><navLabel><text>Two</text></navLabel><content src="two.xhtml#target"/></navPoint></navPoint></navMap></ncx>"#,
        ),
    ] {
        writer.start_file(name, deflated).unwrap();
        writer.write_all(source.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn serve_epub(session: &mut ReadingSession, source: &EpubPublication) {
    for _ in 0..50 {
        let mut more = false;
        for effect in session.take_effects().unwrap() {
            match effect {
                SessionEffect::LoadDocument(request) => {
                    session
                        .provide_document(
                            request.id,
                            source
                                .import_document(&request.document, SourceRetention::Discard)
                                .unwrap(),
                        )
                        .unwrap();
                    more = true;
                }
                SessionEffect::ContinueWork => more = true,
                _ => (),
            }
        }
        if !more {
            return;
        }
    }
    panic!("unbounded host work");
}

#[test]
fn adapter_uses_the_same_navigation_history_outline_and_pagination_contract() {
    let source = EpubPublication::from_bytes(epub_fixture()).unwrap();
    let model = source
        .publication(PublicationKey::new("host book identity").unwrap())
        .unwrap();
    assert_eq!(
        model
            .reading_order()
            .iter()
            .map(DocumentKey::as_str)
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        model.navigation()[0].children[0]
            .target
            .anchor
            .as_ref()
            .unwrap()
            .as_str(),
        "target"
    );
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let handle = session.register_publication(model).unwrap();
    session.open_target(handle, target("one")).unwrap();
    serve_epub(&mut session, &source);
    let destination = {
        let document = session.viewer.active_document().unwrap();
        document
            .preorder(document.root())
            .find_map(|id| match &document.get_node(id).unwrap().kind {
                NodeKind::Link(link) => Some(link.destination.clone()),
                _ => None,
            })
            .unwrap()
    };
    session.follow_destination(destination).unwrap();
    serve_epub(&mut session, &source);
    assert_eq!(session.current_target().unwrap().document.as_str(), "two");
    session.go_back().unwrap();
    serve_epub(&mut session, &source);
    assert_eq!(session.current_target().unwrap().document.as_str(), "one");
    session.go_forward().unwrap();
    serve_epub(&mut session, &source);
    assert_eq!(session.current_target().unwrap().document.as_str(), "two");
    session.set_reading_mode(ReadingMode::Paginated);
    session.prepare_publication_pagination().unwrap();
    serve_epub(&mut session, &source);
    assert!(session.publication_pagination_complete());
}

#[test]
fn duplicate_spine_items_do_not_become_positions_or_extra_pages() {
    let source = EpubPublication::from_bytes(epub_fixture_with_spine(r#"<itemref idref="one"/><itemref idref="one"/><itemref idref="two"/><itemref idref="one"/>"#)).unwrap();
    let model = source
        .publication(PublicationKey::new("duplicates").unwrap())
        .unwrap();
    assert_eq!(model.reading_order().len(), 2);
    let mut session = ReadingSession::new(Settings::default());
    session.resize((320, 480));
    let handle = session.register_publication(model).unwrap();
    session.open_target(handle, target("one")).unwrap();
    serve_epub(&mut session, &source);
    session.move_reading_order(1).unwrap();
    serve_epub(&mut session, &source);
    assert_eq!(session.current_target().unwrap().document.as_str(), "two");
    session.move_reading_order(-1).unwrap();
    serve_epub(&mut session, &source);
    assert_eq!(session.current_target().unwrap().document.as_str(), "one");
    assert!(session.history_is_empty());
}

#[test]
fn image_decoding_is_host_side_and_accepts_gif() {
    let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
    let decoded = image::load_from_memory(gif).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1, 1));
}

#[cfg(feature = "telemetry")]
#[test]
#[allow(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "Test host isolates tracing's process-wide callsite cache"
)]
fn host_subscriber_observes_import_phases_without_timing_results() {
    // Tracing caches callsite interest globally. Other tests import EPUBs
    // without a subscriber, so exercise this host in a fresh test process.
    const CHILD: &str = "FLOW_IMPORT_TELEMETRY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reading_session::tests::epub_tests::host_subscriber_observes_import_phases_without_timing_results",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "isolated telemetry host failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    use flow_document::__tracing as tracing;
    use std::sync::Mutex;
    use tracing::{
        Event, Metadata, Subscriber,
        span::{Attributes, Id, Record},
    };

    struct Host(Arc<Mutex<Vec<&'static str>>>);
    impl Subscriber for Host {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, attrs: &Attributes<'_>) -> Id {
            let mut spans = self.0.lock().unwrap();
            spans.push(attrs.metadata().name());
            Id::from_u64(spans.len() as u64)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, _: &Event<'_>) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    let spans = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::with_default(Host(spans.clone()), || {
        let settings = Settings {
            reading_mode: ReadingMode::Paginated,
            ..Default::default()
        };
        let mut session = ReadingSession::new(settings);
        session.set_cache_limits(1, 0);
        let source = EpubPublication::from_bytes(epub_fixture()).unwrap();
        let model = source
            .publication(PublicationKey::new("instrumented book").unwrap())
            .unwrap();
        let id = session.register_publication(model).unwrap();
        session.resize((320, 480));
        session.open_target(id, target("one")).unwrap();
        serve_epub(&mut session, &source);
        assert!(session.viewer.has_document());
        assert!(session.prepare_pagination(320, 480).is_some());
        session.move_reading_order(1).unwrap();
        serve_epub(&mut session, &source);
        assert!(session.prepare_pagination(320, 480).is_some());
        session.open_target(id, target("one")).unwrap();
        serve_epub(&mut session, &source);
        let before_return = spans.lock().unwrap().len();
        session.go_back().unwrap().unwrap();
        serve_epub(&mut session, &source);
        assert!(session.prepare_pagination(320, 480).is_some());
        let recorded = spans.lock().unwrap();
        assert!(
            !recorded[before_return..].contains(&"flow.layout.compose"),
            "restoring a cached document recomposed its layout"
        );
        assert!(
            !recorded[before_return..].contains(&"flow.layout.paginate"),
            "restoring a cached document repaginated its layout"
        );
    });
    let spans = spans.lock().unwrap();
    for phase in [
        "flow.publication.document.import",
        "flow.publication.item",
        "flow.epub.import",
        "flow.xhtml.parse",
        "flow.css.style",
        "flow.flow.lower",
        "flow.viewer.layout",
        "flow.layout.compose",
        "flow.layout.paginate",
    ] {
        assert!(
            spans.contains(&phase),
            "missing host-observable phase {phase}; observed {spans:?}"
        );
    }
}
