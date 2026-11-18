use super::*;
use flow_publication::{
    document::{model::flow::NodeKind, pixels::PixelBuffer, settings::Settings},
    reading_session::{ReadingSession, SessionEffect},
};

fn epub() -> Vec<u8> {
    use std::io::Write;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("mimetype", options).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    let body = format!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body><h1>First chapter</h1><p><a href="two.xhtml#target">Continue</a></p><img src="image.gif" alt="Figure"/>{}</body></html>"#,
        "<p>Synthetic paragraph with enough text to scroll.</p>".repeat(60)
    );
    for (name, data) in [
        (
            "META-INF/container.xml",
            r#"<container><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        ),
        (
            "OPS/book.opf",
            r#"<package version="2.0"><metadata/><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/><item id="css" href="style.css" media-type="text/css"/><item id="image" href="image.gif" media-type="image/gif"/><item id="font" href="font.ttf" media-type="font/ttf"/></manifest><spine><itemref idref="one"/><itemref idref="one"/><itemref idref="two"/></spine></package>"#,
        ),
        ("OPS/one.xhtml", body.as_str()),
        (
            "OPS/two.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><h1 id="target">Second chapter</h1><p>The end.</p></body></html>"#,
        ),
        ("OPS/style.css", "p { color: #213040; }"),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.start_file("OPS/image.gif", options).unwrap();
    zip.write_all(b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;").unwrap();
    zip.start_file("OPS/font.ttf", options).unwrap();
    zip.write_all(include_bytes!(
        "../../../../crates/flow_document/tests/fixtures/fonts/charter.ttf"
    ))
    .unwrap();
    zip.finish().unwrap().into_inner()
}

fn service(book: &Book, session: &mut ReadingSession) {
    service_with_paging(book, session, None);
}
fn service_with_paging(
    book: &Book,
    session: &mut ReadingSession,
    mut paging: Option<&mut crate::paging::Paging>,
) {
    for _ in 0..40 {
        let mut more = false;
        for effect in session.take_effects().unwrap() {
            match effect {
                SessionEffect::LoadDocument(request) => {
                    session
                        .provide_document(request.id, book.document(&request).unwrap())
                        .unwrap();
                    more = true;
                }
                SessionEffect::LoadResource(request) => {
                    session
                        .provide_resource(request.id, book.resource(&request).unwrap())
                        .unwrap();
                    more = true;
                }
                SessionEffect::ContinueWork => more = true,
                SessionEffect::NavigationFinished { id, state } => {
                    if let Some(paging) = &mut paging {
                        paging.finished(id, &state, session);
                    }
                }
                SessionEffect::ResolveLink { .. } | SessionEffect::ResolvePublication { .. } => {
                    panic!("unexpected resolution")
                }
                _ => (),
            }
        }
        if !more {
            return;
        }
    }
    panic!("unbounded work");
}
fn open(book: &Book) -> ReadingSession {
    let mut session = ReadingSession::new(Settings::default());
    session.set_background_request_limit(0);
    session.resize((400, 240));
    for font in crate::demo_fonts() {
        assert!(session.register_font_resource(font));
    }
    let handle = session.register_publication(book.model.clone()).unwrap();
    session
        .open_target(handle, book.model.start().unwrap())
        .unwrap();
    service(book, &mut session);
    session
}

#[test]
fn markdown_and_owned_xhtml_use_the_same_single_document_path() {
    for (name, bytes) in [
        ("hello.md", "# Hello\n\nA **single** document."),
        (
            "hello.xhtml",
            "<p xmlns=\"http://www.w3.org/1999/xhtml\">Hello</p>",
        ),
    ] {
        let book = Book::from_bytes(name, bytes.as_bytes().to_vec(), 1).unwrap();
        let session = open(&book);
        assert!(
            session
                .viewer()
                .active_document()
                .unwrap()
                .text()
                .as_str()
                .contains("Hello")
        );
        assert_eq!(session.reading_order_position(), Some((1, 1)));
    }
}

#[test]
fn epub_resources_links_back_scrolling_and_pixels_work_without_a_window() {
    let book = Book::from_bytes("demo.epub", epub(), 1).unwrap();
    let mut session = open(&book);
    assert_eq!(session.reading_order_position(), Some((1, 2)));
    let mut pixels = PixelBuffer::new(400, 240, [255; 3]).unwrap();
    session
        .render_into(&mut pixels.surface_mut(), 400, 240, 1.0)
        .unwrap();
    service(&book, &mut session);
    session
        .render_into(&mut pixels.surface_mut(), 400, 240, 1.0)
        .unwrap();
    assert!(
        pixels
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != [255; 3])
    );
    let before = pixels.pixels().to_vec();
    assert!(session.scroll_by(200.0));
    session
        .render_into(&mut pixels.surface_mut(), 400, 240, 1.0)
        .unwrap();
    assert_ne!(pixels.pixels(), before);
    let destination = {
        let document = session.viewer().active_document().unwrap();
        document
            .preorder(document.root())
            .find_map(|id| match &document.get_node(id).unwrap().kind {
                NodeKind::Link(link) => Some(link.destination.clone()),
                _ => None,
            })
            .unwrap()
    };
    session.follow_destination(destination).unwrap();
    service(&book, &mut session);
    assert_eq!(session.reading_order_position(), Some((2, 2)));
    session.go_back().unwrap();
    service(&book, &mut session);
    assert_eq!(session.reading_order_position(), Some((1, 2)));
}

#[test]
fn paginated_standalone_turns_wheel_and_publication_bounds() {
    use crate::{ReadingMode, paging::Paging};
    let book = Book::from_bytes(
        "pages.md",
        "Synthetic paragraph.\n\n".repeat(100).into_bytes(),
        1,
    )
    .unwrap();
    let mut session = open(&book);
    let mut paging = Paging::default();
    session.set_reading_mode(ReadingMode::Paginated);
    let (page, total) = session.prepare_pagination(400, 240).unwrap();
    assert_eq!(page, 1);
    assert!(total > 4);
    paging.turn(&mut session, -1).unwrap();
    assert_eq!(session.viewer().paginated_page_metrics(), Some((1, total)));
    paging.turn(&mut session, 1).unwrap();
    assert_eq!(session.viewer().paginated_page_metrics(), Some((2, total)));
    paging.wheel(&mut session, -39.0).unwrap();
    assert_eq!(session.viewer().paginated_page_metrics(), Some((2, total)));
    paging.wheel(&mut session, -41.0).unwrap();
    assert_eq!(session.viewer().paginated_page_metrics(), Some((1, total)));
    session.scroll_to(f32::MAX);
    paging.turn(&mut session, 1).unwrap();
    assert_eq!(
        session.viewer().paginated_page_metrics(),
        Some((total, total))
    );
}

#[test]
fn paginated_epub_crosses_documents_in_both_directions_and_keeps_end_intent() {
    use crate::{ReadingMode, paging::Paging};
    let book = Book::from_bytes("demo.epub", epub(), 1).unwrap();
    let mut session = open(&book);
    let mut paging = Paging::default();
    session.set_reading_mode(ReadingMode::Paginated);
    let (_, total) = session.prepare_pagination(400, 240).unwrap();
    assert!(total > 4);
    session.scroll_to(f32::MAX);
    paging.turn(&mut session, 1).unwrap();
    // A repeated key during materialization must not supersede its own load.
    paging.turn(&mut session, 1).unwrap();
    service_with_paging(&book, &mut session, Some(&mut paging));
    paging.before_paint(&mut session);
    assert_eq!(session.reading_order_position(), Some((2, 2)));
    assert_eq!(session.prepare_pagination(400, 240), Some((1, 1)));
    paging.turn(&mut session, 1).unwrap();
    assert_eq!(session.reading_order_position(), Some((2, 2)));
    paging.turn(&mut session, -1).unwrap();
    service_with_paging(&book, &mut session, Some(&mut paging));
    paging.before_paint(&mut session);
    assert_eq!(session.reading_order_position(), Some((1, 2)));
    assert_eq!(
        session.viewer().paginated_page_metrics(),
        Some((total, total))
    );
    // End intent follows changing page geometry until another explicit turn.
    session.resize((400, 140));
    paging.before_paint(&mut session);
    let (page, smaller_total) = session.viewer().paginated_page_metrics().unwrap();
    assert!(smaller_total > total);
    assert_eq!(page, smaller_total);
    paging.turn(&mut session, -1).unwrap();
    paging.before_paint(&mut session);
    assert_eq!(
        session.viewer().paginated_page_metrics(),
        Some((smaller_total - 1, smaller_total))
    );
    // No paint between a cached backward chapter load and the next key press.
    paging.reset();
    session.move_reading_order(1).unwrap();
    service_with_paging(&book, &mut session, Some(&mut paging));
    paging.turn(&mut session, -1).unwrap();
    service_with_paging(&book, &mut session, Some(&mut paging));
    paging.turn(&mut session, -1).unwrap();
    assert_eq!(session.reading_order_position(), Some((1, 2)));
    assert_eq!(
        session.viewer().paginated_page_metrics(),
        Some((smaller_total - 1, smaller_total))
    );
}

#[test]
fn bad_inputs_and_external_protocols_fail_closed() {
    assert!(Book::from_bytes("unknown.bin", vec![0], 1).is_err());
    assert!(Book::from_bytes("bad.md", vec![255], 1).is_err());
    assert!(Book::from_bytes("bad.epub", vec![0], 1).is_err());
    for url in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/plain,hi",
        "ftp://example.test/file",
    ] {
        assert!(external_url(url).is_err());
    }
    assert!(external_url("https://example.test/path#fragment").is_ok());
    assert!(decode_data_image("data:text/plain;base64,SGk=").is_err());
    assert_eq!(
        decode_data_image("data:image/png;base64,SGk=").unwrap(),
        b"Hi"
    );
}

