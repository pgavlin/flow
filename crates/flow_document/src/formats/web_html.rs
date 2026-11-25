//! Experimental host-fed web article extraction. No networking or flow XHTML
//! intermediate: HTML5 DOM -> extraction/normalization -> shared lowering -> IR.
//!
//! A budgeted HTML5 sink checks DOM allocations, logical depth, attributes,
//! accounted storage, and construction work before mutation. Tokenizer feeds and
//! validation visits are bounded too. These are not process-wide allocator or
//! interruptible extraction-time quotas.

use dom_smoothie::{
    Config, Readability,
    dom_query::{Document, NodeData, NodeRef},
};
use thiserror::Error;
use url::Url;

mod normalize;
mod notes;
mod parser;
mod responsive;
mod visibility;
#[cfg(test)]
use normalize::first_srcset_candidate;
use normalize::{
    normalize_article, normalize_flow_containment, normalize_sequences, repair_before_extraction,
};
use notes::{capture_referenced_notes, restore_referenced_notes};
use parser::parse_bounded;
use visibility::is_explicitly_hidden;

use super::html::{
    lowering::{self, LoweringOptions, LoweringPolicy},
    source::SourceSyntax,
    tree::{
        Attribute, Element, ExpandedName, NodeProvenance, SourceNodeKind, SourceTree, TreeError,
        TreeLimits,
    },
};
use crate::model::flow::{
    AnchorId, AuthorStyle, BaseDirection, BidiMode, BidiState, BreakOpportunity, BuildError,
    Checkbox, CheckboxState, Diagnostic, DiagnosticKind, DiagnosticSeverity, DocumentReference,
    HeaderScope, ImportOutcome, InlineRole, InlineSemantic, KnownRole, LinkContent, LinkRelation,
    MathDisplay, MathNotation, NodeKind, Preformatted, PreformattedKind, RetainedSourceError,
    SemanticRole, SourceNodeIdentity, SourceRetention,
};

const MAX_SOURCE: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 128;
const PARSE_BATCH_BYTES: usize = 4096;
const MAX_PARSE_VISITS: usize = 8_000_000;
const MAX_DOM_BYTES: usize = 8 * MAX_SOURCE;
const MAX_ATTRIBUTES: usize = 4096;
const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
const SOURCE_NODE_ATTRIBUTE: &str = "data-import-source-node";

#[derive(Clone, Debug, Default)]
pub struct WebOptions {
    /// Final response URL after redirects, supplied by the host.
    pub source_url: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
}

#[derive(Clone, Debug, Default)]
pub struct WebMetadata {
    pub title: String,
    pub byline: Option<String>,
    pub site_name: Option<String>,
    pub lang: Option<String>,
    pub excerpt: Option<String>,
    pub dir: Option<String>,
    pub published_time: Option<String>,
    pub modified_time: Option<String>,
    /// Declared canonical/article URL, not a resource-acquisition instruction.
    pub url: Option<String>,
    pub image: Option<String>,
    pub favicon: Option<String>,
}

#[derive(Debug)]
pub struct WebImportOutcome {
    pub content: ImportOutcome,
    pub metadata: WebMetadata,
}

#[derive(Debug, Error)]
pub enum WebImportError {
    #[error("web input exceeds a size, node, depth, or work limit")]
    TooComplex,
    #[error("web source URL must be an absolute HTTP(S) URL")]
    InvalidUrl,
    #[error("article extraction failed: {0}")]
    Extraction(String),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    RetainedSource(#[from] RetainedSourceError),
}

struct WebPolicy {
    responsive: responsive::ResponsiveImages,
}
impl LoweringPolicy for WebPolicy {
    fn responsive_image(
        &self,
        element: &Element,
        builder: &mut crate::model::flow::FlowBuilder,
    ) -> Result<Option<crate::model::flow::ResponsiveImage>, BuildError> {
        self.responsive.lower(element, builder)
    }

    fn preformatted_all_children(&self) -> bool {
        true
    }

    fn media_flow_fallback(&self) -> bool {
        true
    }

    fn preserve_figure_flow(&self) -> bool {
        true
    }

