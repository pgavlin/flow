//! Versioned, bounded, disposable single-document cache artifacts.
//!
//! These codecs are an optimization boundary, never a durable reading-location
//! boundary. Every decoded artifact is checked against its exact dependency
//! digest and validated before it is returned to a host.

use super::{CacheCodecError, DependencyDigest};
use flow_cache as transport;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    layout::{
        AtomicLayout, AtomicLayoutKind, BlockBoxLayout, BlockLayout, ComposedBlock,
        ComposedDocument, GeneratedAction, LayoutBlockKind, LineLayout, RunLayout, TableLayout,
        TextStyle,
    },
    model::TextRange,
    model::flow::{FlowBuilder, FlowDocument, NodeKind, Origin, OriginId, SourceArtifactId},
    pagination::{PaginatedLayout, build_page_items, validate_paginated_layout},
    syntax::SyntaxKind,
};

// Version 2 adds HTML provenance to the semantic origin vocabulary.
pub const SEMANTIC_CODEC_VERSION: u32 = 2;
pub const COMPOSITION_CODEC_VERSION: u32 = 2;
pub const PAGINATION_CODEC_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheCodecLimits {
    pub max_encoded_bytes: usize,
    pub max_semantic_nodes: usize,
    pub max_composed_blocks: usize,
    pub max_page_fragments: usize,
}

