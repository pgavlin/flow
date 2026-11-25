//! Navigation lifecycle regressions: semantic construction is independent of
//! viewport/composition changes and restored with the document session.
use super::*;
use crate::{
    formats::markdown,
    model::flow::{
        AnchorId, Destination, FlowBuilder, Heading, HeadingLevel, KnownRole, Link, LinkContent,
        LinkRelation, NodeKind, SemanticRole, Semantics,
    },
    navigation::NavigationTarget,
    pixels::PixelBuffer,
};

fn parse(source: &str) -> FlowDocument {
    markdown::parse(source, Default::default())
        .unwrap()
        .document
}

fn reader(document: FlowDocument) -> DocumentViewer {
    let mut viewer = DocumentViewer::new("navigation", Settings::default());
    assert!(viewer.register_font(Arc::from(crate::test_fonts::fixture("charter.ttf"))));
    viewer.replace_document(Ok(document));
    viewer
}

fn paint(viewer: &mut DocumentViewer, width: u32, height: u32) {
    let mut pixels = PixelBuffer::new(width, height, [255; 3]).unwrap();
    viewer
        .render_into(&mut pixels.surface_mut(), width, height, 1.0)
        .unwrap();
}

fn semantic_storage(viewer: &DocumentViewer) -> Vec<(NavigationIdentity, usize, usize, usize)> {
    viewer
        .navigation
        .targets()
        .iter()
        .filter(|target| target.generated_action().is_none())
        .map(|target| {
            let flow = target.flow.as_ref().unwrap();
            (
                target.identity(),
                target.label.as_ptr() as usize,
                flow.anchors.as_ptr() as usize,
                flow.owned_nodes.as_ptr() as usize,
            )
        })
        .collect()
}

fn matches_fresh(viewer: &DocumentViewer) {
    let fresh = NavigationIndex::new(viewer.document.as_ref().unwrap(), &viewer.layout);
    assert_eq!(viewer.navigation.targets(), fresh.targets());
    for (actual, expected) in viewer.navigation.targets().iter().zip(fresh.targets()) {
        assert_eq!(actual.document_y, expected.document_y);
        let identity = actual.identity();
        for direction in [-1, 1] {
            assert_eq!(
                viewer
                    .navigation
                    .adjacent(Some(&identity), direction, true)
                    .map(NavigationTarget::identity),
                fresh
                    .adjacent(Some(&identity), direction, true)
                    .map(NavigationTarget::identity)
            );
        }
    }
}

#[test]
fn first_open_and_reflow_reuse_semantic_navigation_and_its_allocations() {
    let document = parse(&format!(
        "# First\n\n[linked words](#first) {}\n\n## Last\n\nTail.",
        "words that wrap ".repeat(30)
    ));
    let mut viewer = reader(document);
    let (build, bindings) = viewer.navigation.construction_work();
    assert_ne!(build, 0);
    assert_eq!(bindings, 0);
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .all(|target| target.document_y.is_none())
    );
    assert_eq!(viewer.outline().nodes().len(), 2);
    let storage = semantic_storage(&viewer);

    paint(&mut viewer, 320, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 1));
    assert_eq!(semantic_storage(&viewer), storage);
    matches_fresh(&viewer);
    paint(&mut viewer, 320, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 1));

    paint(&mut viewer, 500, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 2));
    assert_eq!(semantic_storage(&viewer), storage);
    matches_fresh(&viewer);

    viewer.set_font_size(viewer.font_size_points + 2.0);
    paint(&mut viewer, 500, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 3));
    assert_eq!(semantic_storage(&viewer), storage);
    matches_fresh(&viewer);

    viewer.set_dark_mode(true);
    paint(&mut viewer, 500, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 3));
}

fn note_before_heading() -> FlowDocument {
    note_document(false)
}

