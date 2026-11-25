//! Host-fed, expanded MediaWiki/Parsoid HTML 2.8.0, lowered directly to flow IR.
//!
//! No extraction, wikitext expansion, CSS execution, or resource acquisition.
//! Cite and Math support is limited to the contracts exercised by the pinned
//! Parsoid fixture (see `tests/fixtures/mediawiki/README.md` in the repository).
//! Unknown extension children receive ordinary HTML semantics with diagnostics.
//!
//! Documents must declare version 2.8.0, or callers must supply it through
//! `expected_version`. Declarations must agree. Body fragments require that
//! option and are parsed explicitly in an HTML body context.
//!
//! Source URL and document identity are independent. Resource base precedence is
//! explicit caller base, valid document base resolved against the source URL,
//! then source URL. Without a usable base, relative references remain relative;
//! fragment-only links remain local even with a document base.
//!
//! Cite preserves call-site IDs, note-body roles, and multiple backlinks. Math
//! consumes `data-mw.body.extsrc` TeX once, retaining a single useful fallback
//! with a diagnostic when metadata is unusable. No extension HTML is reparsed.
//! Responsive choices are stored unevaluated with a deterministic fallback.
//! Script/style/template and HTML `hidden` subtrees are omitted. Stylesheets and
//! CSS visibility are not evaluated; this is not browser-equivalent presentation.
//! Empty table rows are omitted with diagnostics and spans normalized to retained
//! rows; tables without cells fall back to ordinary content. Inline SVG is a
//! diagnosed text fallback, not a preserved vector resource.
//! Parser construction budgets apply, not process-wide allocation or execution
//! quotas. HTML provenance has stable import-local IDs but no exact byte ranges.

use std::collections::HashMap;

use dom_query::{Document, NodeData, NodeRef};
use thiserror::Error;
use url::Url;

use super::html::{
    lowering::{self, LoweringOptions},
    parser::{self, HTML_NS, MAX_DEPTH, MAX_NODES},
    source::SourceSyntax,
    tree::{
        Attribute, Element, ExpandedName, NodeProvenance, SourceNodeId, SourceNodeKind, SourceTree,
        TreeLimits,
    },
};
use crate::model::flow::*;

mod adapt;
mod metadata;
mod policy;
#[cfg(all(test, feature = "render"))]
mod render_tests;
mod responsive;
mod structure;
#[cfg(test)]
mod tests;

