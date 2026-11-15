use std::num::NonZeroU32;

use super::{OriginId, ResourceId, StyleInputId, TextRange};

/// A node's override of its inherited language state.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum LanguageOverride {
    /// Retain the parent's effective language.
    #[default]
    Inherit,
    /// Explicitly establish that no language information is available.
    Unknown,
    /// Establish a normalized language tag for this subtree.
    Tag(LanguageTag),
}

/// A validated and canonically formatted BCP 47 language tag.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Parses, validates, and canonicalizes an RFC 5646 language tag.
    pub fn parse(value: &str) -> Result<Self, InvalidLanguageTag> {
        let tag = language_tags::LanguageTag::parse(value).map_err(|_| InvalidLanguageTag)?;
        tag.validate().map_err(|_| InvalidLanguageTag)?;
        let tag = tag.canonicalize().map_err(|_| InvalidLanguageTag)?;
        Ok(Self(tag.into_string()))
    }

    /// Returns the validated canonical tag.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Error returned when a value is not a valid BCP 47 language tag.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InvalidLanguageTag;

/// Requested base direction for a semantic subtree.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BaseDirection {
    /// Establish left-to-right base direction.
    LeftToRight,
    /// Establish right-to-left base direction.
    RightToLeft,
    /// Determine base direction from the first strong character.
    Auto,
}

/// Unicode bidirectional embedding and isolation behavior.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BidiMode {
    /// No additional embedding, isolation, or override.
    Normal,
    /// Establish an embedding level.
    Embed,
    /// Isolate descendants from surrounding directional context.
    Isolate,
    /// Force descendants to the specified base direction.
    Override,
    /// Combine isolation with a directional override.
    IsolateOverride,
    /// Determine each paragraph's direction independently.
    Plaintext,
}

/// Authoritative direction and bidi-control configuration in computed style.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BidiState {
    /// Base direction requested for the subtree.
    pub direction: BaseDirection,
    /// Embedding, isolation, or override behavior.
    pub mode: BidiMode,
}

impl Default for BidiState {
    fn default() -> Self {
        Self {
            direction: BaseDirection::Auto,
            mode: BidiMode::Normal,
        }
    }
}

/// Cross-cutting semantic metadata shared by all node kinds.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Semantics {
    /// Non-inherited publication and accessibility roles in source-token order.
    pub roles: Vec<SemanticRole>,
    /// Language inheritance, explicit reset, or tagged override.
    pub language: LanguageOverride,
}

/// A node purpose independent of its structural shape.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SemanticRole {
    /// A role with behavior understood by flow.
    Known(KnownRole),
    /// A qualified role retained from an external semantic vocabulary.
    Imported(QualifiedToken),
}

/// Semantic roles whose reading behavior flow understands directly.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KnownRole {
    Article,
    Main,
    Navigation,
    Header,
    Footer,
    Aside,
    Part,
    Chapter,
    Subchapter,
    Frontmatter,
    Bodymatter,
    Backmatter,
    Preface,
    Appendix,
    Bibliography,
    Index,
    Glossary,
    Footnote,
    Endnote,
    Pagebreak,
}

/// A role token retained from an external semantic vocabulary.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct QualifiedToken {
    /// Vocabulary or namespace identity, when supplied.
    pub vocabulary: Option<String>,
    /// Exact token name within that vocabulary.
    pub name: String,
}

/// One semantic tree node owned by a flow document.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Parent identity, or `None` only for the document root.
    pub parent: Option<super::NodeId>,
    /// Child identities in semantic reading order.
    pub children: Vec<super::NodeId>,
    /// Structural, phrasing, or leaf semantics.
    pub kind: NodeKind,
    /// Canonical text emitted by this subtree.
    pub text_range: TextRange,
    /// Immutable semantic and author style input.
    pub style_input: StyleInputId,
    /// Cross-cutting semantic metadata.
    pub semantics: Semantics,
    /// Source construct that produced this node, when known.
    pub origin: Option<OriginId>,
}

