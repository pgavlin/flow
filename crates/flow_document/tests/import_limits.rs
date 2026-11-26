use flow_document::formats::{flow_xhtml, markdown, xhtml};
use flow_document::model::flow::NodeKind;

#[test]
fn xml_budgets_are_host_selected() {
    let source = b"<div a='1' b='2'><p>text</p></div>";
    let defaults = xhtml::XhtmlLimits::default();
    for limits in [
        xhtml::XhtmlLimits {
            max_source_bytes: source.len() - 1,
            ..defaults
        },
        xhtml::XhtmlLimits {
            max_nodes: 2,
            ..defaults
        },
        xhtml::XhtmlLimits {
            max_depth: 1,
            ..defaults
        },
        xhtml::XhtmlLimits {
            max_attributes: 1,
            ..defaults
        },
    ] {
        assert!(matches!(
            xhtml::parse_with_options(
                source,
                xhtml::XhtmlParseOptions {
                    limits,
                    ..Default::default()
                }
            ),
            Err(xhtml::XhtmlParseError::TooComplex)
        ));
    }
    assert!(xhtml::parse(source).is_ok());
    let source = format!("<p>{}</p>", "x".repeat(32 * 1024 * 1024));
    assert!(xhtml::parse(source.as_bytes()).is_err());
    assert!(
        xhtml::parse_with_options(
            source.as_bytes(),
            xhtml::XhtmlParseOptions {
                limits: xhtml::XhtmlLimits {
                    max_source_bytes: source.len(),
                    ..defaults
                },
                ..Default::default()
            }
        )
        .is_ok()
    );
    for source in [b"<div/>".as_slice(), b"<div></div>".as_slice()] {
        assert!(
            xhtml::parse_with_options(
                source,
                xhtml::XhtmlParseOptions {
                    limits: xhtml::XhtmlLimits {
                        max_depth: 0,
                        ..defaults
                    },
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn markdown_budgets_are_host_selected() {
    for limits in [
        markdown::MarkdownLimits {
            max_nodes: 1,
            max_depth: 256,
        },
        markdown::MarkdownLimits {
            max_nodes: 100,
            max_depth: 0,
        },
    ] {
        assert!(matches!(
            markdown::parse(
                "text",
                markdown::MarkdownOptions {
                    limits,
                    ..Default::default()
                }
            ),
            Err(markdown::MarkdownError::TooComplex)
        ));
    }
    assert!(markdown::parse("text", Default::default()).is_ok());
}

#[test]
fn table_span_budget_reaches_finalization() {
    let source = "<table><tr><td colspan='4097'>text</td></tr></table>";
    for (maximum, expected) in [(None, 4096), (Some(2), 2), (Some(8192), 4097)] {
        let result = flow_xhtml::import(
            source,
            flow_xhtml::XhtmlOptions {
                max_table_columns: maximum,
                ..Default::default()
            },
        )
        .unwrap();
        let spans: Vec<_> = result
            .document
            .preorder(result.document.root())
            .filter_map(|id| match &result.document.get_node(id).unwrap().kind {
                NodeKind::Cell(cell) => Some(cell.column_span.get()),
                _ => None,
            })
            .collect();
        assert_eq!(spans, [expected]);
    }
}

#[cfg(feature = "web-html")]
#[test]
fn web_html_accepts_host_budgets() {
    use flow_document::formats::web_html::*;
    let source = format!(
        "<html><head><title>Article</title></head><body><article><h1>Article</h1><p>{}</p></article></body></html>",
        "Readable article sentence. ".repeat(150)
    );
    let limits = HtmlLimits {
        max_source_bytes: source.len() - 1,
        ..Default::default()
    };
    assert!(matches!(
        import(
            &source,
            WebOptions {
                limits,
                ..Default::default()
            }
        ),
        Err(WebImportError::TooComplex)
    ));
    assert!(import(&source, Default::default()).is_ok());
    let padded = format!("<!--{}-->{source}", "x".repeat(2 * 1024 * 1024));
    assert!(matches!(
        import(&padded, Default::default()),
        Err(WebImportError::TooComplex)
    ));
    assert!(
        import(
            &padded,
            WebOptions {
                limits: HtmlLimits {
                    max_source_bytes: padded.len(),
                    ..Default::default()
                },
                ..Default::default()
            }
        )
        .is_ok()
    );
}
