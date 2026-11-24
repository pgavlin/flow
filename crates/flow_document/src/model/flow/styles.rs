use std::collections::HashMap;

use super::{
    BaseDirection, BidiMode, BidiState, CellKind, ComputedStyleId, FlowDocument, InlineRole,
    NodeId, NodeKind, SemanticStyleKey, TextAlignment,
};

/// Reader- and environment-dependent inputs used to resolve immutable style inputs.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StyleContextKey {
    quote_italic: bool,
    table_header_bold: bool,
}

impl StyleContextKey {
    pub(crate) const fn new(quote_italic: bool, table_header_bold: bool) -> Self {
        Self {
            quote_italic,
            table_header_bold,
        }
    }

    const fn quote_italic(self) -> bool {
        self.quote_italic
    }

    const fn table_header_bold(self) -> bool {
        self.table_header_bold
    }
}

impl Default for StyleContextKey {
    fn default() -> Self {
        Self::new(true, true)
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EffectiveFontFamily {
    Proportional,
    SansSerif,
    Monospace,
    Named(String),
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EffectiveFontWeight {
    Normal,
    Bold,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EffectiveVerticalAlign {
    Baseline,
    Subscript,
    Superscript,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EffectiveTextBreaking {
    Prose,
    InlineCode,
}

/// Complete supported effective style for one or more nodes in a snapshot.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ComputedStyle {
    font_family: EffectiveFontFamily,
    font_weight: EffectiveFontWeight,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    link: bool,
    vertical_align: EffectiveVerticalAlign,
    text_alignment: Option<TextAlignment>,
    heading_level: Option<u8>,
    checkbox_marker: bool,
    text_breaking: EffectiveTextBreaking,
    font_size_per_mille: u32,
    author_font_size: bool,
    initial_letter: Option<super::InitialLetter>,
    line_height_per_mille: Option<u32>,
    text_indent: Option<super::CssLength>,
    margin_top: Option<super::CssLength>,
    margin_right: Option<super::CssLength>,
    margin_bottom: Option<super::CssLength>,
    margin_left: Option<super::CssLength>,
    padding_top: Option<super::CssLength>,
    padding_right: Option<super::CssLength>,
    padding_bottom: Option<super::CssLength>,
    padding_left: Option<super::CssLength>,
    border_top: Option<super::CssBorder>,
    border_right: Option<super::CssBorder>,
    border_bottom: Option<super::CssBorder>,
    border_left: Option<super::CssBorder>,
    width: Option<super::CssLength>,
    min_width: Option<super::CssLength>,
    max_width: Option<super::CssLength>,
    height: Option<super::CssLength>,
    min_height: Option<super::CssLength>,
    max_height: Option<super::CssLength>,
    break_before: super::AuthorBreak,
    break_after: super::AuthorBreak,
    break_inside_avoid: bool,
    widows: u8,
    orphans: u8,
    bidi: BidiState,
}

#[allow(dead_code)]
impl ComputedStyle {
    pub(crate) fn font_family(&self) -> EffectiveFontFamily {
        self.font_family.clone()
    }

    pub(crate) const fn font_weight(&self) -> EffectiveFontWeight {
        self.font_weight
    }

    pub(crate) const fn italic(&self) -> bool {
        self.italic
    }

    pub(crate) const fn underline(&self) -> bool {
        self.underline
    }

    pub(crate) const fn strikethrough(&self) -> bool {
        self.strikethrough
    }

    pub(crate) const fn is_link(&self) -> bool {
        self.link
    }

    pub(crate) const fn vertical_align(&self) -> EffectiveVerticalAlign {
        self.vertical_align
    }

    pub(crate) const fn text_alignment(&self) -> Option<TextAlignment> {
        self.text_alignment
    }

    pub(crate) const fn heading_level(&self) -> Option<u8> {
        self.heading_level
    }

    pub(crate) const fn checkbox_marker(&self) -> bool {
        self.checkbox_marker
    }

    pub(crate) const fn text_breaking(&self) -> EffectiveTextBreaking {
        self.text_breaking
    }

    pub(crate) const fn font_size_per_mille(&self) -> u32 {
        self.font_size_per_mille
    }

    pub(crate) const fn has_author_font_size(&self) -> bool {
        self.author_font_size
    }

    pub(crate) const fn initial_letter(&self) -> Option<super::InitialLetter> {
        self.initial_letter
    }

    pub(crate) const fn line_height_per_mille(&self) -> Option<u32> {
        self.line_height_per_mille
    }

    pub(crate) const fn text_indent(&self) -> Option<super::CssLength> {
        self.text_indent
    }

    pub(crate) const fn margin_top(&self) -> Option<super::CssLength> {
        self.margin_top
    }

    pub(crate) const fn margin_right(&self) -> Option<super::CssLength> {
        self.margin_right
    }

    pub(crate) const fn margin_bottom(&self) -> Option<super::CssLength> {
        self.margin_bottom
    }

    pub(crate) const fn margin_left(&self) -> Option<super::CssLength> {
        self.margin_left
    }

    pub(crate) const fn padding_top(&self) -> Option<super::CssLength> {
        self.padding_top
    }

    pub(crate) const fn padding_right(&self) -> Option<super::CssLength> {
        self.padding_right
    }

    pub(crate) const fn padding_bottom(&self) -> Option<super::CssLength> {
        self.padding_bottom
    }

    pub(crate) const fn padding_left(&self) -> Option<super::CssLength> {
        self.padding_left
    }

    pub(crate) const fn borders(&self) -> [Option<super::CssBorder>; 4] {
        [
            self.border_top,
            self.border_right,
            self.border_bottom,
            self.border_left,
        ]
    }

    pub(crate) const fn width(&self) -> Option<super::CssLength> {
        self.width
    }

    pub(crate) const fn min_width(&self) -> Option<super::CssLength> {
        self.min_width
    }

    pub(crate) const fn max_width(&self) -> Option<super::CssLength> {
        self.max_width
    }

    pub(crate) const fn height(&self) -> Option<super::CssLength> {
        self.height
    }

    pub(crate) const fn min_height(&self) -> Option<super::CssLength> {
        self.min_height
    }

    pub(crate) const fn max_height(&self) -> Option<super::CssLength> {
        self.max_height
    }

    pub(crate) const fn break_before(&self) -> super::AuthorBreak {
        self.break_before
    }

    pub(crate) const fn break_after(&self) -> super::AuthorBreak {
        self.break_after
    }

    pub(crate) const fn break_inside_avoid(&self) -> bool {
        self.break_inside_avoid
    }

    pub(crate) const fn widows(&self) -> u8 {
        self.widows
    }

    pub(crate) const fn orphans(&self) -> u8 {
        self.orphans
    }

    pub(crate) const fn bidi(&self) -> BidiState {
        self.bidi
    }
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            font_family: EffectiveFontFamily::Proportional,
            font_weight: EffectiveFontWeight::Normal,
            italic: false,
            underline: false,
            strikethrough: false,
            link: false,
            vertical_align: EffectiveVerticalAlign::Baseline,
            text_alignment: None,
            heading_level: None,
            checkbox_marker: false,
            text_breaking: EffectiveTextBreaking::Prose,
            font_size_per_mille: 1_000,
            author_font_size: false,
            initial_letter: None,
            line_height_per_mille: None,
            text_indent: None,
            margin_top: None,
            margin_right: None,
            margin_bottom: None,
            margin_left: None,
            padding_top: None,
            padding_right: None,
            padding_bottom: None,
            padding_left: None,
            border_top: None,
            border_right: None,
            border_bottom: None,
            border_left: None,
            width: None,
            min_width: None,
            max_width: None,
            height: None,
            min_height: None,
            max_height: None,
            break_before: super::AuthorBreak::Auto,
            break_after: super::AuthorBreak::Auto,
            break_inside_avoid: false,
            widows: 2,
            orphans: 2,
            bidi: BidiState {
                direction: BaseDirection::Auto,
                mode: BidiMode::Normal,
            },
        }
    }
}

/// Replaceable effective-style result for one immutable flow document.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct StyleSnapshot {
    node_styles: Vec<ComputedStyleId>,
    computed_styles: Vec<ComputedStyle>,
    context: StyleContextKey,
}

impl StyleSnapshot {
    pub const fn context(&self) -> &StyleContextKey {
        &self.context
    }

