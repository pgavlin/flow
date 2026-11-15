use crate::{
    model::{
        Affinity, TextPosition, TextRange,
        flow::{BaseDirection, BidiMode, BidiState, NodeId},
    },
    syntax::{SyntaxKind, highlight_code},
};
use hyphenation::{Hyphenator as _, Language, Load, Standard};
use std::sync::OnceLock;
use text_layout::{FirstFit, Item, KnuthPlass, LineWidths, ParagraphLayout, UniformLineWidth};
use unicode_segmentation::UnicodeSegmentation;

mod flow;
mod table;

#[cfg(feature = "render")]
pub(crate) use flow::compose_flow_document;
#[cfg(test)]
pub(crate) use flow::layout_flow_document;

/// Composes a flow document without assigning document-global vertical positions.
pub fn compose_document<M: TextMeasurer>(
    document: &crate::model::flow::FlowDocument,
    measurer: &M,
    options: &LayoutOptions,
) -> ComposedDocument {
    let styles = document.resolve_styles(crate::model::flow::StyleContextKey::default());
    flow::compose_flow_document(document, &styles, measurer, options)
}

/// Places a composed document into one continuous vertical flow.
pub fn place_composed_document(document: ComposedDocument) -> ContinuousLayout {
    document.into_continuous_layout()
}

/// Lays out a flow document with the default reader style context.
pub fn layout_document<M: TextMeasurer>(
    document: &crate::model::flow::FlowDocument,
    measurer: &M,
    options: &LayoutOptions,
) -> ContinuousLayout {
    place_composed_document(compose_document(document, measurer, options))
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: FontFamily,
    pub size: f32,
    pub weight: FontWeight,
    pub italic: bool,
    pub strikethrough: bool,
    pub link: bool,
    pub initial_letter: Option<crate::model::flow::InitialLetter>,
    pub baseline_shift: f32,
    pub letter_spacing: f32,
    pub bidi: BidiState,
    pub bidi_boundary: Option<usize>,
    pub shape_direction: BaseDirection,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum FontFamily {
    Proportional,
    SansSerif,
    Monospace,
    Named(String),
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FontWeight {
    Normal,
    Bold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageMetrics {
    pub key: String,
    pub width: f32,
    pub height: f32,
}

pub trait TextMeasurer {
    fn measure(&self, text: &str, style: &TextStyle) -> TextMetrics;

    fn measure_image(&self, _url: &str) -> Option<ImageMetrics> {
        None
    }

    fn measure_image_resource(
        &self,
        identity: &str,
        _resource_base: Option<&str>,
        intrinsic_size: Option<(f32, f32)>,
    ) -> ImageMetrics {
        self.measure_image(identity).unwrap_or_else(|| {
            let (width, height) = intrinsic_size.unwrap_or((64.0, 48.0));
            ImageMetrics {
                key: identity.to_owned(),
                width,
                height,
            }
        })
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TableOverflowPolicy {
    /// Preserve standards-derived table geometry beyond the available measure.
    Overflow,
    /// Preserve table geometry and rely on the bounded renderer to clip it.
    Clip,
    /// Reflow below min-content width using explicit emergency opportunities.
    #[default]
    EmergencyFit,
}

#[derive(Clone, Debug)]
pub struct LayoutOptions {
    pub width: f32,
    pub body_size: f32,
    pub line_spacing: f32,
    pub block_spacing: f32,
    pub indent: f32,
    pub heading_scales: [f32; 6],
    pub heading_tracking: [f32; 6],
    pub inline_code_scale: f32,
    pub code_block_scale: f32,
    pub superscript_scale: f32,
    pub superscript_shift: f32,
    pub table_horizontal_padding: f32,
    pub table_vertical_padding: f32,
    pub table_min_column_width: f32,
    pub table_overflow: TableOverflowPolicy,
    pub minimum_line_width: f32,
    pub space_stretch: f32,
    pub space_shrink: f32,
    pub hyphenation: bool,
    pub hyphen_penalty: f32,
    pub code_block_indent: f32,
    pub nested_code_block_indent: f32,
    pub quote_bar_offset: f32,
    pub quote_italic: bool,
    pub table_header_bold: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            width: 720.0,
            body_size: 18.0,
            line_spacing: 1.35,
            block_spacing: 16.0,
            indent: 28.0,
            heading_scales: [2.0, 1.6, 1.3, 1.1, 1.1, 1.1],
            heading_tracking: [0.0; 6],
            inline_code_scale: 0.92,
            code_block_scale: 0.9,
            superscript_scale: 0.72,
            superscript_shift: 0.35,
            table_horizontal_padding: 10.0,
            table_vertical_padding: 6.0,
            table_min_column_width: 2.0,
            table_overflow: TableOverflowPolicy::EmergencyFit,
            minimum_line_width: 4.0,
            space_stretch: 0.5,
            space_shrink: 1.0 / 3.0,
            hyphenation: true,
            hyphen_penalty: 50.0,
            code_block_indent: 0.5,
            nested_code_block_indent: 1.0,
            quote_bar_offset: 16.0,
            quote_italic: true,
            table_header_bold: true,
        }
    }
}

/// Width-dependent intrinsic composition with block-local vertical geometry.
///
/// Every contained [`BlockLayout`] has `y == 0`. Line, atomic-object, table,
/// and decoration coordinates are relative to that block. Document-global
/// vertical placement is added only when producing a [`ContinuousLayout`].
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default)]
pub struct ComposedDocument {
    pub width: f32,
    pub blocks: Vec<ComposedBlock>,
    leading_spacing: f32,
    minimum_height: f32,
}

/// One intrinsically composed block and its resolved vertical boundary spacing.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct ComposedBlock {
    pub layout: BlockLayout,
    pub margin_before: f32,
    pub margin_after: f32,
}

