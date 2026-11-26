use super::*;

#[test]
fn host_can_raise_source_limit_for_documents_and_fragments() {
    let body = format!(
        "<p>Readable article.</p><!--{}-->",
        "x".repeat(2 * 1024 * 1024)
    );
    for input in [MediaWikiInput::Document, MediaWikiInput::BodyFragment] {
        let source = if input == MediaWikiInput::Document {
            article(&body)
        } else {
            body.clone()
        };
        let mut options = MediaWikiOptions {
            input,
            expected_version: Some(MediaWikiHtmlVersion::V2_8_0),
            ..options()
        };
        assert!(matches!(
            import(&source, options.clone()),
            Err(MediaWikiImportError::TooComplex)
        ));
        options.limits.max_source_bytes = source.len();
        assert!(import(&source, options).is_ok());
    }
}

#[test]
fn host_can_tighten_each_construction_budget() {
    let source = article("<p title='text'>Readable article.</p>");
    let defaults = HtmlLimits::default();
    let limits = [
        HtmlLimits {
            max_source_bytes: 0,
            ..defaults
        },
        HtmlLimits {
            max_nodes: 0,
            ..defaults
        },
        HtmlLimits {
            max_depth: 0,
            ..defaults
        },
        HtmlLimits {
            max_attributes: 0,
            ..defaults
        },
        HtmlLimits {
            max_dom_bytes: 0,
            ..defaults
        },
        HtmlLimits {
            max_operations: 0,
            ..defaults
        },
        HtmlLimits {
            max_tree_bytes: 0,
            ..defaults
        },
    ];
    assert!(import(&source, options()).is_ok());
    for limits in limits {
        assert!(
            matches!(
                import(
                    &source,
                    MediaWikiOptions {
                        limits,
                        ..options()
                    }
                ),
                Err(MediaWikiImportError::TooComplex)
            ),
            "{limits:?}"
        );
    }
}

fn options() -> MediaWikiOptions {
    MediaWikiOptions {
        source_url: Some("https://example.test/wiki/Article".into()),
        ..Default::default()
    }
}
fn article(body: &str) -> String {
    format!(
        "<!doctype html><html prefix='mw: http://mediawiki.org/rdf/' lang='en' dir='ltr'><head><meta property='mw:htmlVersion' content='2.8.0'><title>Database title</title></head><body>{body}</body></html>"
    )
}
fn fragment(body: &str) -> MediaWikiImportOutcome {
    import(
        body,
        MediaWikiOptions {
            input: MediaWikiInput::BodyFragment,
            expected_version: Some(MediaWikiHtmlVersion::V2_8_0),
            ..options()
        },
    )
    .unwrap()
}
fn nodes(document: &FlowDocument) -> impl Iterator<Item = &Node> {
    document
        .preorder(document.root())
        .map(|id| document.get_node(id).unwrap())
}

#[test]
fn incomplete_definition_lists_preserve_readable_content() {
    for body in [
        "<dl><dt>Studio albums</dt></dl>",
        "<dl><dd>Studio albums</dd></dl>",
        "<dl><dt>Albums</dt><dd>Recordings</dd><dt>Studio albums</dt></dl>",
        "<dl><dt><a href='./Albums'>Studio albums</a></dt><dd><p>Recordings</p><dl><dt>Nested term</dt></dl></dd><dt>Last term</dt></dl>",
    ] {
        let result = fragment(body);
        let document = &result.content.document;
        assert!(document.text().as_str().contains("Studio albums"), "{body}");
        assert!(!nodes(document).any(|node| matches!(node.kind, NodeKind::DefinitionList)));
        if body.contains("Nested term") {
            for text in ["Recordings", "Nested term", "Last term"] {
                assert!(document.text().as_str().contains(text));
            }
            assert!(nodes(document).any(|node| matches!(node.kind, NodeKind::Link(_))));
        }
    }
    assert!(fragment("<dl></dl>").content.document.text().is_empty());
}

#[test]
fn complete_definition_lists_keep_their_semantics() {
    let result = fragment(
        "<dl> <!-- group --> <dt>Albums</dt><dt>Records</dt><dd>First</dd><dd>Second</dd><dt>Singles</dt><dd>Third</dd></dl>",
    );
    let document = &result.content.document;
    assert_eq!(
        nodes(document)
            .filter(|node| matches!(node.kind, NodeKind::DefinitionList))
            .count(),
        1
    );
    assert_eq!(
        nodes(document)
            .filter(|node| matches!(node.kind, NodeKind::Term))
            .count(),
        3
    );
    assert_eq!(
        nodes(document)
            .filter(|node| matches!(node.kind, NodeKind::Definition))
            .count(),
        3
    );
}