fn note_document(prefix: bool) -> FlowDocument {
    let mut builder = FlowBuilder::new();
    let root = builder.add_node(NodeKind::Document).unwrap();
    if prefix {
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("Prefix").unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(root, paragraph).unwrap();
    }
    let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
    let reference = builder
        .add_node(NodeKind::Link(Link {
            content: LinkContent::Phrasing,
            destination: Destination::Local(AnchorId::new("note")),
            relation: LinkRelation::NoteReference {
                return_anchor: Some(AnchorId::new("reference")),
            },
            title: None,
        }))
        .unwrap();
    let label = builder.add_text("[1]").unwrap();
    builder.append(root, paragraph).unwrap();
    builder.append(paragraph, reference).unwrap();
    builder.append(reference, label).unwrap();
    builder
        .add_anchor(AnchorId::new("reference"), reference)
        .unwrap();
    let note = builder.add_node(NodeKind::Paragraph).unwrap();
    builder
        .set_semantics(
            note,
            Semantics {
                roles: vec![SemanticRole::Known(KnownRole::Footnote)],
                ..Default::default()
            },
        )
        .unwrap();
    let text = builder
        .add_text("A note definition with a generated return action.")
        .unwrap();
    builder.append(note, text).unwrap();
    builder.append(root, note).unwrap();
    builder.add_anchor(AnchorId::new("note"), note).unwrap();
    builder
        .add_anchor(AnchorId::new("note-alias"), note)
        .unwrap();
    let heading = builder
        .add_node(NodeKind::Heading(Heading {
            level: HeadingLevel::H2,
        }))
        .unwrap();
    let text = builder.add_text("After the note").unwrap();
    builder.append(heading, text).unwrap();
    builder.append(root, heading).unwrap();
    builder.add_anchor(AnchorId::new("after"), heading).unwrap();
    builder.finish().unwrap()
}

#[test]
fn composition_changes_clear_only_geometry_and_refresh_generated_actions() {
    let mut viewer = reader(note_before_heading());
    let storage = semantic_storage(&viewer);
    let build = viewer.navigation.construction_work().0;
    paint(&mut viewer, 500, 240);
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .any(|target| target.generated_action().is_some())
    );
    let generated = viewer
        .navigation
        .targets()
        .iter()
        .find(|target| target.generated_action().is_some())
        .unwrap()
        .identity();
    viewer.focused = Some(generated.clone());
    matches_fresh(&viewer);

    viewer.set_reading_mode(ReadingMode::Paginated);
    assert!(viewer.prepare_pagination(220, 200).is_some());
    assert_eq!(viewer.navigation.construction_work(), (build, 1));
    assert_eq!(semantic_storage(&viewer), storage);
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .all(|target| target.document_y.is_none())
    );
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .all(|target| target.generated_action().is_none())
    );
    assert!(viewer.navigation.adjacent(None, 1, true).is_some());

    paint(&mut viewer, 220, 200);
    assert_eq!(viewer.navigation.construction_work(), (build, 2));
    assert_eq!(semantic_storage(&viewer), storage);
    assert!(viewer.navigation.target(&generated).is_some());
    assert_eq!(viewer.focused, Some(generated));
    matches_fresh(&viewer);
}

#[test]
fn pagination_only_work_and_mode_or_height_changes_do_not_rebuild_navigation() {
    let mut viewer = reader(parse("# Heading\n\nText [link](#heading)."));
    viewer.set_reading_mode(ReadingMode::Paginated);
    let build = viewer.navigation.construction_work().0;
    assert!(viewer.prepare_pagination(320, 240).is_some());
    assert!(viewer.prepare_pagination(320, 300).is_some());
    assert_eq!(viewer.navigation.construction_work(), (build, 0));
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .all(|target| target.document_y.is_none())
    );
    paint(&mut viewer, 320, 300);
    assert_eq!(viewer.navigation.construction_work(), (build, 1));
    viewer.set_reading_mode(ReadingMode::Continuous);
    paint(&mut viewer, 320, 300);
    viewer.set_reading_mode(ReadingMode::Paginated);
    paint(&mut viewer, 320, 180);
    assert_eq!(viewer.navigation.construction_work(), (build, 1));
    matches_fresh(&viewer);
}