impl ComposedDocument {
    pub(crate) fn leading_spacing(&self) -> f32 {
        self.leading_spacing
    }

    pub(crate) fn minimum_height(&self) -> f32 {
        self.minimum_height
    }

    #[cfg(feature = "render")]
    pub(crate) fn from_cache_parts(
        width: f32,
        blocks: Vec<ComposedBlock>,
        leading_spacing: f32,
        minimum_height: f32,
    ) -> Self {
        Self {
            width,
            blocks,
            leading_spacing,
            minimum_height,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_blocks(
        width: f32,
        blocks: Vec<ComposedBlock>,
        leading_spacing: f32,
        minimum_height: f32,
    ) -> Self {
        Self {
            width,
            blocks,
            leading_spacing,
            minimum_height,
        }
    }

    /// Places this composition into a continuous layout without cloning its
    /// potentially large line, run, and table geometry.
    pub fn into_continuous_layout(self) -> ContinuousLayout {
        let mut y = self.leading_spacing;
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for composed in self.blocks {
            y += composed.margin_before;
            let mut block = composed.layout;
            debug_assert_eq!(block.y, 0.0, "composed block origins must remain local");
            let dy = y - block.y;
            block.translate_y(dy);
            y += block.height + composed.margin_after;
            blocks.push(block);
        }
        ContinuousLayout::new(self.width, y.max(self.minimum_height), blocks)
    }

    /// Produces a continuous layout while retaining the intrinsic composition.
    pub fn continuous_layout(&self) -> ContinuousLayout {
        self.clone().into_continuous_layout()
    }
}

/// Final placement for one unbounded continuous vertical flow.
#[derive(Clone, Debug, Default)]
pub struct ContinuousLayout {
    pub width: f32,
    pub height: f32,
    pub blocks: Vec<BlockLayout>,
    pub quote_bars: Vec<ContinuousQuoteBarLayout>,
}

/// A continuous-mode quotation decoration spanning adjacent quoted blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContinuousQuoteBarLayout {
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

impl ContinuousLayout {
    /// Builds continuous placement and its document-spanning decorations from
    /// globally positioned blocks.
    pub fn new(width: f32, height: f32, blocks: Vec<BlockLayout>) -> Self {
        let quote_bars = continuous_quote_bars(&blocks);
        Self {
            width,
            height,
            blocks,
            quote_bars,
        }
    }
}

/// A continuously placed document layout.
pub type DocumentLayout = ContinuousLayout;

fn continuous_quote_bars(blocks: &[BlockLayout]) -> Vec<ContinuousQuoteBarLayout> {
    let mut bars = Vec::new();
    let depth = blocks
        .iter()
        .map(|block| block.quote_bars.len())
        .max()
        .unwrap_or(0);
    for level in 0..depth {
        let mut active: Option<ContinuousQuoteBarLayout> = None;
        for block in blocks {
            if let Some(&x) = block.quote_bars.get(level) {
                match &mut active {
                    Some(active) => {
                        active.x = active.x.min(x);
                        active.height = block.y + block.height - active.y;
                    }
                    None => {
                        active = Some(ContinuousQuoteBarLayout {
                            x,
                            y: block.y,
                            height: block.height,
                        });
                    }
                }
            } else if let Some(bar) = active.take() {
                bars.push(bar);
            }
        }
        if let Some(bar) = active {
            bars.push(bar);
        }
    }
    bars
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutBlockKind {
    Paragraph,
    Heading,
    Code,
    Image,
    Table,
    Rule,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug)]
pub struct BorderLayout {
    pub width: f32,
    pub color: Option<[u8; 3]>,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct BlockBoxLayout {
    pub x: f32,
    pub width: f32,
    /// Top, right, bottom, left.
    pub borders: [BorderLayout; 4],
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub node: Option<NodeId>,
    pub kind: LayoutBlockKind,
    pub range: TextRange,
    pub y: f32,
    pub height: f32,
    pub lines: Vec<LineLayout>,
    pub atomic_objects: Vec<AtomicLayout>,
    pub table: Option<TableLayout>,
    pub rule: bool,
    pub rule_x: f32,
    pub rule_width: f32,
    pub quoted: bool,
    pub quote_bars: Vec<f32>,
    pub box_layout: Option<BlockBoxLayout>,
    pub break_before: crate::model::flow::AuthorBreak,
    pub break_after: crate::model::flow::AuthorBreak,
    pub break_inside_avoid: bool,
    pub widows: u8,
    pub orphans: u8,
}

impl BlockLayout {
    pub(crate) fn translate_y(&mut self, dy: f32) {
        self.y += dy;
        for line in &mut self.lines {
            line.y += dy;
        }
        for object in &mut self.atomic_objects {
            object.translate(0.0, dy);
        }
        if let Some(table) = &mut self.table {
            table.row_y += dy;
            for rule in &mut table.rules {
                rule.y += dy;
            }
            for bar in &mut table.quote_bars {
                bar.y += dy;
            }
        }
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct AtomicLayout {
    pub node: Option<NodeId>,
    pub range: TextRange,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub leading_x: f32,
    pub trailing_x: f32,
    pub bidi: BidiState,
    pub flow_link: Option<crate::model::flow::Destination>,
    pub kind: AtomicLayoutKind,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub enum AtomicLayoutKind {
    Image { key: String, alt: String },
    Placeholder { label: String },
    ThematicBreak,
}

impl AtomicLayout {
    pub(crate) fn translate(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.leading_x += dx;
        self.trailing_x += dx;
    }

    #[cfg(all(test, feature = "render"))]
    pub(crate) fn resize(&mut self, width: f32, height: f32) {
        let leading_is_right = self.leading_x > self.trailing_x;
        self.width = width;
        self.height = height;
        self.leading_x = if leading_is_right {
            self.x + width
        } else {
            self.x
        };
        self.trailing_x = if leading_is_right {
            self.x
        } else {
            self.x + width
        };
    }

    pub fn position_at(&self, x: f32) -> TextPosition {
        if (x - self.leading_x).abs() <= (x - self.trailing_x).abs() {
            TextPosition {
                offset: self.range.start,
                affinity: Affinity::Downstream,
            }
        } else {
            TextPosition {
                offset: self.range.end,
                affinity: Affinity::Upstream,
            }
        }
    }

    pub fn resolve_position(&self, position: TextPosition) -> Option<TextPosition> {
        if self.range.start == self.range.end {
            return None;
        }
        let interior = self.range.start < position.offset && position.offset < self.range.end;
        let leading =
            position.offset == self.range.start && position.affinity == Affinity::Downstream;
        let trailing = position.offset == self.range.end && position.affinity == Affinity::Upstream;
        if !interior && !leading && !trailing {
            return None;
        }
        Some(
            if interior && position.affinity == Affinity::Downstream || trailing {
                TextPosition {
                    offset: self.range.end,
                    affinity: Affinity::Upstream,
                }
            } else {
                TextPosition {
                    offset: self.range.start,
                    affinity: Affinity::Downstream,
                }
            },
        )
    }

    pub fn caret_x(&self, position: TextPosition) -> Option<f32> {
        let resolved = self.resolve_position(position)?;
        Some(if resolved.offset == self.range.start {
            self.leading_x
        } else {
            self.trailing_x
        })
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableRuleKind {
    Strong,
    Light,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct TableRuleLayout {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub kind: TableRuleKind,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct TableQuoteBarLayout {
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableSizingDiagnostics {
    pub grid_min: f32,
    pub grid_max: f32,
    pub emergency_min: f32,
    pub used_width: f32,
    pub fallback_active: bool,
    pub overflows_available: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct TableLayout {
    pub x: f32,
    pub width: f32,
    pub row_y: f32,
    pub header_row_count: usize,
    pub column_widths: Vec<f32>,
    pub row_heights: Vec<f32>,
    pub row_spans: Vec<(usize, usize)>,
    pub overflow_policy: TableOverflowPolicy,
    pub sizing: TableSizingDiagnostics,
    pub rules: Vec<TableRuleLayout>,
    pub quote_bars: Vec<TableQuoteBarLayout>,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct LineLayout {
    pub range: TextRange,
    pub y: f32,
    pub height: f32,
    pub baseline: f32,
    pub runs: Vec<RunLayout>,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[doc(hidden)]
pub enum GeneratedAction {
    NoteReturn {
        definition: NodeId,
        fallback: crate::model::flow::AnchorId,
    },
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct RunLayout {
    pub text: String,
    pub range: TextRange,
    pub style: TextStyle,
    pub syntax: SyntaxKind,
    pub flow_link: Option<crate::model::flow::Destination>,
    pub(crate) generated_action: Option<GeneratedAction>,
    pub x: f32,
    pub width: f32,
}

impl ContinuousLayout {
    pub(crate) fn generated_action_at(&self, x: f32, y: f32) -> Option<&GeneratedAction> {
        self.blocks
            .iter()
            .filter(|block| block.y <= y && y < block.y + block.height)
            .flat_map(|block| &block.lines)
            .filter(|line| line.y <= y && y < line.y + line.height)
            .flat_map(|line| &line.runs)
            .find(|run| run.generated_action.is_some() && run.x <= x && x < run.x + run.width)
            .and_then(|run| run.generated_action.as_ref())
    }

    pub fn atomic_objects(&self) -> impl Iterator<Item = &AtomicLayout> {
        self.blocks.iter().flat_map(|block| &block.atomic_objects)
    }

    pub fn atomic_at(&self, x: f32, y: f32) -> Option<&AtomicLayout> {
        self.atomic_objects().find(|object| {
            object.x <= x
                && x < object.x + object.width
                && object.y <= y
                && y < object.y + object.height
        })
    }

    pub fn resolve_atomic_position(&self, position: TextPosition) -> TextPosition {
        self.atomic_objects()
            .find_map(|object| object.resolve_position(position))
            .unwrap_or(position)
    }

    pub fn expand_atomic_range(&self, mut range: TextRange) -> TextRange {
        if range.start == range.end {
            return range;
        }
        for object in self.atomic_objects() {
            if object.range.start == object.range.end {
                continue;
            }
            if range.start < object.range.end && object.range.start < range.end {
                range.start = range.start.min(object.range.start);
                range.end = range.end.max(object.range.end);
            }
        }
        range
    }

    pub fn flow_link_at(&self, x: f32, y: f32) -> Option<&crate::model::flow::Destination> {
        self.flow_link_range_at(x, y).map(|(link, _)| link)
    }

    pub(crate) fn flow_link_range_at(
        &self,
        x: f32,
        y: f32,
    ) -> Option<(&crate::model::flow::Destination, TextRange)> {
        if let Some(object) = self.atomic_at(x, y)
            && let Some(link) = object.flow_link.as_ref()
        {
            return Some((link, object.range));
        }
        self.blocks
            .iter()
            .find(|block| block.y <= y && y < block.y + block.height)?
            .lines
            .iter()
            .filter(|line| line.y <= y && y < line.y + line.height)
            .find_map(|line| {
                if let Some(run) = line
                    .runs
                    .iter()
                    .find(|run| run.x <= x && x < run.x + run.width)
                    && let Some(link) = run.flow_link.as_ref()
                {
                    return Some((link, run.range));
                }
                line.runs.windows(2).find_map(|runs| {
                    let [left, right] = runs else {
                        return None;
                    };
                    let link = left.flow_link.as_ref()?;
                    (right.flow_link.as_ref() == Some(link)
                        && left.x + left.width <= x
                        && x < right.x)
                        .then_some((
                            link,
                            TextRange {
                                start: left.range.start,
                                end: right.range.end,
                            },
                        ))
                })
            })
    }

    pub fn text_range_at(&self, x: f32, y: f32) -> Option<TextRange> {
        if let Some(object) = self
            .atomic_at(x, y)
            .filter(|object| object.range.start != object.range.end)
        {
            return Some(object.range);
        }
        let block = self
            .blocks
            .iter()
            .find(|block| block.y <= y && y < block.y + block.height)?;
        let mut lines = block
            .lines
            .iter()
            .filter(|line| line.y <= y && y < line.y + line.height);
        let fallback = lines.clone().next().map(|line| line.range);
        lines
            .find_map(|line| {
                line.runs
                    .iter()
                    .find(|run| run.x <= x && x < run.x + run.width)
                    .map(|run| run.range)
            })
            .or(fallback)
    }

    pub fn y_for_text(&self, offset: usize) -> Option<f32> {
        if let Some(object) = self.atomic_objects().find(|object| {
            object.range.contains(offset)
                || object.range.start == offset
                || object.range.end == offset
        }) {
            return Some(object.y);
        }
        let block = self
            .blocks
            .iter()
            .find(|block| block.range.contains(offset) || block.range.start == offset)?;
        block
            .lines
            .iter()
            .find(|line| line.range.contains(offset) || line.range.start == offset)
            .map_or(Some(block.y), |line| Some(line.y))
    }

    pub fn text_offset_for_y(&self, y: f32) -> Option<usize> {
        let block = self
            .blocks
            .iter()
            .find(|block| y < block.y + block.height)
            .or_else(|| self.blocks.last())?;
        block
            .lines
            .iter()
            .find(|line| y < line.y + line.height)
            .map(|line| line.range.start)
            .or(Some(block.range.start))
    }
}

#[derive(Clone, Debug)]
struct Atom {
    text: String,
    range: TextRange,
    style: TextStyle,
    flow_link: Option<crate::model::flow::Destination>,
    metrics: TextMetrics,
    atomic: bool,
}

#[derive(Clone, Copy, Debug)]
struct Space {
    range: TextRange,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
enum Break {
    Line,
    Emergency,
    Hyphen(Atom),
    Paragraph,
}

type LayoutItem = Item<Atom, Space, Break>;

#[derive(Clone, Copy)]
enum WordBreaking {
    Normal,
    Hyphenate,
    InlineCode(f32),
    Emergency(f32),
}

#[allow(clippy::too_many_arguments)]
fn push_text_items<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
    word_breaking: WordBreaking,
    options: &LayoutOptions,
) {
    if let WordBreaking::InlineCode(width) = word_breaking {
        push_inline_code_items(items, text, range, style, measurer, width, options);
        return;
    }
    for (offset, segment) in text.split_word_bound_indices() {
        let start = range.start.saturating_add(offset);
        let source = TextRange {
            start,
            end: start.saturating_add(segment.len()).min(range.end),
        };
        if segment.contains('\n') {
            for (index, part) in segment.split('\n').enumerate() {
                if !part.is_empty() {
                    push_word(items, part, source, style.clone(), measurer);
                }
                if index + 1 < segment.split('\n').count() {
                    terminate(items, Break::Line);
                }
            }
        } else if segment.chars().all(char::is_whitespace) {
            let metrics = measurer.measure(" ", &style);
            items.push(Item::Glue {
                width: metrics.width,
                stretch: metrics.width * options.space_stretch,
                shrink: metrics.width * options.space_shrink,
                data: Space { range: source },
            });
        } else if matches!(
            word_breaking,
            WordBreaking::Hyphenate | WordBreaking::Emergency(_)
        ) && options.hyphenation
            && style.family == FontFamily::Proportional
            && segment.chars().all(char::is_alphabetic)
            && push_hyphenated_word(
                items,
                segment,
                source,
                style.clone(),
                measurer,
                options.hyphen_penalty,
            )
        {
        } else if matches!(word_breaking, WordBreaking::Emergency(width) if
            measurer.measure(segment, &style).width > width && segment.graphemes(true).count() > 1
        ) {
            let graphemes: Vec<_> = segment.grapheme_indices(true).collect();
            for (index, (grapheme_offset, grapheme)) in graphemes.iter().enumerate() {
                let grapheme_source = TextRange {
                    start: source.start.saturating_add(*grapheme_offset),
                    end: source
                        .start
                        .saturating_add(*grapheme_offset + grapheme.len())
                        .min(source.end),
                };
                push_word(items, grapheme, grapheme_source, style.clone(), measurer);
                if index + 1 < graphemes.len() {
                    items.push(Item::Penalty {
                        width: 0.0,
                        cost: 100.0,
                        flagged: false,
                        data: Break::Emergency,
                    });
                }
            }
        } else {
            push_word(items, segment, source, style.clone(), measurer);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_inline_code_items<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
    width: f32,
    options: &LayoutOptions,
) {
    let ordinary_breaks = unicode_linebreak::linebreaks(text)
        .filter_map(|(offset, opportunity)| {
            (offset < text.len()
                && matches!(opportunity, unicode_linebreak::BreakOpportunity::Allowed))
            .then_some(offset)
        })
        .collect::<Vec<_>>();

    for (offset, segment) in text.split_word_bound_indices() {
        let start = range.start.saturating_add(offset);
        let source = TextRange {
            start,
            end: start.saturating_add(segment.len()).min(range.end),
        };
        if segment.contains('\n') {
            let mut part_offset = 0;
            for (index, part) in segment.split('\n').enumerate() {
                if !part.is_empty() {
                    let part_range = TextRange {
                        start: source.start.saturating_add(part_offset),
                        end: source
                            .start
                            .saturating_add(part_offset + part.len())
                            .min(source.end),
                    };
                    push_emergency_code_piece(
                        items,
                        part,
                        part_range,
                        style.clone(),
                        measurer,
                        width,
                    );
                }
                part_offset += part.len();
                if index + 1 < segment.split('\n').count() {
                    terminate(items, Break::Line);
                    part_offset += 1;
                }
            }
        } else if segment.chars().all(char::is_whitespace) {
            let metrics = measurer.measure(" ", &style);
            items.push(Item::Glue {
                width: metrics.width,
                stretch: metrics.width * options.space_stretch,
                shrink: metrics.width * options.space_shrink,
                data: Space { range: source },
            });
        } else {
            let end = offset + segment.len();
            let mut boundaries = ordinary_breaks
                .iter()
                .copied()
                .filter(|boundary| offset < *boundary && *boundary <= end)
                .collect::<Vec<_>>();
            if boundaries.last().copied() != Some(end) {
                boundaries.push(end);
            }
            let mut part_start = offset;
            for part_end in boundaries {
                if part_end > part_start {
                    let local_start = part_start - offset;
                    let local_end = part_end - offset;
                    push_emergency_code_piece(
                        items,
                        &segment[local_start..local_end],
                        TextRange {
                            start: source.start.saturating_add(local_start),
                            end: source.start.saturating_add(local_end).min(source.end),
                        },
                        style.clone(),
                        measurer,
                        width,
                    );
                }
                if part_end < text.len() && ordinary_breaks.binary_search(&part_end).is_ok() {
                    items.push(Item::Penalty {
                        width: 0.0,
                        cost: 0.0,
                        flagged: false,
                        data: Break::Emergency,
                    });
                }
                part_start = part_end;
            }
        }
    }
}

fn push_emergency_code_piece<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
    width: f32,
) {
    if measurer.measure(text, &style).width <= width || text.graphemes(true).count() <= 1 {
        push_word(items, text, range, style, measurer);
        return;
    }

    let graphemes = text.grapheme_indices(true).collect::<Vec<_>>();
    for (index, (offset, grapheme)) in graphemes.iter().enumerate() {
        push_word(
            items,
            grapheme,
            TextRange {
                start: range.start.saturating_add(*offset),
                end: range
                    .start
                    .saturating_add(*offset + grapheme.len())
                    .min(range.end),
            },
            style.clone(),
            measurer,
        );
        if index + 1 < graphemes.len() {
            items.push(Item::Penalty {
                width: 0.0,
                cost: 100.0,
                flagged: false,
                data: Break::Emergency,
            });
        }
    }
}

fn push_hyphenated_word<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
    penalty: f32,
) -> bool {
    static ENGLISH_US: OnceLock<Option<Standard>> = OnceLock::new();
    let Some(dictionary) = ENGLISH_US
        .get_or_init(|| Standard::from_embedded(Language::EnglishUS).ok())
        .as_ref()
    else {
        return false;
    };
    let breaks = dictionary.hyphenate(text).breaks;
    if breaks.is_empty() {
        return false;
    }

    let mut part_start = 0;
    for part_end in breaks.iter().copied().chain(std::iter::once(text.len())) {
        if part_end <= part_start || part_end > text.len() || !text.is_char_boundary(part_end) {
            continue;
        }
        let part_source = TextRange {
            start: range.start.saturating_add(part_start),
            end: range.start.saturating_add(part_end).min(range.end),
        };
        push_word(
            items,
            &text[part_start..part_end],
            part_source,
            style.clone(),
            measurer,
        );
        if part_end < text.len() {
            let boundary = part_source.end;
            // Use the ordinary hyphen glyph to avoid an unnecessary fallback
            // face when the primary font does not contain U+2010.
            let atom = make_atom(
                "-",
                TextRange {
                    start: boundary,
                    end: boundary,
                },
                style.clone(),
                measurer,
            );
            items.push(Item::Penalty {
                // A selected discretionary hyphen hangs outside the measure.
                // Keep its real glyph advance in the atom for paint/hit testing,
                // but charge no inline width during breaking or justification.
                width: 0.0,
                cost: penalty,
                flagged: true,
                data: Break::Hyphen(atom),
            });
        }
        part_start = part_end;
    }
    true
}

fn make_atom<M: TextMeasurer>(
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
) -> Atom {
    let mut metrics = measurer.measure(text, &style);
    metrics.ascent += style.baseline_shift;
    metrics.descent = (metrics.descent - style.baseline_shift).max(0.0);
    Atom {
        text: text.to_owned(),
        range,
        style,
        flow_link: None,
        metrics,
        atomic: false,
    }
}

fn set_flow_link(items: &mut [LayoutItem], destination: Option<&crate::model::flow::Destination>) {
    let Some(destination) = destination else {
        return;
    };
    for item in items {
        match item {
            Item::Box { data, .. } => data.flow_link = Some(destination.clone()),
            Item::Penalty {
                data: Break::Hyphen(atom),
                ..
            } => atom.flow_link = Some(destination.clone()),
            Item::Glue { .. } | Item::Penalty { .. } => {}
        }
    }
}

fn push_atomic_item(
    items: &mut Vec<LayoutItem>,
    range: TextRange,
    style: TextStyle,
    width: f32,
    height: f32,
) {
    items.push(Item::Box {
        width,
        data: Atom {
            text: String::new(),
            range,
            style,
            flow_link: None,
            metrics: TextMetrics {
                width,
                ascent: height,
                descent: 0.0,
            },
            atomic: true,
        },
    });
}

fn push_break_opportunity<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    range: TextRange,
    hyphen_when_broken: bool,
    style: TextStyle,
    measurer: &M,
) {
    let (data, flagged) = if hyphen_when_broken {
        (Break::Hyphen(make_atom("-", range, style, measurer)), true)
    } else {
        (Break::Emergency, false)
    };
    items.push(Item::Penalty {
        width: 0.0,
        cost: 0.0,
        flagged,
        data,
    });
}

fn push_word<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
) {
    if matches!(
        style.bidi.mode,
        BidiMode::Override | BidiMode::IsolateOverride
    ) {
        push_directional_word(items, text, range, style, measurer);
        return;
    }
    let base_level = match style.bidi.direction {
        BaseDirection::LeftToRight => Some(unicode_bidi::Level::ltr()),
        BaseDirection::RightToLeft => Some(unicode_bidi::Level::rtl()),
        BaseDirection::Auto => None,
    };
    let bidi = unicode_bidi::BidiInfo::new(text, base_level);
    let mut start = 0;
    while start < text.len() {
        let rtl = bidi.levels[start].is_rtl();
        let mut end = start + text[start..].chars().next().unwrap().len_utf8();
        while end < text.len() && bidi.levels[end].is_rtl() == rtl {
            end += text[end..].chars().next().unwrap().len_utf8();
        }
        let mut segment_style = style.clone();
        segment_style.shape_direction = if rtl {
            BaseDirection::RightToLeft
        } else {
            BaseDirection::LeftToRight
        };
        push_directional_word(
            items,
            &text[start..end],
            TextRange {
                start: range.start + start,
                end: (range.start + end).min(range.end),
            },
            segment_style,
            measurer,
        );
        start = end;
    }
}

fn push_directional_word<M: TextMeasurer>(
    items: &mut Vec<LayoutItem>,
    text: &str,
    range: TextRange,
    style: TextStyle,
    measurer: &M,
) {
    let atom = make_atom(text, range, style, measurer);
    items.push(Item::Box {
        width: atom.metrics.width,
        data: atom,
    });
}

fn terminate(items: &mut Vec<LayoutItem>, kind: Break) {
    terminate_at(items, kind, None);
}

fn terminate_at(items: &mut Vec<LayoutItem>, kind: Break, range: Option<TextRange>) {
    items.push(Item::Glue {
        width: 0.0,
        stretch: f32::INFINITY,
        shrink: 0.0,
        data: Space {
            range: range.unwrap_or_default(),
        },
    });
    items.push(Item::Penalty {
        width: 0.0,
        cost: f32::NEG_INFINITY,
        flagged: true,
        data: kind,
    });
}

fn line_box(metrics: TextMetrics, em_size: f32, line_spacing: f32) -> (f32, f32) {
    let glyph_height = metrics.ascent + metrics.descent;
    let height = (em_size * line_spacing).max(glyph_height);
    let baseline = metrics.ascent + (height - glyph_height) / 2.0;
    (height, baseline)
}

fn line_breaks(items: &[LayoutItem], width: f32) -> Vec<text_layout::Line> {
    line_breaks_with_widths(items, UniformLineWidth::from(width))
}

fn line_breaks_with_widths<L>(items: &[LayoutItem], widths: L) -> Vec<text_layout::Line>
where
    L: LineWidths<f32> + Clone,
{
    let breaks = KnuthPlass::new()
        .with_threshold(f32::INFINITY)
        .layout_paragraph(items, widths.clone());
    if breaks.is_empty() {
        FirstFit::new()
            .with_threshold(f32::INFINITY)
            .allow_overflow(true)
            .layout_paragraph(items, widths)
    } else {
        breaks
    }
}

fn materialize_lines(
    items: Vec<LayoutItem>,
    breaks: Vec<text_layout::Line>,
    left: f32,
    top: f32,
    options: &LayoutOptions,
    justify: bool,
) -> Vec<LineLayout> {
    let mut lines = Vec::new();
    let mut first = 0;
    let mut y = top;

    for line_break in breaks {
        if line_break.break_at < first || line_break.break_at >= items.len() {
            continue;
        }
        let line_items = &items[first..=line_break.break_at];
        let break_item = line_items.last();
        let forced_break = matches!(
            break_item,
            Some(Item::Penalty {
                data: Break::Line | Break::Paragraph,
                ..
            })
        );
        let hyphen = match break_item {
            Some(Item::Penalty {
                data: Break::Hyphen(atom),
                ..
            }) => Some(atom),
            _ => None,
        };
        let content = &line_items[..line_items.len().saturating_sub(1)];
        let mut ascent = 0.0_f32;
        let mut descent = 0.0_f32;
        let mut em_size = 0.0_f32;
        for item in content {
            if let Item::Box { data, .. } = item {
                ascent = ascent.max(data.metrics.ascent);
                descent = descent.max(data.metrics.descent);
                em_size = em_size.max(data.style.size);
            }
        }
        if ascent == 0.0 && descent == 0.0 {
            ascent = options.body_size * 0.8;
            descent = options.body_size * 0.2;
            em_size = options.body_size;
        }
        let (height, baseline) = line_box(
            TextMetrics {
                width: 0.0,
                ascent,
                descent,
            },
            em_size,
            options.line_spacing,
        );
        let mut x = left;
        let mut runs: Vec<RunLayout> = Vec::new();
        let mut source = TextRange {
            start: usize::MAX,
            end: 0,
        };

        for item in content {
            match item {
                Item::Box { width, data } => {
                    source.start = source.start.min(data.range.start);
                    source.end = source.end.max(data.range.end);
                    if let Some(previous) = runs.last_mut().filter(|previous| {
                        !data.atomic
                            && !previous.text.is_empty()
                            && previous.range.end == data.range.start
                            && previous.style == data.style
                            && previous.flow_link == data.flow_link
                            && (previous.x + previous.width - x).abs() < 0.001
                    }) {
                        previous.text.push_str(&data.text);
                        previous.range.end = data.range.end;
                        previous.width += *width;
                    } else {
                        runs.push(RunLayout {
                            text: data.text.clone(),
                            range: data.range,
                            style: data.style.clone(),
                            syntax: SyntaxKind::Plain,
                            flow_link: data.flow_link.clone(),
                            generated_action: None,
                            x,
                            width: *width,
                        });
                    }
                    x += *width;
                }
                Item::Glue {
                    width,
                    stretch,
                    shrink,
                    data,
                } => {
                    // Paragraph terminators are layout-only glue and must not
                    // pull a nonzero canonical line range back to byte zero.
                    // Explicit hard-break terminators carry their real range.
                    let synthetic_terminator = *width == 0.0
                        && stretch.is_infinite()
                        && *shrink == 0.0
                        && data.range == TextRange::default();
                    if !synthetic_terminator {
                        source.start = source.start.min(data.range.start);
                        source.end = source.end.max(data.range.end);
                    }
                    let ratio = line_break.adjustment_ratio;
                    x += if !ratio.is_finite() {
                        // An emergency/forced-break path can produce an
                        // infinite adjustment ratio when a chosen line has no
                        // usable flexible glue. Never let that move following
                        // runs to infinity; preserve their natural spacing.
                        *width
                    } else if ratio < 0.0 {
                        width + shrink * ratio
                    } else if ratio > 0.0 && !forced_break && justify {
                        width + stretch * ratio
                    } else {
                        *width
                    };
                }
                Item::Penalty { .. } => {}
            }
        }
        if let Some(atom) = hyphen {
            source.start = source.start.min(atom.range.start);
            source.end = source.end.max(atom.range.end);
            runs.push(RunLayout {
                text: atom.text.clone(),
                range: atom.range,
                style: atom.style.clone(),
                syntax: SyntaxKind::Plain,
                flow_link: atom.flow_link.clone(),
                generated_action: None,
                x,
                width: atom.metrics.width,
            });
        }
        if source.start == usize::MAX {
            source = TextRange::default();
        }
        reorder_bidi_runs(&mut runs);
        // A trailing hyphen becomes the leftmost visual run on an RTL line.
        // Move its protrusion outside that edge instead of extending the text
        // at the opposite (right) edge of the measure.
        if let Some(atom) = hyphen
            && runs
                .first()
                .is_some_and(|run| run.range == atom.range && run.text == atom.text)
            && runs.len() > 1
        {
            for run in &mut runs {
                run.x -= atom.metrics.width;
            }
        }
        lines.push(LineLayout {
            range: source,
            y,
            height,
            baseline,
            runs,
        });
        y += height;
        first = line_break.break_at + 1;
    }

    lines
}

fn bidi_controls(state: BidiState) -> (&'static str, &'static str) {
    let rtl = matches!(state.direction, BaseDirection::RightToLeft);
    match state.mode {
        BidiMode::Normal => ("", ""),
        BidiMode::Embed => {
            if rtl {
                ("\u{202b}", "\u{202c}")
            } else {
                ("\u{202a}", "\u{202c}")
            }
        }
        BidiMode::Isolate => match state.direction {
            BaseDirection::Auto => ("\u{2068}", "\u{2069}"),
            BaseDirection::RightToLeft => ("\u{2067}", "\u{2069}"),
            BaseDirection::LeftToRight => ("\u{2066}", "\u{2069}"),
        },
        BidiMode::Override => {
            if rtl {
                ("\u{202e}", "\u{202c}")
            } else {
                ("\u{202d}", "\u{202c}")
            }
        }
        BidiMode::IsolateOverride => {
            if rtl {
                ("\u{2067}\u{202e}", "\u{202c}\u{2069}")
            } else {
                ("\u{2066}\u{202d}", "\u{202c}\u{2069}")
            }
        }
        BidiMode::Plaintext => ("\u{2068}", "\u{2069}"),
    }
}

fn reorder_bidi_runs(runs: &mut [RunLayout]) {
    if runs.is_empty() {
        return;
    }

    let base_level = runs
        .iter()
        .find(|run| matches!(run.style.bidi.mode, BidiMode::Normal))
        .and_then(|run| match run.style.bidi.direction {
            BaseDirection::LeftToRight => Some(unicode_bidi::Level::ltr()),
            BaseDirection::RightToLeft => Some(unicode_bidi::Level::rtl()),
            BaseDirection::Auto => None,
        });
    let mut text = String::new();
    let mut offsets = Vec::with_capacity(runs.len());
    let mut active = None;
    for (index, run) in runs.iter().enumerate() {
        let state = (run.style.bidi, run.style.bidi_boundary);
        if active != Some(state) {
            if let Some(previous) = active {
                text.push_str(bidi_controls(previous.0).1);
            }
            if index != 0 && runs[index - 1].x + runs[index - 1].width < run.x - f32::EPSILON {
                text.push(' ');
            }
            text.push_str(bidi_controls(state.0).0);
            active = Some(state);
        } else if index != 0 && runs[index - 1].x + runs[index - 1].width < run.x - f32::EPSILON {
            text.push(' ');
        }
        offsets.push(text.len());
        if run.text.is_empty() {
            text.push('\u{fffc}');
        } else {
            text.push_str(&run.text);
        }
    }
    if let Some(active) = active {
        text.push_str(bidi_controls(active.0).1);
    }
    let bidi = unicode_bidi::BidiInfo::new(&text, base_level);
    let levels: Vec<_> = offsets.iter().map(|offset| bidi.levels[*offset]).collect();
    for (run, level) in runs.iter_mut().zip(&levels) {
        if !matches!(
            run.style.bidi.mode,
            BidiMode::Override | BidiMode::IsolateOverride
        ) {
            run.style.shape_direction = if level.is_rtl() {
                BaseDirection::RightToLeft
            } else {
                BaseDirection::LeftToRight
            };
        }
    }
    let visual_order = unicode_bidi::BidiInfo::reorder_visual(&levels);
    if visual_order.iter().copied().eq(0..runs.len()) {
        return;
    }

    let left = runs.first().map_or(0.0, |run| run.x);
    let right = runs
        .iter()
        .map(|run| run.x + run.width)
        .fold(left, f32::max);
    let content_width: f32 = runs.iter().map(|run| run.width).sum();
    let gap = ((right - left - content_width) / (runs.len() - 1) as f32).max(0.0);
    let logical = runs.to_vec();
    let mut x = left;
    for (slot, logical_index) in visual_order.into_iter().enumerate() {
        let mut run = logical[logical_index].clone();
        run.x = x;
        x += run.width;
        if slot + 1 < logical.len() {
            x += gap;
        }
        runs[slot] = run;
    }
}
