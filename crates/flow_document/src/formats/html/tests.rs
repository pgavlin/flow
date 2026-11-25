use super::lowering::{LoweringOptions, LoweringPolicy, lower_tree};
use super::source::SourceSyntax;
use super::tree::{
    Attribute, Element, ExpandedName, NodeProvenance, SourceNodeId, SourceNodeKind, SourceTree,
    TreeError, TreeLimits,
};
use crate::model::flow::{
    AuthorStyle, DocumentReference, GeneratedReason, ImportOutcome, NodeKind, Origin,
    SourceNodeIdentity, SourceRetention,
};

struct HtmlPolicy;
impl LoweringPolicy for HtmlPolicy {}

fn name(local: &str, namespace: Option<&str>) -> ExpandedName {
    ExpandedName::new(
        local.into(),
        local.into(),
        None,
        namespace.map(str::to_owned),
    )
}

fn element(local: &str, attributes: &[(&str, &str)]) -> SourceNodeKind {
    SourceNodeKind::Element(Element::new(
        name(local, None),
        attributes
            .iter()
            .map(|(key, value)| Attribute::new(name(key, None), (*value).into(), None))
            .collect(),
    ))
}

fn append(tree: &mut SourceTree, parent: SourceNodeId, kind: SourceNodeKind) -> SourceNodeId {
    tree.append(
        parent,
        kind,
        None,
        NodeProvenance::Original { identity: None },
    )
    .unwrap()
}

fn lower(source: &str, tree: SourceTree, retention: SourceRetention) -> ImportOutcome {
    let root = tree.root();
    let styles = vec![AuthorStyle::default(); tree.nodes().len()];
    lower_tree(
        source,
        tree,
        &styles,
        LoweringOptions {
            identity: Some(DocumentReference::Host("original-html".into())),
            resource_base: Some("https://example.test/article".into()),
            retention,
            syntax: SourceSyntax::Html,
        },
        root,
        &HtmlPolicy,
    )
    .unwrap()
}

#[test]
fn constructed_dom_lowers_without_xml_serialization_or_xml_character_rules() {
    let original = "<p>HTML source, not a serialization of the normalized tree.";
    let mut tree = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = tree.root();
    let p = append(
        &mut tree,
        root,
        element("p", &[("id", "target"), ("lang", "ar"), ("dir", "rtl")]),
    );
    append(&mut tree, p, SourceNodeKind::Text(" Before\u{a0} ".into()));
    let strong = append(&mut tree, p, element("strong", &[]));
    append(
        &mut tree,
        strong,
        SourceNodeKind::Text("emphasis & <literal> ".into()),
    );
    let link = append(&mut tree, p, element("a", &[("href", "#target")]));
    append(&mut tree, link, SourceNodeKind::Text("back".into()));
    let discarded = lower(original, tree.clone(), SourceRetention::Discard);
    let retained = lower(original, tree, SourceRetention::Retain);
    assert_eq!(
        format!("{:#?}", discarded.document),
        format!("{:#?}", retained.document)
    );
    assert_eq!(
        retained.document.text().as_str(),
        "Before\u{a0} emphasis & <literal> back"
    );
    assert!(retained.diagnostics.is_empty());
    assert!(discarded.retained_sources.is_none());
    let (_, artifact) = retained.document.source_artifacts().next().unwrap();
    assert_eq!(artifact.media_type.as_deref(), Some("text/html"));
    assert_eq!(
        artifact.identity,
        Some(DocumentReference::Host("original-html".into()))
    );
    assert_eq!(
        retained
            .retained_sources
            .as_ref()
            .unwrap()
            .get(&artifact.revision)
            .unwrap(),
        original
    );
    assert!(retained.document.origins().all(|(_, origin)| matches!(
        origin,
        Origin::Html {
            serialized_range: None,
            ..
        }
    )));
    assert_eq!(
        retained.document.resource_base(),
        Some("https://example.test/article")
    );
}