/// Durable semantic node kinds shared by supported flow formats.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Document,
    Flow,
    Section,
    Paragraph,
    Heading(Heading),
    Quotation,
    List(List),
    ListItem(ListItem),
    DefinitionList,
    Term,
    Definition,
    Preformatted(Preformatted),
    Figure,
    Caption,
    Table(Table),
    TableCaption,
    ColumnGroup(ColumnGroup),
    Column(Column),
    RowGroup(RowGroup),
    Row,
    Cell(Cell),
    Disclosure(Disclosure),
    Summary,
    ThematicBreak,
    LocationMarker(LocationMarker),
    Image(Image),
    Math(Math),
    UnsupportedObject(UnsupportedObject),
    Inline,
    InlineSemantic(InlineSemantic),
    Link(Link),
    Ruby,
    RubySegment,
    RubyBaseLevel,
    RubyAnnotationLevel,
    RubyBase,
    RubyAnnotation(RubyAnnotation),
    RubyFallback,
    Text,
    HardBreak,
    BreakOpportunity(BreakOpportunity),
    Checkbox(Checkbox),
}

/// Optional line-break position that contributes no canonical character.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BreakOpportunity {
    /// Whether layout paints a hyphen only when breaking here.
    pub hyphen_when_broken: bool,
}

/// Coverage metadata for one ruby annotation.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RubyAnnotation {
    /// Number of consecutive bases covered from the level cursor.
    pub base_span: NonZeroU32,
}

/// Semantic heading metadata.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Heading {
    /// HTML or Markdown heading level.
    pub level: HeadingLevel,
}

/// One of the six semantic HTML and Markdown heading levels.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum HeadingLevel {
    H1 = 1,
    H2 = 2,
    H3 = 3,
    H4 = 4,
    H5 = 5,
    H6 = 6,
}

impl HeadingLevel {
    /// Creates a heading level from an integer in the range one through six.
    pub const fn new(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::H1),
            2 => Some(Self::H2),
            3 => Some(Self::H3),
            4 => Some(Self::H4),
            5 => Some(Self::H5),
            6 => Some(Self::H6),
            _ => None,
        }
    }

    /// Returns the conventional numeric level.
    pub const fn get(self) -> u8 {
        self as u8
    }
}

/// Whether a list generates ordered or unordered markers.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ListKind {
    Ordered,
    Unordered,
}

/// Semantic data shared by items in one list.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct List {
    pub kind: ListKind,
    pub start: i64,
    pub reversed: bool,
}

/// Semantic data specific to one list item.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ListItem {
    pub ordinal: Option<i64>,
}

/// Read-only semantic checkbox state.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Checkbox {
    pub state: CheckboxState,
}

/// Checked state imported for a semantic checkbox.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CheckboxState {
    Unchecked,
    Checked,
}

/// Metadata governing a whitespace-preserving block.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Preformatted {
    pub language: Option<String>,
    pub content_kind: PreformattedKind,
}

/// Kind of content held by a preformatted block.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PreformattedKind {
    PlainText,
    SourceCode,
    Poetry,
    MermaidSource,
}

/// Finalized metadata for one normalized table.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Table {
    pub column_count: NonZeroU32,
}

/// A table column group and its declared span.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ColumnGroup {
    pub span: NonZeroU32,
}

/// One table column declaration.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Column {
    pub span: NonZeroU32,
}

/// Semantic part occupied by a group of table rows.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RowGroup {
    Head,
    Body,
    Foot,
}

/// Structural and accessibility metadata for one table cell.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    pub kind: CellKind,
    pub column_start: u32,
    pub column_span: NonZeroU32,
    pub row_span: NonZeroU32,
    pub scope: Option<HeaderScope>,
    pub explicit_headers: Option<Vec<super::NodeId>>,
}

/// Semantic identity of a table cell.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CellKind {
    Header,
    Data,
}

/// Cells associated with a table header cell.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HeaderScope {
    Row,
    Column,
    RowGroup,
    ColumnGroup,
}

/// Source state for a disclosure container.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Disclosure {
    pub initially_open: bool,
}

/// Metadata for a non-textual location boundary.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LocationMarker {
    pub kind: LocationMarkerKind,
    pub label: Option<String>,
}

/// Kind of location represented by a marker.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LocationMarkerKind {
    SourcePage,
    PublicationPagebreak,
    Other,
}

/// Semantic image and the resource needed to present it.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub resource: ResourceId,
    pub alternative: ImageAlternative,
    pub title: Option<String>,
    pub intrinsic_size: Option<IntrinsicSize>,
}

/// Alternative-text state imported for an image.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ImageAlternative {
    Text(NonEmptyText),
    Decorative,
    Missing,
}

/// Owned text guaranteed to contain at least one byte.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NonEmptyText(String);