/// Explicitly supported Parsoid DOM version. Versions are not floating point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaWikiHtmlVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}
impl MediaWikiHtmlVersion {
    pub const V2_8_0: Self = Self {
        major: 2,
        minor: 8,
        patch: 0,
    };
}
impl std::str::FromStr for MediaWikiHtmlVersion {
    type Err = MediaWikiImportError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts = value
            .split('.')
            .map(|part| {
                if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(MediaWikiImportError::InvalidVersion(value.into()));
                }
                part.parse::<u32>()
                    .map_err(|_| MediaWikiImportError::InvalidVersion(value.into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        match parts.as_slice() {
            &[major, minor, patch] => Ok(Self {
                major,
                minor,
                patch,
            }),
            _ => Err(MediaWikiImportError::InvalidVersion(value.into())),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MediaWikiInput {
    #[default]
    Document,
    BodyFragment,
}

#[derive(Clone, Debug, Default)]
pub struct MediaWikiOptions {
    pub source_url: Option<String>,
    pub resource_base: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
    pub input: MediaWikiInput,
    pub expected_version: Option<MediaWikiHtmlVersion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaWikiReference {
    pub raw: String,
    pub resolved: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaWikiCategory {
    pub target: MediaWikiReference,
    /// Encoded sort key, without the fragment delimiter. No title normalization.
    pub sort_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MediaWikiMetadata {
    pub declared_version: Option<MediaWikiHtmlVersion>,
    pub effective_version: MediaWikiHtmlVersion,
    pub database_title: Option<String>,
    pub categories: Vec<MediaWikiCategory>,
    pub language_links: Vec<MediaWikiReference>,
    pub redirect: Option<MediaWikiReference>,
}

#[derive(Debug)]
pub struct MediaWikiImportOutcome {
    pub content: ImportOutcome,
    pub metadata: MediaWikiMetadata,
}

#[derive(Debug, Error)]
pub enum MediaWikiImportError {
    #[error("MediaWiki DOM version is missing")]
    MissingVersion,
    #[error("invalid MediaWiki DOM version: {0}")]
    InvalidVersion(String),
    #[error("unsupported MediaWiki DOM version: {0:?}")]
    UnsupportedVersion(MediaWikiHtmlVersion),
    #[error("conflicting MediaWiki DOM versions")]
    ConflictingVersions,
    #[error("body fragments require an expected DOM version")]
    FragmentVersionRequired,
    #[error("MediaWiki input exceeds a construction limit")]
    TooComplex,
    #[error("source URL and explicit resource base must be absolute HTTP(S) URLs")]
    InvalidUrl,
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    RetainedSource(#[from] RetainedSourceError),
}

/// Import one already-expanded article. The host supplies UTF-8 and owns fetching.
pub fn import(
    source: &str,
    options: MediaWikiOptions,
) -> Result<MediaWikiImportOutcome, MediaWikiImportError> {
    if options.input == MediaWikiInput::BodyFragment && options.expected_version.is_none() {
        return Err(MediaWikiImportError::FragmentVersionRequired);
    }
    let document = match options.input {
        MediaWikiInput::Document => parser::parse_bounded(source),
        MediaWikiInput::BodyFragment => parser::parse_body_fragment(source),
    }
    .map_err(|_| MediaWikiImportError::TooComplex)?;
    let source_url = options
        .source_url
        .as_deref()
        .map(absolute_url)
        .transpose()?;
    let base = metadata::effective_base(&document, &options, source_url.as_ref())?;
    let metadata = metadata::extract(&document, &options, base.as_ref())?;
    let root = match options.input {
        MediaWikiInput::Document => document.select("body").nodes().first().copied(),
        MediaWikiInput::BodyFragment => document
            .root()
            .children()
            .into_iter()
            .find(|node| node.has_name("html")),
    }
    .unwrap_or_else(|| document.root());
    let mut adapter = Adapter {
        tree: SourceTree::new(
            None,
            TreeLimits {
                max_nodes: MAX_NODES,
                max_depth: MAX_DEPTH + 2,
                ..Default::default()
            },
        )
        .map_err(|_| MediaWikiImportError::TooComplex)?,
        policy: policy::Policy::default(),
        diagnostics: Vec::new(),
        numbering: 0,
        base: base.as_ref(),
        source_url: source_url.as_ref(),
    };
    let content_root = adapter.tree.root();
    adapter.adapt(root, content_root)?;
    adapter.tree = structure::normalize(adapter.tree, &adapter.policy)?;
    let styles = vec![AuthorStyle::default(); adapter.tree.nodes().len()];
    let mut content = lowering::lower_tree(
        source,
        adapter.tree,
        &styles,
        LoweringOptions {
            resource_base: base.as_ref().map(ToString::to_string),
            identity: options.identity,
            retention: options.retention,
            syntax: SourceSyntax::Html,
        },
        content_root,
        &adapter.policy,
    )
    .map_err(|error| match error {
        lowering::LoweringError::Build(error) => MediaWikiImportError::Build(error),
        lowering::LoweringError::RetainedSource(error) => {
            MediaWikiImportError::RetainedSource(error)
        }
    })?;
    content.diagnostics.extend(adapter.diagnostics);
    for id in content.document.preorder(content.document.root()) {
        let node = content.document.get_node(id).unwrap();
        if let NodeKind::Link(link) = &node.kind
            && !matches!(link.relation, LinkRelation::Ordinary)
        {
            let message = match &link.destination {
                Destination::Local(target) if content.document.anchor(target).is_none() => Some(
                    format!("note link target '{}' is unavailable", target.as_str()),
                ),
                Destination::Local(_) => None,
                _ => Some(
                    "note relationship could not be associated with a local anchor; link retained"
                        .into(),
                ),
            };
            if let Some(message) = message {
                content.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: DiagnosticKind::InvalidDestination,
                    origin: node.origin,
                    message,
                });
            }
        }
    }
    Ok(MediaWikiImportOutcome { content, metadata })
}

fn absolute_url(value: &str) -> Result<Url, MediaWikiImportError> {
    Url::parse(value)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .ok_or(MediaWikiImportError::InvalidUrl)
}

fn reference(value: &str, base: Option<&Url>) -> String {
    base.and_then(|base| base.join(value).ok())
        .or_else(|| Url::parse(value).ok())
        .map_or_else(|| value.to_owned(), String::from)
}

/// Resolve the MediaWiki RDFa vocabulary in the nearest declared prefix context.
/// An undeclared `mw` prefix uses the standard Parsoid vocabulary; explicit
/// rebinding wins, and aliases for that vocabulary work as well.
fn vocabulary(node: NodeRef<'_>, attribute: &str, term: &str) -> bool {
    node.attr(attribute).is_some_and(|value| {
        value.split_ascii_whitespace().any(|token| {
            if token == format!("http://mediawiki.org/rdf/{term}") {
                return true;
            }
            let Some((prefix, local)) = token.split_once(':') else {
                return false;
            };
            if local != term {
                return false;
            }
            for context in std::iter::once(node).chain(node.ancestors(None)) {
                if let Some(declarations) = context.attr("prefix") {
                    let pairs: Vec<_> = declarations.split_ascii_whitespace().collect();
                    for pair in pairs.chunks_exact(2) {
                        if pair[0].strip_suffix(':') == Some(prefix) {
                            return pair[1] == "http://mediawiki.org/rdf/";
                        }
                    }
                }
            }
            prefix == "mw"
        })
    })
}

fn html_node(node: NodeRef<'_>) -> bool {
    node.query(
        |inner| matches!(&inner.data, NodeData::Element(element) if &*element.name.ns == HTML_NS),
    )
    .unwrap_or(false)
}

fn extension(node: NodeRef<'_>) -> bool {
    node.attr("typeof").is_some_and(|value| {
        value.split_ascii_whitespace().any(|token| {
            let term = token
                .strip_prefix("http://mediawiki.org/rdf/")
                .or_else(|| token.split_once(':').map(|(_, local)| local));
            term.is_some_and(|term| {
                term.starts_with("Extension/") && vocabulary(node, "typeof", term)
            })
        })
    })
}

fn class(node: NodeRef<'_>, token: &str) -> bool {
    node.attr("class")
        .is_some_and(|value| value.split_ascii_whitespace().any(|value| value == token))
}

fn attribute(name: &str, value: String) -> Attribute {
    Attribute::new(
        ExpandedName::new(name.into(), name.into(), None, None),
        value,
        None,
    )
}

fn element_name(local: &str) -> ExpandedName {
    ExpandedName::new(local.into(), local.into(), None, Some(HTML_NS.into()))
}

fn identity(node: NodeRef<'_>) -> String {
    format!("dom-{:?}", node.id)
}

struct Adapter<'a> {
    tree: SourceTree,
    policy: policy::Policy,
    diagnostics: Vec<Diagnostic>,
    numbering: usize,
    base: Option<&'a Url>,
    source_url: Option<&'a Url>,
}
