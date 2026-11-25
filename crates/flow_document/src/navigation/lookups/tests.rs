use super::*;
use crate::{
    layout::{
        BlockLayout, LayoutBlockKind, LayoutOptions, TextMeasurer, TextMetrics, TextStyle,
        layout_flow_document,
    },
    model::flow::{
        Affinity, Anchor, AnchorId, AnchorTarget, FlowBuilder, StyleContextKey, TextOffset,
    },
    navigation::{NavigationIndex, reference_flow_geometry, reference_flow_position_and_anchors},
};

struct FixedMeasurer;
impl TextMeasurer for FixedMeasurer {
    fn measure(&self, text: &str, style: &TextStyle) -> TextMetrics {
        TextMetrics {
            width: text.chars().count() as f32 * style.size * 0.5,
            ascent: style.size * 0.8,
            descent: style.size * 0.2,
        }
    }
}

fn position(offset: usize, affinity: Affinity) -> FlowTextPosition {
    FlowTextPosition::new(TextOffset::new(offset), affinity)
}

fn document_and_layout() -> (FlowDocument, ContinuousLayout) {
    let document = crate::formats::markdown::parse(
        "# Heading\n\n[link](#heading) words that wrap over many short lines.\n\n![image](image.png) trailing words\n\n---\n\n| A | B |\n|---|---|\n| cell one has many words | cell two has more words |\n\nref[^n]\n\n[^n]: note",
        Default::default(),
    ).unwrap().document;
    let styles = document.resolve_styles(StyleContextKey::default());
    let layout = layout_flow_document(
        &document,
        &styles,
        &FixedMeasurer,
        &LayoutOptions {
            width: 90.0,
            ..Default::default()
        },
    );
    (document, layout)
}

fn compare_geometry(document: &FlowDocument, layout: &ContinuousLayout, maximum: usize) {
    let lookup = GeometryLookup::new(layout);
    for node in document.preorder(document.root()) {
        for offset in (0..=maximum).chain([usize::MAX]) {
            for affinity in [Affinity::Upstream, Affinity::Downstream] {
                let position = position(offset, affinity);
                assert_eq!(
                    lookup.y(node, position),
                    reference_flow_geometry(layout, node, position),
                    "node={node:?}, position={position:?}"
                );
            }
        }
    }
}

#[test]
fn geometry_matches_reference_for_all_offsets_affinities_and_empty_layout() {
    let (document, layout) = document_and_layout();
    compare_geometry(&document, &layout, document.text().len().as_usize() + 2);
    compare_geometry(&document, &ContinuousLayout::default(), 4);
}

#[test]
fn geometry_preserves_visual_priority_for_interleaved_overlapping_and_empty_ranges() {
    let (document, original) = document_and_layout();
    let nodes: Vec<_> = document.preorder(document.root()).collect();
    let template = &original.blocks[0];
    let mut blocks = Vec::new();
    for (block_index, ranges) in [
        vec![(30, 45), (0, 20), (10, 40), (20, 20), (60, 70)],
        vec![(0, 80), (10, 20), (45, 55), (60, 60)],
        vec![(90, 90), (usize::MAX, usize::MAX)],
    ]
    .into_iter()
    .enumerate()
    {
        let mut block: BlockLayout = template.clone();
        block.kind = LayoutBlockKind::Table;
        block.node = Some(nodes[block_index + 1]);
        block.range = TextRange {
            start: block_index * 5,
            end: 100,
        };
        block.y = block_index as f32 * 100.0;
        block.height = 100.0;
        block.lines = ranges
            .into_iter()
            .enumerate()
            .map(|(index, (start, end))| LineLayout {
                range: TextRange { start, end },
                y: block.y + index as f32 * 10.0,
                height: 10.0,
                baseline: 8.0,
                runs: Vec::new(),
            })
            .collect();
        block.atomic_objects = vec![AtomicLayout {
            node: Some(nodes[4]),
            range: TextRange { start: 75, end: 80 },
            x: 0.0,
            y: block.y + 5.0,
            width: 10.0,
            height: 10.0,
            leading_x: 0.0,
            trailing_x: 10.0,
            bidi: Default::default(),
            flow_link: None,
            kind: crate::layout::AtomicLayoutKind::ThematicBreak,
        }];
        blocks.push(block);
    }
    // Duplicate node IDs must retain the first block/atomic, as the scan did.
    let mut duplicate = blocks[0].clone();
    duplicate.y = 500.0;
    blocks.push(duplicate);
    let layout = ContinuousLayout::new(100.0, 600.0, blocks);
    compare_geometry(&document, &layout, 110);
    let lookup = GeometryLookup::new(&layout);
    assert_eq!(
        lookup
            .line_ranges
            .containing(15)
            .map(|index| lookup.lines[index].y),
        Some(10.0)
    );
    assert_eq!(
        lookup
            .line_ranges
            .following(55)
            .map(|index| lookup.lines[index].y),
        Some(40.0)
    );
}