impl NonEmptyText {
    /// Creates nonempty text without normalization.
    pub fn new(value: impl Into<String>) -> Result<Self, EmptyTextError> {
        let value = value.into();
        if value.is_empty() {
            Err(EmptyTextError)
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Error returned when constructing nonempty text from an empty string.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EmptyTextError;

/// Intrinsic width and height in CSS pixels.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntrinsicSize {
    pub width: PositiveCssPixels,
    pub height: PositiveCssPixels,
}

/// A finite CSS-pixel length strictly greater than zero.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct PositiveCssPixels(f32);

impl PositiveCssPixels {
    /// Creates a positive finite length.
    pub fn new(value: f32) -> Option<Self> {
        (value.is_finite() && value > 0.0).then_some(Self(value))
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Semantic mathematical expression.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Math {
    pub notation: MathNotation,
    pub source: String,
    pub display: MathDisplay,
}

/// Source notation retained for a mathematical expression.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MathNotation {
    Tex,
    MathMl,
    AsciiMath,
}

/// flow placement of a mathematical expression.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MathDisplay {
    Inline,
    Block,
}

/// Lossy placeholder for replaced content flow cannot present natively.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct UnsupportedObject {
    pub placement: Placement,
    pub media_kind: MediaKind,
    pub title: Option<String>,
    pub size_hint: Option<IntrinsicSize>,
    pub activation: Option<Destination>,
    pub reason: UnsupportedReason,
}

/// Whether replaced content participates in block or inline flow.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Placement {
    Flow,
    Inline,
}

/// Broad source-media identity for unsupported replaced content.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaKind {
    Audio,
    Video,
    Iframe,
    Svg,
    Embed,
    Unknown,
}

/// Why content was retained as an unsupported object.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum UnsupportedReason {
    NoNativePresenter,
    RejectedByPolicy,
    UnsupportedFormattingContext(String),
    UnknownSourceElement(String),
}

/// Recognized semantics attached to a phrasing wrapper.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct InlineSemantic {
    pub role: InlineRole,
    pub value: Option<String>,
}

/// Inline semantics shared by supported flow formats.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InlineRole {
    Emphasis,
    Strong,
    Deletion,
    Insertion,
    Code,
    SampleOutput,
    KeyboardInput,
    Variable,
    Definition,
    Small,
    Subscript,
    Superscript,
    Abbreviation,
    Citation,
    Quotation,
    Mark,
    Time,
}

/// Navigable phrasing or flow content.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Link {
    pub content: LinkContent,
    pub destination: Destination,
    pub relation: LinkRelation,
    pub title: Option<String>,
}

/// Child grammar inherited by a transparent link.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LinkContent {
    Phrasing,
    Flow,
}

/// Semantic relationship represented by a link.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum LinkRelation {
    Ordinary,
    NoteReference { return_anchor: Option<AnchorId> },
    NoteReturn,
}

/// Exact, case-sensitive name by which links address a location.
#[cfg_attr(
    any(feature = "render", feature = "serde"),
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnchorId(String);

impl AnchorId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Resolution domain of another flow document.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DocumentReference {
    Relative(String),
    PublicationItem(String),
    Host(String),
}

/// Normalized navigation destination independent of source URL syntax.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Destination {
    Local(AnchorId),
    Document {
        document: DocumentReference,
        anchor: Option<AnchorId>,
    },
    External(String),
}

/// Grammar class used by structural validation.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildGrammar {
    Flow,
    Phrasing,
    Specialized,
    None,
}

/// Canonical text behavior assigned to a node kind.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalTextRule {
    InlineChildren,
    FlowChildren,
    TableRoot,
    TableRows,
    TableCells,
    TextLiteral,
    HardBreak,
    BreakOpportunity,
    AtomicAlternative,
    ObjectReplacement,
    Empty,
}

impl NodeKind {
    /// Returns the broad grammar used to validate this node's children.
    pub fn child_grammar(&self) -> ChildGrammar {
        match self {
            Self::Document
            | Self::Flow
            | Self::Section
            | Self::Quotation
            | Self::ListItem(_)
            | Self::Definition
            | Self::Caption
            | Self::TableCaption
            | Self::Cell(_) => ChildGrammar::Flow,
            Self::Paragraph
            | Self::Heading(_)
            | Self::Term
            | Self::Preformatted(_)
            | Self::Summary
            | Self::Inline
            | Self::InlineSemantic(_)
            | Self::RubyBase
            | Self::RubyAnnotation(_)
            | Self::RubyFallback
            | Self::Link(Link {
                content: LinkContent::Phrasing,
                ..
            }) => ChildGrammar::Phrasing,
            Self::Link(Link {
                content: LinkContent::Flow,
                ..
            }) => ChildGrammar::Flow,
            Self::List(_)
            | Self::DefinitionList
            | Self::Figure
            | Self::Table(_)
            | Self::ColumnGroup(_)
            | Self::RowGroup(_)
            | Self::Row
            | Self::Disclosure(_)
            | Self::Ruby
            | Self::RubySegment
            | Self::RubyBaseLevel
            | Self::RubyAnnotationLevel
            | Self::UnsupportedObject(_) => ChildGrammar::Specialized,
            Self::ThematicBreak
            | Self::LocationMarker(_)
            | Self::Image(_)
            | Self::Math(_)
            | Self::Column(_)
            | Self::Text
            | Self::HardBreak
            | Self::BreakOpportunity(_)
            | Self::Checkbox(_) => ChildGrammar::None,
        }
    }

