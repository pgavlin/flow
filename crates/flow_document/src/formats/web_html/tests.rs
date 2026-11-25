use super::*;
use crate::model::flow::{AnchorId, Destination, NodeKind, Origin};

fn article(extra: &str) -> String {
    format!("<!doctype html><html lang='en'><head><title>A useful article</title><meta name='author' content='A Writer'></head><body><nav>BOILERPLATE navigation</nav><article><h1>A useful article</h1>{}<h2>More details</h2>{extra}</article><footer>FOOTER navigation</footer></body></html>",
        "<p>This is a substantial article paragraph, with sentences, commas, and useful information for the reader. It is not navigation or advertising. We want to preserve the entire story and its ordinary reading structure.</p>".repeat(6))
}

#[test]
fn preserves_tex_math_before_extraction_removes_scripts_and_foreign_nodes() {
    let source = article(
        "<p>Inline <math><semantics><mi>x</mi><annotation encoding='application/x-tex'>x^2</annotation></semantics></math> and <script type='math/tex'>y_1</script>.</p><math display='block'><semantics><mi>z</mi><annotation encoding='application/x-tex'>z+1</annotation></semantics></math><script type='math/tex; mode=display'>a+b</script><script>DO_NOT_KEEP</script>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let math: Vec<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Math(math) => Some((math.source.as_str(), math.display)),
            _ => None,
        })
        .collect();
    assert_eq!(
        math,
        vec![
            ("x^2", MathDisplay::Inline),
            ("y_1", MathDisplay::Inline),
            ("z+1", MathDisplay::Block),
            ("a+b", MathDisplay::Block)
        ]
    );
    assert!(!document.text().as_str().contains("DO_NOT_KEEP"));
}

#[test]
fn keeps_srcset_only_images_and_legacy_anchor_targets() {
    let source = article(
        "<p><a name='legacy'></a>Target <img srcset='/small.png 1x, /large.png 2x' alt='Responsive photo'></p><p><a href='#legacy'>Jump</a></p>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert!(document.anchor(&AnchorId::new("legacy")).is_some());
    assert!(
        document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/small.png")
    );
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Image(image) if matches!(&image.alternative, crate::model::flow::ImageAlternative::Text(text) if text.as_str() == "Responsive photo"))));
}

#[test]
fn base_href_applies_to_fragment_only_links() {
    let source = article("<p><a href='#elsewhere'>Other document</a></p>")
        .replace("<head>", "<head><base href='https://other.test/article'>");
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.destination == Destination::External("https://other.test/article#elsewhere".into()))));
}

#[test]
fn preserves_inline_roles_titles_and_sections() {
    use crate::model::flow::InlineRole;
    let source = article(
        "<section id='part'><h3>Part</h3><p><del>deleted</del><ins>inserted</ins><kbd>keys</kbd><samp>output</samp><var>variable</var><dfn>definition</dfn><small>small print</small><a href='/next' title='Link title'>Next</a><img src='/photo.png' alt='Photo' title='Image title'></p></section>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    let kinds: Vec<_> = document
        .preorder(document.root())
        .map(|id| &document.get_node(id).unwrap().kind)
        .collect();
    for role in [
        InlineRole::Deletion,
        InlineRole::Insertion,
        InlineRole::KeyboardInput,
        InlineRole::SampleOutput,
        InlineRole::Variable,
        InlineRole::Definition,
        InlineRole::Small,
    ] {
        assert!(
            kinds.iter().any(
                |kind| matches!(kind, NodeKind::InlineSemantic(semantic) if semantic.role == role)
            ),
            "{role:?}"
        );
    }
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Section)));
    assert!(kinds.iter().any(
        |kind| matches!(kind, NodeKind::Link(link) if link.title.as_deref() == Some("Link title"))
    ));
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Image(image) if image.title.as_deref() == Some("Image title"))));
}

