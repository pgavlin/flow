//! Importers that lower source formats into flow's semantic document model.
//! Hosts acquire source bytes before importing.

use std::path::Path;

use thiserror::Error;

use crate::model::flow::{DocumentReference, ImportOutcome, SourceRetention};

pub mod epub_xhtml;
pub mod flow_xhtml;
mod html;
#[path = "markdown_flow.rs"]
pub mod markdown;
pub mod xhtml;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceFormat {
    Markdown,
    Xhtml,
}

impl SourceFormat {
    /// Selects a source format by extension, defaulting to Markdown.
    pub fn for_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("xhtml" | "html" | "htm") => Self::Xhtml,
            _ => Self::Markdown,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SourceOptions {
    pub resource_base: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Markdown(#[from] markdown::MarkdownError),
    #[error(transparent)]
    Xhtml(#[from] flow_xhtml::XhtmlImportError),
    #[error(transparent)]
    EpubXhtml(#[from] epub_xhtml::XhtmlImportError),
}

/// Imports a source string through the selected format adapter.
pub fn import_source(
    source: &str,
    format: SourceFormat,
    options: SourceOptions,
) -> Result<ImportOutcome, SourceError> {
    match format {
        SourceFormat::Markdown => markdown::parse(
            source,
            markdown::MarkdownOptions {
                resource_base: options.resource_base,
                identity: options.identity,
                retention: options.retention,
            },
        )
        .map_err(SourceError::from),
        SourceFormat::Xhtml => flow_xhtml::import(
            source,
            flow_xhtml::XhtmlOptions {
                resource_base: options.resource_base,
                identity: options.identity,
                retention: options.retention,
                canonical_version: flow_xhtml::LEGACY_FLOW_SEMANTIC_XHTML_VERSION,
            },
        )
        .map_err(SourceError::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_dispatch_defaults_to_markdown() {
        assert_eq!(
            SourceFormat::for_path(Path::new("article.xhtml")),
            SourceFormat::Xhtml
        );
        assert_eq!(
            SourceFormat::for_path(Path::new("ARTICLE.HTML")),
            SourceFormat::Xhtml
        );
        assert_eq!(
            SourceFormat::for_path(Path::new("article.md")),
            SourceFormat::Markdown
        );
        assert_eq!(
            SourceFormat::for_path(Path::new("README")),
            SourceFormat::Markdown
        );
    }
}
