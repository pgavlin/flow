use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use super::{FlowDocument, NodeId, OriginId, SourceArtifactId, TextPosition, TextRange};

/// Target and provenance stored under an anchor name.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub target: AnchorTarget,
    pub origin: Option<OriginId>,
}

/// Resolved local target of an anchor.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AnchorTarget {
    pub node: NodeId,
    pub position: TextPosition,
}

/// Opaque source resource address with optional type information.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Resource {
    pub identity: String,
    pub media_type: Option<String>,
}

/// Immutable semantic and author-style input retained by a flow document.
///
/// Its concrete CSS representation is intentionally deferred. The zero-valued
/// catalog entry is a complete default rather than a missing style.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct StyleInput {
    semantic: Option<SemanticStyleKey>,
    text_alignment: Option<TextAlignment>,
    bidi: Option<super::BidiState>,
    author: Option<AuthorStyle>,
}

impl StyleInput {
    pub(crate) const fn default_input() -> Self {
        Self {
            semantic: None,
            text_alignment: None,
            bidi: None,
            author: None,
        }
    }

    /// Creates an immutable semantic UA-style input.
    pub const fn semantic(key: SemanticStyleKey) -> Self {
        Self {
            semantic: Some(key),
            text_alignment: None,
            bidi: None,
            author: None,
        }
    }

    /// Returns the semantic UA-style key, if this is not the default input.
    pub const fn semantic_key(&self) -> Option<SemanticStyleKey> {
        self.semantic
    }

    /// Creates a Markdown table-cell alignment input.
    pub const fn table_alignment(alignment: TextAlignment) -> Self {
        Self {
            semantic: None,
            text_alignment: Some(alignment),
            bidi: None,
            author: None,
        }
    }

    pub const fn text_alignment(&self) -> Option<TextAlignment> {
        self.text_alignment
    }

    /// Creates an immutable source direction and bidi-control input.
    pub const fn source_bidi(bidi: super::BidiState) -> Self {
        Self {
            semantic: None,
            text_alignment: None,
            bidi: Some(bidi),
            author: None,
        }
    }

    /// Adds an authoritative source direction and bidi-control input.
    pub const fn with_bidi(mut self, bidi: super::BidiState) -> Self {
        self.bidi = Some(bidi);
        self
    }

    pub const fn bidi(&self) -> Option<super::BidiState> {
        self.bidi
    }

    /// Adds cascaded author declarations retained by an XHTML importer.
    pub fn with_author(mut self, author: AuthorStyle) -> Self {
        self.author = (!author.is_empty()).then_some(author);
        self
    }

    pub fn author(&self) -> Option<&AuthorStyle> {
        self.author.as_ref()
    }
}

/// Portable family intent from supported author CSS. Named families remain
/// available for a publication resource resolver even when layout falls back
/// to a configured reader family.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AuthorFontFamily {
    Named(String),
    Serif,
    SansSerif,
    Monospace,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CssLength {
    /// Signed thousandths of one em.
    Em(i32),
    /// Signed thousandths of the containing inline measure.
    Percent(i32),
    /// Signed thousandths of one typographic point (1pt = 4/3 CSS px).
    Pt(i32),
    Px(i32),
}