#[test]
fn retains_explicit_image_source_and_diagnoses_responsive_reduction() {
    let imported = import(
        &article("<p><img src='https://example.test/default.png' srcset='https://example.test/large.png 2x' alt='Photo'></p>"),
        Default::default(),
    ).unwrap();
    assert_eq!(imported.content.document.resources().len(), 1);
    assert_eq!(
        imported
            .content
            .document
            .resources()
            .next()
            .unwrap()
            .1
            .identity,
        "https://example.test/default.png"
    );
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(
                |diagnostic| diagnostic.kind == DiagnosticKind::DroppedNonTextualMetadata
                    && diagnostic.message.contains("responsive alternatives")
            )
    );
}

#[test]
fn srcset_fallback_parses_descriptors_and_data_urls() {
    for (input, expected) in [
        (" a.png 1x, b.png 2x", Some("a.png")),
        ("a.png, b.png", Some("a.png")),
        (
            "data:image/png;base64,AAAA 1x, b.png 2x",
            Some("data:image/png;base64,AAAA"),
        ),
        ("bad.png 0w, good.png 640w", Some("good.png")),
        ("bad.png nonsense, good.png 2x", Some("good.png")),
        ("bad.png 1x 2x", None),
        (" , , ", None),
    ] {
        assert_eq!(first_srcset_candidate(input), expected, "{input}");
    }
}

#[test]
fn preserves_checkbox_state_and_flow_links() {
    use crate::model::flow::{CheckboxState, LinkContent};
    let source = article(
        "<ul><li><input type='checkbox' checked disabled>Done</li><li><input type='checkbox' disabled>Pending</li></ul><a href='/card' title='Card'><h3>Card heading</h3><p>Card paragraph.</p></a>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    let states: Vec<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Checkbox(checkbox) => Some(checkbox.state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![CheckboxState::Checked, CheckboxState::Unchecked]
    );
    let card = document.preorder(document.root()).find(|id| matches!(&document.get_node(*id).unwrap().kind, NodeKind::Link(link) if link.content == LinkContent::Flow)).unwrap();
    let children = &document.get_node(card).unwrap().children;
    assert!(matches!(
        document.get_node(children[0]).unwrap().kind,
        NodeKind::Heading(_)
    ));
    assert!(matches!(
        document.get_node(children[1]).unwrap().kind,
        NodeKind::Paragraph
    ));
}

#[test]
fn preserves_table_header_associations_including_forward_references() {
    use crate::model::flow::{CellKind, HeaderScope};
    let source = article(
        "<table><caption>Measurements</caption><tbody><tr><td headers='later missing later'>42</td><th id='later' scope='row'>Value</th></tr><tr><th scope='rowgroup'>Group</th><td headers=''>None</td></tr></tbody></table>",
    );
    let imported = import(&source, Default::default()).unwrap();
    let document = &imported.content.document;
    let cells: Vec<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Cell(cell) => Some((id, cell)),
            _ => None,
        })
        .collect();
    assert_eq!(cells.len(), 4);
    assert_eq!(
        cells[0].1.explicit_headers.as_deref(),
        Some([cells[1].0].as_slice())
    );
    assert_eq!(cells[1].1.kind, CellKind::Header);
    assert_eq!(cells[1].1.scope, Some(HeaderScope::Row));
    assert_eq!(cells[2].1.scope, Some(HeaderScope::RowGroup));
    assert_eq!(cells[3].1.explicit_headers.as_deref(), Some([].as_slice()));
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidTableHeaderReference)
    );
}

#[test]
fn preserves_simple_and_multilevel_ruby() {
    let source = article(
        "<p><ruby>漢<rp>(</rp><rt>kan</rt><rp>)</rp>字<rt>ji</rt></ruby> <ruby><rb>東</rb><rb>京</rb><rt>とう</rt><rt>きょう</rt><rtc><rt rbspan='2'>Tokyo</rt></rtc></ruby></p>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let kinds: Vec<_> = document
        .preorder(document.root())
        .map(|id| &document.get_node(id).unwrap().kind)
        .collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, NodeKind::Ruby))
            .count(),
        2
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, NodeKind::RubySegment))
            .count(),
        3
    );
    assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::RubyAnnotation(annotation) if annotation.base_span.get() == 2)));
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, NodeKind::RubyAnnotation(_)))
            .count(),
        5
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, NodeKind::RubyFallback))
            .count(),
        2
    );
}

