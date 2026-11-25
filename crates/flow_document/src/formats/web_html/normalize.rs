//! Web-only extraction repairs and structural normalization. No networking.

use std::collections::HashSet;

use dom_smoothie::dom_query::{Document, NodeRef, Selection};
use url::Url;

use super::visibility::is_explicitly_hidden;
use super::{
    MAX_NODES, MAX_SOURCE, SOURCE_NODE_ATTRIBUTE, WebImportError, check_dom, local_fragment,
    resolve,
};
use crate::model::flow::{Diagnostic, DiagnosticKind, DiagnosticSeverity};

fn serialize_mathml(node: NodeRef<'_>) -> String {
    // Provenance carriers belong to the adapter, not to the stored math source.
    let mut pending = vec![node];
    let mut identities = Vec::new();
    while let Some(node) = pending.pop() {
        if let Some(identity) = node.attr(SOURCE_NODE_ATTRIBUTE) {
            identities.push((node, identity));
            node.remove_attr(SOURCE_NODE_ATTRIBUTE);
        }
        pending.extend(node.children_it(true));
    }
    let source = node.html().to_string();
    for (node, identity) in identities {
        node.set_attr(SOURCE_NODE_ATTRIBUTE, &identity);
    }
    source
}

/// Split phrasing wrappers around flow content instead of flattening its
/// structure or inserting flow-only atoms into inline IR. Transparent links
/// remain around the resulting blocks.
pub(super) fn normalize_flow_containment(
    root: NodeRef<'_>,
    math_only: bool,
) -> Result<(), WebImportError> {
    let selector = if math_only {
        "[data-tex],[data-import-mathml]"
    } else {
        "html,body,div,section,article,main,header,footer,aside,nav,address,h1,h2,h3,h4,h5,h6,p,blockquote,pre,hr,figure,ul,ol,dl,table,details,iframe,audio,video,[data-tex],[data-import-mathml]"
    };
    for node in Selection::from(root).select(selector).nodes() {
        let math = node.attr("data-tex").is_some() || node.attr("data-import-mathml").is_some();
        let display_math = math
            && node.attr("class").is_some_and(|class| {
                class
                    .split_ascii_whitespace()
                    .any(|class| class == "katex-display")
            });
        if math && !display_math {
            continue;
        }
        let mut flow = *node;
        while let Some(parent) = flow.parent() {
            let Some(name) = parent.node_name() else {
                break;
            };
            if &*name == "a" && parent.attr("href").is_some() {
                flow = parent;
                continue;
            }
            if matches!(
                &*name,
                "html"
                    | "body"
                    | "div"
                    | "section"
                    | "article"
                    | "main"
                    | "header"
                    | "footer"
                    | "aside"
                    | "nav"
                    | "address"
                    | "blockquote"
                    | "li"
                    | "dd"
                    | "td"
                    | "th"
                    | "figure"
                    | "figcaption"
                    | "details"
                    | "pre"
            ) && !(&*name == "pre" && display_math)
            {
                break;
            }
            if root.tree.len() >= MAX_NODES {
                return Err(WebImportError::TooComplex);
            }
            let after = root.tree.new_element(&name);
            for attr in parent.attrs() {
                if !matches!(&*attr.name.local, "id" | "name") {
                    after.set_attr(attr.name.local.as_ref(), &attr.value);
                }
            }
            while let Some(sibling) = flow.next_sibling() {
                after.append_child(&sibling);
            }
            parent.insert_after(&flow);
            if !after.children().is_empty() {
                flow.insert_after(&after);
            }
            if parent.children().is_empty() && parent.attr("id").is_some() {
                parent.rename("span");
            }
        }
    }
    Ok(())
}