    fn flow_container(&self, element: &Element) -> Option<NodeKind> {
        if element.name().local() == "section" {
            Some(NodeKind::Section)
        } else if matches!(element.name().local(), "li" | "dt" | "dd")
            && !self.roles(element).is_empty()
        {
            // A restored note can stand outside its original list, while its
            // body still needs flow children and its explicit note role.
            Some(NodeKind::Flow)
        } else {
            None
        }
    }

    fn inline_semantic(&self, element: &Element) -> Option<InlineSemantic> {
        let role = match element.name().local() {
            "del" => InlineRole::Deletion,
            "ins" => InlineRole::Insertion,
            "kbd" => InlineRole::KeyboardInput,
            "samp" => InlineRole::SampleOutput,
            "var" => InlineRole::Variable,
            "dfn" => InlineRole::Definition,
            "small" => InlineRole::Small,
            "abbr" => InlineRole::Abbreviation,
            "time" => InlineRole::Time,
            _ => return None,
        };
        let value = match role {
            InlineRole::Abbreviation => element.attribute(None, "title"),
            InlineRole::Time => element.attribute(None, "datetime"),
            _ => None,
        }
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
        Some(InlineSemantic { role, value })
    }

    fn atomic_spacing_metadata(&self) -> bool {
        false
    }

    fn inline_atom(&self, element: &Element) -> Option<NodeKind> {
        if element.name().local() == "wbr" {
            return Some(NodeKind::BreakOpportunity(BreakOpportunity {
                hyphen_when_broken: false,
            }));
        }
        let state = match element.attribute(None, "data-import-checkbox")? {
            "checked" => CheckboxState::Checked,
            "unchecked" => CheckboxState::Unchecked,
            _ => return None,
        };
        Some(NodeKind::Checkbox(Checkbox { state }))
    }

    fn bidi(&self, element: &Element) -> Option<BidiState> {
        let direction = match element
            .attribute(None, "dir")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("ltr") => Some(BaseDirection::LeftToRight),
            Some("rtl") => Some(BaseDirection::RightToLeft),
            _ => None,
        };
        match element.name().local() {
            "bdi" => Some(BidiState {
                direction: direction.unwrap_or(BaseDirection::Auto),
                mode: BidiMode::Isolate,
            }),
            "bdo" => direction.map(|direction| BidiState {
                direction,
                mode: BidiMode::Override,
            }),
            _ => None,
        }
    }

    fn ruby(&self, element: &Element) -> bool {
        element.name().local() == "ruby"
    }

    fn link_content(&self, _element: &Element, has_flow_children: bool) -> LinkContent {
        if has_flow_children {
            LinkContent::Flow
        } else {
            LinkContent::Phrasing
        }
    }

    fn link_relation(&self, element: &Element) -> LinkRelation {
        let roles = element.attribute(None, "role").unwrap_or_default();
        if roles
            .split_ascii_whitespace()
            .any(|role| role == "doc-noteref")
            || element
                .attribute(None, "rel")
                .is_some_and(|rel| rel.split_ascii_whitespace().any(|rel| rel == "footnote"))
        {
            LinkRelation::NoteReference {
                return_anchor: element.attribute(None, "id").map(AnchorId::new),
            }
        } else if roles
            .split_ascii_whitespace()
            .any(|role| role == "doc-backlink")
        {
            LinkRelation::NoteReturn
        } else {
            LinkRelation::Ordinary
        }
    }

    fn roles(&self, element: &Element) -> Vec<SemanticRole> {
        let mut roles = Vec::new();
        for token in element
            .attribute(None, "role")
            .unwrap_or_default()
            .split_ascii_whitespace()
        {
            let role = match token {
                "doc-footnote" => KnownRole::Footnote,
                "doc-endnote" => KnownRole::Endnote,
                _ => continue,
            };
            let role = SemanticRole::Known(role);
            if !roles.contains(&role) {
                roles.push(role);
            }
        }
        roles
    }

    fn zero_row_span_to_end(&self) -> bool {
        true
    }

    fn cell_scope(&self, element: &Element) -> Option<HeaderScope> {
        match element
            .attribute(None, "scope")?
            .to_ascii_lowercase()
            .as_str()
        {
            "row" => Some(HeaderScope::Row),
            "col" => Some(HeaderScope::Column),
            "rowgroup" => Some(HeaderScope::RowGroup),
            "colgroup" => Some(HeaderScope::ColumnGroup),
            _ => None,
        }
    }