#[test]
fn restores_only_referenced_local_note_bodies_after_extraction() {
    use crate::model::flow::{KnownRole, LinkRelation, SemanticRole};
    let source = article(
        "<p>Article claim<a id='ref-1' role='doc-noteref' href='#note-1'>1</a>.</p><aside id='note-1' role='doc-footnote'><p>Retained note evidence. <a role='doc-backlink' href='#ref-1'>Return</a></p></aside><aside id='unused' role='doc-footnote'><p>UNRELATED NOTE</p></aside>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = &imported.content.document;
    assert!(document.text().as_str().contains("Retained note evidence."));
    assert!(!document.text().as_str().contains("UNRELATED NOTE"));
    assert!(document.anchor(&AnchorId::new("note-1")).is_some());
    assert!(document.preorder(document.root()).any(|id| {
        document
            .get_node(id)
            .unwrap()
            .semantics
            .roles
            .contains(&SemanticRole::Known(KnownRole::Footnote))
    }));
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.relation == LinkRelation::NoteReference { return_anchor: Some(AnchorId::new("ref-1")) })));
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.relation == LinkRelation::NoteReturn)));
}

#[test]
fn diagnoses_missing_notes_without_importing_remote_or_hidden_bodies() {
    let source = article(
        "<p>Claim <a role='doc-noteref' href='#missing'>missing</a> <a role='doc-noteref' href='#hidden-note'>hidden</a><a role='doc-noteref' href='https://other.test/story#remote-note'>remote</a></p><aside hidden id='hidden-note'>HIDDEN EVIDENCE</aside><aside id='remote-note'>REMOTE EVIDENCE</aside>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !imported
            .content
            .document
            .text()
            .as_str()
            .contains("HIDDEN EVIDENCE")
    );
    assert!(
        !imported
            .content
            .document
            .text()
            .as_str()
            .contains("REMOTE EVIDENCE")
    );
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidDestination)
    );
}

#[test]
fn malformed_ruby_falls_back_with_a_diagnostic_and_keeps_anchors() {
    let source = article(
        "<p><ruby id='bad-ruby'><rb>base</rb><rt rbspan='99'>annotation</rt></ruby> <ruby id='good-ruby'><span id='base-anchor'>漢</span><rt>kan</rt></ruby></p>",
    );
    let imported = import(&source, Default::default()).unwrap();
    assert!(
        imported
            .content
            .document
            .text()
            .as_str()
            .contains("baseannotation")
    );
    for id in ["bad-ruby", "good-ruby", "base-anchor"] {
        assert!(
            imported
                .content
                .document
                .anchor(&AnchorId::new(id))
                .is_some()
        );
    }
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidRubyStructure)
    );
    assert!(
        !imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::DuplicateAnchor)
    );
}

#[test]
fn normalization_does_not_guess_that_nested_or_prose_tables_are_layout() {
    let document = Document::from(
        "<table id='outer'><tr><td><p>Prose data.</p></td><td><table id='inner'><tr><td><p>Nested data.</p></td></tr></table></td></tr></table>",
    );
    let root = document.root().id;
    let extracted = dom_smoothie::Extracted {
        document,
        root,
        metadata: Default::default(),
    };
    normalize_article(&extracted, "");
    assert_eq!(extracted.selection().select("table").nodes().len(), 2);
    let document = Document::from(
        "<table id='layout' role='presentation'><tr><td id='cell'>First</td><td>Second</td></tr></table>",
    );
    let root = document.root().id;
    let extracted = dom_smoothie::Extracted {
        document,
        root,
        metadata: Default::default(),
    };
    normalize_article(&extracted, "");
    assert!(extracted.selection().select("table").nodes().is_empty());
    assert_eq!(extracted.selection().select("#layout").nodes().len(), 1);
    assert_eq!(extracted.selection().select("#cell").nodes().len(), 1);
    assert!(extracted.root().text().contains("FirstSecond"));
}

