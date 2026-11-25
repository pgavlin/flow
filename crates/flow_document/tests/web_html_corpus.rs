//! Pinned semantic comparisons, not serialized-HTML snapshots.
#![cfg(feature = "web-html")]
#![allow(
    clippy::disallowed_methods,
    reason = "Integration-test host reads fixtures from the pinned Readability submodule"
)]
#![allow(
    clippy::print_stderr,
    reason = "Corpus comparisons emit investigation results under --nocapture"
)]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use dom_smoothie::dom_query::{Document, NodeRef};
use flow_document::{
    formats::web_html::{self, WebOptions},
    model::flow::{AnchorId, Destination, NodeKind},
};
use url::Url;

const SOURCE_URL: &str = "http://fakehost/test/page.html";
const PINNED_CASES: &[&str] = &[
    "basic-tags-cleaning",
    "base-url",
    "base-url-base-element",
    "base-url-base-element-relative",
    "normalize-spaces",
    "remove-aria-hidden",
    "remove-script-tags",
    "replace-brs",
];

fn corpus_root() -> PathBuf {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/readability/test/test-pages");
    assert!(
        root.join("basic-tags-cleaning/source.html").is_file(),
        "Readability corpus is missing; run `git submodule update --init --recursive` from the repository root"
    );
    root
}

fn compact(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn project_dom_text(root: NodeRef<'_>) -> String {
    let mut text = String::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.node_name().is_some_and(|name| &*name == "hr") {
            text.push('\u{fffc}');
        } else if node.node_name().is_some_and(|name| &*name == "img") {
            text.push_str(node.attr("alt").as_deref().unwrap_or("\u{fffc}"));
        } else if node
            .node_name()
            .is_some_and(|name| matches!(&*name, "iframe" | "audio" | "video"))
            && node.text().trim().is_empty()
            && dom_smoothie::dom_query::Selection::from(node)
                .select("img,math,br,iframe,audio,video")
                .nodes()
                .is_empty()
        {
            // Replaced media with no fallback still contributes an accessible
            // object or advisory label, not an invisible empty string.
            text.push_str(
                node.attr("title")
                    .filter(|title| !title.is_empty())
                    .as_deref()
                    .unwrap_or("\u{fffc}"),
            );
        } else if node.is_text() {
            text.push_str(&node.text());
        } else {
            pending.extend(node.children_it(true));
        }
    }
    text
}

fn difference_layer<T: PartialEq>(actual: &T, expected: &T, native: Option<&T>) -> &'static str {
    match native {
        Some(native) if native == expected => "pipeline",
        Some(native) if native == actual => "native extraction",
        Some(_) => "native extraction + pipeline",
        None => "native extraction unavailable",
    }
}

