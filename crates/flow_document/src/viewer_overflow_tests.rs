use super::*;
use crate::{formats::markdown, pixels::PixelBuffer};

const CODE: &str = include_str!("../tests/fixtures/horizontal-overflow/wide-code.md");
const TABLE: &str = include_str!("../tests/fixtures/horizontal-overflow/wide-table.md");

fn paint(viewer: &mut DocumentViewer, width: u32, height: u32) -> PixelBuffer {
    let mut pixels = PixelBuffer::new(width, height, [0; 3]).unwrap();
    viewer
        .render_into(&mut pixels.surface_mut(), width, height, 1.0)
        .unwrap();
    pixels
}

fn viewer(source: &str, mode: ReadingMode) -> DocumentViewer {
    let mut viewer = DocumentViewer::new(
        "overflow.md",
        Settings {
            reading_mode: mode,
            ..Settings::default()
        },
    );
    viewer.register_font_resource(crate::test_fonts::charter_regular());
    viewer.replace_document(Ok(markdown::parse(source, Default::default())
        .unwrap()
        .document));
    paint(&mut viewer, 360, 240);
    viewer
}

#[test]
fn wide_fixtures_preserve_intrinsics_and_inline_code_stays_ordinary() {
    for source in [CODE, TABLE] {
        for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
            let viewer = viewer(source, mode);
            let placements = viewer.visible_horizontal_overflows();
            assert_eq!(placements.len(), 1);
            assert!(placements[0].content_width > placements[0].viewport.width);
            let block = &viewer.composed.blocks[0].layout;
            if let Some(table) = &block.table {
                assert!(!table.sizing.fallback_active);
                assert!(table.width >= table.sizing.grid_min - 0.01);
            } else {
                assert_eq!(block.lines.len(), 36, "authored code lines must not wrap");
            }
            assert_eq!(viewer.layout.horizontal_overflows().len(), 1);
        }
    }
}

#[test]
fn scrolling_only_repaints_and_hit_testing_inverts_the_offset() {
    for source in [CODE, TABLE] {
        for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
            let mut viewer = viewer(source, mode);
            let placement = viewer.visible_horizontal_overflows()[0];
            let (x, y) = (placement.viewport.x + 10.0, placement.viewport.y + 8.0);
            let composition = Arc::clone(&viewer.composed);
            let pages = Arc::clone(&viewer.pages);
            let count = viewer.pages.pages.len();
            viewer.pointer_moved(Some((x, y)), Duration::ZERO);
            let before = paint(&mut viewer, 360, 240);
            assert!(viewer.wheel_horizontal(80.0));
            assert_eq!(viewer.horizontal_offset(placement), 80.0);
            assert!(viewer.overflow.focused.is_none());
            let scrolled = viewer.text_position_at_pointer(x, y);
            viewer.overflow.offsets.clear();
            assert_eq!(scrolled, viewer.text_position_at_pointer(x + 80.0, y));
            viewer.overflow.scroll(placement, 80.0);
            let after = paint(&mut viewer, 360, 240);
            assert_ne!(before.pixels(), after.pixels());
            assert!(Arc::ptr_eq(&composition, &viewer.composed));
            assert!(Arc::ptr_eq(&pages, &viewer.pages));
            assert_eq!(count, viewer.pages.pages.len());
        }
    }
}

#[test]
fn fragments_share_offsets_but_keyboard_focus_is_fragment_local() {
    for source in [CODE, TABLE] {
        let mut viewer = viewer(source, ReadingMode::Paginated);
        let first = viewer.visible_horizontal_overflows()[0];
        assert!(viewer.focus_adjacent(1, false));
        assert_eq!(viewer.overflow.focused, Some(first.owner));
        assert!(viewer.scroll_focused_horizontal(90.0));
        assert!(viewer.scroll_by(1.0));
        paint(&mut viewer, 360, 240);
        let second = viewer.visible_horizontal_overflows()[0];
        if source == TABLE {
            let page = &viewer.pages.pages[viewer.current_page];
            assert!(
                viewer.pages.fragments[page.fragments.as_range()]
                    .iter()
                    .any(|f| {
                        matches!(f, crate::pagination::PageFragment::Lines(f) if f.replica)
                    }),
                "fixture must exercise repeated table headers"
            );
        }
        assert_eq!(first.owner, second.owner);
        assert_eq!(viewer.horizontal_offset(second), 90.0);
        assert!(!viewer.scroll_focused_horizontal(10.0));
        let page = viewer.current_page;
        assert!(viewer.focus_adjacent(1, false));
        assert_eq!(
            viewer.current_page, page,
            "Tab must focus this fragment, not reveal the first page"
        );
        assert!(viewer.scroll_focused_horizontal(f32::MAX));
        assert!(viewer.scroll_focused_horizontal(40.0));
        assert_eq!(viewer.current_page, page);
        viewer.clear_focus();
        assert!(!viewer.scroll_focused_horizontal(-40.0));
    }
}

