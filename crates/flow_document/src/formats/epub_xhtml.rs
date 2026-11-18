//! EPUB spine XHTML import: complete-document validation, author stylesheets,
//! and semantic import. Publication containers and resource retrieval belong to
//! the host or publication layer.

use thiserror::Error;

use super::xhtml::{
    SourceNodeId, XhtmlDiagnostic, XhtmlElement, XhtmlParseOptions, XhtmlPrepareError,
    XhtmlProfileError, XhtmlSourceNode, XhtmlSourceNodeKind, XhtmlSourceTree, XhtmlTreeProfile,
    lowering, prepare_with_options,
};
use crate::model::flow::{
    BuildError, DocumentReference, ImportOutcome, MathDisplay, PreformattedKind,
    RetainedSourceError, SourceRetention,
};

mod css;
#[cfg(test)]
mod tests;

/// Inputs for one EPUB spine XHTML document. Stylesheets are supplied in
/// document order after publication-relative resource resolution by the host.
#[derive(Clone, Debug, Default)]
pub struct EpubXhtmlOptions {
    pub resource_base: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
    pub stylesheets: Vec<String>,
}

#[derive(Debug, Error)]
pub enum XhtmlImportError {
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

/// EPUB spine XHTML profile. EPUB content is still parsed as XML; the profile
/// only accepts complete XHTML documents and does not provide HTML recovery.
#[derive(Clone, Copy, Debug, Default)]
pub struct EpubXhtmlProfile;

const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";

impl XhtmlTreeProfile for EpubXhtmlProfile {
    fn apply(
        &self,
        tree: &mut XhtmlSourceTree,
        _diagnostics: &mut Vec<XhtmlDiagnostic>,
    ) -> Result<(), XhtmlProfileError> {
        body(tree).map(|_| ())
    }
}

fn body(tree: &XhtmlSourceTree) -> Result<SourceNodeId, XhtmlProfileError> {
    let elements = tree
        .get(tree.root())
        .expect("source trees retain their root")
        .children()
        .iter()
        .copied()
        .filter(|child| {
            matches!(
                tree.get(*child).map(XhtmlSourceNode::kind),
                Some(XhtmlSourceNodeKind::Element(_))
            )
        })
        .collect::<Vec<_>>();
    if elements.len() != 1 {
        return Err(XhtmlProfileError {
            message: "EPUB spine XHTML must contain exactly one document element".into(),
        });
    }
    let XhtmlSourceNodeKind::Element(html) = tree.get(elements[0]).unwrap().kind() else {
        unreachable!()
    };
    if html.name().local() != "html" || html.name().namespace() != Some(XHTML_NAMESPACE) {
        return Err(XhtmlProfileError {
            message: "EPUB spine document element must be XHTML html".into(),
        });
    }
    tree.get(elements[0])
        .unwrap()
        .children()
        .iter()
        .copied()
        .find(|child| {
            matches!(tree.get(*child).map(XhtmlSourceNode::kind),
            Some(XhtmlSourceNodeKind::Element(element))
                if element.name().local() == "body"
                    && element.name().namespace() == Some(XHTML_NAMESPACE))
        })
        .ok_or_else(|| XhtmlProfileError {
            message: "EPUB spine XHTML is missing its body element".into(),
        })
}

// EPUB explicitly supports these source-preserving extensions as well.
// They are adapter policy, not assumptions made by common XHTML lowering.
impl lowering::LoweringPolicy for EpubXhtmlProfile {
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

    fn code_kind(&self, language: Option<&str>) -> PreformattedKind {
        if language == Some("mermaid") {
            PreformattedKind::MermaidSource
        } else {
            PreformattedKind::SourceCode
        }
    }
}

/// Imports one complete document. Tracing spans cover parsing, styling, and lowering.
pub fn import(source: &str, options: EpubXhtmlOptions) -> Result<ImportOutcome, XhtmlImportError> {
    let _span = crate::telemetry::phase_span!(
        "flow.epub.import",
        source_bytes = source.len() as u64,
        stylesheets = options.stylesheets.len() as u64,
    );
    let prepared = {
        let _span =
            crate::telemetry::phase_span!("flow.xhtml.parse", source_bytes = source.len() as u64,);
        prepare_with_options(
            source.as_bytes(),
            XhtmlParseOptions {
                allow_doctype: true,
            },
            &EpubXhtmlProfile,
        )?
    };
    let content_root = body(&prepared.tree).map_err(XhtmlPrepareError::Profile)?;
    let author_styles = {
        let _span = crate::telemetry::phase_span!(
            "flow.css.style",
            stylesheets = options.stylesheets.len() as u64,
        );
        css::cascade(&prepared.tree, &options.stylesheets)
    };
    let outcome = {
        let _span = crate::telemetry::phase_span!("flow.flow.lower", format = "xhtml");
        lowering::lower_prepared(
            source,
            prepared,
            &author_styles,
            lowering::LoweringOptions {
                resource_base: options.resource_base,
                identity: options.identity,
                retention: options.retention,
            },
            content_root,
            &EpubXhtmlProfile,
        )
        .map_err(XhtmlImportError::from_lowering)?
    };
    crate::telemetry::phase_event!(
        "flow.phase.complete",
        canonical_bytes = outcome.document.text().len().as_usize() as u64,
    );
    Ok(outcome)
}

/// Returns linked author stylesheets in XHTML document order. Resolution and
/// retrieval remain the publication host's responsibility.
pub fn stylesheet_hrefs(source: &str) -> Result<Vec<String>, XhtmlPrepareError> {
    let prepared = prepare_with_options(
        source.as_bytes(),
        XhtmlParseOptions {
            allow_doctype: true,
        },
        &EpubXhtmlProfile,
    )?;
    Ok(prepared
        .tree
        .nodes()
        .filter_map(|(_, node)| {
            let XhtmlSourceNodeKind::Element(element) = node.kind() else {
                return None;
            };
            (element.name().local() == "link"
                && element.attribute(None, "rel").is_some_and(|rel| {
                    rel.split_ascii_whitespace()
                        .any(|token| token.eq_ignore_ascii_case("stylesheet"))
                }))
            .then(|| element.attribute(None, "href").map(str::to_owned))
            .flatten()
        })
        .collect())
}