pub(super) fn repair_before_extraction(
    document: &Document,
    base: Option<&Url>,
    source_url: Option<&Url>,
) -> Result<(), WebImportError> {
    // In preformatted content a break is literal whitespace, not a paragraph
    // hint. Convert it before extraction's global break-cleanup heuristics can
    // discard it or collapse consecutive breaks.
    for br in document.select("pre br").nodes() {
        br.rename("span");
        br.set_text("\n");
    }
    // Native extraction strips form controls. Preserve only static checkbox
    // state as an inert carrier; no interactive form behavior enters the IR.
    for input in document.select("input[type]").nodes() {
        if input
            .attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("checkbox"))
        {
            let checked = input.attr("checked").is_some();
            input.rename("span");
            input.set_attr(
                "data-import-checkbox",
                if checked { "checked" } else { "unchecked" },
            );
            input.set_text(if checked { "☑" } else { "☐" });
        }
    }
    // Convert supported math encodings before extraction removes scripts and
    // before the HTML-only tree adapter discards foreign-namespace content.
    let mut math_bytes = 0usize;
    for node in document.select("math, script[type]").nodes() {
        let is_math = node.node_name().is_some_and(|name| &*name == "math");
        let (math_source, block, mathml) = if is_math {
            let annotation = Selection::from(*node).select("annotation[encoding]");
            let annotation = annotation.nodes().iter().find(|annotation| {
                annotation.attr("encoding").is_some_and(|encoding| {
                    matches!(
                        encoding.trim().to_ascii_lowercase().as_str(),
                        "application/x-tex" | "application/x-latex"
                    )
                })
            });
            (
                annotation.map_or_else(
                    || serialize_mathml(*node),
                    |annotation| annotation.text().to_string(),
                ),
                node.attr("display")
                    .is_some_and(|value| value.eq_ignore_ascii_case("block")),
                annotation.is_none(),
            )
        } else {
            let kind = node.attr("type").unwrap_or_default();
            let mut parts = kind.split(';');
            if !parts
                .next()
                .is_some_and(|part| part.trim().eq_ignore_ascii_case("math/tex"))
            {
                continue;
            }
            let block = parts.any(|part| {
                part.split_once('=').is_some_and(|(key, value)| {
                    key.trim().eq_ignore_ascii_case("mode")
                        && value.trim().eq_ignore_ascii_case("display")
                })
            });
            (node.text().to_string(), block, false)
        };
        math_bytes = math_bytes
            .checked_add(math_source.len())
            .ok_or(WebImportError::TooComplex)?;
        if math_bytes > MAX_SOURCE {
            return Err(WebImportError::TooComplex);
        }
        // An inaccessible math source and an aria-hidden image can be two
        // representations of one visible formula. Promote the semantic source
        // only when explicit alternative text proves the pairing; otherwise a
        // hidden math subtree stays hidden.
        let paired = if is_math {
            node.attr("alttext").and_then(|alternative| {
                node.ancestors(None).into_iter().take(3).find(|ancestor| {
                    let children = ancestor.children();
                    if children.len() > 16
                        || !children.iter().any(|image| {
                            image.has_name("img")
                                && image
                                    .attr("aria-hidden")
                                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                                && image
                                    .attr("alt")
                                    .is_some_and(|alt| alt.trim() == alternative.trim())
                        })
                    {
                        return false;
                    }
                    let mut pending = children;
                    let mut math_count = 0;
                    let mut visited = 0;
                    while let Some(child) = pending.pop() {
                        visited += 1;
                        if visited > 128 {
                            return false;
                        }
                        if child.has_name("math") {
                            math_count += 1;
                            if math_count > 1 {
                                return false;
                            }
                        } else {
                            pending.extend(child.children());
                        }
                    }
                    math_count == 1
                })
            })
        } else {
            None
        };
        let related = paired.unwrap_or(*node);
        let replacement = document.tree.new_element(if block { "p" } else { "span" });
        for key in [
            "id",
            "lang",
            "dir",
            "hidden",
            "aria-hidden",
            "style",
            SOURCE_NODE_ATTRIBUTE,
        ] {
            if let Some(value) = related.attr(key) {
                replacement.set_attr(key, &value);
            }
        }
        if let Some(identity) = node.attr(SOURCE_NODE_ATTRIBUTE) {
            replacement.set_attr(SOURCE_NODE_ATTRIBUTE, &identity);
        }
        replacement.set_attr(
            if mathml {
                "data-import-mathml"
            } else {
                "data-tex"
            },
            &math_source,
        );
        if block {
            replacement.set_attr("class", "katex-display");
        }
        if mathml {
            let fallback = node.text();
            replacement.set_text(if fallback.is_empty() {
                "Math"
            } else {
                &fallback
            });
        } else {
            replacement.set_text(math_source.as_str());
        }
        related.replace_with(&replacement);
    }
    normalize_flow_containment(document.root(), true)?;
    // Resolve links before extraction: Readability intentionally leaves bare
    // fragments alone, but HTML base URLs apply to them too.
    for link in document.select("a").nodes() {
        if let Some(name) = link.attr("name").filter(|name| !name.is_empty()) {
            match link.attr("id").filter(|id| !id.is_empty()) {
                None => link.set_attr("id", &name),
                Some(id) if id != name => {
                    // HTML permits distinct id and legacy name targets on one
                    // anchor. Keep both at the same reading-order boundary.
                    let target = document.tree.new_element("span");
                    target.set_attr("id", &name);
                    if let Some(identity) = link.attr(SOURCE_NODE_ATTRIBUTE) {
                        target.set_attr(SOURCE_NODE_ATTRIBUTE, &identity);
                    }
                    link.insert_before(&target);
                }
                _ => {}
            }
        }
        if let Some(href) = link.attr("href")
            && href.starts_with('#')
            && let Some(url) = resolve(&href, base)
            && (source_url.is_none() || local_fragment(url.as_str(), source_url).is_none())
        {
            // Preserve same-document bare fragments during scoring: native
            // extraction discounts their link density. Only an external base
            // requires expansion before extraction's fragment special case.
            link.set_attr("href", url.as_str());
        }
        if link.attr("href").is_none() && link.attr("id").is_some() && link.children().is_empty() {
            // Readability removes empty anchors without href, including targets.
            link.rename("span");
        }
    }
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
                for key in ["id", "title", "lang", "dir", SOURCE_NODE_ATTRIBUTE] {
                    if let Some(value) = link.attr(key) {
                        a.set_attr(key, &value);
                    }
                }
                if let Some(identity) = wrapper.attr(SOURCE_NODE_ATTRIBUTE) {
                    p.set_attr(SOURCE_NODE_ATTRIBUTE, &identity);
                }
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
    // Responsive choices were snapshotted by the adapter. Collapse wrappers
    // for extraction without losing the alternatives associated with the image.
    for picture in document.select("picture").nodes() {
        if let Some(image) = Selection::from(*picture).select("img").nodes().first() {
            picture.replace_with(image);
        } else {
            picture.remove_from_parent();
        }
    }
    if document.tree.len() > MAX_NODES {
        return Err(WebImportError::TooComplex);
    }
    check_dom(document.root())
}