#[test]
fn pan_locks_axis_and_never_hands_off_at_limits() {
    for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
        let mut viewer = viewer(CODE, mode);
        let p = viewer.visible_horizontal_overflows()[0];
        let (x, y) = (p.viewport.x + 20.0, p.viewport.y + 10.0);
        viewer.begin_pan(x, y);
        assert!(!viewer.pan_to(x - 2.0, y));
        assert!(viewer.pan_to(x - 80.0, y + 1.0));
        assert_eq!(viewer.horizontal_offset(p), 80.0);
        assert!(
            viewer.pan_to(x - 90.0, y + 200.0),
            "axis stays locked outside the viewport"
        );
        viewer.end_pan();
        viewer.overflow.scroll(p, f32::MAX);
        viewer.begin_pan(x, y);
        assert!(viewer.pan_to(x - 50.0, y));
        viewer.end_pan();
        viewer.begin_pan(x, y);
        assert!(!viewer.pan_to(x + 1.0, y + 20.0));
        assert!(
            !viewer.pan_to(x - 100.0, y + 25.0),
            "vertical lock must not switch axes"
        );
        viewer.end_pan();
        viewer.begin_pan(0.0, 0.0);
        assert!(
            !viewer.pan_to(x, y),
            "gestures starting outside cannot capture overflow"
        );
        assert!(viewer.overflow.focused.is_none());
    }
}

#[test]
fn offsets_clamp_on_resize_survive_suspension_but_not_document_replacement() {
    let mut viewer = viewer(CODE, ReadingMode::Continuous);
    let p = viewer.visible_horizontal_overflows()[0];
    viewer.overflow.scroll(p, 90.0);
    let session = viewer.suspend_with_document(
        "temporary",
        markdown::parse("temporary", Default::default())
            .unwrap()
            .document,
    );
    viewer.activate_session(session);
    paint(&mut viewer, 360, 240);
    let p = viewer.visible_horizontal_overflows()[0];
    assert_eq!(viewer.horizontal_offset(p), 90.0);
    viewer.set_max_width(4000.0);
    paint(&mut viewer, 4000, 240);
    assert!(viewer.overflow.offsets.is_empty());
    paint(&mut viewer, 360, 240);
    let p = viewer.visible_horizontal_overflows()[0];
    assert_eq!(viewer.horizontal_offset(p), 0.0);
    viewer.overflow.scroll(p, 90.0);
    viewer.replace_document(Ok(markdown::parse(CODE, Default::default())
        .unwrap()
        .document));
    paint(&mut viewer, 360, 240);
    assert!(viewer.overflow.offsets.is_empty());
}