    /// Returns the effective style assigned to a node from the source document.
    pub fn style_for_node(&self, node: NodeId) -> Option<ComputedStyleId> {
        self.node_styles.get(node.index()).copied()
    }

    /// Resolves a same-snapshot computed style identity.
    pub fn get_computed_style(&self, style: ComputedStyleId) -> Option<&ComputedStyle> {
        self.computed_styles.get(style.index())
    }

    #[allow(dead_code)]
    pub(crate) fn computed_styles(
        &self,
    ) -> impl ExactSizeIterator<Item = (ComputedStyleId, &ComputedStyle)> {
        self.computed_styles
            .iter()
            .enumerate()
            .map(|(index, style)| (ComputedStyleId::from_index(index as u32), style))
    }
}

impl FlowDocument {
    /// Resolves immutable document inputs against replaceable reader policy.
    pub fn resolve_styles(&self, context: StyleContextKey) -> StyleSnapshot {
        let order = self.preorder(self.root()).collect::<Vec<_>>();
        let mut node_styles = Vec::with_capacity(order.len());
        let mut computed_styles = Vec::<ComputedStyle>::new();
        let mut computed_style_ids = HashMap::<ComputedStyle, ComputedStyleId>::new();
        let mut effective = Vec::<ComputedStyle>::with_capacity(order.len());

        for node_id in order {
            let node = self
                .get_node(node_id)
                .expect("preorder only returns document-owned nodes");
            let mut style = node
                .parent
                .and_then(|parent| effective.get(parent.index()))
                .cloned()
                .unwrap_or_default();
            // CSS direction inherits, but unicode-bidi and marker replacement
            // apply only to the node whose style input declares them.
            style.bidi.mode = BidiMode::Normal;
            style.checkbox_marker = false;
            if style
                .initial_letter
                .is_some_and(|initial| initial.target == super::InitialTarget::FirstLetter)
            {
                style.initial_letter = None;
            }
            // These CSS properties do not inherit.
            style.text_indent = None;
            style.margin_top = None;
            style.margin_right = None;
            style.margin_bottom = None;
            style.margin_left = None;
            style.padding_top = None;
            style.padding_right = None;
            style.padding_bottom = None;
            style.padding_left = None;
            style.border_top = None;
            style.border_right = None;
            style.border_bottom = None;
            style.border_left = None;
            style.width = None;
            style.min_width = None;
            style.max_width = None;
            style.height = None;
            style.min_height = None;
            style.max_height = None;
            style.break_before = super::AuthorBreak::Auto;
            style.break_after = super::AuthorBreak::Auto;
            style.break_inside_avoid = false;
            apply_node_defaults(&mut style, &node.kind, context);
            if let Some(input) = self.get_style_input(node.style_input) {
                if let Some(key) = input.semantic_key() {
                    apply_semantic_key(&mut style, key, context);
                }
                if let Some(alignment) = input.text_alignment() {
                    style.text_alignment = Some(alignment);
                }
                if let Some(bidi) = input.bidi() {
                    style.bidi = bidi;
                }
                if let Some(author) = input.author() {
                    apply_author_style(&mut style, author);
                }
            }
            let style_id = if let Some(id) = computed_style_ids.get(&style) {
                *id
            } else {
                let id = ComputedStyleId::from_index(computed_styles.len() as u32);
                computed_style_ids.insert(style.clone(), id);
                computed_styles.push(style.clone());
                id
            };
            node_styles.push(style_id);
            effective.push(style);
        }

        StyleSnapshot {
            node_styles,
            computed_styles,
            context,
        }
    }
}

fn apply_node_defaults(style: &mut ComputedStyle, kind: &NodeKind, context: StyleContextKey) {
    match kind {
        NodeKind::Heading(heading) => {
            style.font_weight = EffectiveFontWeight::Bold;
            style.heading_level = Some(heading.level.get());
            // An inherited author size establishes the heading's body-size
            // context; only a declaration on the heading or one of its inline
            // descendants replaces the reader's semantic heading scale.
            style.author_font_size = false;
        }
        NodeKind::Preformatted(_) => style.font_family = EffectiveFontFamily::Monospace,
        NodeKind::Quotation if context.quote_italic() => style.italic = true,
        NodeKind::Cell(cell) if cell.kind == CellKind::Header && context.table_header_bold() => {
            style.font_weight = EffectiveFontWeight::Bold;
        }
        NodeKind::InlineSemantic(semantic) => match semantic.role {
            InlineRole::Emphasis
            | InlineRole::Variable
            | InlineRole::Definition
            | InlineRole::Citation => style.italic = true,
            InlineRole::Strong => style.font_weight = EffectiveFontWeight::Bold,
            InlineRole::Deletion => style.strikethrough = true,
            InlineRole::Insertion => style.underline = true,
            InlineRole::Code | InlineRole::SampleOutput | InlineRole::KeyboardInput => {
                style.font_family = EffectiveFontFamily::Monospace;
                style.text_breaking = EffectiveTextBreaking::InlineCode;
            }
            InlineRole::Subscript => style.vertical_align = EffectiveVerticalAlign::Subscript,
            InlineRole::Superscript => style.vertical_align = EffectiveVerticalAlign::Superscript,
            _ => {}
        },
        NodeKind::Link(_) => style.link = true,
        _ => {}
    }
}

fn apply_author_style(style: &mut ComputedStyle, author: &super::AuthorStyle) {
    if let Some(family) = &author.font_family {
        style.font_family = match family {
            super::AuthorFontFamily::Monospace => EffectiveFontFamily::Monospace,
            super::AuthorFontFamily::Named(name) => EffectiveFontFamily::Named(name.clone()),
            super::AuthorFontFamily::Serif => EffectiveFontFamily::Proportional,
            super::AuthorFontFamily::SansSerif => EffectiveFontFamily::SansSerif,
        };
    }
    if let Some(scale) = author.font_size_per_mille {
        style.font_size_per_mille = style
            .font_size_per_mille
            .saturating_mul(scale)
            .saturating_div(1_000)
            .clamp(100, 10_000);
        style.author_font_size = true;
    }
    if let Some(initial_letter) = author.initial_letter {
        style.initial_letter = Some(initial_letter);
    }
    if let Some(weight) = author.font_weight {
        style.font_weight = if weight >= 600 {
            EffectiveFontWeight::Bold
        } else {
            EffectiveFontWeight::Normal
        };
    }
    if let Some(italic) = author.italic {
        style.italic = italic;
    }
    if let Some(line_height) = author.line_height_per_mille {
        style.line_height_per_mille = Some(line_height);
    }
    if let Some(alignment) = author.text_alignment {
        style.text_alignment = Some(alignment);
    }
    style.text_indent = author.text_indent;
    style.margin_top = author.margin_top;
    style.margin_right = author.margin_right;
    style.margin_bottom = author.margin_bottom;
    style.margin_left = author.margin_left;
    style.padding_top = author.padding_top;
    style.padding_right = author.padding_right;
    style.padding_bottom = author.padding_bottom;
    style.padding_left = author.padding_left;
    style.border_top = author.border_top;
    style.border_right = author.border_right;
    style.border_bottom = author.border_bottom;
    style.border_left = author.border_left;
    style.width = author.width;
    style.min_width = author.min_width;
    style.max_width = author.max_width;
    style.height = author.height;
    style.min_height = author.min_height;
    style.max_height = author.max_height;
    style.break_before = author.break_before.unwrap_or(super::AuthorBreak::Auto);
    style.break_after = author.break_after.unwrap_or(super::AuthorBreak::Auto);
    style.break_inside_avoid = author.break_inside_avoid.unwrap_or(false);
    if let Some(widows) = author.widows {
        style.widows = widows;
    }
    if let Some(orphans) = author.orphans {
        style.orphans = orphans;
    }
}

fn apply_semantic_key(style: &mut ComputedStyle, key: SemanticStyleKey, context: StyleContextKey) {
    match key {
        SemanticStyleKey::Heading => style.font_weight = EffectiveFontWeight::Bold,
        SemanticStyleKey::Quotation if context.quote_italic() => style.italic = true,
        SemanticStyleKey::Preformatted => style.font_family = EffectiveFontFamily::Monospace,
        SemanticStyleKey::InlineCode => {
            style.font_family = EffectiveFontFamily::Monospace;
            style.text_breaking = EffectiveTextBreaking::InlineCode;
        }
        SemanticStyleKey::Emphasis => style.italic = true,
        SemanticStyleKey::Strong => style.font_weight = EffectiveFontWeight::Bold,
        SemanticStyleKey::Deletion => style.strikethrough = true,
        SemanticStyleKey::Link => style.link = true,
        SemanticStyleKey::CheckboxMarker => style.checkbox_marker = true,
        SemanticStyleKey::OrderedList
        | SemanticStyleKey::UnorderedList
        | SemanticStyleKey::Table
        | SemanticStyleKey::ThematicBreak
        | SemanticStyleKey::Footnote
        | SemanticStyleKey::Quotation => {}
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::model::flow::{
        AnchorId, AuthorStyle, Destination, FlowBuilder, HeaderScope, Heading, HeadingLevel,
        InlineSemantic, Link, LinkContent, LinkRelation, RowGroup, StyleInput, Table,
    };

    #[test]
    fn resolves_inherited_markdown_ua_styles_and_interns_equal_results() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let heading = builder
            .add_node(NodeKind::Heading(Heading {
                level: HeadingLevel::H2,
            }))
            .unwrap();
        let strong = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Strong,
                value: None,
            }))
            .unwrap();
        let first = builder.add_text("first").unwrap();
        let second = builder.add_text("second").unwrap();
        builder.append(strong, first).unwrap();
        builder.append(heading, strong).unwrap();
        builder.append(heading, second).unwrap();
        builder.append(root, heading).unwrap();
        let document = builder.finish().unwrap();

        let snapshot = document.resolve_styles(StyleContextKey::default());
        let heading = document.children(document.root()).unwrap()[0];
        let strong = document.children(heading).unwrap()[0];
        let first = document.children(strong).unwrap()[0];
        let second = document.children(heading).unwrap()[1];
        let heading_style = snapshot
            .get_computed_style(snapshot.style_for_node(heading).unwrap())
            .unwrap();
        assert_eq!(heading_style.heading_level, Some(2));
        assert_eq!(heading_style.font_weight, EffectiveFontWeight::Bold);
        assert_eq!(
            snapshot.style_for_node(first),
            snapshot.style_for_node(second)
        );
        assert_eq!(snapshot.computed_styles().len(), 2);
        assert!(snapshot.style_for_node(NodeId::from_index(99)).is_none());

        // Hash-based lookup must not make snapshot-local identities depend on
        // the map's randomized iteration order.
        let repeated = document.resolve_styles(StyleContextKey::default());
        assert_eq!(repeated.node_styles, snapshot.node_styles);
        assert_eq!(repeated.computed_styles, snapshot.computed_styles);
    }

    #[test]
    fn explicit_author_heading_sizes_replace_semantic_heading_scale() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let heading = builder
            .add_node(NodeKind::Heading(Heading {
                level: HeadingLevel::H1,
            }))
            .unwrap();
        let style = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                font_size_per_mille: Some(1_750),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(heading, style).unwrap();
        let text = builder.add_text("Restrained title").unwrap();
        builder.append(heading, text).unwrap();
        builder.append(root, heading).unwrap();
        let document = builder.finish().unwrap();

        let snapshot = document.resolve_styles(StyleContextKey::default());
        for node in [heading, text] {
            let style = snapshot
                .get_computed_style(snapshot.style_for_node(node).unwrap())
                .unwrap();
            assert_eq!(style.heading_level, Some(1));
            assert_eq!(style.font_size_per_mille, 1_750);
            assert!(style.author_font_size);
        }
    }

    #[test]
    fn resolves_links_inline_roles_and_table_alignment() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: Destination::Local(AnchorId::new("target")),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        let code = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Code,
                value: None,
            }))
            .unwrap();
        let text = builder.add_text("code").unwrap();
        builder.append(code, text).unwrap();
        builder.append(link, code).unwrap();
        builder.append(paragraph, link).unwrap();
        builder.append(root, paragraph).unwrap();

        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let group = builder
            .add_node(NodeKind::RowGroup(RowGroup::Head))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = builder
            .add_node(NodeKind::Cell(super::super::Cell {
                kind: CellKind::Header,
                column_start: 0,
                column_span: NonZeroU32::new(1).unwrap(),
                row_span: NonZeroU32::new(1).unwrap(),
                scope: Some(HeaderScope::Column),
                explicit_headers: None,
            }))
            .unwrap();
        let cell_paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let cell_text = builder.add_text("header").unwrap();
        let alignment = builder
            .intern_style_input(
                super::super::StyleInput::table_alignment(TextAlignment::Right).with_bidi(
                    BidiState {
                        direction: BaseDirection::RightToLeft,
                        mode: BidiMode::Isolate,
                    },
                ),
            )
            .unwrap();
        builder.set_style_input(cell, alignment).unwrap();
        builder.append(cell_paragraph, cell_text).unwrap();
        builder.append(cell, cell_paragraph).unwrap();
        builder.append(row, cell).unwrap();
        builder.append(group, row).unwrap();
        builder.append(table, group).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();

        let snapshot = document.resolve_styles(StyleContextKey::default());
        let code_text = document
            .preorder(document.root())
            .find(|id| {
                let node = document.get_node(*id).unwrap();
                matches!(node.kind, NodeKind::Text)
                    && document.get_text(node.text_range) == Some("code")
            })
            .unwrap();
        let code_style = snapshot
            .get_computed_style(snapshot.style_for_node(code_text).unwrap())
            .unwrap();
        assert_eq!(code_style.font_family, EffectiveFontFamily::Monospace);
        assert!(code_style.link);

        let header_text = document
            .preorder(document.root())
            .find(|id| {
                let node = document.get_node(*id).unwrap();
                matches!(node.kind, NodeKind::Text)
                    && document.get_text(node.text_range) == Some("header")
            })
            .unwrap();
        let header_style = snapshot
            .get_computed_style(snapshot.style_for_node(header_text).unwrap())
            .unwrap();
        assert_eq!(header_style.font_weight, EffectiveFontWeight::Bold);
        assert_eq!(header_style.text_alignment, Some(TextAlignment::Right));
        assert_eq!(header_style.bidi.direction, BaseDirection::RightToLeft);
        assert_eq!(header_style.bidi.mode, BidiMode::Normal);
        let header_cell = document
            .ancestors(header_text)
            .find(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Cell(_)))
            .unwrap();
        let cell_style = snapshot
            .get_computed_style(snapshot.style_for_node(header_cell).unwrap())
            .unwrap();
        assert_eq!(cell_style.bidi.mode, BidiMode::Isolate);

        let plain_headers = document.resolve_styles(StyleContextKey::new(true, false));
        let plain_header_style = plain_headers
            .get_computed_style(plain_headers.style_for_node(header_text).unwrap())
            .unwrap();
        assert_eq!(plain_header_style.font_weight, EffectiveFontWeight::Normal);
    }

    #[test]
    fn resolves_observable_semantic_style_keys() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        builder.append(root, paragraph).unwrap();
        for (key, label) in [
            (SemanticStyleKey::Heading, "heading"),
            (SemanticStyleKey::Quotation, "quotation"),
            (SemanticStyleKey::Preformatted, "preformatted"),
            (SemanticStyleKey::InlineCode, "code"),
            (SemanticStyleKey::Emphasis, "emphasis"),
            (SemanticStyleKey::Strong, "strong"),
            (SemanticStyleKey::Deletion, "deletion"),
            (SemanticStyleKey::Link, "link"),
        ] {
            let wrapper = builder.add_node(NodeKind::Inline).unwrap();
            let style = builder
                .intern_style_input(super::super::StyleInput::semantic(key))
                .unwrap();
            builder.set_style_input(wrapper, style).unwrap();
            let text = builder.add_text(label).unwrap();
            builder.append(wrapper, text).unwrap();
            builder.append(paragraph, wrapper).unwrap();
        }
        let document = builder.finish().unwrap();
        let snapshot = document.resolve_styles(StyleContextKey::default());
        let style_for = |label: &str| {
            let text = document
                .preorder(document.root())
                .find(|id| {
                    let node = document.get_node(*id).unwrap();
                    matches!(node.kind, NodeKind::Text)
                        && document.get_text(node.text_range) == Some(label)
                })
                .unwrap();
            snapshot
                .get_computed_style(snapshot.style_for_node(text).unwrap())
                .unwrap()
        };

        assert_eq!(style_for("heading").font_weight, EffectiveFontWeight::Bold);
        assert!(style_for("quotation").italic);
        assert_eq!(
            style_for("preformatted").font_family,
            EffectiveFontFamily::Monospace
        );
        assert_eq!(
            style_for("code").font_family,
            EffectiveFontFamily::Monospace
        );
        assert!(style_for("emphasis").italic);
        assert_eq!(style_for("strong").font_weight, EffectiveFontWeight::Bold);
        assert!(style_for("deletion").strikethrough);
        assert!(style_for("link").link);

        let no_quote_italics = document.resolve_styles(StyleContextKey::new(false, true));
        let quotation = document
            .preorder(document.root())
            .find(|id| {
                let node = document.get_node(*id).unwrap();
                matches!(node.kind, NodeKind::Text)
                    && document.get_text(node.text_range) == Some("quotation")
            })
            .unwrap();
        assert!(
            !no_quote_italics
                .get_computed_style(no_quote_italics.style_for_node(quotation).unwrap())
                .unwrap()
                .italic
        );
    }

    #[test]
    fn reader_context_replaces_quote_and_header_policy_without_mutating_document() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let quote = builder.add_node(NodeKind::Quotation).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("quote").unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(quote, paragraph).unwrap();
        builder.append(root, quote).unwrap();
        let document = builder.finish().unwrap();
        let text = document.preorder(document.root()).last().unwrap();

        let italic = document.resolve_styles(StyleContextKey::new(true, true));
        let plain = document.resolve_styles(StyleContextKey::new(false, false));
        assert!(
            italic
                .get_computed_style(italic.style_for_node(text).unwrap())
                .unwrap()
                .italic
        );
        assert!(
            !plain
                .get_computed_style(plain.style_for_node(text).unwrap())
                .unwrap()
                .italic
        );
        assert_ne!(italic.context(), plain.context());
        assert_eq!(document.text().as_str(), "quote");
    }
}