    fn cell_headers(&self, element: &Element) -> Option<Vec<String>> {
        element.attribute(None, "headers").map(|headers| {
            headers
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect()
        })
    }

    fn title(&self, element: &Element) -> Option<String> {
        element
            .attribute(None, "title")
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
    }

    fn code_metadata(&self, element: &Element) -> Preformatted {
        let language = element
            .attribute(None, "class")
            .and_then(|class| {
                class
                    .split_ascii_whitespace()
                    .find_map(|token| token.strip_prefix("language-"))
            })
            .filter(|value| !value.is_empty());
        Preformatted {
            language: language.map(str::to_owned),
            content_kind: if language == Some("mermaid") {
                PreformattedKind::MermaidSource
            } else {
                PreformattedKind::SourceCode
            },
        }
    }
    fn math_notation(&self, element: &Element) -> MathNotation {
        if element.attribute(None, "data-import-mathml").is_some() {
            MathNotation::MathMl
        } else {
            MathNotation::Tex
        }
    }

    fn math_metadata(&self, element: &Element) -> Option<(String, MathDisplay)> {
        let tex = element
            .attribute(None, "data-tex")
            .or_else(|| element.attribute(None, "data-import-mathml"))?;
        let display = if element
            .attribute(None, "class")
            .is_some_and(|class| class.split_ascii_whitespace().any(|c| c == "katex-display"))
        {
            MathDisplay::Block
        } else {
            MathDisplay::Inline
        };
        Some((tex.into(), display))
    }
}