impl CssLength {
    pub fn resolve(self, em: f32, percentage_basis: f32) -> f32 {
        match self {
            Self::Em(value) => em * value as f32 / 1_000.0,
            Self::Percent(value) => percentage_basis * value as f32 / 100_000.0,
            Self::Pt(value) => value as f32 * (4.0 / 3.0) / 1_000.0,
            Self::Px(value) => value as f32 / 1_000.0,
        }
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CssColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CssBorder {
    pub width: CssLength,
    pub color: Option<CssColor>,
}

/// Supported cascaded EPUB declarations. Relative font and line-height values
/// are fixed-point ratios so style catalogs remain deterministic and hashable.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct AuthorStyle {
    pub font_family: Option<AuthorFontFamily>,
    pub font_size_per_mille: Option<u32>,
    pub initial_letter: Option<InitialLetter>,
    pub float: Option<AuthorFloat>,
    pub font_weight: Option<u16>,
    pub italic: Option<bool>,
    pub line_height_per_mille: Option<u32>,
    pub text_alignment: Option<TextAlignment>,
    pub text_indent: Option<CssLength>,
    pub margin_top: Option<CssLength>,
    pub margin_right: Option<CssLength>,
    pub margin_bottom: Option<CssLength>,
    pub margin_left: Option<CssLength>,
    pub padding_top: Option<CssLength>,
    pub padding_right: Option<CssLength>,
    pub padding_bottom: Option<CssLength>,
    pub padding_left: Option<CssLength>,
    pub border_top: Option<CssBorder>,
    pub border_right: Option<CssBorder>,
    pub border_bottom: Option<CssBorder>,
    pub border_left: Option<CssBorder>,
    pub width: Option<CssLength>,
    pub min_width: Option<CssLength>,
    pub max_width: Option<CssLength>,
    pub height: Option<CssLength>,
    pub min_height: Option<CssLength>,
    pub max_height: Option<CssLength>,
    pub break_before: Option<AuthorBreak>,
    pub break_after: Option<AuthorBreak>,
    pub break_inside_avoid: Option<bool>,
    pub widows: Option<u8>,
    pub orphans: Option<u8>,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AuthorFloat {
    None,
    Left,
    Right,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
/// One initial-letter treatment, independent of the source CSS syntax.
pub struct InitialLetter {
    pub target: InitialTarget,
    pub sizing: InitialSizing,
}

impl InitialLetter {
    pub fn typographic(self) -> Option<TypographicInitial> {
        match self.sizing {
            InitialSizing::Typographic(value) => Some(value),
            InitialSizing::LegacyFloat(_) => None,
        }
    }

    pub fn floated(self) -> Option<FloatedInitial> {
        match self.sizing {
            InitialSizing::LegacyFloat(value) => Some(value),
            InitialSizing::Typographic(_) => None,
        }
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InitialTarget {
    /// The initial is already isolated in an inline node.
    ThisInline,
    /// Extract the first typographic letter from the block's text.
    FirstLetter,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InitialSizing {
    /// Cap-height-based sizing in surrounding lines, with an explicit sink.
    Typographic(TypographicInitial),
    /// Derive a whole-line span from the authored cap height, preserving CSS
    /// box sizing. Layout normalizes cap-height/baseline alignment and contains
    /// painted ink; no general floats.
    LegacyFloat(FloatedInitial),
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypographicInitial {
    /// Thousandths of the number of surrounding lines occupied by the letter.
    pub size_per_mille: u32,
    /// Number of lines from the first baseline to the letter's baseline.
    pub sink: u16,
    pub margin_top: Option<CssLength>,
    pub margin_right: Option<CssLength>,
    pub margin_bottom: Option<CssLength>,
    pub margin_left: Option<CssLength>,
}

/// Restricted leading left-float presentation, separate from CSS initial-letter.
/// Edges are in top/right/bottom/left order. Placement is paragraph-local;
/// this descriptor does not enable arbitrary CSS floats.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FloatedInitial {
    pub line_height_per_mille: Option<u32>,
    pub margin: [Option<CssLength>; 4],
    pub padding: [Option<CssLength>; 4],
    pub border: [Option<CssBorder>; 4],
    pub width: Option<CssLength>,
    pub min_width: Option<CssLength>,
    pub max_width: Option<CssLength>,
    pub height: Option<CssLength>,
    pub min_height: Option<CssLength>,
    pub max_height: Option<CssLength>,
}

impl AuthorStyle {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AuthorBreak {
    Auto,
    Avoid,
    Page,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
    Justify,
}

/// Format-independent semantic keys used by the Markdown user-agent stylesheet.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SemanticStyleKey {
    Heading,
    Quotation,
    OrderedList,
    UnorderedList,
    CheckboxMarker,
    Preformatted,
    Table,
    ThematicBreak,
    Emphasis,
    Strong,
    Deletion,
    InlineCode,
    Link,
    Footnote,
}

/// Identity and revision of one source buffer used during import.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceArtifactDescriptor {
    pub identity: Option<super::DocumentReference>,
    pub revision: SourceDigest,
    pub media_type: Option<String>,
}

/// Algorithm-tagged digest of an exact source revision.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceDigest {
    Blake3([u8; 32]),
}

impl SourceDigest {
    /// Computes the revision of the exact UTF-8 bytes used for source coordinates.
    pub fn for_text(source: &str) -> Self {
        Self::Blake3(*blake3::hash(source.as_bytes()).as_bytes())
    }

    /// Verifies candidate text against this exact revision.
    pub fn verifies(&self, source: &str) -> bool {
        self == &Self::for_text(source)
    }
}

/// Provenance of an imported or generated semantic node.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    Markdown {
        source: SourceArtifactId,
        source_range: Range<usize>,
    },
    Xhtml {
        source: SourceArtifactId,
        source_node: SourceNodeIdentity,
        serialized_range: Option<Range<usize>>,
    },
    Generated {
        reason: GeneratedReason,
        related: Option<OriginId>,
    },
}

/// Importer-assigned identity of a node in a temporary source tree.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceNodeIdentity(pub String);

/// Fine-grained correspondence between canonical text and importer source.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginSpan {
    pub logical: TextRange,
    pub origin: OriginId,
    pub source_range: Option<Range<usize>>,
}

/// Reason semantic content was generated rather than directly imported.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GeneratedReason {
    AnonymousParagraph,
    ListNormalization,
    TableNormalization,
    FootnoteLabel,
    MetadataPrologue,
    UnsupportedFallback,
}