#[test]
fn constructed_and_xml_parsed_trees_have_identical_structural_outputs() {
    let source = "<p id='p'>Before <em>middle</em><br/>after <img src='photo.png' alt='photo' width='3' height='4'/></p>";
    let parsed = crate::formats::xhtml::parse(source.as_bytes()).unwrap();
    let mut constructed = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = constructed.root();
    let p = append(&mut constructed, root, element("p", &[("id", "p")]));
    append(&mut constructed, p, SourceNodeKind::Text("Before ".into()));
    let em = append(&mut constructed, p, element("em", &[]));
    append(&mut constructed, em, SourceNodeKind::Text("middle".into()));
    append(&mut constructed, p, element("br", &[]));
    append(&mut constructed, p, SourceNodeKind::Text("after ".into()));
    append(
        &mut constructed,
        p,
        element(
            "img",
            &[
                ("src", "photo.png"),
                ("alt", "photo"),
                ("width", "3"),
                ("height", "4"),
            ],
        ),
    );
    let a = lower(source, parsed, SourceRetention::Discard).document;
    let b = lower(source, constructed, SourceRetention::Discard).document;
    let nodes = |document: &crate::model::flow::FlowDocument| {
        document
            .preorder(document.root())
            .map(|id| (id, document.get_node(id).unwrap().clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(nodes(&a), nodes(&b));
    assert_eq!(a.text(), b.text());
    assert_eq!(
        a.resources().collect::<Vec<_>>(),
        b.resources().collect::<Vec<_>>()
    );
    assert_eq!(
        a.style_inputs().collect::<Vec<_>>(),
        b.style_inputs().collect::<Vec<_>>()
    );
}

#[test]
fn foreign_local_names_do_not_acquire_html_semantics() {
    let mut tree = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = tree.root();
    let p = append(&mut tree, root, element("p", &[]));
    let foreign = append(
        &mut tree,
        p,
        SourceNodeKind::Element(Element::new(name("img", Some("urn:foreign")), Vec::new())),
    );
    append(
        &mut tree,
        foreign,
        SourceNodeKind::Text("foreign fallback".into()),
    );
    append(
        &mut tree,
        p,
        SourceNodeKind::Element(Element::new(
            name("br", Some("http://www.w3.org/1999/xhtml")),
            Vec::new(),
        )),
    );
    let outcome = lower("original", tree, SourceRetention::Discard);
    assert_eq!(outcome.document.text().as_str(), "foreign fallback\n");
    assert!(outcome.document.resources().next().is_none());
    assert!(
        outcome
            .document
            .preorder(outcome.document.root())
            .any(|id| matches!(
                outcome.document.get_node(id).unwrap().kind,
                NodeKind::HardBreak
            ))
    );
}

fn provenance_outcome() -> ImportOutcome {
    let original = "<p>old</p>";
    let mut tree = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = tree.root();
    let p = tree
        .append(
            root,
            element("p", &[]),
            Some(0..10),
            NodeProvenance::Original {
                identity: Some(SourceNodeIdentity("original:42".into())),
            },
        )
        .unwrap();
    tree.append(
        p,
        SourceNodeKind::Text("generated".into()),
        None,
        NodeProvenance::Generated {
            reason: GeneratedReason::UnsupportedFallback,
            related: p,
        },
    )
    .unwrap();
    lower(original, tree, SourceRetention::Retain)
}

#[test]
fn rewriting_keeps_original_identity_and_generated_provenance() {
    let outcome = provenance_outcome();
    assert!(
        outcome
            .document
            .origins()
            .any(|(_, origin)| matches!(origin,
        Origin::Html { source_node, serialized_range: Some(range), .. }
        if source_node.0 == "original:42" && *range == (0..10)))
    );
    let generated = outcome
        .document
        .origins()
        .find_map(|(_, origin)| match origin {
            Origin::Generated {
                reason: GeneratedReason::UnsupportedFallback,
                related,
            } => *related,
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(outcome.document.get_origin(generated), Some(Origin::Html { source_node, .. }) if source_node.0 == "original:42")
    );
    assert_eq!(outcome.document.text().as_str(), "generated");
}

#[cfg(feature = "render")]
#[test]
fn html_and_generated_origins_round_trip_semantic_cache() {
    use crate::cache_codec::{
        CacheCodecLimits, DependencyDigest, decode_semantic, encode_semantic,
    };
    let document = provenance_outcome().document;
    let dependency = DependencyDigest::for_bytes(b"original-html");
    let limits = CacheCodecLimits::default();
    let bytes = encode_semantic(&document, dependency, limits).unwrap();
    let decoded = decode_semantic(&bytes, dependency, limits).unwrap();
    assert_eq!(format!("{document:#?}"), format!("{decoded:#?}"));
}

#[test]
fn construction_limits_are_checked_before_inserting_nodes() {
    let default = TreeLimits::default();
    assert_eq!(
        SourceTree::new(
            None,
            TreeLimits {
                max_nodes: 0,
                ..default
            }
        )
        .unwrap_err(),
        TreeError::TooComplex
    );
    for limits in [
        TreeLimits {
            max_nodes: 1,
            ..default
        },
        TreeLimits {
            max_depth: 0,
            ..default
        },
        TreeLimits {
            max_text_bytes: 0,
            ..default
        },
        TreeLimits {
            max_attributes: 0,
            ..default
        },
    ] {
        let mut tree = SourceTree::new(None, limits).unwrap();
        let root = tree.root();
        assert_eq!(
            tree.append(
                root,
                element("p", &[("id", "a")]),
                None,
                NodeProvenance::Original { identity: None }
            ),
            Err(TreeError::TooComplex)
        );
        assert_eq!(tree.nodes().len(), 1);
        assert!(tree.get(root).unwrap().children().is_empty());
    }
    let mut tree = SourceTree::new(
        None,
        TreeLimits {
            max_text_bytes: 4,
            ..default
        },
    )
    .unwrap();
    let root = tree.root();
    append(&mut tree, root, SourceNodeKind::Text("1234".into()));
    assert_eq!(
        tree.append(
            root,
            SourceNodeKind::Text("5".into()),
            None,
            NodeProvenance::Original { identity: None }
        ),
        Err(TreeError::TooComplex)
    );
    assert_eq!(tree.nodes().len(), 2);
    assert_eq!(
        tree.append(
            root,
            element("p", &[]),
            Some(std::ops::Range { start: 3, end: 2 }),
            NodeProvenance::Original { identity: None }
        ),
        Err(TreeError::InvalidSource)
    );
}

#[test]
fn anonymous_paragraphs_keep_generated_origins_and_whitespace_boundaries() {
    let mut tree = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = tree.root();
    append(&mut tree, root, SourceNodeKind::Text("  before ".into()));
    let span = append(&mut tree, root, element("span", &[]));
    append(&mut tree, span, SourceNodeKind::Text(" after  ".into()));
    let outcome = lower("original", tree, SourceRetention::Discard);
    assert_eq!(outcome.document.text().as_str(), "before after");
    assert!(outcome.document.origins().any(|(_, origin)| matches!(
        origin,
        Origin::Generated {
            reason: GeneratedReason::AnonymousParagraph,
            related: Some(_)
        }
    )));
}

#[test]
fn adapter_hooks_return_flow_concepts_without_shared_source_conventions() {
    use crate::model::flow::{MathDisplay, Preformatted, PreformattedKind};
    struct Adapter;
    impl LoweringPolicy for Adapter {
        fn math_metadata(&self, element: &Element) -> Option<(String, MathDisplay)> {
            element
                .attribute(None, "formula")
                .map(|value| (value.to_owned(), MathDisplay::Block))
        }
        fn code_metadata(&self, _element: &Element) -> Preformatted {
            Preformatted {
                language: None,
                content_kind: PreformattedKind::Poetry,
            }
        }
    }
    let mut tree = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = tree.root();
    let math = append(&mut tree, root, element("span", &[("formula", "x+1")]));
    append(
        &mut tree,
        math,
        SourceNodeKind::Text("not additional reading content".into()),
    );
    let pre = append(&mut tree, root, element("pre", &[]));
    let code = append(&mut tree, pre, element("code", &[]));
    append(&mut tree, code, SourceNodeKind::Text("verse".into()));
    let styles = vec![AuthorStyle::default(); tree.nodes().len()];
    let result = lower_tree(
        "original",
        tree,
        &styles,
        LoweringOptions {
            identity: None,
            resource_base: None,
            retention: SourceRetention::Discard,
            syntax: SourceSyntax::Html,
        },
        root,
        &Adapter,
    )
    .unwrap();
    let kinds: Vec<_> = result
        .document
        .preorder(result.document.root())
        .map(|id| &result.document.get_node(id).unwrap().kind)
        .collect();
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Math(math) if math.source == "x+1" && math.display == MathDisplay::Block)));
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Preformatted(code) if code.content_kind == PreformattedKind::Poetry)));
    assert!(!result.document.text().as_str().contains("not additional"));
}