#[test]
fn randomized_interval_lookup_matches_first_visual_match_and_next_source_start() {
    let mut state = 17u64;
    for count in [1, 8, 64, 512] {
        let ranges: Vec<_> = (0..count)
            .map(|index| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let start = (state as usize) % 128;
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (start, start + (state as usize) % 64, index)
            })
            .collect();
        let lookup = RangeLookup::new(ranges.iter().copied());
        for offset in 0..200 {
            assert_eq!(
                lookup.containing(offset),
                ranges
                    .iter()
                    .find(|(start, end, _)| *start <= offset && offset <= *end)
                    .map(|(_, _, index)| *index)
            );
            assert_eq!(
                lookup.following(offset),
                ranges
                    .iter()
                    .filter(|(start, _, _)| *start > offset)
                    .min_by_key(|(start, _, _)| *start)
                    .map(|(_, _, index)| *index)
            );
        }
    }
    let lookup = RangeLookup::new([(usize::MAX, usize::MAX, 0)]);
    assert_eq!(lookup.containing(usize::MAX), Some(0));
    assert_eq!(lookup.containing(usize::MAX - 1), None);
    assert_eq!(lookup.following(usize::MAX - 1), Some(0));
    assert_eq!(lookup.following(usize::MAX), None);
}

#[test]
fn range_construction_is_linear_visitation_and_queries_are_logarithmic() {
    for count in [64usize, 256, 1_024, 4_096] {
        // Disjoint ranges give a large searchable span index.
        let lookup = RangeLookup::new((0..count).map(|index| (index * 3, index * 3 + 1, index)));
        assert_eq!(lookup.construction_visits, count * 3); // one input + two endpoint events
        for offset in 0..count * 3 {
            assert_eq!(
                lookup.containing(offset),
                (offset % 3 < 2).then_some(offset / 3)
            );
            lookup.following(offset);
        }
        let logarithmic_bound = ((count * 2).ilog2() as usize + 2) * count * 3 * 2;
        assert!(
            lookup.probes.get() <= logarithmic_bound,
            "{} probes for {count} ranges",
            lookup.probes.get()
        );

        // Heavy overlap must not turn a point query back into a range scan.
        let overlap = RangeLookup::new((0..count).map(|index| (0, count, index)));
        for offset in 0..=count {
            assert_eq!(overlap.containing(offset), Some(0));
        }
        assert!(overlap.probes.get() <= (count + 1) * 3);
    }
}

#[test]
fn anchor_groups_preserve_interleaved_alias_order_and_default_position_choice() {
    let (document, _) = document_and_layout();
    let node = document.root();
    let ids: Vec<_> = ["a", "b", "c", "d"]
        .into_iter()
        .map(AnchorId::new)
        .collect();
    let anchors: Vec<_> = [1, 2, 1, 2]
        .into_iter()
        .map(|offset| Anchor {
            target: AnchorTarget {
                node,
                position: position(offset, Affinity::Downstream),
            },
            origin: None,
        })
        .collect();
    let lookup = AnchorLookup::from_anchors(ids.iter().zip(&anchors));
    assert_eq!(lookup.inspected_anchors, 4);
    assert_eq!(lookup.groups.len(), 2);
    assert_eq!(
        lookup
            .position_and_anchors(node, position(2, Affinity::Downstream))
            .1,
        vec![ids[1].clone(), ids[3].clone()]
    );
    let (chosen, aliases) = lookup.position_and_anchors(node, position(3, Affinity::Downstream));
    assert_eq!(chosen.offset.as_usize(), 1);
    assert_eq!(aliases, vec![ids[0].clone(), ids[2].clone()]);
    assert_eq!(lookup.all_anchors(node), ids);
}