#[test]
fn session_restoration_preserves_semantics_and_rebinds_only_if_placement_changes() {
    let mut viewer = reader(note_before_heading());
    paint(&mut viewer, 320, 240);
    let storage = semantic_storage(&viewer);
    let work = viewer.navigation.construction_work();
    let focus = viewer.navigation.targets()[0].identity();
    viewer.focused = Some(focus.clone());
    let selection = Selection {
        anchor: TextPosition::new(0),
        active: TextPosition::new(3),
    };
    viewer.selection = Some(selection);
    let suspended = viewer.suspend_with_document("temporary", parse("# Temporary\n\nOther text."));
    assert_ne!(viewer.navigation.construction_work().0, work.0);
    paint(&mut viewer, 320, 240);
    let _temporary = viewer.activate_session(suspended);
    assert_eq!(viewer.navigation.construction_work(), work);
    assert_eq!(viewer.focused, Some(focus));
    assert_eq!(viewer.selection, Some(selection));
    assert_eq!(semantic_storage(&viewer), storage);
    paint(&mut viewer, 320, 240);
    assert_eq!(viewer.navigation.construction_work(), work);
    paint(&mut viewer, 500, 240);
    assert_eq!(viewer.navigation.construction_work(), (work.0, work.1 + 1));
    assert_eq!(semantic_storage(&viewer), storage);
    matches_fresh(&viewer);
}

#[test]
fn generated_focus_recovery_waits_for_replacement_geometry() {
    let mut viewer = reader(note_before_heading());
    paint(&mut viewer, 400, 240);
    let focus = viewer
        .navigation
        .targets()
        .iter()
        .find(|target| target.generated_action().is_some())
        .unwrap()
        .identity();
    viewer.focused = Some(focus.clone());
    viewer.replace_document(Ok(note_document(true)));
    assert_eq!(viewer.focused, Some(focus.clone()));
    assert!(viewer.navigation.target(&focus).is_none());
    paint(&mut viewer, 400, 240);
    assert!(
        viewer
            .navigation
            .target(viewer.focused.as_ref().unwrap())
            .is_some()
    );

    viewer.replace_document(Ok(parse("# No note")));
    paint(&mut viewer, 400, 240);
    assert!(viewer.focused.is_none());
}

#[test]
fn image_geometry_changes_rebind_without_rebuilding_semantics() {
    let mut viewer = reader(parse(
        "# Heading\n\n![Image](image.png)\n\n[link](#heading)",
    ));
    paint(&mut viewer, 400, 240);
    let (build, _) = viewer.navigation.construction_work();
    let storage = semantic_storage(&viewer);
    assert!(viewer.provide_image_metadata(ImageRequest::new("image.png", None), 40, 20));
    paint(&mut viewer, 400, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 2));
    assert_eq!(semantic_storage(&viewer), storage);
    matches_fresh(&viewer);
    let mut request = ImageRequest::new("image.png", None);
    request.desired_width = Some(40);
    request.desired_height = Some(20);
    assert!(viewer.provide_image_pixels(
        request,
        ImagePixels::from_rgba(40, 20, vec![255; 40 * 20 * 4]).unwrap()
    ));
    paint(&mut viewer, 400, 240);
    assert_eq!(viewer.navigation.construction_work(), (build, 2));
    assert_eq!(semantic_storage(&viewer), storage);
}

#[test]
fn replacement_builds_once_and_never_binds_to_the_previous_documents_layout() {
    let mut viewer = reader(note_before_heading());
    paint(&mut viewer, 320, 240);
    let previous = viewer.navigation.construction_work().0;
    viewer.replace_document(Ok(parse("# Replacement\n\nNew content.")));
    let (replacement, bindings) = viewer.navigation.construction_work();
    assert_ne!(replacement, previous);
    assert_eq!(bindings, 0);
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .all(|target| target.document_y.is_none())
    );
    assert!(
        viewer
            .navigation
            .targets()
            .iter()
            .all(|target| target.generated_action().is_none())
    );
    paint(&mut viewer, 320, 240);
    assert_eq!(viewer.navigation.construction_work(), (replacement, 1));
    matches_fresh(&viewer);
    viewer.replace_document(Err("unavailable".into()));
    assert!(viewer.navigation.targets().is_empty());
    assert_eq!(viewer.navigation.construction_work(), (0, 0));
}