#[test]
fn versions_are_explicit_and_must_agree() {
    assert!(matches!(
        import("<p>body</p>", Default::default()),
        Err(MediaWikiImportError::MissingVersion)
    ));
    assert!(matches!(
        import(
            "",
            MediaWikiOptions {
                input: MediaWikiInput::BodyFragment,
                ..Default::default()
            }
        ),
        Err(MediaWikiImportError::FragmentVersionRequired)
    ));
    let declared = article("<p>Article</p>");
    for key in ["mw:htmlVersion", "mw:html:version"] {
        let result = import(&declared.replace("mw:htmlVersion", key), Default::default()).unwrap();
        assert_eq!(
            result.metadata.declared_version,
            Some(MediaWikiHtmlVersion::V2_8_0)
        );
    }
    assert!(matches!(
        import(&declared.replace("2.8.0", "2.9.0"), Default::default()),
        Err(MediaWikiImportError::UnsupportedVersion(_))
    ));
    assert!(matches!(
        import(
            &declared.replace(
                "</head>",
                "<meta property='mw:html:version' content='2.7.0'></head>"
            ),
            Default::default()
        ),
        Err(MediaWikiImportError::ConflictingVersions)
    ));
    assert!(matches!(
        import(
            &declared,
            MediaWikiOptions {
                expected_version: Some("2.7.0".parse().unwrap()),
                ..Default::default()
            }
        ),
        Err(MediaWikiImportError::ConflictingVersions)
    ));
    assert!(matches!(
        import(&declared.replace("2.8.0", "2.8"), Default::default()),
        Err(MediaWikiImportError::InvalidVersion(_))
    ));
    assert!(fragment("").content.document.text().is_empty());
    let supplied = import(
        "<body>body</body>",
        MediaWikiOptions {
            expected_version: Some(MediaWikiHtmlVersion::V2_8_0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(supplied.metadata.declared_version, None);
}

#[test]
fn parses_body_fragments_without_extracting_or_hiding_transclusions() {
    let result = fragment(
        "<section data-mw-section-id='0'></section><section><h2 id='Heading'>Heading</h2><nav>Keep navigation</nav><p typeof='mw:Transclusion' about='#mwt1' data-mw='{\"parts\":[\"DO NOT EXPAND\"]}'>Expanded</p><p about='#mwt1'>Sibling&nbsp;text</p><a name='legacy'></a><p><a href='#legacy'>Jump</a></p></section>",
    );
    let document = &result.content.document;
    assert_eq!(
        nodes(document)
            .filter(|node| matches!(node.kind, NodeKind::Section))
            .count(),
        2
    );
    assert!(document.text().as_str().contains("Keep navigation"));
    assert!(document.text().as_str().contains("Sibling\u{a0}text"));
    assert!(!document.text().as_str().contains("DO NOT EXPAND"));
    assert!(document.anchor(&AnchorId::new("legacy")).is_some());
    assert!(document.anchor(&AnchorId::new("Heading")).is_some());
}

#[test]
fn metadata_base_precedence_and_fragment_policy() {
    let source = article("<link rel='mw:PageProp/Category' href='./Category:Test#Sort%20key'><link rel='mw:PageProp/Language' href='//fr.example.test/wiki/Article'><p id='here'><a href='#here'>local</a> <a href='./Article#here'>same</a> <a href='./Other?redlink=1'>red</a><a rel='mw:ExtLink' class='external autonumber' href='https://outside.test'></a></p><img src='image.png' alt='image'>")
        .replace("</head>", "<base href='../wiki/'></head>");
    let result = import(&source, options()).unwrap();
    assert_eq!(
        result.metadata.database_title.as_deref(),
        Some("Database title")
    );
    assert_eq!(
        result.metadata.categories[0].target.resolved,
        "https://example.test/wiki/Category:Test#Sort%20key"
    );
    assert_eq!(
        result.metadata.categories[0].sort_key.as_deref(),
        Some("Sort%20key")
    );
    assert_eq!(
        result.metadata.language_links[0].resolved,
        "https://fr.example.test/wiki/Article"
    );
    let links: Vec<_> = nodes(&result.content.document)
        .filter_map(|node| {
            if let NodeKind::Link(link) = &node.kind {
                Some(link)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        links
            .iter()
            .filter(|link| link.destination == Destination::Local(AnchorId::new("here")))
            .count(),
        2
    );
    assert!(
        result
            .content
            .document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/wiki/image.png")
    );
    assert!(
        result
            .content
            .document
            .origins()
            .any(|(_, origin)| matches!(
                origin,
                Origin::Generated {
                    reason: GeneratedReason::LinkLabel,
                    related: Some(_)
                }
            ))
    );
    let overridden = import(
        &source,
        MediaWikiOptions {
            resource_base: Some("https://other.test/base/".into()),
            ..options()
        },
    )
    .unwrap();
    assert!(
        overridden
            .content
            .document
            .resources()
            .any(|(_, resource)| resource.identity == "https://other.test/base/image.png")
    );
    let redirect = import(
        &article("<link rel='mw:PageProp/redirect' href='./Destination'>"),
        options(),
    )
    .unwrap();
    assert_eq!(redirect.metadata.redirect.unwrap().raw, "./Destination");
    assert!(redirect.content.document.text().is_empty());
}

#[test]
fn vocabulary_aliases_rebinding_and_unknown_extension_boundaries() {
    let source = article(
        "<a prefix='x: http://mediawiki.org/rdf/' rel='x:ExtLink' class='external autonumber' href='https://outside.test'></a><span prefix='mw: https://unrelated.test/'><a rel='mw:ExtLink' class='autonumber' href='https://outside.test'></a></span><div typeof='mw:Extension/unknown'><a rel='mw:ExtLink' class='autonumber' href='https://outside.test'></a><span typeof='mw:Extension/math' data-mw='{\"name\":\"math\",\"body\":{\"extsrc\":\"hidden_tex\"}}'>visible fallback</span></div>",
    );
    let result = import(&source, options()).unwrap();
    assert_eq!(
        result
            .content
            .document
            .text()
            .as_str()
            .matches("[1]")
            .count(),
        1
    );
    assert!(!result.content.document.text().as_str().contains("[2]"));
    assert!(
        result
            .content
            .document
            .text()
            .as_str()
            .contains("visible fallback")
    );
    assert!(!nodes(&result.content.document).any(|node| matches!(node.kind, NodeKind::Math(_))));
    assert!(!result.content.diagnostics.is_empty());
}

#[test]
fn numbered_links_preserve_authored_content_and_generated_provenance() {
    let result = fragment(
        "<p><a rel='mw:ExtLink' class='autonumber' href='https://a.test'></a><a rel='mw:ExtLink' class='autonumber' href='https://b.test'>authored</a><a rel='mw:ExtLink' class='autonumber' href='https://c.test'></a></p>",
    );
    assert_eq!(result.content.document.text().as_str(), "[1]authored[2]");
    assert_eq!(
        result
            .content
            .document
            .origins()
            .filter(|(_, origin)| matches!(
                origin,
                Origin::Generated {
                    reason: GeneratedReason::LinkLabel,
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn cite_repeated_references_preserve_call_sites_bodies_and_multiple_backlinks() {
    let result = fragment(
        "<p>Text<sup typeof='mw:Extension/ref' id='ref-a'><a href='#note'>[1]</a></sup>Again<sup typeof='mw:Transclusion mw:Extension/ref' id='ref-b'><a href='#note'>[1]</a></sup></p><div typeof='mw:Extension/references'><ol><li id='note'><span class='mw-cite-backlink'><a href='#ref-a'>a</a><a href='#ref-b'>b</a></span><span>Note body</span></li></ol></div>",
    );
    let document = &result.content.document;
    let links: Vec<_> = nodes(document)
        .filter_map(|node| {
            if let NodeKind::Link(link) = &node.kind {
                Some(link)
            } else {
                None
            }
        })
        .collect();
    assert!(links.iter().any(|link| link.relation
        == LinkRelation::NoteReference {
            return_anchor: Some(AnchorId::new("ref-a"))
        }));
    assert!(links.iter().any(|link| link.relation
        == LinkRelation::NoteReference {
            return_anchor: Some(AnchorId::new("ref-b"))
        }));
    assert_eq!(
        links
            .iter()
            .filter(|link| link.relation == LinkRelation::NoteReturn)
            .count(),
        2
    );
    assert!(document.anchor(&AnchorId::new("note")).is_some());
    assert!(nodes(document).any(|node| {
        node.semantics
            .roles
            .contains(&SemanticRole::Known(KnownRole::Footnote))
    }));
    assert_eq!(document.text().as_str().matches("Note body").count(), 1);
    assert!(
        result.content.diagnostics.is_empty(),
        "{:?}",
        result.content.diagnostics
    );
    let incomplete =
        fragment("<p><sup typeof='mw:Extension/ref'><a href='#missing'>[1]</a></sup></p>");
    assert!(
        incomplete
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidDestination)
    );
}

#[test]
fn math_is_one_semantic_node_without_its_rendered_fallbacks() {
    let result = fragment(
        "<p>Before<span typeof='mw:Extension/math' id='formula' data-mw='{\"name\":\"math\",\"attrs\":{},\"body\":{\"extsrc\":\"x^2\"}}'><math><mi>duplicate</mi></math><img src='fallback.svg' alt='duplicate'></span>After</p><math display='block' typeof='mw:Extension/math' data-mw='{\"name\":\"math\",\"attrs\":{\"display\":\"block\"},\"body\":{\"extsrc\":\"a+b\"}}'><mi>duplicate</mi></math>",
    );
    let math: Vec<_> = nodes(&result.content.document)
        .filter_map(|node| {
            if let NodeKind::Math(math) = &node.kind {
                Some((math.source.as_str(), math.display))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        math,
        vec![("x^2", MathDisplay::Inline), ("a+b", MathDisplay::Block)]
    );
    assert!(
        !result
            .content
            .document
            .text()
            .as_str()
            .contains("duplicate")
    );
    assert!(
        result
            .content
            .document
            .anchor(&AnchorId::new("formula"))
            .is_some()
    );
    assert_eq!(result.content.document.resources().count(), 0);
    let malformed = fragment(
        "<span typeof='mw:Extension/math' data-mw='{'><img src='fallback.svg' alt='readable fallback'></span>",
    );
    assert!(
        malformed
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::UnsupportedMedia)
    );
}

#[test]
fn responsive_images_keep_order_conditions_sizes_and_fallback() {
    let result = fragment(
        "<figure><picture><source media='(min-width: 800px)' type='image/avif' srcset='wide.avif 800w, wider.avif 1600w' sizes='80vw'><img src='fallback.png' srcset='large.png 2x, bad.png 0x, huge.png 3x' alt='Photo' title='Image title'></picture><figcaption>Caption</figcaption></figure><img srcset='only.png 2x' alt='Only responsive'>",
    );
    let images: Vec<_> = nodes(&result.content.document)
        .filter_map(|node| {
            if let NodeKind::Image(image) = &node.kind {
                Some(image)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(images.len(), 2);
    let responsive = images[0].responsive.as_ref().unwrap();
    assert_eq!(
        responsive.sources[0].media.as_deref(),
        Some("(min-width: 800px)")
    );
    assert_eq!(
        responsive.sources[0].candidates.sizes.as_deref(),
        Some("80vw")
    );
    assert_eq!(responsive.fallback.as_ref().unwrap().candidates.len(), 3);
    assert!(result.content.document.text().as_str().contains("Caption"));
    assert!(!result.content.diagnostics.is_empty());
}

#[test]
fn inactive_content_foreign_content_and_malformed_html() {
    let result = fragment(
        "<script>NO SCRIPT</script><style>NO STYLE</style><template>NO TEMPLATE</template><p hidden>NO HIDDEN</p><p>A&nbsp;B<br>line<table>fostered<tr><td>cell</table><svg><title>Vector title</title></svg><p><a href='javascript:alert(1)'>Keep label</a><img src='javascript:no' alt='Keep alt'></p>",
    );
    let text = result.content.document.text().as_str();
    for excluded in ["NO SCRIPT", "NO STYLE", "NO TEMPLATE", "NO HIDDEN"] {
        assert!(!text.contains(excluded));
    }
    for included in [
        "A\u{a0}B",
        "line",
        "fostered",
        "cell",
        "Vector title",
        "Keep label",
        "Keep alt",
    ] {
        assert!(text.contains(included), "missing {included}: {text}");
    }
    assert!(
        result
            .content
            .document
            .resources()
            .all(|(_, resource)| !resource.identity.starts_with("javascript:"))
    );
}

#[test]
fn mixed_extension_flow_preserves_inline_runs_anchors_and_language() {
    let result = fragment(
        r#"<p id='wrapper' lang='ja'><strong>Before<math typeof='mw:Extension/math' display='block' data-mw='{"name":"math","body":{"extsrc":"x^2"}}'><mi>fallback</mi></math>After</strong></p><a href='./Other'><h2>Linked heading</h2><p>Linked paragraph</p></a>"#,
    );
    let document = &result.content.document;
    assert!(document.anchor(&AnchorId::new("wrapper")).is_some());
    assert!(nodes(document).any(|node| matches!(&node.semantics.language, LanguageOverride::Tag(tag) if tag.as_str() == "ja")));
    assert_eq!(nodes(document).filter(|node| matches!(&node.kind, NodeKind::InlineSemantic(semantic) if semantic.role == InlineRole::Strong)).count(), 2);
    assert_eq!(nodes(document).filter(|node| matches!(&node.kind, NodeKind::Math(math) if math.display == MathDisplay::Block)).count(), 1);
    assert!(nodes(document).any(
        |node| matches!(&node.kind, NodeKind::Link(link) if link.content == LinkContent::Flow)
    ));
    for text in ["Before", "After", "Linked heading", "Linked paragraph"] {
        assert!(document.text().as_str().contains(text));
    }
}

#[test]
fn fallback_nodes_keep_anchors_and_legacy_aliases() {
    let result = fragment(
        "<a id='new' name='old'></a><img id='broken' src='' alt='No image'><svg id='vector'><title>Vector</title><script>NO SCRIPT</script></svg><span id='formula' typeof='mw:Extension/math' data-mw='{'>single fallback</span>",
    );
    let document = &result.content.document;
    for target in ["new", "old", "broken", "vector", "formula"] {
        assert!(
            document.anchor(&AnchorId::new(target)).is_some(),
            "{target}"
        );
    }
    assert!(document.text().as_str().contains("No image"));
    assert!(!document.text().as_str().contains("NO SCRIPT"));
    assert_eq!(document.resources().count(), 0);
}

#[test]
fn table_associations_and_document_language_survive() {
    let result = import(&article("<table><thead><tr><th id='column' scope='col'>Column</th><th>Other</th></tr></thead><tbody><tr><td headers='column' rowspan='0'>Cell</td><td>A</td></tr><tr><td>B</td></tr></tbody></table><p><bdi dir='rtl'>Text</bdi></p>"), options()).unwrap();
    let document = &result.content.document;
    assert!(document.anchor(&AnchorId::new("column")).is_some());
    assert!(nodes(document).any(|node| matches!(&node.semantics.language, LanguageOverride::Tag(tag) if tag.as_str() == "en")));
    assert!(nodes(document).any(
        |node| matches!(&node.kind, NodeKind::Cell(cell) if cell.scope == Some(HeaderScope::Column))
    ));
    assert!(nodes(document).any(|node| matches!(&node.kind, NodeKind::Cell(cell) if cell.row_span.get() == 2 && cell.explicit_headers.as_ref().is_some_and(|headers| !headers.is_empty()))));
}

#[test]
fn empty_tables_and_rows_recover_without_aborting_the_article() {
    let result = fragment(
        "<p>Before</p><table id='empty'><caption>Empty caption</caption></table><table><tbody><tr><td rowspan='0'>Spanning cell</td></tr><tr></tr></tbody></table><p>After</p>",
    );
    for text in ["Before", "Empty caption", "Spanning cell", "After"] {
        assert!(result.content.document.text().as_str().contains(text));
    }
    assert!(
        result
            .content
            .document
            .anchor(&AnchorId::new("empty"))
            .is_some()
    );
    assert!(
        result
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidSpan)
    );
}

#[test]
fn foreign_page_properties_are_not_html_metadata() {
    let result = fragment(
        "<svg><link rel='mw:PageProp/Category' href='./Category:NotHTML'/><title>Vector</title></svg>",
    );
    assert!(result.metadata.categories.is_empty());
    assert!(result.content.document.text().as_str().contains("Vector"));
}

#[test]
fn source_retention_and_imports_are_deterministic() {
    let source = article(
        "<p id='target'>Text <a rel='mw:ExtLink' class='autonumber' href='https://outside.test'></a></p>",
    );
    let discarded = import(&source, options()).unwrap();
    let retained = import(
        &source,
        MediaWikiOptions {
            retention: SourceRetention::Retain,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(
        format!("{:#?}", discarded.content.document),
        format!("{:#?}", retained.content.document)
    );
    assert!(discarded.content.retained_sources.is_none());
    let (_, artifact) = retained.content.document.source_artifacts().next().unwrap();
    assert_eq!(
        retained
            .content
            .retained_sources
            .as_ref()
            .unwrap()
            .get(&artifact.revision)
            .unwrap(),
        source
    );
    assert!(
        retained
            .content
            .document
            .origins()
            .any(|(_, origin)| matches!(
                origin,
                Origin::Html {
                    serialized_range: None,
                    ..
                }
            ))
    );
}