/// Extract an article from supplied UTF-8 HTML and lower directly into flow IR.
/// Failure to find an article is reported, not silently replaced by the whole page.
pub fn import(source: &str, options: WebOptions) -> Result<WebImportOutcome, WebImportError> {
    if source.len() > MAX_SOURCE {
        return Err(WebImportError::TooComplex);
    }
    let source_url = options
        .source_url
        .as_deref()
        .map(|value| {
            Url::parse(value)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .ok_or(WebImportError::InvalidUrl)
        })
        .transpose()?;
    let document = parse_bounded(source)?;
    // Stable original-element associations survive extraction's cloned DOMs and
    // repair carriers. Never trust a source-supplied value for this private key.
    for (index, node) in document.select("*").nodes().iter().enumerate() {
        node.set_attr(SOURCE_NODE_ATTRIBUTE, &format!("html:element:{index}"));
    }
    let base = document
        .select("base[href]")
        .attr("href")
        .and_then(|href| resolve(&href, source_url.as_ref()))
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .or_else(|| source_url.clone());
    // Supply the response URL as document identity and canonicalize the HTML
    // base once. Passing an already-resolved base as the document URL would
    // apply a relative <base> twice inside extraction.
    for node in document.select("base[href]").nodes() {
        if let Some(base) = &base {
            node.set_attr("href", base.as_str());
        } else {
            node.remove_from_parent();
        }
    }
    let mut responsive = responsive::ResponsiveImages::capture(&document, base.as_ref());
    repair_before_extraction(&document, base.as_ref(), source_url.as_ref())?;
    let notes = capture_referenced_notes(&document, source_url.as_ref(), base.as_ref())?;
    let declared_modified = meta_content(&document, &["article:modified_time", "dcterms.modified"]);
    let declared_url = document
        .select("link[rel][href]")
        .nodes()
        .iter()
        .find(|node| {
            node.attr("rel").is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("canonical"))
            })
        })
        .and_then(|node| node.attr("href").map(|href| href.to_string()))
        .or_else(|| meta_content(&document, &["og:url"]));
    let extracted = Readability::with_document(
        document,
        source_url.as_ref().map(Url::as_str),
        Some(Config {
            max_elements_to_parse: MAX_NODES,
            keep_classes: true,
            ..Default::default()
        }),
    )
    .map_err(|error| WebImportError::Extraction(error.to_string()))?
    .extract()
    .map_err(|error| WebImportError::Extraction(error.to_string()))?;
    let mut metadata = WebMetadata {
        title: extracted.metadata.title.clone(),
        byline: extracted.metadata.byline.clone(),
        site_name: extracted.metadata.site_name.clone(),
        lang: extracted.metadata.lang.clone(),
        excerpt: extracted.metadata.excerpt.clone(),
        dir: extracted.metadata.dir.clone(),
        published_time: extracted.metadata.published_time.clone(),
        modified_time: extracted
            .metadata
            .modified_time
            .clone()
            .or(declared_modified),
        url: metadata_url(
            extracted
                .metadata
                .url
                .as_deref()
                .or(declared_url.as_deref()),
            base.as_ref(),
        ),
        image: metadata_url(extracted.metadata.image.as_deref(), base.as_ref()),
        favicon: metadata_url(extracted.metadata.favicon.as_deref(), base.as_ref()),
    };
    if let Some(site) = &metadata.site_name {
        for separator in [" - ", " | ", " – ", " — "] {
            if let Some((title, suffix)) = metadata.title.rsplit_once(separator)
                && suffix.trim() == site.trim()
                && !title.trim().is_empty()
            {
                metadata.title = title.trim().into();
                break;
            }
        }
    }
    restore_referenced_notes(&extracted, notes, source_url.as_ref(), base.as_ref())?;
    normalize_flow_containment(extracted.root(), false)?;
    let mut normalization_diagnostics = normalize_sequences(&extracted);
    normalize_article(&extracted, &metadata.title);
    for image in extracted.selection().select("img").nodes() {
        responsive.recover(*image, base.as_ref());
    }
    normalization_diagnostics.append(&mut responsive.diagnostics);
    check_dom(extracted.root())?;
    let mut tree = SourceTree::new(
        None,
        TreeLimits {
            max_nodes: MAX_NODES,
            max_depth: MAX_DEPTH + 1,
            ..Default::default()
        },
    )
    .map_err(tree_error)?;
    let root = tree.root();
    // Carry page language/direction without pretending its wrapper has byte offsets.
    let mut attributes = Vec::new();
    for (key, value) in [
        ("lang", metadata.lang.as_deref()),
        ("dir", extracted.metadata.dir.as_deref()),
    ] {
        if let Some(value) = value {
            attributes.push(Attribute::new(name(key, None, None), value.into(), None));
        }
    }
    let content_root = tree
        .append(
            root,
            SourceNodeKind::Element(Element::new(name("div", Some(HTML_NS), None), attributes)),
            None,
            original(),
        )
        .map_err(tree_error)?;
    let mut pending: Vec<_> = extracted
        .root()
        .children_it(true)
        .map(|node| (node, content_root))
        .collect();
    while let Some((node, parent)) = pending.pop() {
        let Some(data) = node.query(|inner| inner.data.clone()) else {
            continue;
        };
        let kind = match data {
            NodeData::Text { contents } => SourceNodeKind::Text(contents.to_string()),
            NodeData::Element(element) => {
                if &*element.name.ns != HTML_NS {
                    continue;
                }
                let tag: &str = element.name.local.as_ref();
                if matches!(
                    tag,
                    "script"
                        | "style"
                        | "head"
                        | "link"
                        | "meta"
                        | "template"
                        | "form"
                        | "input"
                        | "button"
                        | "select"
                        | "textarea"
                        | "object"
                        | "embed"
                ) || is_explicitly_hidden(node)
                {
                    continue;
                }
                let tag = match tag {
                    "b" => "strong",
                    "i" => "em",
                    other => other,
                };
                let mut attributes = Vec::new();
                for attr in node.attrs() {
                    let key: &str = attr.name.local.as_ref();
                    if !matches!(
                        key,
                        "id" | "lang"
                            | "dir"
                            | "href"
                            | "src"
                            | "alt"
                            | "width"
                            | "height"
                            | "start"
                            | "reversed"
                            | "value"
                            | "colspan"
                            | "rowspan"
                            | "datetime"
                            | "open"
                            | "class"
                            | "data-tex"
                            | "title"
                            | "scope"
                            | "headers"
                            | "rbspan"
                            | "role"
                            | "rel"
                            | "data-import-checkbox"
                            | "data-import-mathml"
                            | SOURCE_NODE_ATTRIBUTE
                    ) {
                        continue;
                    }
                    let mut value = attr.value.to_string();
                    if matches!(key, "rowspan" | "colspan" | "start" | "value") {
                        value = value
                            .trim_matches(|c: char| c.is_ascii_whitespace())
                            .to_owned();
                    }
                    if key == "title" && value.is_empty() {
                        continue;
                    }
                    if matches!(key, "href" | "src") {
                        if value.starts_with('#') && base.is_none() {
                            // Without a document URL, the only known resolution is local.
                        } else if let Some(mut url) = resolve(&value, base.as_ref()) {
                            let allowed = if key == "href" {
                                matches!(url.scheme(), "http" | "https" | "mailto")
                            } else {
                                matches!(url.scheme(), "http" | "https" | "data")
                            };
                            if !allowed {
                                continue;
                            }
                            let fragment = url.fragment().map(str::to_owned);
                            url.set_fragment(None);
                            let same_page = source_url.as_ref().is_some_and(|source| {
                                let mut source = source.clone();
                                source.set_fragment(None);
                                source == url
                            });
                            if key == "href"
                                && same_page
                                && let Some(fragment) = fragment
                            {
                                value = format!("#{fragment}");
                            } else {
                                url.set_fragment(fragment.as_deref());
                                value = url.into();
                            }
                        } else if base.is_some() {
                            continue;
                        }
                    }
                    attributes.push(Attribute::new(name(key, None, None), value, None));
                }
                if tag == "img"
                    && !attributes
                        .iter()
                        .any(|attribute| attribute.name().local() == "src")
                {
                    normalization_diagnostics.push(Diagnostic {
                        severity: DiagnosticSeverity::Warning,
                        kind: DiagnosticKind::UnsupportedMedia,
                        origin: None,
                        message:
                            "image resource was rejected; alternative text was retained when available"
                                .into(),
                    });
                    let alternative = node.attr("alt").unwrap_or_default();
                    if alternative.is_empty() {
                        continue;
                    }
                    SourceNodeKind::Text(alternative.to_string())
                } else {
                    SourceNodeKind::Element(Element::new(
                        name(tag, Some(HTML_NS), None),
                        attributes,
                    ))
                }
            }
            _ => continue,
        };
        let id = tree
            .append(parent, kind, None, original_node(node))
            .map_err(tree_error)?;
        pending.extend(node.children_it(true).map(|child| (child, id)));
    }
    let styles = vec![AuthorStyle::default(); tree.nodes().len()];
    let mut content = lowering::lower_tree(
        source,
        tree,
        &styles,
        LoweringOptions {
            identity: options.identity,
            resource_base: base.map(String::from),
            retention: options.retention,
            syntax: SourceSyntax::Html,
        },
        content_root,
        &WebPolicy { responsive },
    )
    .map_err(|error| match error {
        lowering::LoweringError::Build(error) => WebImportError::Build(error),
        lowering::LoweringError::RetainedSource(error) => WebImportError::RetainedSource(error),
    })?;
    content.diagnostics.extend(normalization_diagnostics);
    for id in content.document.preorder(content.document.root()) {
        let node = content.document.get_node(id).unwrap();
        if let NodeKind::Link(link) = &node.kind
            && matches!(link.relation, LinkRelation::NoteReference { .. })
            && let crate::model::flow::Destination::Local(anchor) = &link.destination
            && content.document.anchor(anchor).is_none()
        {
            content.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: DiagnosticKind::InvalidDestination,
                origin: node.origin,
                message: format!(
                    "note reference target '{}' was not available in the extracted article",
                    anchor.as_str()
                ),
            });
        }
    }
    Ok(WebImportOutcome { content, metadata })
}

