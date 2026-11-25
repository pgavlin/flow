//! Split phrasing wrappers around flow content introduced by extension output.
//! A flow wrapper retains the original anchor/language; phrasing runs retain
//! their original inline semantics. No serialized ranges are invented.

use super::*;

struct Plan {
    source: SourceNodeId,
    kind: SourceNodeKind,
    children: Vec<usize>,
    block: bool,
}

fn block_tag(tag: &str) -> bool {
    matches!(
        tag,
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
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "pre"
            | "hr"
            | "figure"
            | "figcaption"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "th"
            | "td"
            | "caption"
            | "details"
            | "summary"
            | "iframe"
            | "audio"
            | "video"
    )
}

pub(super) fn normalize(
    tree: SourceTree,
    policy: &policy::Policy,
) -> Result<SourceTree, MediaWikiImportError> {
    let mut arena: Vec<Plan> = Vec::new();
    let mut results: HashMap<SourceNodeId, usize> = HashMap::new();
    for (id, node) in tree.nodes().collect::<Vec<_>>().into_iter().rev() {
        let mut children: Vec<_> = node.children().iter().map(|child| results[child]).collect();
        let mut kind = node.kind().clone();
        let mut block = matches!(kind, SourceNodeKind::Document);
        if let SourceNodeKind::Element(element) = &mut kind {
            let tag = element.name().local();
            let math = element
                .attribute(None, "data-flow-import-key")
                .and_then(|key| policy.math.get(key));
            block = math.map_or_else(
                || block_tag(tag),
                |(_, display)| *display == MathDisplay::Block,
            );
            let paragraph = matches!(tag, "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
            let phrasing = paragraph || (!block && tag != "a");
            if math.is_none() && phrasing && children.iter().any(|child| arena[*child].block) {
                let original = element.clone();
                let mut runs = Vec::new();
                let mut inline = Vec::new();
                for child in children {
                    if arena[child].block {
                        flush(&mut arena, &mut runs, &mut inline, id, &original, paragraph);
                        runs.push(child);
                    } else {
                        inline.push(child);
                    }
                }
                flush(&mut arena, &mut runs, &mut inline, id, &original, paragraph);
                element.name = element_name("div");
                children = runs;
                block = true;
            } else if tag == "a" && children.iter().any(|child| arena[*child].block) {
                block = true;
            }
        }
        let index = arena.len();
        arena.push(Plan {
            source: id,
            kind,
            children,
            block,
        });
        results.insert(id, index);
    }
    let mut output = SourceTree::new(
        None,
        TreeLimits {
            max_nodes: MAX_NODES,
            max_depth: MAX_DEPTH + 2,
            ..Default::default()
        },
    )
    .map_err(|_| MediaWikiImportError::TooComplex)?;
    let root = output.root();
    let mut remap = HashMap::from([(tree.root(), root)]);
    let mut pending: Vec<_> = arena[results[&tree.root()]]
        .children
        .iter()
        .rev()
        .map(|child| (*child, root))
        .collect();
    while let Some((index, parent)) = pending.pop() {
        let plan = &arena[index];
        let mut provenance = tree.get(plan.source).unwrap().provenance.clone();
        if let NodeProvenance::Generated { related, .. } = &mut provenance {
            *related = *remap.get(related).ok_or(MediaWikiImportError::TooComplex)?;
        }
        let node = output
            .append(parent, plan.kind.clone(), None, provenance)
            .map_err(|_| MediaWikiImportError::TooComplex)?;
        remap.entry(plan.source).or_insert(node);
        pending.extend(plan.children.iter().rev().map(|child| (*child, node)));
    }
    Ok(output)
}

fn flush(
    arena: &mut Vec<Plan>,
    runs: &mut Vec<usize>,
    inline: &mut Vec<usize>,
    source: SourceNodeId,
    original: &Element,
    block: bool,
) {
    if inline.is_empty() {
        return;
    }
    let mut element = original.clone();
    // The outer flow wrapper owns the original anchor and source policy facts.
    element
        .attributes
        .retain(|attribute| !matches!(attribute.name().local(), "id" | "data-flow-import-key"));
    runs.push(arena.len());
    arena.push(Plan {
        source,
        kind: SourceNodeKind::Element(element),
        children: std::mem::take(inline),
        block,
    });
}
