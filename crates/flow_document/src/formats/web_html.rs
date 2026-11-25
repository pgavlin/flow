//! Experimental host-fed web article extraction. No networking or flow XHTML
//! intermediate: HTML5 DOM -> extraction/normalization -> shared lowering -> IR.
//!
//! This spike bounds input bytes and checks DOM size/depth before Readability,
//! but the third-party parser allocates its DOM before those checks. It is not
//! yet a hardened untrusted-input boundary or a complete web conversion pipeline.

use std::collections::HashSet;

use dom_smoothie::{
    Config, Readability,
    dom_query::{Document, NodeData, NodeRef, Selection},
};
use thiserror::Error;
use url::Url;

use super::html::{
    lowering::{self, LoweringOptions, LoweringPolicy},
    source::SourceSyntax,
    tree::{
        Attribute, Element, ExpandedName, NodeProvenance, SourceNodeKind, SourceTree, TreeError,
        TreeLimits,
    },
};
use crate::model::flow::{
    AuthorStyle, BuildError, DocumentReference, ImportOutcome, MathDisplay, Preformatted,
    PreformattedKind, RetainedSourceError, SourceRetention,
};

const MAX_SOURCE: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 128;
const HTML_NS: &str = "http://www.w3.org/1999/xhtml";

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
}

#[derive(Debug)]
pub struct WebImportOutcome {
    pub content: ImportOutcome,
    pub metadata: WebMetadata,
}

#[derive(Debug, Error)]
pub enum WebImportError {
    #[error("web input exceeds the spike's size, node, or depth limit")]
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

struct WebPolicy;
impl LoweringPolicy for WebPolicy {
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
    fn math_metadata(&self, element: &Element) -> Option<(String, MathDisplay)> {
        let tex = element.attribute(None, "data-tex")?;
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
    let document = Document::from(source);
    check_dom(document.root())?;
    let base = document
        .select("base[href]")
        .attr("href")
        .and_then(|href| resolve(&href, source_url.as_ref()))
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .or_else(|| source_url.clone());
    repair_before_extraction(&document, base.as_ref());
    let extracted = Readability::with_document(
        document,
        base.as_ref().map(Url::as_str),
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
    normalize_article(&extracted, &metadata.title);
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
                ) || node.attr("hidden").is_some()
                    || node
                        .attr("aria-hidden")
                        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
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
                    ) {
                        continue;
                    }
                    let mut value = attr.value.to_string();
                    if matches!(key, "href" | "src") {
                        if key == "href" && value.starts_with('#') { /* preserve local anchors */
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
                    continue;
                }
                SourceNodeKind::Element(Element::new(name(tag, Some(HTML_NS), None), attributes))
            }
            _ => continue,
        };
        let id = tree
            .append(parent, kind, None, original())
            .map_err(tree_error)?;
        pending.extend(node.children_it(true).map(|child| (child, id)));
    }
    let styles = vec![AuthorStyle::default(); tree.nodes().len()];
    let content = lowering::lower_tree(
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
        &WebPolicy,
    )
    .map_err(|error| match error {
        lowering::LoweringError::Build(error) => WebImportError::Build(error),
        lowering::LoweringError::RetainedSource(error) => WebImportError::RetainedSource(error),
    })?;
    Ok(WebImportOutcome { content, metadata })
}

fn original() -> NodeProvenance {
    NodeProvenance::Original { identity: None }
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
fn resolve(reference: &str, base: Option<&Url>) -> Option<Url> {
    base.map_or_else(|| Url::parse(reference), |base| base.join(reference))
        .ok()
}

fn check_dom(root: NodeRef<'_>) -> Result<(), WebImportError> {
    let mut pending = vec![(root, 0)];
    let mut count = 0;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        if count > MAX_NODES || depth > MAX_DEPTH || node.attrs().len() > 4096 {
            return Err(WebImportError::TooComplex);
        }
        pending.extend(node.children_it(true).map(|child| (child, depth + 1)));
    }
    Ok(())
}