#[test]
fn default_pixel_packing_is_explicit_and_discards_alpha() {
    for format in [crate::PixelFormat::Bgra8888, crate::PixelFormat::Rgba8888] {
        let mut pixels = PixelBuffer::new_with_format(1, 1, [0; 3], format).unwrap();
        let bytes = match format {
            crate::PixelFormat::Bgra8888 => [30, 20, 10, 123],
            crate::PixelFormat::Rgba8888 => [10, 20, 30, 123],
        };
        pixels.pixels_mut().copy_from_slice(&bytes);
        let mut packed = [0];
        crate::pack_pixels(&pixels, &mut packed, 1);
        assert_eq!(packed, [0x000a141e]);
    }
}

#[test]
fn odd_hidpi_sizes_use_padded_rasters_and_crop_rows_correctly() {
    for scale in [1.0, 1.25, 1.5, 2.0, 2.5] {
        let physical = winit::dpi::PhysicalSize::new(701, 559);
        let (width, height) = crate::logical_extent(physical, scale);
        let (rw, rh) = flow_publication::document::render::render_surface_size(
            width as f32,
            height as f32,
            scale as f32,
        )
        .unwrap();
        assert!(rw >= physical.width && rh >= physical.height);
    }
    let mut pixels = PixelBuffer::new(3, 3, [0; 3]).unwrap();
    for (index, pixel) in pixels.pixels_mut().chunks_exact_mut(4).enumerate() {
        pixel[2] = index as u8;
    }
    let mut output = [0; 4];
    crate::pack_pixels(&pixels, &mut output, 2);
    assert_eq!(output, [0, 1 << 16, 3 << 16, 4 << 16]);
}

