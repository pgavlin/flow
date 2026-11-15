/// Canonical characters with structural meaning.
pub mod canonical_text {
    /// Between adjacent contributing flow children.
    pub const BLOCK_SEPARATOR: &str = "\n\n";
    /// Between explicit cells in a table row.
    pub const CELL_SEPARATOR: char = '\t';
    /// Between adjacent table rows and row groups.
    pub const ROW_SEPARATOR: char = '\n';
    /// Atomic object with no useful textual alternative.
    pub const OBJECT_REPLACEMENT: char = '\u{fffc}';
}

/// A UTF-8 byte offset in a flow document's canonical logical text.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct TextOffset(usize);

impl TextOffset {
    /// The beginning of canonical logical text.
    pub const ZERO: Self = Self(0);

    /// Wraps an in-memory canonical-text byte offset.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the byte offset used to index resident UTF-8 storage.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// A half-open UTF-8 byte range in canonical logical text.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TextRange {
    /// Inclusive starting byte offset.
    pub start: TextOffset,
    /// Exclusive ending byte offset.
    pub end: TextOffset,
}

impl TextRange {
    /// Creates a half-open range without asserting document bounds.
    pub const fn new(start: TextOffset, end: TextOffset) -> Self {
        Self { start, end }
    }

    /// Returns whether this range contains the offset.
    pub const fn contains(self, offset: TextOffset) -> bool {
        self.start.0 <= offset.0 && offset.0 < self.end.0
    }

    /// Returns whether the range contains no bytes.
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }
}

/// Chooses one of the two semantic positions available at a text boundary.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Affinity {
    /// Associate the boundary with preceding content in logical order.
    Upstream,
    /// Associate the boundary with following content in logical order.
    Downstream,
}

/// A canonical text offset together with its boundary affinity.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextPosition {
    /// UTF-8 byte offset in canonical logical text.
    pub offset: TextOffset,
    /// Side chosen at a semantic or layout boundary.
    pub affinity: Affinity,
}

impl TextPosition {
    /// Creates a text position.
    pub const fn new(offset: TextOffset, affinity: Affinity) -> Self {
        Self { offset, affinity }
    }
}

/// Immutable contiguous canonical UTF-8 text for one flow document.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogicalText {
    utf8: Box<str>,
}

impl LogicalText {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_boxed_str(utf8: Box<str>) -> Self {
        Self { utf8 }
    }

    /// Returns the canonical text as one UTF-8 string.
    pub fn as_str(&self) -> &str {
        &self.utf8
    }

    /// Returns the canonical text length in bytes.
    pub fn len(&self) -> TextOffset {
        TextOffset::new(self.utf8.len())
    }

    /// Returns whether the document has no canonical text.
    pub fn is_empty(&self) -> bool {
        self.utf8.is_empty()
    }

    /// Returns a slice when the range is ordered, in bounds, and on UTF-8 boundaries.
    pub fn get(&self, range: TextRange) -> Option<&str> {
        self.utf8.get(range.start.as_usize()..range.end.as_usize())
    }
}