fn repair_before_extraction(document: &Document, base: Option<&Url>) {
    // Replace high-link-density WordPress button wrappers
    // with paragraphs, before Readability can classify them as boilerplate.
    for selector in [".wp-block-buttons", ".wp-block-button"] {
        for wrapper in document.select(selector).nodes() {
            let mut count = 0;
            for link in Selection::from(*wrapper).select("a").nodes() {
                let Some(href) = link.attr("href") else {
                    continue;
                };
                let text = link.text();
                if href.trim().starts_with('#') || text.trim().is_empty() {
                    continue;
                }
                let Some(url) =
                    resolve(&href, base).filter(|url| matches!(url.scheme(), "http" | "https"))
                else {
                    continue;
                };
                let p = document.tree.new_element("p");
                let a = document.tree.new_element("a");
                a.set_attr("href", url.as_str());
                a.set_text(text.trim());
                p.append_child(&a);
                wrapper.insert_before(&p);
                count += 1;
            }
            if count > 0 {
                wrapper.remove_from_parent();
            }
        }
    }
    // Rescue wrapped content images. The deliberate tradeoff is losing image links.
    for picture in document.select("picture").nodes() {
        if let Some(image) = Selection::from(*picture).select("img").nodes().first() {
            picture.replace_with(image);
        } else {
            picture.remove_from_parent();
        }
    }
    for link in document.select("a").nodes() {
        if !Selection::from(*link).select("img").nodes().is_empty() {
            link.unwrap_node();
        }
    }
}

fn normalize_article(extracted: &dom_smoothie::Extracted, title: &str) {
    if let Some(first) = extracted
        .root()
        .children()
        .into_iter()
        .find(|node| node.node_name().is_some())
        && first.node_name().is_some_and(|name| name.as_ref() == "h1")
        && first.text().trim() == title.trim()
    {
        first.remove_from_parent();
    }
    let selection = extracted.selection();
    // Identify layout tables using positive signals. Decide before mutating, then
    // unwrap deepest first; semantic header/caption signals always win.
    let tables = selection.select("table");
    let layout: Vec<_> = tables
        .nodes()
        .iter()
        .filter(|table| {
            let table_selection = Selection::from(**table);
            if !table_selection
                .select("thead,th,caption")
                .nodes()
                .is_empty()
            {
                return false;
            }
            let cells = table_selection.select("td");
            if cells.nodes().is_empty() {
                return false;
            }
            let explicit = table
                .attr("role")
                .is_some_and(|role| matches!(&*role, "presentation" | "none"));
            let nested = table
                .ancestors(None)
                .iter()
                .any(|node| node.node_name().is_some_and(|name| &*name == "table"));
            explicit
                || nested
                || !table_selection.select("table").nodes().is_empty()
                || cells.nodes().iter().all(|cell| {
                    let mut saw_block = false;
                    for child in cell.children() {
                        if let Some(name) = child.node_name() {
                            if !matches!(
                                &*name,
                                "p" | "h1"
                                    | "h2"
                                    | "h3"
                                    | "h4"
                                    | "h5"
                                    | "h6"
                                    | "ul"
                                    | "ol"
                                    | "dl"
                                    | "blockquote"
                                    | "pre"
                                    | "figure"
                                    | "hr"
                                    | "table"
                            ) {
                                return false;
                            }
                            saw_block = true;
                        } else if !child.text().trim().is_empty() {
                            return false;
                        }
                    }
                    saw_block
                })
        })
        .copied()
        .collect();
    for table in layout.into_iter().rev() {
        for cell in Selection::from(table).select("td").nodes() {
            for child in cell.children() {
                table.insert_before(&child);
            }
        }
        table.remove_from_parent();
    }
    let mut used: HashSet<String> = selection
        .select("[id]")
        .nodes()
        .iter()
        .filter_map(|node| node.attr("id").map(|id| id.to_string()))
        .collect();
    for heading in selection.select("h1,h2,h3,h4,h5,h6").nodes() {
        if heading.attr("id").is_some_and(|id| !id.is_empty()) {
            continue;
        }
        let slug = heading
            .text()
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let slug = if slug.is_empty() { "section" } else { &slug };
        let mut id = slug.to_owned();
        let mut suffix = 1;
        while !used.insert(id.clone()) {
            id = format!("{slug}-{suffix}");
            suffix += 1;
        }
        heading.set_attr("id", &id);
    }
}

#[cfg(test)]
mod tests;