/// Recover HTML structures that cannot satisfy the IR's specialized grammars.
/// Preserve their content and IDs as flow rather than failing the whole article.
pub(super) fn normalize_sequences(extracted: &dom_smoothie::Extracted) -> Vec<Diagnostic> {
    let selection = extracted.selection();
    selection
        .select(
            "script,style,head,link,meta,template,form,input,button,select,textarea,object,embed",
        )
        .remove();
    for node in selection.select("[hidden],[aria-hidden],[style]").nodes() {
        if is_explicitly_hidden(*node) {
            node.remove_from_parent();
        }
    }
    let mut diagnostics = Vec::new();
    for list in selection.select("ul,ol").nodes() {
        if !list
            .children()
            .iter()
            .any(|child| child.node_name().is_some_and(|name| &*name == "li"))
        {
            list.rename("div");
            diagnostics.push(structure_diagnostic(
                "list without list items was retained as flow",
            ));
        } else {
            let mut repaired = false;
            for child in list.children() {
                if child.has_name("li")
                    || (child.node_name().is_none() && child.text().trim().is_empty())
                {
                    continue;
                }
                let item = extracted.document.tree.new_element("li");
                child.insert_before(&item);
                item.append_child(&child);
                repaired = true;
            }
            if repaired {
                diagnostics.push(structure_diagnostic(
                    "non-item list content was wrapped as a list item",
                ));
            }
        }
    }
    for list in selection.select("dl").nodes() {
        let children: Vec<_> = list
            .children()
            .into_iter()
            .filter(|child| child.node_name().is_some() || !child.text().trim().is_empty())
            .collect();
        let mut index = 0;
        let mut valid = !children.is_empty();
        while valid && index < children.len() {
            let start = index;
            while index < children.len()
                && children[index]
                    .node_name()
                    .is_some_and(|name| &*name == "dt")
            {
                index += 1;
            }
            valid &= index > start;
            let start = index;
            while index < children.len()
                && children[index]
                    .node_name()
                    .is_some_and(|name| &*name == "dd")
            {
                index += 1;
            }
            valid &= index > start;
        }
        if !valid {
            for child in Selection::from(*list).select("dt,dd").nodes() {
                if child
                    .ancestors(None)
                    .iter()
                    .find(|ancestor| ancestor.node_name().is_some_and(|name| &*name == "dl"))
                    .is_some_and(|ancestor| ancestor.id == list.id)
                {
                    child.rename("div");
                }
            }
            list.rename("div");
            diagnostics.push(structure_diagnostic(
                "definition list with incomplete or grouped entries was retained as flow",
            ));
        }
    }
    for table in selection.select("table").nodes() {
        let captions: Vec<_> = table
            .children()
            .into_iter()
            .filter(|child| child.has_name("caption"))
            .collect();
        if let Some((first, rest)) = captions.split_first() {
            for caption in rest {
                caption.rename("div");
                first.append_child(caption);
            }
            if !rest.is_empty() {
                diagnostics.push(structure_diagnostic(
                    "additional table captions were retained inside the first caption",
                ));
            }
        }
        let has_cells = Selection::from(*table)
            .select("tr > td, tr > th")
            .nodes()
            .iter()
            .any(|cell| {
                cell.ancestors(None)
                    .iter()
                    .find(|ancestor| ancestor.node_name().is_some_and(|name| &*name == "table"))
                    .is_some_and(|ancestor| ancestor.id == table.id)
            });
        if !has_cells {
            table.rename("div");
            diagnostics.push(structure_diagnostic(
                "table without cells was retained as flow",
            ));
        }
    }
    diagnostics
}