fn original() -> NodeProvenance {
    NodeProvenance::Original { identity: None }
}

fn original_node(node: NodeRef<'_>) -> NodeProvenance {
    // Text and structural wrappers can be associated with their nearest known
    // original element without inventing byte ranges for a normalized DOM.
    let identity = std::iter::once(node)
        .chain(node.ancestors(None))
        .find_map(|node| node.attr(SOURCE_NODE_ATTRIBUTE))
        .map(|identity| SourceNodeIdentity(identity.to_string()));
    NodeProvenance::Original { identity }
}
fn tree_error(_error: TreeError) -> WebImportError {
    WebImportError::TooComplex
}
fn name(local: &str, namespace: Option<&str>, prefix: Option<&str>) -> ExpandedName {
    ExpandedName::new(
        local.into(),
        local.into(),
        prefix.map(str::to_owned),
        namespace.map(str::to_owned),
    )
}
fn meta_content(document: &Document, keys: &[&str]) -> Option<String> {
    document
        .select("meta[content]")
        .nodes()
        .iter()
        .find_map(|node| {
            let key = node.attr("property").or_else(|| node.attr("name"))?;
            if !key
                .split_ascii_whitespace()
                .any(|token| keys.iter().any(|key| token.eq_ignore_ascii_case(key)))
            {
                return None;
            }
            let content = node.attr("content")?;
            (!content.trim().is_empty()).then(|| content.trim().to_owned())
        })
}