#[test]
fn many_link_targets_share_one_anchor_pass_and_keep_target_order() {
    for count in [64, 256, 1_024] {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        for index in 0..count {
            let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
            let link = builder
                .add_node(NodeKind::Link(flow::Link {
                    content: flow::LinkContent::Phrasing,
                    destination: flow::Destination::Local(AnchorId::new(format!("a-{index:06}"))),
                    relation: LinkRelation::Ordinary,
                    title: None,
                }))
                .unwrap();
            let text = builder.add_text(format!("label-{index}")).unwrap();
            builder.append(root, paragraph).unwrap();
            builder.append(paragraph, link).unwrap();
            builder.append(link, text).unwrap();
            builder
                .add_anchor(AnchorId::new(format!("p-{index:06}")), paragraph)
                .unwrap();
            for prefix in ["a", "z"] {
                builder
                    .add_anchor(AnchorId::new(format!("{prefix}-{index:06}")), link)
                    .unwrap();
            }
        }
        let document = builder.finish().unwrap();
        let lookup = AnchorLookup::new(&document);
        assert_eq!(lookup.inspected_anchors, count * 3);
        assert_eq!(lookup.groups.len(), count * 2);
        let navigation = NavigationIndex::new(&document, &ContinuousLayout::default());
        assert_eq!(navigation.targets().len(), count * 2);
        for (index, targets) in navigation.targets().chunks_exact(2).enumerate() {
            assert_eq!(targets[0].kind, NavigationKind::Anchor);
            assert_eq!(targets[0].label, format!("p-{index:06}"));
            assert_eq!(targets[1].kind, NavigationKind::Link);
            assert_eq!(targets[1].label, format!("label-{index}"));
            assert_eq!(
                targets[1].flow_anchors(),
                &[
                    AnchorId::new(format!("a-{index:06}")),
                    AnchorId::new(format!("z-{index:06}")),
                ]
            );
            let identity = targets[1].flow.as_ref().unwrap();
            assert_eq!(
                lookup
                    .position_and_anchors(identity.node, identity.position)
                    .1,
                targets[1].flow_anchors()
            );
        }
        assert_eq!(lookup.inspected_anchors, count * 3);
    }
}

#[test]
fn many_aliases_are_visited_once_and_coalesced_without_pairwise_membership_scans() {
    for count in [64, 256, 1_024, 4_096] {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("content").unwrap();
        builder.append(root, paragraph).unwrap();
        builder.append(paragraph, text).unwrap();
        for index in 0..count {
            builder
                .add_anchor(AnchorId::new(format!("alias-{index:06}")), paragraph)
                .unwrap();
        }
        let document = builder.finish().unwrap();
        let lookup = AnchorLookup::new(&document);
        assert_eq!(lookup.inspected_anchors, count);
        assert_eq!(lookup.groups.len(), 1);
        for node in document.preorder(document.root()) {
            for affinity in [Affinity::Downstream, Affinity::Upstream] {
                let default = position(0, affinity);
                assert_eq!(
                    lookup.position_and_anchors(node, default),
                    reference_flow_position_and_anchors(&document, node, default)
                );
            }
        }
        let navigation = NavigationIndex::new(&document, &ContinuousLayout::default());
        assert_eq!(navigation.targets().len(), 1);
        assert_eq!(navigation.targets()[0].flow_anchors().len(), count);
        assert_eq!(navigation.targets()[0].label, "alias-000000");
    }
}