impl Default for CacheCodecLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 64 * 1024 * 1024,
            max_semantic_nodes: 1_000_000,
            max_composed_blocks: 1_000_000,
            max_page_fragments: 10_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheDisposition {
    Hit,
    Recomputed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ArtifactKind {
    Semantic = 1,
    Composition = 2,
    Pagination = 3,
}

impl ArtifactKind {
    fn format(self, version: u32) -> transport::ArtifactFormat {
        transport::ArtifactFormat {
            kind: self as u8,
            version,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SemanticPayload {
    document: FlowDocument,
}

#[derive(Serialize, Deserialize)]
struct CompositionArchive {
    width: f32,
    leading_spacing: f32,
    minimum_height: f32,
    styles: Vec<TextStyle>,
    blocks: Vec<ArchivedComposedBlock>,
}

#[derive(Serialize, Deserialize)]
struct ArchivedComposedBlock {
    layout: ArchivedBlockLayout,
    margin_before: f32,
    margin_after: f32,
}

#[derive(Serialize, Deserialize)]
struct ArchivedBlockLayout {
    node: Option<crate::model::flow::NodeId>,
    kind: LayoutBlockKind,
    range: TextRange,
    y: f32,
    height: f32,
    lines: Vec<ArchivedLineLayout>,
    atomic_objects: Vec<AtomicLayout>,
    table: Option<TableLayout>,
    rule: bool,
    rule_x: f32,
    rule_width: f32,
    quoted: bool,
    quote_bars: Vec<f32>,
    box_layout: Option<BlockBoxLayout>,
    break_before: crate::model::flow::AuthorBreak,
    break_after: crate::model::flow::AuthorBreak,
    break_inside_avoid: bool,
    widows: u8,
    orphans: u8,
}

#[derive(Serialize, Deserialize)]
struct ArchivedLineLayout {
    range: TextRange,
    y: f32,
    height: f32,
    baseline: f32,
    runs: Vec<ArchivedRunLayout>,
}

#[derive(Serialize, Deserialize)]
struct ArchivedRunLayout {
    text: String,
    range: TextRange,
    style: u32,
    syntax: SyntaxKind,
    flow_link: Option<crate::model::flow::Destination>,
    generated_action: Option<GeneratedAction>,
    x: f32,
    width: f32,
}

pub fn encode_semantic(
    document: &FlowDocument,
    dependency: DependencyDigest,
    limits: CacheCodecLimits,
) -> Result<Vec<u8>, CacheCodecError> {
    encode(
        ArtifactKind::Semantic,
        SEMANTIC_CODEC_VERSION,
        dependency,
        &SemanticPayload {
            document: document.clone(),
        },
        limits,
    )
}

pub fn decode_semantic(
    bytes: &[u8],
    dependency: DependencyDigest,
    limits: CacheCodecLimits,
) -> Result<FlowDocument, CacheCodecError> {
    let payload: SemanticPayload = decode(
        bytes,
        ArtifactKind::Semantic,
        SEMANTIC_CODEC_VERSION,
        dependency,
        limits,
    )?;
    if payload.document.nodes().len() > limits.max_semantic_nodes {
        return Err(CacheCodecError::StructuralLimit);
    }
    rebuild_semantic(&payload.document, limits)
}

pub fn semantic_or_recompute<E, F>(
    bytes: Option<&[u8]>,
    dependency: DependencyDigest,
    limits: CacheCodecLimits,
    recompute: F,
) -> Result<(FlowDocument, CacheDisposition), E>
where
    F: FnOnce() -> Result<FlowDocument, E>,
{
    if let Some(bytes) = bytes
        && let Ok(document) = decode_semantic(bytes, dependency, limits)
    {
        return Ok((document, CacheDisposition::Hit));
    }
    recompute().map(|document| (document, CacheDisposition::Recomputed))
}

pub fn encode_composition(
    composition: &ComposedDocument,
    dependency: DependencyDigest,
    limits: CacheCodecLimits,
) -> Result<Vec<u8>, CacheCodecError> {
    let _span = crate::telemetry::phase_span!(
        "flow.cache.encode",
        artifact_kind = ArtifactKind::Composition as u64,
    );
    validate_composition(composition, None, limits)?;
    let archive = archive_composition(composition)?;
    transport::encode_binary(
        ArtifactKind::Composition.format(COMPOSITION_CODEC_VERSION),
        dependency,
        &archive,
        limits.max_encoded_bytes,
    )
}

pub fn decode_composition(
    bytes: &[u8],
    dependency: DependencyDigest,
    semantic: &FlowDocument,
    limits: CacheCodecLimits,
) -> Result<ComposedDocument, CacheCodecError> {
    let _span = crate::telemetry::phase_span!(
        "flow.cache.decode",
        artifact_kind = ArtifactKind::Composition as u64,
        encoded_bytes = bytes.len() as u64,
    );
    let archive: CompositionArchive = transport::decode_binary(
        bytes,
        ArtifactKind::Composition.format(COMPOSITION_CODEC_VERSION),
        dependency,
        limits.max_encoded_bytes,
    )?;
    let composition = restore_composition(archive, limits)?;
    validate_composition(&composition, Some(semantic), limits)?;
    Ok(composition)
}

pub fn composition_or_recompute<E, F>(
    bytes: Option<&[u8]>,
    dependency: DependencyDigest,
    semantic: &FlowDocument,
    limits: CacheCodecLimits,
    recompute: F,
) -> Result<(ComposedDocument, CacheDisposition), E>
where
    F: FnOnce() -> Result<ComposedDocument, E>,
{
    if let Some(bytes) = bytes
        && let Ok(composition) = decode_composition(bytes, dependency, semantic, limits)
    {
        return Ok((composition, CacheDisposition::Hit));
    }
    recompute().map(|composition| (composition, CacheDisposition::Recomputed))
}

pub fn encode_pagination(
    pages: &PaginatedLayout,
    dependency: DependencyDigest,
    composition: &ComposedDocument,
    limits: CacheCodecLimits,
) -> Result<Vec<u8>, CacheCodecError> {
    if pages.fragments.len() > limits.max_page_fragments {
        return Err(CacheCodecError::StructuralLimit);
    }
    let items = build_page_items(composition);
    validate_paginated_layout(composition, &items, pages)
        .map_err(|_| CacheCodecError::Validation)?;
    encode(
        ArtifactKind::Pagination,
        PAGINATION_CODEC_VERSION,
        dependency,
        pages,
        limits,
    )
}

pub fn decode_pagination(
    bytes: &[u8],
    dependency: DependencyDigest,
    composition: &ComposedDocument,
    limits: CacheCodecLimits,
) -> Result<PaginatedLayout, CacheCodecError> {
    let pages: PaginatedLayout = decode(
        bytes,
        ArtifactKind::Pagination,
        PAGINATION_CODEC_VERSION,
        dependency,
        limits,
    )?;
    if pages.fragments.len() > limits.max_page_fragments
        || pages.pages.len() > limits.max_page_fragments
    {
        return Err(CacheCodecError::StructuralLimit);
    }
    let items = build_page_items(composition);
    validate_paginated_layout(composition, &items, &pages)
        .map_err(|_| CacheCodecError::Validation)?;
    Ok(pages)
}

pub fn pagination_or_recompute<E, F>(
    bytes: Option<&[u8]>,
    dependency: DependencyDigest,
    composition: &ComposedDocument,
    limits: CacheCodecLimits,
    recompute: F,
) -> Result<(PaginatedLayout, CacheDisposition), E>
where
    F: FnOnce() -> Result<PaginatedLayout, E>,
{
    if let Some(bytes) = bytes
        && let Ok(pages) = decode_pagination(bytes, dependency, composition, limits)
    {
        return Ok((pages, CacheDisposition::Hit));
    }
    recompute().map(|pages| (pages, CacheDisposition::Recomputed))
}

fn encode<T: Serialize>(
    kind: ArtifactKind,
    version: u32,
    dependency: DependencyDigest,
    value: &T,
    limits: CacheCodecLimits,
) -> Result<Vec<u8>, CacheCodecError> {
    let _span = crate::telemetry::phase_span!("flow.cache.encode", artifact_kind = kind as u64,);
    let payload = serde_json::to_vec(value).map_err(|_| CacheCodecError::Malformed)?;
    encode_payload(kind, version, dependency, payload, limits)
}

fn encode_payload(
    kind: ArtifactKind,
    version: u32,
    dependency: DependencyDigest,
    payload: Vec<u8>,
    limits: CacheCodecLimits,
) -> Result<Vec<u8>, CacheCodecError> {
    transport::encode_payload(
        kind.format(version),
        dependency,
        &payload,
        limits.max_encoded_bytes,
    )
}

fn decode<T: DeserializeOwned>(
    bytes: &[u8],
    kind: ArtifactKind,
    version: u32,
    dependency: DependencyDigest,
    limits: CacheCodecLimits,
) -> Result<T, CacheCodecError> {
    let payload = decode_payload(bytes, kind, version, dependency, limits)?;
    serde_json::from_slice(payload).map_err(|_| CacheCodecError::Malformed)
}

fn decode_payload(
    bytes: &[u8],
    kind: ArtifactKind,
    version: u32,
    dependency: DependencyDigest,
    limits: CacheCodecLimits,
) -> Result<&[u8], CacheCodecError> {
    let _span = crate::telemetry::phase_span!(
        "flow.cache.decode",
        artifact_kind = kind as u64,
        encoded_bytes = bytes.len() as u64,
    );
    transport::decode_payload(
        bytes,
        kind.format(version),
        dependency,
        limits.max_encoded_bytes,
    )
}

fn rebuild_semantic(
    document: &FlowDocument,
    limits: CacheCodecLimits,
) -> Result<FlowDocument, CacheCodecError> {
    let mut builder = FlowBuilder::new();
    builder.set_resource_base(document.resource_base().map(str::to_owned));
    for (_, resource) in document.resources() {
        builder
            .add_resource(resource.clone())
            .map_err(|_| CacheCodecError::Validation)?;
    }
    let mut source_lengths = vec![0usize; document.source_artifacts().len()];
    for (_, origin) in document.origins() {
        if let Some((source, end)) = direct_origin_source_end(origin) {
            let index = source_index(document, source).ok_or(CacheCodecError::Validation)?;
            source_lengths[index] = source_lengths[index].max(end);
        }
    }
    for span in document.origin_spans() {
        if let Some(range) = &span.source_range
            && let Some(source) = origin_source(document, span.origin)
        {
            let index = source_index(document, source).ok_or(CacheCodecError::Validation)?;
            source_lengths[index] = source_lengths[index].max(range.end);
        }
    }
    let total_source_bounds = source_lengths.iter().try_fold(0usize, |total, length| {
        total
            .checked_add(*length)
            .ok_or(CacheCodecError::StructuralLimit)
    })?;
    if total_source_bounds > limits.max_encoded_bytes {
        return Err(CacheCodecError::StructuralLimit);
    }
    for ((_, source), source_len) in document.source_artifacts().zip(source_lengths) {
        builder
            .add_cached_source_artifact(source.clone(), source_len)
            .map_err(|_| CacheCodecError::Validation)?;
    }
    for (_, style) in document.style_inputs() {
        builder
            .intern_style_input(style.clone())
            .map_err(|_| CacheCodecError::Validation)?;
    }
    for (_, origin) in document.origins() {
        builder
            .add_origin(origin.clone())
            .map_err(|_| CacheCodecError::Validation)?;
    }

    let order: Vec<_> = document.nodes().map(|(id, _)| id).collect();
    let mut rebuilt_ids = Vec::with_capacity(order.len());
    for &id in &order {
        let node = document.get_node(id).ok_or(CacheCodecError::Validation)?;
        let rebuilt = if matches!(node.kind, NodeKind::Text) {
            builder
                .add_text(
                    document
                        .get_text(node.text_range)
                        .ok_or(CacheCodecError::Validation)?,
                )
                .map_err(|_| CacheCodecError::Validation)?
        } else {
            builder
                .add_node(node.kind.clone())
                .map_err(|_| CacheCodecError::Validation)?
        };
        if rebuilt_ids.len() != id.layout_index() {
            return Err(CacheCodecError::Validation);
        }
        rebuilt_ids.push(rebuilt);
        builder
            .set_semantics(rebuilt, node.semantics.clone())
            .and_then(|_| builder.set_style_input(rebuilt, node.style_input))
            .map_err(|_| CacheCodecError::Validation)?;
        if let Some(origin) = node.origin {
            builder
                .set_origin(rebuilt, origin)
                .map_err(|_| CacheCodecError::Validation)?;
        }
    }
    for &id in &order {
        let node = document.get_node(id).ok_or(CacheCodecError::Validation)?;
        for &child in &node.children {
            let parent = rebuilt_ids
                .get(id.layout_index())
                .copied()
                .ok_or(CacheCodecError::Validation)?;
            let child = rebuilt_ids
                .get(child.layout_index())
                .copied()
                .ok_or(CacheCodecError::Validation)?;
            builder
                .append(parent, child)
                .map_err(|_| CacheCodecError::Validation)?;
        }
    }
    for (name, anchor) in document.anchors() {
        let target = rebuilt_ids
            .get(anchor.target.node.layout_index())
            .copied()
            .ok_or(CacheCodecError::Validation)?;
        builder
            .add_anchor_with_origin(name.clone(), target, anchor.origin)
            .map_err(|_| CacheCodecError::Validation)?;
    }
    let rebuilt = builder.finish().map_err(|_| CacheCodecError::Validation)?;
    let nodes_match = rebuilt.nodes().eq(document.nodes());
    let resources_match = rebuilt.resources().eq(document.resources());
    let sources_match = rebuilt.source_artifacts().eq(document.source_artifacts());
    let styles_match = rebuilt.style_inputs().eq(document.style_inputs());
    let origins_match = rebuilt.origins().eq(document.origins());
    let anchors_match = rebuilt.anchors().eq(document.anchors());
    if rebuilt.root() != document.root()
        || rebuilt.text().as_str() != document.text().as_str()
        || rebuilt.resource_base() != document.resource_base()
        || !nodes_match
        || !resources_match
        || !sources_match
        || !styles_match
        || !origins_match
        || !anchors_match
    {
        return Err(CacheCodecError::Validation);
    }
    for span in document.origin_spans() {
        if span.logical.start > span.logical.end
            || document.text().get(span.logical).is_none()
            || document.get_origin(span.origin).is_none()
            || span
                .source_range
                .as_ref()
                .is_some_and(|range| range.start > range.end)
        {
            return Err(CacheCodecError::Validation);
        }
    }
    Ok(document.clone())
}

fn direct_origin_source_end(origin: &Origin) -> Option<(SourceArtifactId, usize)> {
    match origin {
        Origin::Markdown {
            source,
            source_range,
        } => Some((*source, source_range.end)),
        Origin::Xhtml {
            source,
            serialized_range,
            ..
        }
        | Origin::Html {
            source,
            serialized_range,
            ..
        } => Some((
            *source,
            serialized_range.as_ref().map_or(0, |range| range.end),
        )),
        Origin::Generated { .. } => None,
    }
}

fn source_index(document: &FlowDocument, source: SourceArtifactId) -> Option<usize> {
    document
        .source_artifacts()
        .position(|(candidate, _)| candidate == source)
}

fn origin_source(document: &FlowDocument, mut origin: OriginId) -> Option<SourceArtifactId> {
    for _ in 0..document.origins().len() {
        match document.get_origin(origin)? {
            Origin::Markdown { source, .. }
            | Origin::Xhtml { source, .. }
            | Origin::Html { source, .. } => return Some(*source),
            Origin::Generated {
                related: Some(related),
                ..
            } => origin = *related,
            Origin::Generated { related: None, .. } => return None,
        }
    }
    None
}

fn archive_composition(
    composition: &ComposedDocument,
) -> Result<CompositionArchive, CacheCodecError> {
    let mut styles = Vec::<TextStyle>::new();
    let mut blocks = Vec::with_capacity(composition.blocks.len());
    for block in &composition.blocks {
        let mut lines = Vec::with_capacity(block.layout.lines.len());
        for line in &block.layout.lines {
            let mut runs = Vec::with_capacity(line.runs.len());
            for run in &line.runs {
                let style = match styles.iter().position(|style| style == &run.style) {
                    Some(index) => index,
                    None => {
                        styles.push(run.style.clone());
                        styles.len() - 1
                    }
                };
                runs.push(ArchivedRunLayout {
                    text: run.text.clone(),
                    range: run.range,
                    style: u32::try_from(style).map_err(|_| CacheCodecError::StructuralLimit)?,
                    syntax: run.syntax,
                    flow_link: run.flow_link.clone(),
                    generated_action: run.generated_action.clone(),
                    x: run.x,
                    width: run.width,
                });
            }
            lines.push(ArchivedLineLayout {
                range: line.range,
                y: line.y,
                height: line.height,
                baseline: line.baseline,
                runs,
            });
        }
        let layout = &block.layout;
        blocks.push(ArchivedComposedBlock {
            layout: ArchivedBlockLayout {
                node: layout.node,
                kind: layout.kind,
                range: layout.range,
                y: layout.y,
                height: layout.height,
                lines,
                atomic_objects: layout.atomic_objects.clone(),
                table: layout.table.clone(),
                rule: layout.rule,
                rule_x: layout.rule_x,
                rule_width: layout.rule_width,
                quoted: layout.quoted,
                quote_bars: layout.quote_bars.clone(),
                box_layout: layout.box_layout.clone(),
                break_before: layout.break_before,
                break_after: layout.break_after,
                break_inside_avoid: layout.break_inside_avoid,
                widows: layout.widows,
                orphans: layout.orphans,
            },
            margin_before: block.margin_before,
            margin_after: block.margin_after,
        });
    }
    Ok(CompositionArchive {
        width: composition.width,
        leading_spacing: composition.leading_spacing(),
        minimum_height: composition.minimum_height(),
        styles,
        blocks,
    })
}

fn restore_composition(
    archive: CompositionArchive,
    limits: CacheCodecLimits,
) -> Result<ComposedDocument, CacheCodecError> {
    if archive.blocks.len() > limits.max_composed_blocks
        || archive.styles.len() > limits.max_composed_blocks
    {
        return Err(CacheCodecError::StructuralLimit);
    }
    let mut total_runs = 0usize;
    let mut blocks = Vec::with_capacity(archive.blocks.len());
    for block in archive.blocks {
        let mut layout = block.layout;
        let archived_lines = std::mem::take(&mut layout.lines);
        let mut lines = Vec::with_capacity(archived_lines.len());
        for line in archived_lines {
            total_runs = total_runs
                .checked_add(line.runs.len())
                .ok_or(CacheCodecError::StructuralLimit)?;
            if total_runs > limits.max_page_fragments {
                return Err(CacheCodecError::StructuralLimit);
            }
            let mut runs = Vec::with_capacity(line.runs.len());
            for run in line.runs {
                let style = archive
                    .styles
                    .get(run.style as usize)
                    .cloned()
                    .ok_or(CacheCodecError::Validation)?;
                runs.push(RunLayout {
                    text: run.text,
                    range: run.range,
                    style,
                    syntax: run.syntax,
                    flow_link: run.flow_link,
                    generated_action: run.generated_action,
                    x: run.x,
                    width: run.width,
                });
            }
            lines.push(LineLayout {
                range: line.range,
                y: line.y,
                height: line.height,
                baseline: line.baseline,
                runs,
            });
        }
        blocks.push(ComposedBlock {
            layout: BlockLayout {
                node: layout.node,
                kind: layout.kind,
                range: layout.range,
                y: layout.y,
                height: layout.height,
                lines,
                atomic_objects: layout.atomic_objects,
                table: layout.table,
                rule: layout.rule,
                rule_x: layout.rule_x,
                rule_width: layout.rule_width,
                quoted: layout.quoted,
                quote_bars: layout.quote_bars,
                box_layout: layout.box_layout,
                break_before: layout.break_before,
                break_after: layout.break_after,
                break_inside_avoid: layout.break_inside_avoid,
                widows: layout.widows,
                orphans: layout.orphans,
            },
            margin_before: block.margin_before,
            margin_after: block.margin_after,
        });
    }
    Ok(ComposedDocument::from_cache_parts(
        archive.width,
        blocks,
        archive.leading_spacing,
        archive.minimum_height,
    ))
}

fn validate_composition(
    composition: &ComposedDocument,
    semantic: Option<&FlowDocument>,
    limits: CacheCodecLimits,
) -> Result<(), CacheCodecError> {
    if composition.blocks.len() > limits.max_composed_blocks
        || !finite_positive(composition.width)
        || !finite_nonnegative(composition.leading_spacing())
        || !finite_nonnegative(composition.minimum_height())
    {
        return Err(CacheCodecError::StructuralLimit);
    }
    for block in &composition.blocks {
        if !finite_nonnegative(block.margin_before)
            || !finite_nonnegative(block.margin_after)
            || block.layout.y != 0.0
            || !valid_block_geometry(&block.layout)
        {
            return Err(CacheCodecError::Validation);
        }
        if let Some(document) = semantic {
            validate_block_semantics(&block.layout, document)?;
        }
    }
    Ok(())
}

fn valid_block_geometry(block: &BlockLayout) -> bool {
    finite_nonnegative(block.height)
        && finite_nonnegative(block.rule_x)
        && finite_nonnegative(block.rule_width)
        && block.quote_bars.iter().all(|value| value.is_finite())
        && block.lines.iter().all(|line| {
            finite_nonnegative(line.y)
                && finite_nonnegative(line.height)
                && line.baseline.is_finite()
                && line.runs.iter().all(|run| {
                    run.x.is_finite()
                        && finite_nonnegative(run.width)
                        && run.style.size.is_finite()
                        && run.style.baseline_shift.is_finite()
                        && run.style.letter_spacing.is_finite()
                })
        })
        && block.atomic_objects.iter().all(|object| {
            object.x.is_finite()
                && object.y.is_finite()
                && finite_nonnegative(object.width)
                && finite_nonnegative(object.height)
                && object.leading_x.is_finite()
                && object.trailing_x.is_finite()
                && match &object.kind {
                    AtomicLayoutKind::Image { key, .. } => !key.is_empty(),
                    _ => true,
                }
        })
        && block.table.as_ref().is_none_or(|table| {
            table.x.is_finite()
                && finite_nonnegative(table.width)
                && table.row_y.is_finite()
                && table
                    .column_widths
                    .iter()
                    .all(|value| finite_nonnegative(*value))
                && table
                    .row_heights
                    .iter()
                    .all(|value| finite_nonnegative(*value))
                && table.rules.iter().all(|rule| {
                    rule.x.is_finite() && rule.y.is_finite() && finite_nonnegative(rule.width)
                })
                && table.quote_bars.iter().all(|bar| {
                    bar.x.is_finite() && bar.y.is_finite() && finite_nonnegative(bar.height)
                })
        })
}

fn validate_block_semantics(
    block: &BlockLayout,
    document: &FlowDocument,
) -> Result<(), CacheCodecError> {
    if !valid_text_range(document, block.range.start, block.range.end) {
        return Err(CacheCodecError::Validation);
    }
    if let Some(node) = block.node
        && document.get_node(node).is_none()
    {
        return Err(CacheCodecError::Validation);
    }
    for line in &block.lines {
        if !valid_text_range(document, line.range.start, line.range.end) {
            return Err(CacheCodecError::Validation);
        }
        for run in &line.runs {
            if !valid_text_range(document, run.range.start, run.range.end) {
                return Err(CacheCodecError::Validation);
            }
        }
    }
    for object in &block.atomic_objects {
        if !valid_text_range(document, object.range.start, object.range.end) {
            return Err(CacheCodecError::Validation);
        }
        if let Some(node) = object.node
            && document.get_node(node).is_none()
        {
            return Err(CacheCodecError::Validation);
        }
    }
    Ok(())
}

fn valid_text_range(document: &FlowDocument, start: usize, end: usize) -> bool {
    start <= end
        && end <= document.text().as_str().len()
        && document.text().as_str().is_char_boundary(start)
        && document.text().as_str().is_char_boundary(end)
}

fn finite_positive(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

fn finite_nonnegative(value: f32) -> bool {
    value.is_finite() && value >= 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        formats::markdown,
        pixels::PixelBuffer,
        render::{InteractionState, PageSelection, PaintOptions, Renderer},
        settings::RenderSettings,
    };

    fn fixture() -> (FlowDocument, ComposedDocument, PaginatedLayout) {
        let document = markdown::parse(
            "# Cache\n\nA paragraph with [a link](#cache).\n\n- one\n- two",
            markdown::MarkdownOptions::default(),
        )
        .unwrap()
        .document;
        let renderer = Renderer::new(RenderSettings::default());
        let composition = renderer.compose_document(&document, 420.0);
        let pages = renderer
            .paginate_composed_document(&composition, 500)
            .unwrap();
        (document, composition, pages)
    }

    #[test]
    fn artifacts_round_trip_deterministically_and_validate() {
        let limits = CacheCodecLimits::default();
        let source = DependencyDigest::for_bytes(b"source");
        let (document, composition, pages) = fixture();
        let semantic_bytes = encode_semantic(&document, source, limits).unwrap();
        assert_eq!(
            semantic_bytes,
            encode_semantic(&document, source, limits).unwrap()
        );
        let decoded_document = decode_semantic(&semantic_bytes, source, limits).unwrap();
        assert_eq!(decoded_document.text().as_str(), document.text().as_str());
        assert_eq!(
            encode_semantic(&decoded_document, source, limits).unwrap(),
            semantic_bytes
        );

        let semantic_digest = DependencyDigest::for_bytes(&semantic_bytes);
        let composition_bytes = encode_composition(&composition, semantic_digest, limits).unwrap();
        let direct_json = serde_json::to_vec(&composition).unwrap();
        assert!(composition_bytes.len() * 4 < direct_json.len());
        let decoded_composition = decode_composition(
            &composition_bytes,
            semantic_digest,
            &decoded_document,
            limits,
        )
        .unwrap();
        assert_eq!(decoded_composition.blocks.len(), composition.blocks.len());
        assert_eq!(
            encode_composition(&decoded_composition, semantic_digest, limits).unwrap(),
            composition_bytes
        );

        let composition_digest = DependencyDigest::for_bytes(&composition_bytes);
        let page_bytes =
            encode_pagination(&pages, composition_digest, &composition, limits).unwrap();
        let decoded_pages = decode_pagination(
            &page_bytes,
            composition_digest,
            &decoded_composition,
            limits,
        )
        .unwrap();
        assert_eq!(decoded_pages, pages);

        let renderer = Renderer::new(RenderSettings::default());
        let viewport = (520, 500);
        let mut fresh = PixelBuffer::new(viewport.0, viewport.1, [255; 3]).unwrap();
        let mut restored = PixelBuffer::new(viewport.0, viewport.1, [255; 3]).unwrap();
        let selection = PageSelection {
            requested_page: 0,
            text_anchor: None,
        };
        let options = PaintOptions {
            scale: 1.0,
            interaction: InteractionState::default(),
        };
        renderer
            .paint_paginated_page_into(
                &mut fresh.surface_mut(),
                &composition,
                &pages,
                viewport,
                selection,
                options,
            )
            .unwrap();
        renderer
            .paint_paginated_page_into(
                &mut restored.surface_mut(),
                &decoded_composition,
                &decoded_pages,
                viewport,
                selection,
                options,
            )
            .unwrap();
        assert_eq!(fresh.pixels(), restored.pixels());
        for (x, y) in [(80.0, 80.0), (180.0, 130.0), (320.0, 220.0)] {
            assert_eq!(
                renderer.text_position_at_paginated(&composition, &pages, 0, x, y),
                renderer.text_position_at_paginated(&decoded_composition, &decoded_pages, 0, x, y,)
            );
        }

        assert_eq!(
            encode_pagination(
                &decoded_pages,
                composition_digest,
                &decoded_composition,
                limits,
            )
            .unwrap(),
            page_bytes
        );
    }

    fn replace_payload(
        kind: ArtifactKind,
        version: u32,
        dependency: DependencyDigest,
        payload: &[u8],
    ) -> Vec<u8> {
        transport::encode_payload(kind.format(version), dependency, payload, usize::MAX).unwrap()
    }

    #[test]
    fn rejects_corruption_truncation_versions_dependencies_and_limits() {
        let limits = CacheCodecLimits::default();
        let source = DependencyDigest::for_bytes(b"source");
        let (document, _, _) = fixture();
        let bytes = encode_semantic(&document, source, limits).unwrap();

        assert_eq!(
            decode_semantic(&bytes[..bytes.len() - 1], source, limits).unwrap_err(),
            CacheCodecError::Truncated
        );
        let mut corrupted = bytes.clone();
        *corrupted.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode_semantic(&corrupted, source, limits).unwrap_err(),
            CacheCodecError::Checksum
        );
        let mut previous = bytes.clone();
        previous[12..16].copy_from_slice(&(SEMANTIC_CODEC_VERSION - 1).to_le_bytes());
        assert_eq!(
            decode_semantic(&previous, source, limits).unwrap_err(),
            CacheCodecError::Version
        );
        let mut future = bytes.clone();
        future[12..16].copy_from_slice(&(SEMANTIC_CODEC_VERSION + 1).to_le_bytes());
        assert_eq!(
            decode_semantic(&future, source, limits).unwrap_err(),
            CacheCodecError::Version
        );
        assert_eq!(
            decode_semantic(&bytes, DependencyDigest::for_bytes(b"other"), limits).unwrap_err(),
            CacheCodecError::Dependency
        );
        assert_eq!(
            decode_semantic(
                &bytes,
                source,
                CacheCodecLimits {
                    max_encoded_bytes: bytes.len() - 1,
                    ..limits
                }
            )
            .unwrap_err(),
            CacheCodecError::ByteLimit
        );

        let payload = decode_payload(
            &bytes,
            ArtifactKind::Semantic,
            SEMANTIC_CODEC_VERSION,
            source,
            limits,
        )
        .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(payload).unwrap();
        value["document"]["root"] = serde_json::json!(999);
        let invalid_root = replace_payload(
            ArtifactKind::Semantic,
            SEMANTIC_CODEC_VERSION,
            source,
            &serde_json::to_vec(&value).unwrap(),
        );
        assert_eq!(
            decode_semantic(&invalid_root, source, limits).unwrap_err(),
            CacheCodecError::Validation
        );
    }

    #[test]
    fn malformed_binary_composition_payload_is_bounded_and_rejected() {
        let limits = CacheCodecLimits::default();
        let dependency = DependencyDigest::for_bytes(b"semantic");
        let (document, composition, _) = fixture();
        let valid = encode_composition(&composition, dependency, limits).unwrap();
        assert!(decode_composition(&valid, dependency, &document, limits).is_ok());
        for payload in [
            vec![0xff; 19],
            vec![0xff, 0xff, 0xff, 0xff, 0x7f],
            vec![0; 32],
        ] {
            let malformed = replace_payload(
                ArtifactKind::Composition,
                COMPOSITION_CODEC_VERSION,
                dependency,
                &payload,
            );
            assert_eq!(
                decode_composition(&malformed, dependency, &document, limits).unwrap_err(),
                CacheCodecError::Malformed
            );
        }

        let mut invalid_style = archive_composition(&composition).unwrap();
        invalid_style.styles.clear();
        let malformed = transport::encode_binary(
            ArtifactKind::Composition.format(COMPOSITION_CODEC_VERSION),
            dependency,
            &invalid_style,
            limits.max_encoded_bytes,
        )
        .unwrap();
        assert_eq!(
            decode_composition(&malformed, dependency, &document, limits).unwrap_err(),
            CacheCodecError::Validation
        );
    }

    #[test]
    fn rejected_or_missing_artifacts_recompute_transparently() {
        let limits = CacheCodecLimits::default();
        let dependency = DependencyDigest::for_bytes(b"source");
        let (document, _, _) = fixture();
        let bytes = encode_semantic(&document, dependency, limits).unwrap();
        let mut corrupted = bytes.clone();
        *corrupted.last_mut().unwrap() ^= 1;

        let (decoded, disposition) =
            semantic_or_recompute(Some(&corrupted), dependency, limits, || {
                Ok::<_, ()>(document.clone())
            })
            .unwrap();
        assert_eq!(disposition, CacheDisposition::Recomputed);
        assert_eq!(decoded.text().as_str(), document.text().as_str());

        let (_, disposition) =
            semantic_or_recompute(None, dependency, limits, || Ok::<_, ()>(document)).unwrap();
        assert_eq!(disposition, CacheDisposition::Recomputed);
    }

    #[test]
    fn rejects_structurally_invalid_page_artifacts() {
        let limits = CacheCodecLimits::default();
        let (_, composition, mut pages) = fixture();
        let dependency = DependencyDigest::for_bytes(b"composition");
        pages.fragments.pop();
        let bytes = encode(
            ArtifactKind::Pagination,
            PAGINATION_CODEC_VERSION,
            dependency,
            &pages,
            limits,
        )
        .unwrap();
        assert_eq!(
            decode_pagination(&bytes, dependency, &composition, limits).unwrap_err(),
            CacheCodecError::Validation
        );
    }
}