#[test]
fn html_ranges_are_validated_against_original_utf8_bytes() {
    use super::lowering::LoweringError;
    use crate::model::flow::BuildError;
    let tree = SourceTree::new(Some(1..2), TreeLimits::default()).unwrap();
    let root = tree.root();
    let styles = vec![AuthorStyle::default()];
    let result = lower_tree(
        "é",
        tree,
        &styles,
        LoweringOptions {
            identity: None,
            resource_base: None,
            retention: SourceRetention::Discard,
            syntax: SourceSyntax::Html,
        },
        root,
        &HtmlPolicy,
    );
    assert!(matches!(
        result,
        Err(LoweringError::Build(BuildError::InvalidSourceRange { .. }))
    ));
}

#[test]
fn source_references_cannot_introduce_cycles_or_missing_parents() {
    let mut other = SourceTree::new(None, TreeLimits::default()).unwrap();
    let other_root = other.root();
    let missing = append(&mut other, other_root, element("p", &[]));
    let mut tree = SourceTree::new(None, TreeLimits::default()).unwrap();
    let root = tree.root();
    assert_eq!(
        tree.append(
            missing,
            element("p", &[]),
            None,
            NodeProvenance::Original { identity: None }
        ),
        Err(TreeError::InvalidSource)
    );
    assert_eq!(
        tree.append(
            root,
            element("p", &[]),
            None,
            NodeProvenance::Generated {
                reason: GeneratedReason::AnonymousParagraph,
                related: missing
            }
        ),
        Err(TreeError::InvalidSource)
    );
    assert_eq!(tree.nodes().len(), 1);
}