#[test]
fn incremental_parser_bounds_arena_work_and_utf8_batches() {
    assert!(matches!(
        parse_bounded(&"<i></i>".repeat(MAX_NODES)),
        Err(WebImportError::TooComplex)
    ));
    let source = format!("<p>{}</p>", "字".repeat(PARSE_BATCH_BYTES));
    let document = parse_bounded(&source).unwrap();
    assert_eq!(
        document.select("p").text().as_ref(),
        "字".repeat(PARSE_BATCH_BYTES)
    );
    let mut exhausted = MAX_PARSE_VISITS;
    assert!(matches!(
        check_dom_with_work(document.root(), &mut exhausted),
        Err(WebImportError::TooComplex)
    ));
}

#[test]
fn exposes_extracted_metadata_without_fetching_declared_resources() {
    let source = article("<p>Metadata story.</p>").replace("<head>", "<head><meta property='article:published_time' content='2026-01-02T03:04:05Z'><meta property='article:modified_time' content='2026-01-03T03:04:05Z'><meta property='og:url' content='https://example.test/canonical'><meta property='og:image' content='/cover.png'><link rel='icon' href='/favicon.ico'>");
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        imported.metadata.published_time.as_deref(),
        Some("2026-01-02T03:04:05Z")
    );
    assert_eq!(
        imported.metadata.modified_time.as_deref(),
        Some("2026-01-03T03:04:05Z")
    );
    assert_eq!(
        imported.metadata.url.as_deref(),
        Some("https://example.test/canonical")
    );
    assert_eq!(
        imported.metadata.image.as_deref(),
        Some("https://example.test/cover.png")
    );
    assert_eq!(
        imported.metadata.favicon.as_deref(),
        Some("https://example.test/favicon.ico")
    );
    assert_eq!(imported.content.document.resources().len(), 0);
}

#[test]
fn preserves_mathml_without_tex_annotations() {
    let source =
        article("<p>Formula <math id='formula'><mfrac><mi>a</mi><mi>b</mi></mfrac></math>.</p>");
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(document.anchor(&AnchorId::new("formula")).is_some());
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Math(math) if math.notation == MathNotation::MathMl && math.source.contains("<mfrac>"))));
}

#[test]
fn malformed_extracted_sequences_remain_readable_instead_of_failing_the_article() {
    let source = article(
        "<ul id='empty-list'></ul><dl id='indent'><dd>Indented evidence.</dd></dl><table id='empty-table'><caption>Empty table explanation.</caption></table>",
    );
    let imported = import(&source, Default::default()).unwrap();
    assert!(
        imported
            .content
            .document
            .text()
            .as_str()
            .contains("Indented evidence.")
    );
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidChild)
    );
}

#[test]
fn preserves_paired_hidden_math_source_without_duplicating_visual_fallback() {
    let source = article(
        "<p>Formula <span><span style='display:none'><math alttext='x+1'><semantics><mi>x</mi><annotation encoding='application/x-tex'>x+1</annotation></semantics></math></span><img aria-hidden='true' src='/formula.svg' alt='x+1'></span>.</p>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Math(math) if math.source == "x+1")));
    assert_eq!(document.resources().len(), 0);
    assert_eq!(document.text().as_str().matches("x+1").count(), 1);
}

#[test]
fn hoists_block_math_and_keeps_inline_code_links_phrasing() {
    let source = article(
        "<p id='math-paragraph'>Before <nobr><span><math display='block'><mi>x</mi></math></span></nobr> after.</p><p><a href='/api'><code>api()</code></a></p>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert!(document.anchor(&AnchorId::new("math-paragraph")).is_some());
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Math(math) if math.display == MathDisplay::Block)));
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.content == LinkContent::Phrasing && link.destination == Destination::External("https://example.test/api".into()))));
}