fn structure_diagnostic(message: &str) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Warning,
        kind: DiagnosticKind::InvalidChild,
        origin: None,
        message: message.into(),
    }
}

pub(super) fn normalize_article(extracted: &dom_smoothie::Extracted, title: &str) {
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
    // Choose a deterministic fallback after lazy-image recovery. Responsive
    // selection remains host/IR work; do not lose srcset-only images meanwhile.
    for image in selection.select("img[srcset]").nodes() {
        if image.attr("src").is_none_or(|src| src.trim().is_empty())
            && let Some(srcset) = image.attr("srcset")
            && let Some(candidate) = first_srcset_candidate(&srcset)
        {
            image.set_attr("src", candidate);
        }
    }
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
            // Nesting and block-containing cells do not imply presentation:
            // both are valid data-table content. Require explicit author intent.
            table
                .attr("role")
                .is_some_and(|role| matches!(&*role, "presentation" | "none"))
                && table.attr("summary").is_none()
                && table_selection
                    .select("[scope],[headers]")
                    .nodes()
                    .is_empty()
        })
        .copied()
        .collect();
    for table in layout.into_iter().rev() {
        let wrapper = extracted.document.tree.new_element("div");
        for key in ["id", "lang", "dir", SOURCE_NODE_ATTRIBUTE] {
            if let Some(value) = table.attr(key) {
                wrapper.set_attr(key, &value);
            }
        }
        for cell in Selection::from(table).select("td").nodes() {
            if cell
                .ancestors(None)
                .iter()
                .find(|ancestor| ancestor.node_name().is_some_and(|name| &*name == "table"))
                .is_none_or(|ancestor| ancestor.id != table.id)
            {
                continue;
            }
            let paragraph = extracted.document.tree.new_element("div");
            for key in ["id", "lang", "dir", SOURCE_NODE_ATTRIBUTE] {
                if let Some(value) = cell.attr(key) {
                    paragraph.set_attr(key, &value);
                }
            }
            for child in cell.children() {
                paragraph.append_child(&child);
            }
            wrapper.append_child(&paragraph);
        }
        table.replace_with(&wrapper);
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

/// Read the first usable candidate, keeping commas inside URLs (notably data
/// URLs). Descriptor validation avoids turning malformed candidates into fetches.
pub(super) fn first_srcset_candidate(input: &str) -> Option<&str> {
    super::responsive::parse_srcset(input)
        .0
        .first()
        .map(|(identity, _)| *identity)
}
