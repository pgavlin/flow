//! Render-independent evidence for positions in canonical logical text.

use crate::model::flow::FlowDocument;

/// Durable evidence, not a promise that an offset survives content changes.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableTextPosition {
    pub text_offset: usize,
    pub context_before: String,
    pub context_after: String,
}

impl StableTextPosition {
    pub fn capture(text: &str, offset: usize) -> Self {
        let offset = floor_char_boundary(text, offset.min(text.len()));
        let before = text[..offset]
            .char_indices()
            .rev()
            .nth(31)
            .map_or(0, |(i, _)| i);
        let after = text[offset..]
            .char_indices()
            .nth(32)
            .map_or(text.len(), |(i, _)| offset + i);
        Self {
            text_offset: offset,
            context_before: text[before..offset].into(),
            context_after: text[offset..after].into(),
        }
    }

    /// Exact offsets require verified text identity. Changed text requires
    /// nonempty context; an empty quote must never match every byte position.
    pub fn recover_offset(&self, text: &str, same_text: bool) -> Option<usize> {
        if same_text {
            return (self.text_offset <= text.len() && text.is_char_boundary(self.text_offset))
                .then_some(self.text_offset);
        }
        if self.context_before.is_empty() && self.context_after.is_empty() {
            return text.is_empty().then_some(0);
        }
        let matches = |offset: usize| {
            text[..offset].ends_with(&self.context_before)
                && text[offset..].starts_with(&self.context_after)
        };
        text.char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .filter(|offset| matches(*offset))
            .min_by_key(|offset| offset.abs_diff(self.text_offset))
    }

    pub fn is_bounded(&self) -> bool {
        self.context_before.len() <= 1024 && self.context_after.len() <= 1024
    }
}

/// Computed from canonical text, independent of source bytes/importer version.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextFingerprint([u8; 32]);

impl TextFingerprint {
    pub fn of_document(document: &FlowDocument) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"flow-canonical-text-v1\0");
        hash.update(document.text().as_str().as_bytes());
        Self(*hash.finalize().as_bytes())
    }
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_recovers_changed_text_without_trusting_offsets() {
        let position = StableTextPosition::capture("before café after", 12);
        let changed = "new before café after";
        assert_eq!(position.recover_offset(changed, false), Some(16));
        assert_eq!(
            StableTextPosition::capture("", 0).recover_offset("different", false),
            None
        );
        let invalid = StableTextPosition {
            text_offset: 1,
            context_before: String::new(),
            context_after: String::new(),
        };
        assert_eq!(invalid.recover_offset("é", true), None);
    }
}