#[test]
fn image_links_survive_repairs_and_blocked_images_keep_alternatives() {
    let imported = import(&article("<p><a href='/gallery' title='Gallery'><img src='/photo.png' alt='Photo'></a> <img src='javascript:bad()' alt='Unavailable diagram'></p>"), WebOptions { source_url: Some("https://example.test/story".into()), ..Default::default() }).unwrap();
    let document = &imported.content.document;
    assert!(document.text().as_str().contains("Unavailable diagram"));
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.title.as_deref() == Some("Gallery"))));
    assert_eq!(document.resources().len(), 1);
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::UnsupportedMedia)
    );
}

#[test]
fn math_repairs_retain_original_element_identity_without_private_payload_attributes() {
    let source = article("<p><math id='original-math'><mi>x</mi></math></p>");
    let original = Document::from(source.as_str());
    let index = original
        .select("*")
        .nodes()
        .iter()
        .position(|node| node.attr("id").is_some_and(|id| &*id == "original-math"))
        .unwrap();
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let math = document
        .preorder(document.root())
        .find_map(|id| {
            let node = document.get_node(id).unwrap();
            matches!(node.kind, NodeKind::Math(_)).then_some(node)
        })
        .unwrap();
    assert!(
        matches!(document.get_origin(math.origin.unwrap()).unwrap(), Origin::Html { source_node, serialized_range: None, .. } if source_node.0 == format!("html:element:{index}"))
    );
    assert!(
        matches!(&math.kind, NodeKind::Math(math) if !math.source.contains(SOURCE_NODE_ATTRIBUTE))
    );
}

#[test]
fn restores_transitive_and_percent_encoded_note_targets_once() {
    let source = article(
        "<p>Claim <a role='doc-noteref' href='#note%201'>1</a></p><aside id='note 1'><p>First note <a role='doc-noteref' href='#note-2'>2</a></p></aside><aside id='note-2'><p>Second note <a role='doc-noteref' href='#note%201'>1</a></p></aside>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = &imported.content.document;
    for name in ["note 1", "note-2"] {
        assert!(document.anchor(&AnchorId::new(name)).is_some());
    }
    assert_eq!(document.text().as_str().matches("First note").count(), 1);
    assert_eq!(document.text().as_str().matches("Second note").count(), 1);
    assert!(
        !imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidDestination)
    );
}

#[test]
fn distinct_id_and_legacy_name_both_remain_targets() {
    let source = article(
        "<p><a id='modern' name='legacy'>Named location</a> <a href='#modern'>modern</a> <a href='#legacy'>legacy</a></p>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(document.anchor(&AnchorId::new("modern")).is_some());
    assert!(document.anchor(&AnchorId::new("legacy")).is_some());
}

#[test]
fn figures_preserve_linked_images_and_arbitrary_flow_content() {
    let source = article(
        "<figure><span>Illustration label</span><a href='/full'><img src='/plot.png' alt='Plot'></a><div><h3>Supporting heading</h3><p>Supporting explanation.</p></div><figcaption>Caption one</figcaption><figcaption>Caption two</figcaption></figure>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = &imported.content.document;
    for text in [
        "Illustration label",
        "Plot",
        "Supporting heading",
        "Supporting explanation.",
        "Caption one",
        "Caption two",
    ] {
        assert!(document.text().as_str().contains(text), "{text}");
    }
    assert!(
        document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/plot.png")
    );
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.destination == Destination::External("https://example.test/full".into()))));
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidChild)
    );
}

#[test]
fn preformatted_content_includes_all_code_siblings_and_explicit_breaks() {
    let source =
        article("<pre><code>first<br>second<p>third</p></code><code>fourth</code>tail</pre>");
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let pre = document
        .preorder(document.root())
        .find_map(|id| {
            let node = document.get_node(id).unwrap();
            matches!(node.kind, NodeKind::Preformatted(_)).then_some(node)
        })
        .unwrap();
    let text =
        &document.text().as_str()[pre.text_range.start.as_usize()..pre.text_range.end.as_usize()];
    for part in ["first", "second", "third", "fourth", "tail"] {
        assert!(text.contains(part), "{text}");
    }
    assert!(text.contains("first\nsecond"), "{text}");
}

