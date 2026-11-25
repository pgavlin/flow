//! Source artifact metadata is separate from syntax and semantic policy.

use crate::model::flow::{Origin, SourceArtifactId, SourceNodeIdentity};

use super::tree::{SourceNode, SourceNodeId};

/// Selects provenance spelling, not parsing or semantic conventions.
#[derive(Clone, Copy)]
pub(in crate::formats) enum SourceSyntax {
    Xhtml,
    #[allow(dead_code, reason = "HTML adapters use this source context")]
    Html,
}

impl SourceSyntax {
    pub(in crate::formats) fn media_type(self) -> &'static str {
        match self {
            Self::Xhtml => "application/xhtml+xml",
            Self::Html => "text/html",
        }
    }

    pub(in crate::formats) fn origin(
        self,
        source: SourceArtifactId,
        id: SourceNodeId,
        node: &SourceNode,
        identity: Option<&SourceNodeIdentity>,
    ) -> Origin {
        let prefix = match self {
            Self::Xhtml => "xhtml",
            Self::Html => "html",
        };
        let source_node = identity
            .cloned()
            .unwrap_or_else(|| SourceNodeIdentity(format!("{prefix}:{}", id.index())));
        let serialized_range = node.serialized_range();
        match self {
            Self::Xhtml => Origin::Xhtml {
                source,
                source_node,
                serialized_range,
            },
            Self::Html => Origin::Html {
                source,
                source_node,
                serialized_range,
            },
        }
    }
}