fn metadata_url(reference: Option<&str>, base: Option<&Url>) -> Option<String> {
    resolve(reference?, base)
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .map(String::from)
}

fn resolve(reference: &str, base: Option<&Url>) -> Option<Url> {
    base.map_or_else(|| Url::parse(reference), |base| base.join(reference))
        .ok()
}

fn check_dom(root: NodeRef<'_>) -> Result<(), WebImportError> {
    check_dom_with_work(root, &mut 0)
}

fn check_dom_with_work(root: NodeRef<'_>, visits: &mut usize) -> Result<(), WebImportError> {
    let mut pending = vec![(root, 0)];
    let mut count = 0;
    let mut bytes = 0usize;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        // Inspect borrowed data instead of cloning every attribute vector on
        // every validation pass. Account attributes as work as well as storage.
        let (attributes, payload) = node
            .query(|inner| match &inner.data {
                NodeData::Element(element) => {
                    if element.attrs.len() > MAX_ATTRIBUTES {
                        return (element.attrs.len(), usize::MAX);
                    }
                    let bytes =
                        element
                            .attrs
                            .iter()
                            .fold(element.name.local.len(), |bytes, attr| {
                                bytes
                                    .saturating_add(std::mem::size_of::<
                                        dom_smoothie::dom_query::Attr,
                                    >())
                                    .saturating_add(attr.name.local.len())
                                    .saturating_add(
                                        attr.name.prefix.as_ref().map_or(0, |prefix| prefix.len()),
                                    )
                                    .saturating_add(attr.value.len())
                            });
                    (element.attrs.len(), bytes)
                }
                NodeData::Text { contents } | NodeData::Comment { contents } => (0, contents.len()),
                NodeData::ProcessingInstruction { target, contents } => {
                    (0, target.len().saturating_add(contents.len()))
                }
                NodeData::Doctype {
                    name,
                    public_id,
                    system_id,
                } => (
                    0,
                    name.len()
                        .saturating_add(public_id.len())
                        .saturating_add(system_id.len()),
                ),
                _ => (0, 0),
            })
            .unwrap_or((0, 0));
        bytes = bytes
            .saturating_add(std::mem::size_of::<dom_smoothie::dom_query::TreeNode>())
            .saturating_add(payload);
        *visits = visits.saturating_add(1 + attributes);
        if *visits > MAX_PARSE_VISITS
            || count > MAX_NODES
            || depth > MAX_DEPTH
            || attributes > MAX_ATTRIBUTES
            || bytes > MAX_DOM_BYTES
        {
            return Err(WebImportError::TooComplex);
        }
        pending.extend(node.children_it(true).map(|child| (child, depth + 1)));
    }
    Ok(())
}

fn local_fragment(reference: &str, source_url: Option<&Url>) -> Option<String> {
    if source_url.is_none() {
        return reference.strip_prefix('#').map(lowering::percent_decode);
    }
    let mut resolved = resolve(reference, source_url)?;
    let fragment = lowering::percent_decode(resolved.fragment()?);
    resolved.set_fragment(None);
    let mut source = source_url?.clone();
    source.set_fragment(None);
    (resolved == source).then_some(fragment)
}

#[cfg(test)]
mod tests;