#[test]
fn preformatted_blank_lines_survive_extraction_break_cleanup() {
    let document = import(
        &article("<pre><code>first<br><br>second</code></pre>"),
        Default::default(),
    )
    .unwrap()
    .content
    .document;
    assert!(document.text().as_str().contains("first\n\nsecond"));
}

#[test]
fn preserves_break_opportunities_bidi_and_inline_metadata() {
    let source = article(
        "<p>long<wbr>word <abbr title='Hypertext Markup Language'>HTML</abbr> <time datetime=''>today</time> <bdi>عربي</bdi> <bdo dir='rtl'>abc</bdo></p>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let nodes: Vec<_> = document
        .preorder(document.root())
        .map(|id| document.get_node(id).unwrap())
        .collect();
    assert!(nodes.iter().any(|node| matches!(
        node.kind,
        NodeKind::BreakOpportunity(BreakOpportunity {
            hyphen_when_broken: false
        })
    )));
    assert!(nodes.iter().any(|node| matches!(&node.kind, NodeKind::InlineSemantic(semantic) if semantic.role == InlineRole::Abbreviation && semantic.value.as_deref() == Some("Hypertext Markup Language"))));
    assert!(nodes.iter().any(|node| matches!(&node.kind, NodeKind::InlineSemantic(semantic) if semantic.role == InlineRole::Time && semantic.value.is_none())));
    for bidi in [
        BidiState {
            direction: BaseDirection::Auto,
            mode: BidiMode::Isolate,
        },
        BidiState {
            direction: BaseDirection::RightToLeft,
            mode: BidiMode::Override,
        },
    ] {
        assert!(
            nodes.iter().any(
                |node| document.get_style_input(node.style_input).unwrap().bidi() == Some(bidi)
            )
        );
    }
    assert!(document.text().as_str().contains("longword"));
}

#[test]
fn preformatted_links_anchors_and_literal_spaces_survive() {
    let source = article(
        "<pre>  start <a id='code-target' href='/reference'>link  text</a> end\nnext</pre>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert!(
        document
            .text()
            .as_str()
            .contains("  start link  text end\nnext")
    );
    assert!(document.anchor(&AnchorId::new("code-target")).is_some());
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.destination == Destination::External("https://example.test/reference".into()))));
}

#[test]
fn display_math_splits_preformatted_containers_without_invalid_inline_ir() {
    let source = article("<pre>before<math display='block'><mi>x</mi></math>after</pre>");
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(document.text().as_str().contains("before"));
    assert!(document.text().as_str().contains("after"));
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Math(math) if math.display == MathDisplay::Block)));
}

#[test]
fn invisible_breaks_do_not_turn_leading_whitespace_into_content() {
    let source = article("<p><wbr> Leading text</p><p>Trailing <wbr> text</p>");
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(document.text().as_str().contains("\n\nLeading text"));
    assert!(document.text().as_str().contains("Trailing text"));
    assert!(!document.text().as_str().contains("\n\n Leading"));
}

#[test]
fn restored_notes_do_not_resurrect_hidden_descendants() {
    let source = article(
        "<p>Claim <a role='doc-noteref' href='#visible-note'>1</a> <a role='doc-noteref' href='#hidden-note'>2</a></p><aside id='visible-note'><p>Visible evidence. <span style='display:/**/none !IMPORTANT'>HIDDEN CHILD</span><span style='visibility:hidden'>HIDDEN VISIBILITY</span></p></aside><aside id='hidden-note' style='display:none !IMPORTANT'>HIDDEN NOTE</aside>",
    );
    let imported = import(&source, Default::default()).unwrap();
    let text = imported.content.document.text().as_str();
    assert!(text.contains("Visible evidence."));
    for hidden in ["HIDDEN CHILD", "HIDDEN VISIBILITY", "HIDDEN NOTE"] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidDestination)
    );
}