#[test]
fn adjacent_resources_cannot_escape_the_selected_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("book");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let document = root.join("one.md");
    std::fs::write(&document, "# Hello").unwrap();
    std::fs::write(root.join("image.png"), b"image").unwrap();
    std::fs::write(directory.path().join("secret"), b"secret").unwrap();
    let sibling = directory.path().join("book-other");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("image.png"), b"sibling").unwrap();
    let document = document.canonicalize().unwrap();
    let image = root.join("image.png").canonicalize().unwrap();
    for reference in [
        "image.png",
        "./image.png",
        "child/../image.png",
        "image.png#fragment",
    ] {
        assert_eq!(adjacent_path(&document, reference).unwrap(), image);
    }
    let absolute_image = Url::from_file_path(&image).unwrap();
    assert_eq!(
        adjacent_path(&document, absolute_image.as_str()).unwrap(),
        image
    );
    for reference in [
        "../secret",
        "%2e%2e/secret",
        "../book-other/image.png",
        "https://example.test/image.png",
        "//example.test/image.png",
    ] {
        assert!(adjacent_path(&document, reference).is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(directory.path().join("secret"), root.join("escape")).unwrap();
        assert!(adjacent_path(&document, "escape").is_err());
    }
    assert!(read_bounded(&document, 1).is_err());
}