fn compare_case(root: &Path, name: &str) -> Vec<String> {
    let path = root.join(name);
    let source = std::fs::read_to_string(path.join("source.html")).unwrap();
    let expected = Document::from(std::fs::read_to_string(path.join("expected.html")).unwrap());
    let metadata: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path.join("expected-metadata.json")).unwrap(),
    )
    .unwrap();
    let imported = match web_html::import(
        &source,
        WebOptions {
            source_url: Some(SOURCE_URL.into()),
            ..Default::default()
        },
    ) {
        Ok(imported) => imported,
        Err(error) => return vec![format!("import: {error}")],
    };
    let native = dom_smoothie::Readability::with_document(
        Document::from(source.as_str()),
        Some(SOURCE_URL),
        Some(dom_smoothie::Config {
            keep_classes: true,
            ..Default::default()
        }),
    )
    .ok()
    .and_then(|readability| readability.extract().ok());
    let document = imported.content.document;
    let mut differences = Vec::new();
    let expected_text = compact(&project_dom_text(expected.root()));
    let native_text = native
        .as_ref()
        .map(|native| compact(&project_dom_text(native.root())));
    let actual_text = compact(document.text().as_str());
    if actual_text != expected_text {
        differences.push(format!(
            "text [{}] (actual {} bytes, expected {} bytes)",
            difference_layer(&actual_text, &expected_text, native_text.as_ref()),
            actual_text.len(),
            expected_text.len()
        ));
    }
    if metadata["title"].as_str() != Some(imported.metadata.title.as_str()) {
        let actual = Some(imported.metadata.title.as_str());
        let expected = metadata["title"].as_str();
        let native = native
            .as_ref()
            .map(|native| Some(native.metadata.title.as_str()));
        differences.push(format!(
            "title [{}]",
            difference_layer(&actual, &expected, native.as_ref())
        ));
    }
    if metadata["byline"].as_str() != imported.metadata.byline.as_deref() {
        let actual = imported.metadata.byline.as_deref();
        let expected = metadata["byline"].as_str();
        let native = native
            .as_ref()
            .map(|native| native.metadata.byline.as_deref());
        differences.push(format!(
            "byline [{}]",
            difference_layer(&actual, &expected, native.as_ref())
        ));
    }
    let source_dom = Document::from(source);
    let source_url = Url::parse(SOURCE_URL).unwrap();
    let base = source_dom
        .select("base[href]")
        .attr("href")
        .and_then(|href| source_url.join(&href).ok())
        .unwrap_or_else(|| source_url.clone());
    let destination = |href: &str| {
        let mut url = base.join(href).ok()?;
        if !matches!(url.scheme(), "http" | "https" | "mailto") {
            return None;
        }
        let fragment = url.fragment().map(str::to_owned);
        url.set_fragment(None);
        if url == source_url
            && let Some(fragment) = fragment
        {
            Some(Destination::Local(AnchorId::new(
                percent_encoding::percent_decode_str(&fragment)
                    .decode_utf8_lossy()
                    .into_owned(),
            )))
        } else {
            url.set_fragment(fragment.as_deref());
            Some(Destination::External(url.into()))
        }
    };
    let expected_links: BTreeSet<_> = expected
        .select("a[href]")
        .nodes()
        .iter()
        .filter_map(|node| {
            destination(&node.attr("href")?).map(|destination| format!("{destination:?}"))
        })
        .collect();
    let actual_links: BTreeSet<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Link(link) => Some(format!("{:?}", link.destination)),
            _ => None,
        })
        .collect();
    if actual_links != expected_links {
        if std::env::var_os("FLOW_READABILITY_CORPUS_VERBOSE").is_some() {
            eprintln!(
                "{name}: missing links {:?}; additional links {:?}",
                expected_links.difference(&actual_links).collect::<Vec<_>>(),
                actual_links.difference(&expected_links).collect::<Vec<_>>()
            );
        }
        let native_links = native.as_ref().map(|native| {
            native
                .selection()
                .select("a[href]")
                .nodes()
                .iter()
                .filter_map(|node| {
                    destination(&node.attr("href")?).map(|destination| format!("{destination:?}"))
                })
                .collect::<BTreeSet<_>>()
        });
        differences.push(format!(
            "link destinations [{}]",
            difference_layer(&actual_links, &expected_links, native_links.as_ref())
        ));
    }
    let expected_images: BTreeSet<_> = expected
        .select("img[src]")
        .nodes()
        .iter()
        .filter_map(|node| base.join(&node.attr("src")?).ok().map(String::from))
        .collect();
    // Compare the deterministic displayed fallback, not responsive candidates
    // that upstream expected HTML does not project into img src.
    let actual_images: BTreeSet<_> = document
        .preorder(document.root())
        .filter_map(|id| match &document.get_node(id).unwrap().kind {
            NodeKind::Image(image) => document.get_resource(image.resource),
            _ => None,
        })
        .map(|resource| resource.identity.clone())
        .collect();
    if actual_images != expected_images {
        if std::env::var_os("FLOW_READABILITY_CORPUS_VERBOSE").is_some() {
            eprintln!(
                "{name}: missing images {:?}; additional images {:?}",
                expected_images
                    .difference(&actual_images)
                    .collect::<Vec<_>>(),
                actual_images
                    .difference(&expected_images)
                    .collect::<Vec<_>>()
            );
        }
        let native_images = native.as_ref().map(|native| {
            native
                .selection()
                .select("img[src]")
                .nodes()
                .iter()
                .filter_map(|node| base.join(&node.attr("src")?).ok().map(String::from))
                .collect::<BTreeSet<_>>()
        });
        differences.push(format!(
            "image resources [{}]",
            difference_layer(&actual_images, &expected_images, native_images.as_ref())
        ));
    }
    let actual_headings = document
        .preorder(document.root())
        .filter(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Heading(_)))
        .count();
    let expected_headings = expected.select("h1,h2,h3,h4,h5,h6").nodes().len();
    if actual_headings != expected_headings {
        let native_headings = native
            .as_ref()
            .map(|native| native.selection().select("h1,h2,h3,h4,h5,h6").nodes().len());
        differences.push(format!(
            "heading count [{}]",
            difference_layer(
                &actual_headings,
                &expected_headings,
                native_headings.as_ref()
            )
        ));
    }
    differences
}

#[test]
fn pinned_readability_semantics() {
    let root = corpus_root();
    let failures: Vec<_> = PINNED_CASES
        .iter()
        .filter_map(|name| {
            let differences = compare_case(&root, name);
            (!differences.is_empty()).then(|| format!("{name}: {}", differences.join(", ")))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every pinned corpus input must construct valid IR. Differences against the
/// reference HTML are reported separately, not treated as extraction failures.
#[test]
fn readability_corpus_imports() {
    let root = corpus_root();
    let mut cases: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    cases.sort();
    let mut matched = 0;
    let mut different = 0;
    let mut import_errors = 0;
    for name in cases {
        let name = name.to_str().unwrap();
        if !root.join(name).join("expected.html").exists() {
            continue;
        }
        let differences = compare_case(&root, name);
        import_errors += usize::from(
            differences
                .iter()
                .any(|difference| difference.starts_with("import:")),
        );
        if differences.is_empty() {
            matched += 1;
        } else {
            different += 1;
            eprintln!("{name}: {}", differences.join(", "));
        }
    }
    eprintln!("Readability semantic comparison: {matched} matching, {different} differing");
    assert_eq!(matched + different, 130, "pinned corpus case count changed");
    assert_eq!(
        import_errors, 0,
        "corpus inputs must not fail IR construction"
    );
}