#[test]
fn restored_list_notes_keep_flow_structure_and_explicit_roles() {
    let source = article(
        "<p>Claim <a role='doc-noteref' href='#endnote'>1</a></p><div class='footnotes'><ol><li id='endnote' role='doc-endnote'><p>First evidence paragraph.</p><p>Second evidence paragraph.</p></li></ol></div>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(
        document
            .text()
            .as_str()
            .contains("First evidence paragraph.\n\nSecond evidence paragraph.")
    );
    let note = document
        .preorder(document.root())
        .map(|id| document.get_node(id).unwrap())
        .find(|node| {
            node.semantics
                .roles
                .contains(&SemanticRole::Known(KnownRole::Endnote))
        })
        .unwrap();
    assert!(
        note.children
            .iter()
            .filter(|id| matches!(document.get_node(**id).unwrap().kind, NodeKind::Paragraph))
            .count()
            >= 2
    );
}

#[test]
fn media_placeholders_keep_inline_fallback_links_and_empty_titles_are_valid() {
    let source = article(
        "<video title=''><source src='/movie.mp4'>Video fallback <a href='/download'>Download video</a>.</video><audio title=''><source src='/sound.ogg'>Audio fallback.</audio>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = &imported.content.document;
    assert!(
        document
            .text()
            .as_str()
            .contains("Video fallback Download video.")
    );
    assert!(document.text().as_str().contains("Audio fallback."));
    assert!(document.preorder(document.root()).any(|id| matches!(&document.get_node(id).unwrap().kind, NodeKind::Link(link) if link.destination == Destination::External("https://example.test/download".into()))));
    assert!(
        imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::UnsupportedMedia)
    );
}

#[test]
fn ruby_inside_preformatted_content_keeps_literal_spacing() {
    let source = article("<pre><ruby><rb>漢  字</rb><rt>kan  ji</rt></ruby></pre>");
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    assert!(document.text().as_str().contains("漢  字"));
    assert!(document.text().as_str().contains("kan  ji"));
}

#[test]
fn only_empty_anchor_targets_are_renamed_before_extraction() {
    let document = Document::from("<p><a id='target'></a><a id='choice'>Choice text</a></p>");
    repair_before_extraction(&document, None, None).unwrap();
    assert!(document.select("#target").nodes()[0].has_name("span"));
    assert!(document.select("#choice").nodes()[0].has_name("a"));
}

#[test]
fn zero_rowspan_extends_to_the_end_of_its_own_row_group() {
    let source = article(
        "<table><tbody><tr><th rowspan=' 0 ' scope='rowgroup'>Group</th><td>First</td></tr><tr><td>Second</td></tr></tbody><tbody><tr><td>Other group</td><td>End</td></tr></tbody></table>",
    );
    let imported = import(&source, Default::default()).unwrap();
    let document = &imported.content.document;
    let header = document
        .preorder(document.root())
        .find_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Cell(cell) if cell.kind == crate::model::flow::CellKind::Header => Some(cell),
            _ => None,
        })
        .unwrap();
    assert_eq!(header.row_span.get(), 2);
    assert!(
        !imported
            .content
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidSpan)
    );
}

#[test]
fn extracts_metadata_and_lowers_html5_dom_directly() {
    let source = article(
        "<p>A&nbsp;<b>bold</b> thought with <i>emphasis</i> and a<br>line break.</p><script>EVIL()</script>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/articles/story".into()),
            retention: SourceRetention::Retain,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(imported.metadata.title, "A useful article");
    assert_eq!(imported.metadata.byline.as_deref(), Some("A Writer"));
    let document = &imported.content.document;
    let text = document.text().as_str();
    assert!(
        text.contains("A\u{a0}bold thought with emphasis and a\nline break."),
        "{text}"
    );
    assert!(!text.contains("BOILERPLATE"));
    assert!(!text.contains("EVIL"));
    assert!(document.anchor(&AnchorId::new("more-details")).is_some());
    assert!(
        document
            .origins()
            .all(|(_, origin)| !matches!(origin, Origin::Xhtml { .. }))
    );
    assert!(document.origins().any(|(_, origin)| matches!(
        origin,
        Origin::Html {
            serialized_range: None,
            ..
        }
    )));
    let artifact = document.source_artifacts().next().unwrap().1;
    assert_eq!(artifact.media_type.as_deref(), Some("text/html"));
    assert_eq!(
        imported
            .content
            .retained_sources
            .unwrap()
            .get(&artifact.revision),
        Some(source.as_str())
    );
}