/// Consumer policy for source bytes after import.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SourceRetention {
    #[default]
    Discard,
    Retain,
}

/// Exact source buffers returned separately from the immutable document.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default)]
pub struct RetainedSourceBundle {
    entries: BTreeMap<SourceDigest, Arc<str>>,
}

impl RetainedSourceBundle {
    #[allow(dead_code)]
    pub(crate) fn insert(
        &mut self,
        revision: SourceDigest,
        source: Arc<str>,
    ) -> Result<(), RetainedSourceError> {
        if !revision.verifies(&source) {
            return Err(RetainedSourceError);
        }
        self.entries.entry(revision).or_insert(source);
        Ok(())
    }

    /// Resolves retained UTF-8 text by its verified digest.
    pub fn get(&self, revision: &SourceDigest) -> Option<&str> {
        self.entries.get(revision).map(AsRef::as_ref)
    }

    /// Returns the number of distinct retained source revisions.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedSourceError;

impl std::fmt::Display for RetainedSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("retained source bytes do not match their revision")
    }
}

impl std::error::Error for RetainedSourceError {}

/// Successful import plus non-fatal recovery information.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct ImportOutcome {
    pub document: FlowDocument,
    pub diagnostics: Vec<Diagnostic>,
    pub retained_sources: Option<RetainedSourceBundle>,
}

/// Observable non-fatal importer loss or repair.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub kind: DiagnosticKind,
    pub origin: Option<OriginId>,
    pub message: String,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Note,
    Warning,
    Error,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    DuplicateAnchor,
    InvalidChild,
    SynthesizedWrapper,
    UnsupportedElement,
    UnsupportedMedia,
    UnsupportedDeclaration,
    UnsupportedFormattingContext,
    UnsupportedGeneratedContent,
    InvalidSpan,
    InvalidTableHeaderReference,
    InvalidDestination,
    InvalidLanguage,
    InvalidHeadingLevel,
    InvalidRubyStructure,
    UnsupportedAccessibilityMetadata,
    InvalidDimension,
    MissingImageAlternative,
    DroppedNonTextualMetadata,
}
