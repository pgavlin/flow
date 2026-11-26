//! flow semantic XHTML import.
//!
//! This module owns the flow vocabulary's versioning and source conventions.
//! It accepts strict XML fragments without tree repair or author CSS.

use thiserror::Error;

use super::html::{lowering, source::SourceSyntax};

use super::xhtml::{
    XhtmlDiagnostic, XhtmlElement, XhtmlPrepareError, XhtmlProfileError, XhtmlSourceTree,
    XhtmlTreeProfile,
};
use crate::model::flow::{
    AuthorStyle, BuildError, DocumentReference, ImportOutcome, MathDisplay, PreformattedKind,
    RetainedSourceError, SourceRetention,
};

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

/// Version used for unversioned flow semantic XHTML input.
pub const LEGACY_FLOW_SEMANTIC_XHTML_VERSION: u32 = 0;
/// Current flow semantic XHTML contract version maintained by flow.
pub const CURRENT_FLOW_SEMANTIC_XHTML_VERSION: u32 = 1;

#[derive(Clone, Debug, Default)]
pub struct XhtmlOptions {
    pub limits: super::xhtml::XhtmlLimits,
    /// Maximum table width and normalized column span: None uses 4,096; zero is treated as one.
    pub max_table_columns: Option<u32>,
    pub resource_base: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
    /// flow semantic XHTML contract version. Unversioned input is version zero.
    pub canonical_version: u32,
}

#[derive(Debug, Error)]
pub enum XhtmlImportError {
    #[error("unsupported flow semantic XHTML version {0}")]
    UnsupportedVersion(u32),
    #[error(transparent)]
    Prepare(#[from] XhtmlPrepareError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    RetainedSource(#[from] RetainedSourceError),
}

impl XhtmlImportError {
    fn from_lowering(error: lowering::LoweringError) -> Self {
        match error {
            lowering::LoweringError::Build(error) => Self::Build(error),
            lowering::LoweringError::RetainedSource(error) => Self::RetainedSource(error),
        }
    }
}

/// Strict flow semantic XHTML profile. Input needs no tree repair.
#[derive(Clone, Copy, Debug, Default)]
pub struct StrictFlowProfile;

impl XhtmlTreeProfile for StrictFlowProfile {
    fn apply(
        &self,
        _tree: &mut XhtmlSourceTree,
        _diagnostics: &mut Vec<XhtmlDiagnostic>,
    ) -> Result<(), XhtmlProfileError> {
        Ok(())
    }
}

impl lowering::LoweringPolicy for StrictFlowProfile {
    fn math_metadata(&self, element: &XhtmlElement) -> Option<(String, MathDisplay)> {
        let tex = element.attribute(None, "data-tex")?.to_owned();
        let display = if element.attribute(None, "class").is_some_and(|class| {
            class
                .split_ascii_whitespace()
                .any(|token| token == "katex-display")
        }) {
            MathDisplay::Block
        } else {
            MathDisplay::Inline
        };
        Some((tex, display))
    }

    fn element_name<'a>(&self, element: &'a XhtmlElement) -> Option<&'a str> {
        // Preserve the existing XML import contract, including foreign wrappers.
        Some(element.name().local())
    }

    fn code_metadata(&self, element: &XhtmlElement) -> crate::model::flow::Preformatted {
        code_metadata(element)
    }
}

/// Source-preserving code conventions explicitly selected by both XML adapters.
pub(super) fn code_metadata(element: &XhtmlElement) -> crate::model::flow::Preformatted {
    let language = element
        .attribute(None, "class")
        .and_then(|class| {
            class
                .split_ascii_whitespace()
                .find_map(|token| token.strip_prefix("language-"))
        })
        .filter(|language| !language.is_empty());
    crate::model::flow::Preformatted {
        language: language.map(str::to_owned),
        content_kind: if language == Some("mermaid") {
            PreformattedKind::MermaidSource
        } else {
            PreformattedKind::SourceCode
        },
    }
}

/// Imports a flow semantic XHTML fragment at a supported contract version.
pub fn import(source: &str, options: XhtmlOptions) -> Result<ImportOutcome, XhtmlImportError> {
    if options.canonical_version > CURRENT_FLOW_SEMANTIC_XHTML_VERSION {
        return Err(XhtmlImportError::UnsupportedVersion(
            options.canonical_version,
        ));
    }
    let prepared = super::xhtml::prepare_with_options(
        source.as_bytes(),
        super::xhtml::XhtmlParseOptions {
            limits: options.limits,
            ..Default::default()
        },
        &StrictFlowProfile,
    )?;
    let content_root = prepared.tree.root();
    let author_styles = vec![AuthorStyle::default(); prepared.tree.nodes().len()];
    lowering::lower_tree(
        source,
        prepared.tree,
        &author_styles,
        lowering::LoweringOptions {
            max_table_columns: options.max_table_columns,
            resource_base: options.resource_base,
            identity: options.identity,
            retention: options.retention,
            syntax: SourceSyntax::Xhtml,
        },
        content_root,
        &StrictFlowProfile,
    )
    .map_err(XhtmlImportError::from_lowering)
}