#[test]
fn resolves_resources_and_keeps_self_fragments_local() {
    let source = article(
        "<p><a href='https://example.test/story#details'>self</a> <a href='/next'>next</a> <a href='javascript:alert(1)'>unsafe</a></p><p><img src='images/photo.png' alt='A photo'></p><h2 id='details'>Detail</h2>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = imported.content.document;
    let links: Vec<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Link(link) => Some(&link.destination),
            _ => None,
        })
        .collect();
    assert!(
        links.contains(&&Destination::Local(AnchorId::new("details"))),
        "{links:?}"
    );
    assert!(links.contains(&&Destination::External("https://example.test/next".into())));
    assert!(
        !links.iter().any(
            |link| matches!(link, Destination::External(url) if url.starts_with("javascript:"))
        )
    );
    assert!(
        document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/images/photo.png")
    );
}

#[test]
fn ports_button_and_image_repairs_before_extraction() {
    let source = article(
        "<div class='wp-block-buttons'><div class='wp-block-button'><a class='wp-block-button__link' href='/full'>Read the complete story</a></div></div><p><a href='/gallery'><picture><source srcset='large.png'><img src='/photo.png' alt='Rescued photo'></picture></a></p>",
    );
    let imported = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/story".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let document = imported.content.document;
    assert!(document.text().as_str().contains("Read the complete story"));
    assert!(
        document
            .resources()
            .any(|(_, resource)| resource.identity == "https://example.test/photo.png")
    );
}

#[test]
fn rejects_unsupported_bases_and_limits_before_extraction() {
    assert!(matches!(
        import(
            "hello",
            WebOptions {
                source_url: Some("file:///tmp/article".into()),
                ..Default::default()
            }
        ),
        Err(WebImportError::InvalidUrl)
    ));
    assert!(matches!(
        import(&"x".repeat(MAX_SOURCE + 1), Default::default()),
        Err(WebImportError::TooComplex)
    ));
    let deeply_nested = format!(
        "{}text{}",
        "<div>".repeat(MAX_DEPTH + 1),
        "</div>".repeat(MAX_DEPTH + 1)
    );
    assert!(matches!(
        import(&deeply_nested, Default::default()),
        Err(WebImportError::TooComplex)
    ));
}

#[test]
fn separates_layout_tables_from_data_tables() {
    let source = article(
        "<table role='presentation'><tr><td><p>Layout prose.</p></td></tr></table><table><thead><tr><th>Label</th><th>Value</th></tr></thead><tbody><tr><td>Data</td><td>42</td></tr></tbody></table>",
    );
    let document = import(&source, Default::default())
        .unwrap()
        .content
        .document;
    let tables = document
        .preorder(document.root())
        .filter(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Table(_)))
        .count();
    assert_eq!(tables, 1);
    assert!(document.text().as_str().contains("Layout prose."));
    assert!(document.text().as_str().contains("42"));
}

#[test]
fn document_base_resolves_images_without_loading_them() {
    let source = article(
        "<p><img src='photo.png' alt='Photo'><img src='javascript:bad()' alt='Rejected'></p>",
    )
    .replace(
        "<head>",
        "<head><base href='https://assets.example.test/root/'>",
    );
    let document = import(
        &source,
        WebOptions {
            source_url: Some("https://example.test/article".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .content
    .document;
    assert_eq!(
        document.resource_base(),
        Some("https://assets.example.test/root/")
    );
    assert_eq!(document.resources().len(), 1);
    assert_eq!(
        document.resources().next().unwrap().1.identity,
        "https://assets.example.test/root/photo.png"
    );
}