#[test]
fn shared_paint_targets_clip_search_and_selection_to_the_viewport() {
    use crate::pixels::test_support::RecordingPaintTarget;
    for source in [CODE, TABLE] {
        for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
            let mut viewer = viewer(source, mode);
            let p = viewer.visible_horizontal_overflows()[0];
            viewer.set_search_query("record");
            viewer.selection = Some(Selection {
                anchor: TextPosition::new(0),
                active: TextPosition::new(
                    viewer.document.as_ref().unwrap().text().len().as_usize(),
                ),
            });
            let before = paint(&mut viewer, 360, 240);
            assert!(viewer.scroll_horizontal(p.owner, 100.0));
            let pixels = paint(&mut viewer, 360, 240);
            let mut recording = RecordingPaintTarget::new(360, 240).unwrap();
            viewer.render_into(&mut recording, 360, 240, 1.0).unwrap();
            assert_eq!(pixels.pixels(), recording.buffer.pixels());
            for y in 0..240usize {
                for x in 0..360usize {
                    if x < p.clip.x.floor().max(0.0) as usize
                        || x >= (p.clip.x + p.clip.width).ceil() as usize
                    {
                        let i = (y * 360 + x) * 4;
                        assert_eq!(
                            &pixels.pixels()[i..i + 4],
                            &before.pixels()[i..i + 4],
                            "scrolling must not paint outside the viewport"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn overflow_clipping_matches_host_targets_at_fractional_scale_after_navigation() {
    use crate::pixels::test_support::RecordingPaintTarget;
    for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
        let mut viewer = viewer(TABLE, mode);
        let owner = viewer.visible_horizontal_overflows()[0].owner;
        viewer.scroll_horizontal(owner, 123.5);
        viewer.scroll_by(if mode == ReadingMode::Continuous {
            70.5
        } else {
            1.0
        });
        let mut pixels = PixelBuffer::new(450, 300, [0; 3]).unwrap();
        viewer
            .render_into(&mut pixels.surface_mut(), 360, 240, 1.25)
            .unwrap();
        let mut recording = RecordingPaintTarget::new(450, 300).unwrap();
        viewer.render_into(&mut recording, 360, 240, 1.25).unwrap();
        assert_eq!(pixels.pixels(), recording.buffer.pixels());
    }
}

#[test]
fn focused_code_overflow_draws_one_viewport_outline_not_per_line_boxes() {
    use crate::pixels::test_support::{PaintCommand, RecordingPaintTarget};
    for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
        let mut viewer = viewer(CODE, mode);
        assert!(viewer.focus_adjacent(1, false));
        let mut recording = RecordingPaintTarget::new(360, 240).unwrap();
        viewer.render_into(&mut recording, 360, 240, 1.0).unwrap();
        assert!(viewer.overflow.focused.is_some());
        assert!(
            !recording
                .commands
                .iter()
                .any(|command| matches!(command, PaintCommand::Line(..))),
            "legacy semantic focus boxes must not be drawn around individual code lines"
        );
    }
}

#[test]
fn far_right_table_links_follow_displayed_geometry() {
    for mode in [ReadingMode::Continuous, ReadingMode::Paginated] {
        let mut viewer = viewer(TABLE, mode);
        let p = viewer.visible_horizontal_overflows()[0];
        viewer.scroll_horizontal(p.owner, f32::MAX);
        let (run_x, run_y) = viewer.layout.blocks[0]
            .lines
            .iter()
            .find_map(|line| {
                line.runs
                    .iter()
                    .find(|run| run.flow_link.is_some())
                    .map(|run| (run.x + run.width * 0.5, line.y + line.height * 0.5))
            })
            .unwrap();
        let x = viewer.layout_x + run_x - viewer.horizontal_offset(p);
        let margin = viewer.renderer.settings().theme.page_margin;
        let y = if mode == ReadingMode::Paginated {
            let lines = &viewer.composed.blocks[0].layout.lines;
            let index = lines
                .iter()
                .position(|line| line.runs.iter().any(|r| r.flow_link.is_some()))
                .unwrap();
            let fragment = viewer
                .pages
                .fragments
                .iter()
                .find_map(|f| match f {
                    crate::pagination::PageFragment::Lines(f)
                        if f.line_start <= index && index < f.line_end =>
                    {
                        Some(f)
                    }
                    _ => None,
                })
                .unwrap();
            margin + fragment.y + lines[index].y - lines[fragment.line_start].y
                + lines[index].height * 0.5
        } else {
            margin + run_y - viewer.scroll_y
        };
        assert!(p.viewport.contains(x, y));
        assert!(viewer.has_activation_at(x, y), "mode={mode:?}");
    }
}

#[test]
fn nested_code_and_tables_use_the_outer_table_owner() {
    let source = include_str!("../tests/fixtures/horizontal-overflow/nested-code.xhtml");
    let document = crate::formats::flow_xhtml::import(source, Default::default())
        .unwrap()
        .document;
    let mut viewer = DocumentViewer::new("nested.xhtml", Settings::default());
    viewer.replace_document(Ok(document));
    paint(&mut viewer, 360, 240);
    let placements = viewer.layout.horizontal_overflows();
    assert_eq!(placements.len(), 1);
    let outer_table = viewer
        .layout
        .blocks
        .iter()
        .find(|block| block.table.is_some())
        .unwrap();
    assert_eq!(Some(placements[0].owner), outer_table.node);
    assert!(viewer.scroll_horizontal(placements[0].owner, 50.0));
    assert_eq!(viewer.horizontal_offset(placements[0]), 50.0);
}

#[test]
fn static_renderer_keeps_explicit_noninteractive_table_policy() {
    let document = markdown::parse(TABLE, Default::default()).unwrap().document;
    let renderer = Renderer::new(Settings::default());
    let composition = renderer.compose_document(&document, 200.0);
    let table = composition.blocks[0].layout.table.as_ref().unwrap();
    assert_eq!(
        table.overflow_policy,
        crate::layout::TableOverflowPolicy::EmergencyFit
    );
    assert!(table.sizing.fallback_active);
}
