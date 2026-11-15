//! Shared document-facing text geometry and the authoritative flow IR model.
//!
//! Input formats lower directly into [`flow::FlowDocument`]. Viewer-facing byte
//! positions and ranges address that document's canonical logical text.

pub mod flow;

/// A UTF-8 byte offset in a document's canonical logical text.
pub type TextOffset = usize;

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TextRange {
    pub start: TextOffset,
    pub end: TextOffset,
}

impl TextRange {
    pub fn contains(self, offset: TextOffset) -> bool {
        self.start <= offset && offset < self.end
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Affinity {
    Upstream,
    Downstream,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextPosition {
    pub offset: TextOffset,
    pub affinity: Affinity,
}

impl TextPosition {
    pub const fn new(offset: TextOffset) -> Self {
        Self {
            offset,
            affinity: Affinity::Downstream,
        }
    }
}