    /// Returns whether this kind may directly occupy an ordinary flow slot.
    pub fn is_flow_child(&self) -> bool {
        matches!(
            self,
            Self::Flow
                | Self::Section
                | Self::Paragraph
                | Self::Heading(_)
                | Self::Quotation
                | Self::List(_)
                | Self::DefinitionList
                | Self::Preformatted(_)
                | Self::Figure
                | Self::Table(_)
                | Self::Disclosure(_)
                | Self::ThematicBreak
                | Self::LocationMarker(_)
                | Self::Image(_)
                | Self::Link(Link {
                    content: LinkContent::Flow,
                    ..
                })
                | Self::Math(Math {
                    display: MathDisplay::Block,
                    ..
                })
                | Self::UnsupportedObject(UnsupportedObject {
                    placement: Placement::Flow,
                    ..
                })
        )
    }

    /// Returns whether this kind may directly occupy a phrasing slot.
    pub fn is_phrasing_child(&self) -> bool {
        matches!(
            self,
            Self::Inline
                | Self::InlineSemantic(_)
                | Self::Ruby
                | Self::Text
                | Self::HardBreak
                | Self::BreakOpportunity(_)
                | Self::Checkbox(_)
                | Self::LocationMarker(_)
                | Self::Image(_)
                | Self::Link(Link {
                    content: LinkContent::Phrasing,
                    ..
                })
                | Self::Math(Math {
                    display: MathDisplay::Inline,
                    ..
                })
                | Self::UnsupportedObject(UnsupportedObject {
                    placement: Placement::Inline,
                    ..
                })
        )
    }

    /// Returns the canonical-text finalization rule for this kind.
    pub fn logical_text_rule(&self) -> LogicalTextRule {
        match self {
            Self::Document
            | Self::Flow
            | Self::Section
            | Self::Quotation
            | Self::List(_)
            | Self::ListItem(_)
            | Self::DefinitionList
            | Self::Definition
            | Self::Figure
            | Self::Caption
            | Self::TableCaption
            | Self::Cell(_)
            | Self::Disclosure(_) => LogicalTextRule::FlowChildren,
            Self::Paragraph
            | Self::Heading(_)
            | Self::Term
            | Self::Preformatted(_)
            | Self::Summary
            | Self::Inline
            | Self::InlineSemantic(_)
            | Self::Ruby
            | Self::RubySegment
            | Self::RubyBaseLevel
            | Self::RubyAnnotationLevel
            | Self::RubyBase
            | Self::RubyAnnotation(_)
            | Self::RubyFallback
            | Self::Link(Link {
                content: LinkContent::Phrasing,
                ..
            }) => LogicalTextRule::InlineChildren,
            Self::Link(Link {
                content: LinkContent::Flow,
                ..
            }) => LogicalTextRule::FlowChildren,
            Self::Table(_) => LogicalTextRule::TableRoot,
            Self::RowGroup(_) => LogicalTextRule::TableRows,
            Self::Row => LogicalTextRule::TableCells,
            Self::Text => LogicalTextRule::TextLiteral,
            Self::HardBreak => LogicalTextRule::HardBreak,
            Self::BreakOpportunity(_) => LogicalTextRule::BreakOpportunity,
            Self::Image(_) | Self::Math(_) | Self::UnsupportedObject(_) => {
                LogicalTextRule::AtomicAlternative
            }
            Self::ThematicBreak => LogicalTextRule::ObjectReplacement,
            Self::LocationMarker(_)
            | Self::ColumnGroup(_)
            | Self::Column(_)
            | Self::Checkbox(_) => LogicalTextRule::Empty,
        }
    }
}
